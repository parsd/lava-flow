﻿# lava-flow Architecture

## TL;DR

Two-layer design separating memory allocation (Layer 1: GPU + CPU) from channel transports (Layer 2: Vulkan
IPC, CPU shared memory, MPI). Scope detection routes local traffic through Vulkan IPC or CPU shared memory and remote
traffic through MPI.

## Two-Layer Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│ Layer 2: Channel Transports                                 │
│                                                             │
│  Same Host                    Different Host                │
│  ┌────────────────────┐      ┌────────────────────┐         │
│  │ Vulkan IPC         │      │ MPI Messaging      │         │
│  │ < 100 ns latency   │      │ ~10 us latency     │         │
│  │ GPU <-> GPU direct │      │ Process <-> Process│         │
│  └────────────────────┘      └────────────────────┘         │
│           ↑                            ↑                    │
│           └────────────┬───────────────┘                    │
│                        │ Scope Detection                    │
│                        │ (Compare hostnames)                │
│                        ↓                                    │
├─────────────────────────────────────────────────────────────┤
│ Layer 1: Memory & Allocator (Phase 2)                       │
│                                                             │
│  GPU Memory (Vulkan)          CPU Memory (Host)             │
│  ┌──────────────────────┐     ┌──────────────────────┐      │
│  │ DEVICE_LOCAL         │     │ Standard (malloc)    │      │
│  │ 100-900 GB/s         │     │ 50-100 GB/s          │      │
│  │                      │     │                      │      │
│  │ HOST_VISIBLE         │     │ NUMA-aware           │      │
│  │ 10-50 GB/s (PCIe)    │     │ 10-100 GB/s          │      │
│  │                      │     │                      │      │
│  │ External export      │     │ Hugepages            │      │
│  │ (fd/HANDLE)          │     │ 50-100 GB/s          │      │
│  │                      │     │                      │      │
│  │                      │     │ GPU-pinned           │      │
│  │                      │     │ PCIe bandwidth       │      │
│  └──────────────────────┘     └──────────────────────┘      │
│                  ↑                         ↑                │
│                  └────────────┬────────────┘                │
│                               │ Unified API                 │
│                               ↓                             │
│                      MemoryAllocator                        │
└─────────────────────────────────────────────────────────────┘
```

Note: local CPU channels use OS shared memory, while local GPU channels use Vulkan IPC.

## Layer 1: Memory & Allocator

### Responsibility

Allocate GPU and CPU memory with:

- Single unified API
- Cross-API GPU memory (importable by CUDA, OpenCL, OpenGL)
- Multi-strategy CPU allocation (performance-tuned per scenario)
- Transparent fallback (if GPU unavailable, use CPU)

### GPU Memory (Vulkan-Based)

#### VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT

- Fastest GPU memory (on GPU)
- 100-900 GB/s throughput
- Only GPU-accessible
- Ideal for GPU-GPU communication

#### VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT

- CPU-accessible via PCIe
- 10-50 GB/s throughput
- Useful for CPU→GPU staging

#### External Memory Export

```text
Linux:  VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT (file descriptor)
Windows: VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT (HANDLE)
```

Enables GPU memory sharing: Process A exports → Process B imports via OS handle.

### CPU Memory (Host Buffers) — Four Strategies

| Strategy | Throughput | Latency | Use Case |
| -------- | ---------- | ------- | -------- |
| **Standard** | 50-100 GB/s | 1-10 us | Default, small buffers |
| **NUMA** | 10-100 GB/s | Reduced | Multi-socket systems |
| **Hugepages** | 50-100 GB/s | 1-10 us | Large buffers (remote) |
| **GPU-pinned** | PCIe bandwidth | PCIe latency | CPU→GPU staging |

### Unified API

```rust
pub trait MemoryAllocator {
    // Explicit allocation with location hint
    fn allocate(&mut self, size: usize, location: MemoryLocation)
        -> Result<Box<dyn MemoryBuffer>, Error>;

    // Automatic based on scope detection
    fn allocate_auto(&mut self, size: usize,
                     my_location: &ProcessLocation,
                     peer_location: &ProcessLocation)
        -> Result<Box<dyn MemoryBuffer>, Error>;
}
```

## Layer 2: Channel Transports

### Responsibility

Provide producer/consumer message passing with:

- Automatic transport selection (Vulkan IPC, CPU shared memory, or MPI)
- Non-blocking API (async/await integration)
- Guaranteed message ordering
- Proper error handling and recovery

### Local Transport: Vulkan IPC

**For:** GPU-to-GPU on same host

#### How it works

1. Process A allocates GPU memory (Vulkan, DEVICE_LOCAL)
2. Exports external memory handle (file descriptor on Linux)
3. Process B imports handle, gains access to same GPU memory
4. Message is GPU memory pointer + metadata
5. Receiver uses pointer directly (zero-copy)

#### Performance

```text
Latency:    < 100 ns (shared GPU memory)
Throughput: 100-900 GB/s (full GPU bandwidth)
Copy cost:  0 bytes
```

**See:** [ADR-003: External Memory Handle Types](../adr/003-external-memory-handle-types.md)

### Local Transport: CPU Shared Memory

**For:** CPU-to-CPU on same host

#### How it works

1. Process A allocates CPU memory (standard or NUMA)
2. Uses OS shared memory to expose the buffer to Process B
3. Metadata + synchronization sent via shared control channel
4. Receiver reads directly from shared RAM (zero-copy)

#### Performance

```text
Latency:    1-10 us (shared memory, no network)
Throughput: 50-100 GB/s (host memory bandwidth)
Copy cost:  0 bytes
```

### Remote Transport: MPI

**For:** CPU-to-CPU on different hosts

#### How it works

1. Process A allocates CPU memory (hugepages for remote)
2. Calls `MPI_Send()` with buffer pointer
3. MPI runtime transfers data (RDMA if available, TCP fallback)
4. Process B receives via `MPI_Recv()`
5. Data in receiver's memory space

#### Performance

```text
Latency:    ~10 us (network + MPI overhead)
Throughput: 1-100 GB/s (network-dependent)
Copy cost:  1x PCIe for staging, 1x network transfer
```

#### Benefits

- Proven, stable for HPC
- Vendor optimizations (OpenMPI with UCX)
- GPU-aware MPI available
- Process-level sync (not just collective operations)

**See:** [ADR-006: MPI for Remote Communication](../adr/006-mpi-for-inter-node.md)

## Scope Detection

### Algorithm

```rust
pub fn detect_scope(
    my_location: &ProcessLocation,     // e.g., "gpu-node-0"
    peer_location: &ProcessLocation,   // e.g., "gpu-node-1"
) -> CommunicationScope {
    if my_location.hostname == peer_location.hostname {
        CommunicationScope::Local
    } else {
        CommunicationScope::Remote
    }
}
```

### Data Flow

```text
Input:  ProcessLocation { hostname, node_id, device_id }
        ProcessLocation { hostname, node_id, device_id }
              ↓
       Compare hostnames
              ↓
    ┌─────────┴─────────┐
    ↓                   ↓
  Same?             Different?
    ↓                   ↓
  Local               Remote
    ↓                   ↓
Vulkan IPC/        MPI Transport
CPU shared memory
```

### Usage

```rust
// Users provide location info
let my_loc = ProcessLocation::from_hostname();  // Detects automatically
let peer_loc = ProcessLocation::from_config("path/to/json");

// Library detects scope automatically
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
// -> If same host: Vulkan IPC for GPU buffers or CPU shared memory for CPU buffers
// -> If different host: MPI transport
```

### Advantages

- No user configuration needed
- Single codebase works everywhere
- Optimal transport per scenario
- Transparent switching

**See:** [ADR-001: Scope-Based Backend Selection](../adr/001-scope-detection.md)

## Memory Type Selection Matrix

### Local (Same Host)

| Source | Destination | Memory Type | Transport | Latency | Throughput |
| ------ | ----------- | ----------- | --------- | ------- | ---------- |
| GPU | GPU (same host) | Vulkan GPU | Vulkan IPC | < 100 ns | 100-900 GB/s |
| CPU | CPU (same host) | CPU standard | Shared memory | 1-10 us | 50-100 GB/s |
| CPU | CPU (multi-socket) | CPU NUMA | Shared memory | 5-100 us | 10-100 GB/s |
| CPU | GPU (same host) | CPU GPU-pinned | PCIe | PCIe latency | PCIe bandwidth |

### Remote (Different Hosts)

| Source | Destination | Memory Type | Transport | Latency | Throughput |
| ------ | ----------- | ----------- | --------- | ------- | ---------- |
| CPU | CPU (different host) | CPU hugepages | MPI | ~10 us | 1-100 GB/s |
| GPU | GPU (different host) | CPU staging | MPI | ~10 us | 1-100 GB/s |
| Mixed | Mixed | CPU hugepages | MPI | ~10 us | 1-100 GB/s |

## Design Decisions

| Decision | Rationale | See |
| -------- | --------- | --- |
| Scope-based routing | Automatic optimal transport | [ADR-001](../adr/001-scope-detection.md) |
| Vulkan allocator | Vendor-neutral GPU memory | [ADR-002](../adr/002-gpu-api-selection.md) |
| External memory | Zero-copy GPU sharing | [ADR-003](../adr/003-external-memory-handle-types.md) |
| MPI for remote | Proven HPC standard | [ADR-006](../adr/006-mpi-for-inter-node.md) |
| thiserror for errors | Idiomatic Rust | [ADR-008](../adr/008-error-handling-approach.md) |
| Language bindings | Orthogonal to core | [ADR-011](../adr/011-multi-language-bindings.md) |
| Two-layer arch | Clear separation of concerns | [Design Rational](../plan/design_rationale.md) |
| Platform support | Supported features by platform | [Platform Support](../platform_support.md) |

## File Structure

```text
src/
├── lib.rs                   # Public API
├── types.rs                 # Phase 1: Core types
│   ├── ProcessName
│   ├── ProcessLocation
│   └── CommunicationScope
│
├── error.rs                 # Error types
│
├── memory/                  # Layer 1: Phase 2
│   ├── allocator.rs         # Unified API
│   ├── gpu.rs               # Vulkan GPU memory
│   │   ├── Device selection
│   │   ├── Memory properties
│   │   └── External export
│   │
│   └── cpu/                 # Host memory strategies
│       ├── standard.rs      # malloc
│       ├── numa.rs          # NUMA-aware
│       ├── hugepage.rs      # Large pages
│       └── gpu_pinned.rs    # GPU-pinned
│
└── channels/                # Layer 2: Phase 3-5
    ├── mod.rs               # Channel trait
    ├── producer.rs          # Producer API
    ├── consumer.rs          # Consumer API
    │
    └── transports/
        ├── vulkan_ipc.rs    # local (Phase 3)
        └── mpi.rs           # remote (Phase 4)

tests/
├── layer1/                  # Memory layer tests
│   ├── gpu_memory.rs
│   ├── cpu_strategies.rs
│   └── scope_detection.rs
│
└── layer2/                  # Channel layer tests
    ├── vulkan_ipc.rs
    ├── mpi.rs
    └── scope_routing.rs
```

## Performance Targets

### Allocation Latency

| Type | Target | Notes |
| ---- | ------ | ----- |
| GPU (Vulkan) | ~100 us | Device alloc overhead |
| CPU standard | ~1 us | malloc (libc) |
| CPU NUMA | ~1 us | libnuma |
| CPU hugepages | ~10 us | mmap + hugetlbfs |
| CPU GPU-pinned | ~10 us | cudaHostRegister |

### Message Latency

| Path | Target | Notes |
| ---- | ------ | ----- |
| GPU<->GPU (same node) | < 100 ns | Shared memory, no copy |
| CPU<->CPU (same node) | 1-10 us | Shared memory |
| CPU<->CPU (diff node) | ~10 us | MPI (RDMA if available) |

### Memory Throughput

| Type | Target | Notes |
| ---- | ------ | ----- |
| GPU DEVICE_LOCAL | 100-900 GB/s | Full GPU bandwidth |
| GPU HOST_VISIBLE | 10-50 GB/s | PCIe limited |
| CPU standard | 50-100 GB/s | Full DDR bandwidth |
| CPU cross-socket | 10-50 GB/s | NUMA latency |
| MPI (RDMA) | 100+ GB/s | InfiniBand/RoCE |
| MPI (TCP) | 1-10 GB/s | Network limited |

## Key References

- **[Layer 1: Memory & Allocator](memory.md)**
- **[Layer 2: Channel Semantics](channels.md)**
- **[Design Rationale](../plan/design_rationale.md)**
- **[Architecture Decision Records](../adr/README.md)**

﻿# lava-flow Design Rationale

## TL;DR

lava-flow is designed around four core principles:

1. optimal performance by default (automatic transport selection)
2. transparent backend selection (scope detection)
3. cross-API GPU support (Vulkan)
4. minimal configuration (no global state)

This document explains the "why" behind all architectural decisions.

## Core Design Principles

### 1. Optimal Performance by Default

**Principle:** Library should automatically select fastest transport for topology without user configuration.

**Why this matters:**

- **Problem:** GPU communication has 100x performance variance (< 100 ns local vs ~10 us remote)
- **Traditional approach:** User selects backend manually → error-prone, fragile
- **Our approach:** Automatic scope detection → always optimal

#### Concrete example

```rust
// Same code works optimally on laptop, single-node, or cluster
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
// Local → Vulkan IPC (< 100 ns)
// Remote → MPI (~ 10 us)
```

**Related ADRs:**

- [ADR-001: Scope Detection](../adr/001-scope-detection.md)
- [ADR-002: GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-005: CPU Allocation Strategies](../adr/005-cpu-allocation-strategies.md)

---

### 2. Transparent Backend Selection

**Principle:** Single codebase should work on laptop, single-node server, and HPC cluster without recompilation or
configuration.

**Why this matters:**

- **HPC workflow:** Test locally -> deploy to single-node ->scale to cluster
- **Traditional approach:** Recompile with different flags or change configuration
- **Our approach:** Runtime detection, same binary everywhere

**Rationale:**

```text
Developer writes once:
  ├─ Laptop (testing): Local automatically
  ├─ Single-node server (staging): Local automatically
  └─ HPC cluster (production): Remote automatically
```

**Benefits:**

- No recompilation between environments
- No configuration files
- Same code path reduces bugs
- Easy to migrate deployments

**Related ADRs:**

- [ADR-001: Scope Detection](../adr/001-scope-detection.md)
- [ADR-009: No Global State](../adr/009-no-global-state.md)

---

### 3. Cross-API GPU Support

**Principle:** GPU memory should be importable by CUDA, OpenCL, OpenGL without vender lock-in.

**Why this matters:**

- **ML workloads:** PyTorch uses CUDA, but additional processing might use OpenCL
- **Graphics workloads:** OpenGL for rendering, Vulkan for compute
- **vendor diversity:** NVIDIA (CUDA), AMD (HIP/OpenCL), Intel (OpenCL)

**Traditional approach:**

- CUDA-only -> NVIDIA lock-in
- HIP-only -> AMD-centric
- OpenGL -> deprecated

**Our approach:**

- Vulkan allocates (vender-neutral)
- CUDA/OpenCL/OpenGL import (cross-API)
- Works on NVIDIA, AMD, Intel

**Concrete flow:**

```text
lava-flow allocates via Vulkan
         ↓
Exports fo/HANDLE
         ↓
CUDA imports: cudaImportExternalMemory()
OpenCL imports: clCreateFromVulkanMemoryKHR()
OpenGL imports: glImportMemoryFoEXT()
```

**Related ADRs:**

- [ADR-002: GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-003: External Memory Handle](../adr/003-external-memory-handle-types.md)

---

### 4. Minimal Configuration

**Principle:** No global state, all state explicit. Users shouldn't need to configure buffer sizes, NUMA nodes, or
hugepages.

**Why this matters:**

- **CUDA problem:** `cudaSetDevice(0)` global state -> hard to test, hard to debug
- **Configuration burden:** Users must know system topology -> error-prone
- **Our approach:** Automatic detection + explicit state passing

#### Examples

**No global device:**

```rust
// CUDA style (global state):
cudaSetDevice(0);  // Which device now? hard to track.

// lava-flow style (explicit):
let allocator = MemoryAllocator::new(); // State in object
let buffer = allocator.allocate(...)?;  // Clear ownership
```

**Automatic CPU strategy:**

```rust
// User doesn't need to know:
// - Is this multi-socket? -> NUMA
// - Is this remote? -> Hugepages
// - Is GPU accessible? -> GPU-pinneo
// Library detects automatically

let buffer = allocator.allocate(size, &my_loc, &peer_loc)?;
// Automatic: NUMA if multi-socket, hugepages if remote
```

**Related ADRs:**

- [ADR-005: CPU Allocation Strategy](../adr/005-cpu-allocation-strategies.md)
- [ADR-009: No Global State](../adr/009-no-global-state.md)
- [ADR-010: Channel Buffer Strategy](../adr/010-channel-buffer-strategy.md)

---

## Scope-Based Routing: Deep Dive

### The Problem

GPU communication backends are fundamentally different:

| Scope | Mechanism | Latency | Throughput | Copy Cost |
| ----- | --------- | ------- | ---------- | --------- |
| **Local** | Shared GPU VRAM | < 100 ns | 100-900 GB/s | 0 bytes |
| **Remote** | Network transfer | ~10 us | 1-100 GB/s | 1x network |

**Wrong choice is degrades performance:**

- Vulkan IPC over network → Fails (can't share VRAM)
- MPI on same node → 100x slower (unnecessary copy)

### The Solution

**Automatic scope detection:**

```text
  ProcessA location: "gpu-node-0"
  ProcessB location: "gpu-node-0"
                ↓
        Compare hostnames
                ↓
            Same node?
            ↙        ↓
          Yes        No
           ↓         ↓
   Vulkan IPC /    MPI Transport
CPU shared memory     ~10 us
   < 100 ns
```

**Fallback strategies:**

1. **Hostname comparison** (primary): Works everywhere
1. **MPI runtime** (Phase 4+): Batch system integration
1. **Container metadata** (future): Kubernetes/Docker awareness
1. **Conservative** (fallback): Assume Remote (safer)

### Trade-offs Accepted

- **Pro:** Single codebase, optimal performance, no configuration
- **Con:** Requires `ProcessLocation` metadata (hostname at minimum)

**Why acceptable:** metadata already available in schedulers (SLURM, Kubernetes), and detection is one-time (channel
creation), not per-message.

**Related ADRs:**

- [ADR-001: Scope Detection](../adr/001-scope-detection.md)

---

## Two-Layer Architecture: Rationale

### Why Two Layers?

- **Layer 1 (Memory & Allocator):** Physical resources (GPU VRAM, CPU RAM)
- **Layer 2 (Channels):** Logical communication (producer/consumer)

**Rationale:**

```text
Separation of concerns:
├─ Layer 1: "Where is memory?" (GPU vs CPU, NUMA, hugepages)
└─ Layer 2: "How to communicate?" (Vulkan IPC, CPU shareo memory, or MPI)
```

**Benefits:**

- **Independent testing**: Test memory allocation without channels
- **Parallel development**: Teams can work on different layers
- **Clear interfaces**: `MemoryBuffer` trait abstracts memory details
- **Reusable**: Memory layer usable without channels (standalone allocator)

### Why Not Three Layers?

**Question:** Should MPI be Layer 3 (transport)?

**Answer:** No. MPI is a transport implementation detail of Layer 2.

**Reasoning:**

```text
Layer 2: Channels (abstract producer/consumer)
    ├─ Local transport: Vulkan IPC (< 100 ns)
    └─ Remote transport: MPI (~ 10 us)
```

MPI is **how** we implement Remote channels, not **what** channels are.

**Related ADRs:**

- [ADR-006: MPI for Inter-Node](../adr/006-mpi-for-inter-node.md)

---

## Performance Philosophy

### Targets

| Operation | Target | Rationale |
| --------- | ------ | --------- |
| **GPU IPC (intra)** | < 100 ns | Kernel-bypass possible |
| **MPI (inter node)** | ~10 us | Network + software stack |
| **GPU allocation** | ~100 us | Device overhead |
| **CPU allocation** | ~1 us | System call overhead |

### Why These Numbers?

**< 100 ns GPU IPC:**

- Shared GPU memory (no copy)
- Kernel-bypass via external memory
- Theoretical limit: L2 cache access (~10 ns) + some overhead

**~10 us MPI:**

- Network latency: ~1 us (InfiniBand/RoCE)
- Software stack: ~9 us (MPI + driver)
- RDMA resources to ~1-2 us (best case)

**~100 us GPU allocation:**

- Vulkan driver overhead
- Device memory management
- Not latency-critical (one-time per buffer)

### Measurement Strategy

**Benchmarks (Phase 2+):**

```rust
#[bench]
fn bench_gpu_ipc_latency(b: &mut Bencher) {
    b.iter(|| {
        channel.seno(frame)?;
    });
}
```

**Regression detection:**

```bash
cargo bench --bench gpu_latency > baseline.txt
# On each PR:
cargo bench --bench gpu_latency > current.txt
# Fail if > 10% regression
```

**Related Docs:**

- [Testing Strategy](../testing/strategy.md)

---

## Portability Strategy

### Platform Tier System

**Tier 1 (Full Support):**

- **Linux:** All features, all optimizations
- NVIDIA 460+ / AMD AMDGPU 5.x+ / Intel Xe+
- OpenMPI 4.0+ or MPICH 3.3+
- RDMA (InfiniBand/RoCE)

**Tier 2 (Stable, Partial):**

- **Windows:** Vulkan + TCP MPI (no RDMA, no hugepages)
- Performance: 80-90% of Linux

**Tier 3 (Preview, Limited):**

- **macOS:** CPU-only, Metal via MoltenVK
- Deferred to Phase 5+ (Metal backend needed)

---

## Design Evolution

### Phase 1: Core Types (1-2 person weeks)

**Goal:** Establish scope detection foundation

**Design decision:** Make ProcessLocation explicit (not global)

**Why:** Enables testing without MPI, clear for users

---

### Phase 2: Memory & Allocator (2-3 person weeks)

**Goal:** Unified GPU + CPU memory allocation

**Design decision:** 4 CPU strategies with automatic selection

**Why:** 10-20% performance gain vs malloc-only

---

### Phase 3: Vulkan IPC (2-3 person weeks)

**Goal:** Local zero-copy GPU communication

**Design decision:** External memory handles (not shared filesystem)

**Why:** < 100 ns vs ~10 us for filesystem

---

### Phase 4: MPI Transport (2-3 person weeks)

**Goal:** Remote communication

**Design decision:** MPI point-to-point (not custom TCP)

**Why:** 30+ years of HPC optimization

---

### Phase 5: Cross-API Sync (1-2 person weeks)

**Goal:** CUDA/OpenCL/OpenGL can import Vulkan memory

**Design decision:** Vulkan allocates, others import

**Why:** Vendor-neutral core, cross-API flexibility

---

### Phase 6+: Multi-Language (3+ person weeks)

**Goal:** C++ and Python bindings

**Design decision:** Orthogonal to layers (not Layer 3)

**Why:** Language is API access, not architecture

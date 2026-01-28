# ADR-005: CPU Memory Allocation Strategies

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Support 4 CPU allocation strategies (standard malloc, NUMA-aware, hugepages, GPU-pinned) with automatic selection based
on buffer size, system topology, and communication scope. 10x performance difference possible between strategies.

## Problem

CPU memory allocation has massive performance variance:

- **Standard malloc:** 50-100 GB/s (single-socket, cache-line aligned)
- **NUMA-aware:** 10-100 GB/s (multi-socket, avoid cross-socket latency)
- **Hugepages:** 50-100 GB/s (large buffers, reduce TLB misses by 99%)
- **GPU-pinned:** PCIe bandwidth (enable GPU direct access)

Single strategy insufficient:

- malloc wastes 20% performance on multi-socket (NUMA)
- malloc wastes 10-20% on remote (TLB misses)
- GPU-pinned wastes kernel memory if not needed

## Decision

**Implement 4 strategies with automatic selection:**

```rust
pub enum CpuAllocationStrategy {
    Standard,      // malloc (default)
    Numa,          // libnuma (multi-socket)
    Hugepages,     // mmap + hugetlbfs (remote)
    GpuPinned,     // cudaHostRegister (CPU<->GPU staging)
}
```

**Automatic selection logic:**

```rust
if cpu_to_gpu_transfer_needed:
    GpuPinned
else if multi_node_communication:
    Hugepages
else if multi_socket_system && buffer_size > 1MB:
    Numa
else:
    Standard
```

## Rationale

### Strategy 1: Standard (malloc)

**When:** Default, single-socket, small buffers

**Implementation:**

```rust
let layout = Layout::from_size_align(size, 64)?; // Cache-line aligned
let ptr = alloc::alloc(layout);
```

**Performance:** 50-100 GB/s (full DDR bandwidth)

**Use case:** Baseline, works everywhere

---

### Strategy 2: NUMA-Aware

**When:** Multi-socket systems (detect via `libnuma`)

**Implementation:**

```rust
unsafe {
    let ptr = numa_alloc_onnode(size, numa_node);
}
```

**Performance:** 10-100 GB/s (avoids cross-socket penalty)

**Use case:** 2-socket servers (e.g., dual Xeon)

**Why needed:** Cross-socket access 2-3x slower (NUMA distance)

---

### Strategy 3: Hugepages

**When:** remote communication, large buffers (> 1 MB)

**Implementation:**

```rust
let ptr = unsafe {
    libc::mmap(
        ptr::null_mut(),
        size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB,
        -1,
        0,
    )
};
```

**Performance:** 50-100 GB/s (reduces TLB misses from ~1% to ~0%)

**Use case:** Large frame buffers (10 MB+) sent over network

**Why needed:** Regular pages (4 KB) cause TLB misses, hugepages (2 MB or 1 GB) eliminate this

---

### Strategy 4: GPU-Pinned

**When:** CPU produces data, GPU consumes (or vice versa)

**Implementation:**

```rust
let ptr = alloc::alloc(layout);
unsafe {
    cuda::cudaHostRegister(ptr, size, cudaHostRegisterPortable);
}
```

**Performance:** PCIe bandwidth (10-50 GB/s)

**Use case:** CPU preprocessing → GPU inference

**Why needed:** Enables GPU direct access to CPU memory (no extra copy)

---

## Alternatives Considered

### Alternative 1: Single Strategy (malloc only)

**Pros:**

- Simple
- Portable

**Cons:**

- 20% slower on multi-socket
- 10-20% slower on remote
- Can't do CPU <-> GPU zero-copy

**Rejected:** Performance loss unacceptable for infrastructure library.

---

### Alternative 2: User-Selected Strategy

**Pros:**

- Maximum control

**Cons:**

- User must know system topology
- Misconfiguration likely
- Violates "works automatically" principle

**Rejected:** Automatic selection better user experience.

---

### Alternative 3: Four Strategies with Automatic Selection **CHOSEN**

**Pros:**

- Optimal performance per scenario
- Works automatically
- Explicit override available if needed

**Cons:**

- More complex implementation
- Platform differences (Windows no hugepages)

**Chosen:** Performance gains (10-20%) justify complexity.

## Implementation

```rust
pub fn select_cpu_strategy(
    buffer_size: usize,
    scope: CommunicationScope,
    gpu_accessible: bool,
) -> CpuAllocationStrategy {
    if gpu_accessible {
        return CpuAllocationStrategy::GpuPinned;
    }

    if scope == CommunicationScope::Remote {
        return CpuAllocationStrategy::Hugepages;
    }

    let numa_nodes = detect_numa_nodes();
    if numa_nodes > 1 && buffer_size > 1_000_000 {
        return CpuAllocationStrategy::Numa;
    }

    CpuAllocationStrategy::Standard
}
```

## Consequences

- **10-20% faster** — Automatic selection picks optimal strategy
- **No user configuration** — Works transparently
- **Explicit override** — Advanced users can force strategy
- **Platform differences** — Windows no hugepages (fallback to malloc)

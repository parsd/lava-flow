# ADR-009: No Global State

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

All state passed explicitly (allocator, channel instances). No global singletons like CUDA's `cudaGetDevice()`.
Simplifies testing, enables multi-allocator per process.

## Decision

**No global state. All state passed explicitly via `&mut self`.**

## Rationale

**Why no global state?**

- Testing easier (no global pollution)
- Multi-process aware (global state per-process meaningless)
- Multi-allocator support (can have multiple allocators per process)
- Predictable (no hidden state changes)

**CUDA's problem:**

```cpp
cudaSetDevice(0);  // Thread local global state
cudaMalloc(&ptr, size);  // Uses set device
// Later: which device was used? Must track separately.
```

**lava-flow approach:**

```rust
let allocator = MemoryAllocator::new();
let buffer = allocator.allocate(...)?;
// State explicit, no global confusion
```

## Implementation

```rust
pub struct MemoryAllocator {
    devices: Vec<VulkanDevice>,
    numa_nodes: Vec<NumaNode>,
}

impl MemoryAllocator {
    pub fn new() -> Result<Self, Error> { ... }

    pub fn allocate(&mut self, size: usize, location: MemoryLocation)
        -> Result<Box<dyn MemoryBuffer>, Error> {
        // All state in self, no globals
    }
}
```

## Consequences

- **Testable** — No global state to reset
- **Multi-allocator** — Can have multiple per process
- **Predictable** — All state explicit *verbosity* — must pass allocator reference (acceptable)

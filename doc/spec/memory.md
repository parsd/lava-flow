# Layer 1: Memory & Allocator Architecture

This document specifies the current Layer-1 memory implementation for lava-flow.

## Scope

Layer 1 provides a unified allocation API across:

- GPU memory (Vulkan-oriented backend abstraction)
- CPU memory (host shared-memory backend)
- Unified memory metadata/handle export for channel transports
- Allocator building blocks for channel-owned receive materialization strategies

Channels (Layer 2) consume `MemoryBuffer` regardless of backing type.
Layer 2 channels may also consume/export external references and decide materialization through channel allocator
configuration.

## Implementation Priority

When trade-offs occur in Layer 1, use this priority order:

1. Security and correctness
2. API stability
3. Coverage completeness

## Current Implementation (Authoritative)

### CPU

Implemented API:

- `CpuAllocator` (owns CPU allocation cap)
- `CpuMemoryBuffer` with:
  - safe accessors: `as_slice`, `as_mut_slice`
  - raw pointer accessors: `as_ptr`, `as_mut_ptr`
  - `shared_handle()` export

Allocation cap behavior:

- `cpu::Allocator::new()` reads `LAVA_FLOW_MAX_CPU_ALLOCATION_SIZE`
- `cpu::Allocator::with_max_allocation_size(cap)` sets an explicit cap
- invalid or zero env values fall back to platform hard cap
- hard cap respects pointer/off_t limits

### GPU

Implemented backend abstraction:

- `gpu::Allocator` probe-based optional backend
- `gpu::MemoryBuffer` with external-handle metadata
- `MemoryAllocator::new()` remains infallible; GPU backend may be unavailable at runtime

### Unified Allocator

Implemented API:

- `MemoryAllocator::new()`
- `MemoryAllocator::with_cpu_max_allocation_size(cap)`
- `allocate(size, location)`
- `allocate_with_fallback(size, primary)`

Implemented fallback behavior:

- `GpuVulkan` -> `CpuHost`
- `CpuHost` -> no fallback

Pinned-memory note:

- The default CPU allocator does not expose a public pinned-allocation option.
- `AllocationStrategy` still indicates whether a buffer is pinned.
- A dedicated fast-transfer allocator can be introduced later for backend-specific GPU transfer paths.

### Channel Integration Contract

Layer 2 channel allocators should be fixed-target by default:

- CPU-target allocators deliver `MemoryLocation::CpuHost`
- GPU-target allocators deliver `MemoryLocation::GpuVulkan { .. }`

This keeps allocator implementations simple on platforms without GPU support and avoids mandatory dual-path
`CPU/GPU` error branching in every allocator implementation.

Allocator contract is allocation-only for receive materialization paths:

- no local receive mode flags on allocator traits
- local/remote receive behavior is a channel runtime concern

If a deployment needs hybrid behavior, composition wrappers can combine fixed-target allocators without changing the
base Layer-1 allocator contracts.

## Interprocess Handles

Unified type:

- `InterprocessMemoryHandle`

Variants:

- Unix: `GpuOpaqueFd`, `CpuSharedFd`
- Windows: `GpuOpaqueWin32Handle`, `CpuSharedWin32Handle`

`MemoryBuffer::export_handle()` behavior:

- GPU buffer: exports GPU external handle metadata
- CPU buffer: exports CPU shared-memory transport handle

## Platform Implementation Layout

Non-trivial OS-specific logic is split by platform:

- `src/memory/allocator/mod.rs`
- `src/memory/allocator/unix.rs`
- `src/memory/allocator/windows.rs`
- `src/memory/cpu/mod.rs`
- `src/memory/cpu/unix.rs`
- `src/memory/cpu/windows.rs`

Rules:

- keep test fault-injection paths test-only
- keep production OS branching localized to platform files
- avoid widening public API for internal-only helpers

## Deferred / Not Implemented Yet

The following are intentionally deferred and not part of current Layer-1 behavior:

- NUMA-aware CPU allocation strategy
- Hugepage allocation strategy
- Full Vulkan device-memory implementation details (current code models API and handle flow)

If added later:

- keep feature-gated where appropriate
- preserve current API compatibility
- document behavior and security constraints in ADR/spec updates

## Minimal API Shape (Current)

```rust
pub enum MemoryLocation {
    GpuVulkan { device_id: u32 },
    CpuHost,
}

pub struct CpuAllocator { /* max_allocation_size */ }

impl MemoryAllocator {
    pub fn new() -> Self;
    pub fn with_cpu_max_allocation_size(cap: usize) -> Self;
    pub fn allocate(&self, size: usize, location: MemoryLocation) -> Result<MemoryBuffer>;
    pub fn allocate_with_fallback(&self, size: usize, primary: MemoryLocation) -> Result<MemoryBuffer>;
}
```

## Notes for Reviewers

- This spec describes implemented behavior, not historical/future strategy sketches.
- Keep conceptual future ideas in ADRs or dedicated planning docs, not in authoritative implementation sections.

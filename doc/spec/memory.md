# Layer 1: Memory & Allocator Architecture

This document specifies the current Layer-1 memory implementation for lava-flow.

## Scope

Layer 1 provides allocation APIs across:

- GPU memory (Vulkan-oriented backend abstraction)
- CPU memory (host shared-memory backend)
- Unified memory metadata/handle export for channel transports
- Allocator building blocks for channel-owned receive materialization strategies

Channels (Layer 2) consume memory buffers regardless of backing type.
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

- `cpu::Allocator` (owns CPU allocation cap)
- `cpu::MemoryBuffer` with:
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

- `gpu::Allocator` with strict backend invariant (`new() -> Result<Self>`)
- `gpu::Allocator::new_for_device(device_id)` for explicit logical device selection
- each allocator is device-scoped and `allocate(size)` uses its configured device id
- `gpu::MemoryBuffer` with:
  - logical `size()` (payload bytes)
  - `allocation_size()` (Vulkan-required backing bytes)
  - external-handle metadata
  - internal external-handle import/export for channel transports
- GPU allocator construction fails when backend is unavailable/disabled
- GPU allocation path does not expose host-mapped pointer access in the public API

### Allocator Composition

Implemented API is split by backend:

- `cpu::Allocator`
- `gpu::Allocator`

Selection/composition across CPU/GPU is a Layer-2 channel concern.

Pinned-memory note:

- The default CPU allocator does not expose a public pinned-allocation option.
- `AllocationStrategy` still indicates whether a buffer is pinned.
- A dedicated fast-transfer allocator can be introduced later for backend-specific GPU transfer paths.

### Channel Integration Contract

Layer 2 channel allocators should be fixed-target by default:

- CPU-target allocators deliver CPU-backed payload buffers
- GPU-target allocators deliver GPU-backed payload buffers

This keeps allocator implementations simple on platforms without GPU support and avoids mandatory dual-path
`CPU/GPU` error branching in every allocator implementation.

Allocator contract is allocation-only for receive materialization paths:

- no local receive mode flags on allocator traits
- local/remote receive behavior is a channel runtime concern

If a deployment needs hybrid behavior, composition wrappers can combine fixed-target allocators without changing the
base Layer-1 allocator contracts.
Planned Layer-2 channel integration should continue through traits/builders and should not introduce a separate
`src/memory/unified.rs` abstraction layer.

## Interprocess Handles

Unified type:

- `InterprocessMemoryHandle`

Variants:

- Unix: `GpuOpaqueFd`, `CpuSharedFd`
- Windows: `GpuOpaqueWin32Handle`, `CpuSharedWin32Handle`

Handle export behavior:

- GPU buffer: exports GPU external handle metadata
- CPU buffer: exports CPU shared-memory transport handle

Handle import behavior:

- GPU buffer: imports GPU external handles into a Vulkan buffer on the selected logical device
- CPU buffer: imports CPU shared-memory transport handles

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
- Advanced Vulkan policies (memory-type policy tuning and queue-family strategy)

If added later:

- keep feature-gated where appropriate
- preserve current API compatibility
- document behavior and security constraints in ADR/spec updates

## Minimal API Shape (Current)

```rust
pub mod cpu {
    pub struct Allocator { /* max_allocation_size */ }
}

pub mod gpu {
    pub struct Allocator { /* device_id/runtime */ }
}
```

## Notes for Reviewers

- This spec describes implemented behavior, not historical/future strategy sketches.
- Keep conceptual future ideas in ADRs or dedicated planning docs, not in authoritative implementation sections.

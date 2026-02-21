# Phase 2: Memory & Allocator

## TL;DR

Implement Layer 1: a unified allocator for Vulkan GPU memory and core CPU strategies, with explicit location requests.
NUMA support is deferred from the first implementation and treated as optional future work.

## Scope

- Vulkan GPU memory allocation and external export
- CPU allocation strategy (first implementation): standard
- A single `MemoryAllocator` with explicit `MemoryLocation` requests and deterministic fallback
- A unified interprocess handle type for GPU external memory and CPU shared memory
- Parallel allocation support via thread-safe internal mutability in allocator backends
- Optional future extension: NUMA-aware CPU allocation

## Applied Follow-up Learnings

- CPU allocation cap is now owned by `CpuAllocator` and can be injected into
  `MemoryAllocator::with_cpu_max_allocation_size(...)`.
- CPU allocation logic is exposed as `CpuAllocator` to avoid implicit global configuration in tests and call sites.
- `CpuMemoryBuffer` now supports safer slice-first access (`as_slice` / `as_mut_slice`) in addition to raw pointers.
- Fault-injection paths used for coverage stay test-only; production paths do not branch on test hooks.
- Platform-specific implementation code should be split by OS (`mod.rs` + `unix.rs` + `windows.rs`) when complexity
  goes beyond trivial wrappers.
- Layer 2 receive materialization is channel-owned and allocator-driven; Phase 2 provides the allocation primitives
  (`MemoryAllocator`, `CpuAllocator`) used by channel allocator implementations.
- Channel allocators are expected to be fixed-target (`CPU` or `GPU`) by default; composition wrappers can add hybrid
  policies without forcing dual-branch logic into every allocator.
- The default CPU allocator does not expose public pinned-allocation; specialized fast-transfer allocators are deferred
  to later phases.

## Deliverables

- `MemoryAllocator` with GPU + CPU backends
- Interprocess handle export (GPU external + CPU shared-memory handle variants)
- Deterministic fallback logic
- Integration tests for allocation and export/import
- Documented extension point for optional future NUMA backend

## Example (API Shape)

```rust
use lava_flow::{InterprocessMemoryHandle, MemoryAllocator, MemoryLocation};

let allocator = MemoryAllocator::new();
let buffer = allocator.allocate(
    1_000_000,
    MemoryLocation::GpuVulkan { device_id: 0 }
)?;

let handle: InterprocessMemoryHandle = buffer.export_handle()?;
```

## Related Docs

- [Memory Spec](../spec/memory.md)
- [Architecture](../spec/architecture.md)
- [ADR-002 GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-003 External Memory Handle Types](../adr/003-external-memory-handle-types.md)
- [ADR-004 Vulkan Version Requirement](../adr/004-vulkan-version-requirement.md)
- [ADR-005 CPU Allocation Strategies](../adr/005-cpu-allocation-strategies.md)

## External References

- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)
- [Vulkan External Memory (fd)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_fd.html)
- [Vulkan External Memory (win32)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_win32.html)
- [libnuma](https://github.com/numactl/numactl)

## Deferred (Optional Future)

- NUMA support (`libnuma`/`numactl`) is not part of the first Phase 2 implementation.
- If introduced later, it should be feature-gated and added without breaking the initial allocator API.
- License note: `numactl` is GPL and `libnuma` is LGPL; this conflicts with the preferred dependency policy for the
  initial implementation.

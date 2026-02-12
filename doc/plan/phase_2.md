# Phase 2: Memory & Allocator

## TL;DR

Implement Layer 1: a unified allocator for Vulkan GPU memory and core CPU strategies, with automatic selection.
NUMA support is deferred from the first implementation and treated as optional future work.

## Scope

- Vulkan GPU memory allocation and external export
- CPU allocation strategies (first implementation): standard, GPU-pinned
- A single `MemoryAllocator` that picks strategies based on scope and profile
- A unified interprocess handle type for GPU external memory and CPU shared memory
- Parallel allocation support via thread-safe internal mutability in allocator backends
- Optional future extension: NUMA-aware CPU allocation

## Deliverables

- `MemoryAllocator` with GPU + CPU backends
- Interprocess handle export (GPU external + CPU shared-memory handle variants)
- Strategy selection logic
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

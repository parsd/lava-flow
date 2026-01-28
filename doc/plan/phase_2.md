# Phase 2: Memory & Allocator

## TL;DR

Implement Layer 1: a unified allocator for Vulkan GPU memory and multiple CPU strategies, with automatic selection.

## Scope

- Vulkan GPU memory allocation and external export
- CPU allocation strategies: standard, NUMA, hugepages, GPU-pinned
- A single `MemoryAllocator` that picks strategies based on scope and profile
- A unified external handle type that hides fd vs Win32 HANDLE

## Deliverables

- `MemoryAllocator` with GPU + CPU backends
- External memory export (fd/HANDLE wrapped in a single handle type)
- Strategy selection logic
- Integration tests for allocation and export/import

## Example (API Shape)

```rust
use lava_flow::{MemoryAllocator, MemoryLocation, ExternalMemoryHandle};

let mut allocator = MemoryAllocator::new()?;
let buffer = allocator.allocate(
    1_000_000,
    MemoryLocation::GpuVulkan { device_id: 0 }
)?;

let handle: ExternalMemoryHandle = buffer.export_handle()?;
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

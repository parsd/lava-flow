# Phase 3: Vulkan IPC Channels

## TL;DR

Implement Layer 2 for local communication: zero-copy Vulkan IPC channels with send/recv semantics. The public
`Channel` API is shared with Phase 4; transport selection is internal.

## Scope

- Channel API for local scope
- Vulkan IPC transport (external memory handles + shared metadata)
- Frame metadata format
- Dual receive API:
  - allocator-driven receive (`recv_alloc`)
  - caller-buffer receive (`recv_into`) with optional caller-managed staging buffer
- Local-only tests and benchmarks

## Deliverables

- `Channel` API with send/recv and blocking helpers
- Receive options and policy types (`RecvAllocOptions`, `RecvIntoOptions`, `RecvPolicy`)
- `recv_into` staging parameter as an explicit `Option<&mut CpuMemoryBuffer>`
- Convenience receive wrappers (`recv_alloc_default`, `recv_into_strict`)
- Metadata serialization configuration (codec selection)
- `VulkanIpcTransport`
- Frame metadata serialization
- Integration tests for local IPC

## Example (API Shape)

```rust
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
channel.send(frame)?;
let received = channel.recv_alloc(
    &allocator,
    RecvAllocOptions {
        preferred_location: MemoryLocation::GpuVulkan { device_id: 0 },
        policy: RecvPolicy::Auto,
    },
)?;
```

## Related Docs

- [Channels Spec](../spec/channels.md)
- [Interop Overview](../spec/interop/README.md)
- [ADR-002 GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-010 Channel Buffer Strategy](../adr/010-channel-buffer-strategy.md)

## External References

- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)
- [Vulkan External Semaphore](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_semaphore.html)

# Phase 3: Vulkan IPC Channels

## TL;DR

Implement Layer 2 for local communication: zero-copy Vulkan IPC channels with send/recv semantics. The public
`Channel` API is shared with Phase 4; transport selection is internal.

## Scope

- Channel API for local scope
- Vulkan IPC transport (external memory handles + shared metadata)
- Frame metadata format
- Local-only tests and benchmarks

## Deliverables

- `Channel` API with send/recv and blocking helpers
- `VulkanIpcTransport`
- Frame metadata serialization
- Integration tests for local IPC

## Example (API Shape)

```rust
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
channel.send(frame)?;
let received = channel.recv()?;
```

## Related Docs

- [Channels Spec](../spec/channels.md)
- [Interop Overview](../spec/interop/README.md)
- [ADR-002 GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-010 Channel Buffer Strategy](../adr/010-channel-buffer-strategy.md)

## External References

- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)
- [Vulkan External Semaphore](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_semaphore.html)

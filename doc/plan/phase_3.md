# Phase 3: Local Channel Runtime

## TL;DR

Implement Layer 2 for local communication with directional endpoints, payload-only frames, and separate metadata.
Transport selection remains internal, and receive materialization behavior is configured once on the receiver endpoint.

## Scope

- Directional endpoint API for local scope (`Sender`, `Receiver`)
- Distinct builders (`SenderBuilder`, `ReceiverBuilder`)
- Trait-based channel allocator integration over concrete Layer-1 allocator backends
- Vulkan IPC transport (external memory handles + shared metadata)
- Local CPU shared-memory transport
- Two receive variants:
  - typed default: `recv::<M>() -> (Frame, M)`
  - dynamic map: `recv_map() -> (Frame, MessageMeta)`
- Receiver-owned allocation strategy (no per-recv target hints)
- Lightweight endpoint introspection (`scope()`, `receive_representation()`, `configured_buffer_kind()`)
- Local-only tests and benchmarks

## Deliverables

- `ChannelBuilder::sender(...)` and `ChannelBuilder::receiver(...)` returning distinct builders
- `Sender` / `Receiver` endpoint types
- `ChannelAllocator` trait with fixed-target allocation-only implementations
- No `src/memory/unified.rs` planning; allocator composition stays in traits/builders and existing module boundaries
- Payload envelope (`Frame::{External, Owned}`) without embedded metadata
- Metadata contract (`ChannelMetadata` + `MessageMeta`) with mandatory `used_size`
- Receiver-level `ReceiveRepresentation` (`ExternalShare`, `DirectTransfer`, `Materialized`)
- Metadata serialization configuration (codec selection)
- `VulkanIpcTransport`
- Local shared-memory transport integration
- Integration tests for local IPC paths

## Example (API Shape)

```rust
let tx = ChannelBuilder::sender(my_loc.clone(), peer_loc.clone())
    .with_metadata_encoding(MetadataEncoding::Cbor)
    .build()?;

let rx = ChannelBuilder::receiver(my_loc, peer_loc)
    .with_allocator(cpu_allocator)
    .with_metadata_encoding(MetadataEncoding::Cbor)
    .build()?;

let frame = Frame::Owned(payload_buffer);
let meta = ImageMeta {
    used_size: payload_bytes,
    width: 1920,
    height: 1080,
};

tx.send(frame, &meta)?;
let (frame, meta) = rx.recv::<ImageMeta>()?;
let used = meta.used_size();

let representation = rx.receive_representation();
let target = rx.configured_buffer_kind();
```

## Related Docs

- [Channels Spec](../spec/channels.md)
- [Interop Overview](../spec/interop/README.md)
- [ADR-002 GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-010 Channel Buffer Strategy](../adr/010-channel-buffer-strategy.md)

## External References

- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)
- [Vulkan External Semaphore](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_semaphore.html)

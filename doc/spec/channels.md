# Layer 2: Channel Semantics

This document defines the channel API and transport behavior for local and remote communication.

## TL;DR

- Distinct builders are used: `SenderBuilder` and `ReceiverBuilder`.
- Distinct endpoints are used: `Sender` and `Receiver`.
- `Frame` carries payload only; metadata is separate.
- Receiver allocation policy is configured once at channel creation (default CPU).
- Channel allocators are fixed-target (`CPU` or `GPU`) and allocation-only.
- Receiver introspection is lightweight and stable: `scope()`, `receive_representation()`, and
  `configured_buffer_kind()`.
- Two receive variants are provided:
  - typed default: `recv::<M>() -> (Frame, M)`
  - dynamic map: `recv_map() -> (Frame, MessageMeta)`

## API Shape

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

// Dynamic fallback variant
let (frame, map_meta) = rx.recv_map()?;
let used = map_meta.used_size;
```

### Why Distinct Builders

Distinct builders avoid typestate complexity while preserving normal builder ergonomics:

- sender options stay sender-specific
- receiver options stay receiver-specific
- modifier ordering remains natural
- `build()` return type is always unambiguous

## Endpoint and Frame Model

```rust
pub enum Frame {
    External(ExternalBufferRef),
    Owned(MemoryBuffer),
}

pub trait ChannelMetadata: Serialize + DeserializeOwned {
    fn used_size(&self) -> usize;
}

pub enum ReceiveRepresentation {
    ExternalShare,
    DirectTransfer,
    Materialized,
}

pub enum BufferKind {
    Cpu,
    Gpu,
}

impl Sender {
    pub fn send<M: ChannelMetadata>(&self, frame: Frame, metadata: &M) -> Result<()>;
    pub fn send_map(&self, frame: Frame, metadata: MessageMeta) -> Result<()>;
    pub fn scope(&self) -> CommunicationScope;
}

impl Receiver {
    pub fn recv<M: ChannelMetadata>(&self) -> Result<(Frame, M)>;
    pub fn recv_map(&self) -> Result<(Frame, MessageMeta)>;
    pub fn scope(&self) -> CommunicationScope;
    pub fn receive_representation(&self) -> ReceiveRepresentation;
    pub fn configured_buffer_kind(&self) -> BufferKind;
}
```

Rationale:

- `Frame` stays non-generic and focused on payload ownership/memory.
- Metadata concerns stay separate from payload concerns.
- Typed metadata (`recv::<M>()`) is the common ergonomic path.
- Dynamic metadata (`recv_map()`) remains available for schema-less workflows.

## Channel Allocator Model

Receiver materialization is channel-owned and configured once at receiver creation.

```rust
pub trait ChannelAllocator: Send + Sync {
    /// Fixed output target for materialized payloads.
    fn delivery_target(&self) -> MemoryLocation;
    /// Allocate destination memory using the allocator's fixed-target strategy.
    fn allocate(&self, size: usize) -> Result<MemoryBuffer>;
}
```

Expected implementations:

- `cpu::Allocator` (CPU target)
- `gpu::Allocator` (GPU target)
- Optional composite wrappers if needed (e.g. in Cuda-adapter).

Notes:

- `local_receive_mode` is not part of allocator API.
- Local receive behavior is channel runtime policy; allocator is used when materialization is needed.
- Memory strategy details (ring/arena/hybrid/pinned) stay allocator-internal.

## Transport Selection (Internal)

- **Local + GPU payload:** Vulkan IPC (external memory handles)
- **Local + CPU payload:** shared memory
- **Remote:** MPI point-to-point (including direct-transfer capable paths where available)

```rust
fn select_transport(scope: CommunicationScope, frame: &Frame) -> TransportKind {
    match (scope, frame_is_gpu(frame)) {
        (CommunicationScope::Local, true) => TransportKind::VulkanIpc,
        (CommunicationScope::Local, false) => TransportKind::CpuSharedMemory,
        (CommunicationScope::Remote, _) => TransportKind::MpiPointToPoint,
    }
}
```

## Why No `recv_into` In Core API

- `recv_into` couples channel API to concrete buffer ownership details.
- Conflicts with intra-node non-materialization policy (might require unnecessary alloc)
- Ring/arena/hybrid reuse belongs in allocator strategy and can be implemented without expanding channel API.
- Interop integrations (Torch/NumPy/etc.) are simpler with `Frame::External` / `Frame::Owned` envelopes.

## Metadata Envelope

```rust
pub type MetadataMap = std::collections::BTreeMap<String, MetaValue>;

pub enum MetaValue {
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<MetaValue>),
    Map(MetadataMap),
}

pub struct MessageMeta {
    pub used_size: usize,
    pub values: MetadataMap,
}
```

Typed metadata remains serde-driven:

- send typed: `M: ChannelMetadata`
- receive typed: `M: ChannelMetadata`
- receive dynamic: `MessageMeta` via `recv_map()`

Metadata contract:

- Metadata is mandatory for every message.
- `used_size` defines the valid payload region for each message.
- Receivers must use `used_size` rather than assuming full buffer capacity is filled.

## Semantics (All Transports)

- **Ordering:** producer order is preserved per channel
- **Ownership:** sender must not mutate in-flight payloads
- **Backpressure:** bounded queues or transport-level flow control
- **Errors:** surfaced via unified `Result<T, Error>`
- **Observability:** receiver properties via `scope()` / `receive_representation()` /
  `configured_buffer_kind()`
- **Synchronization:** in Phase 5+, channel manages external semaphore wait/signal internally

## Related Docs

- [Architecture](architecture.md)
- [Memory Spec](memory.md)
- [Interop Overview](interop/README.md)
- [Phase 3](../plan/phase_3.md)
- [Phase 4](../plan/phase_4.md)
- [ADR-012 Serde Serialization](../adr/012-serde-serialization.md)

## External References

- [MPI Standard](https://www.mpi-forum.org/docs/)
- [OpenMPI Documentation](https://www.open-mpi.org/doc/)
- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)

# Layer 2: Channel Semantics

This document defines the channel API and transport behavior for local and remote communication.

## TL;DR

- Distinct builders are used: `SenderBuilder` and `ReceiverBuilder`.
- Distinct endpoints are used: `Sender` and `Receiver`.
- `Frame` carries payload only; metadata is separate.
- Phase 3 local channels are point-to-point.
- Receiver allocation policy is configured once at channel creation (default CPU).
- Channel allocators are fixed-target (`CPU` or `GPU`) and allocation-only.
- Receiver introspection is lightweight and stable: `scope()` and `receive_representation()`.
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
    .build()?;

let meta = ImageMeta {
    used_size: payload_bytes,
    width: 1920,
    height: 1080,
};

tx.send(payload_buffer, &meta)?;
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

## Endpoint and Payload Model

```rust
pub enum Frame {
    Cpu(cpu::MemoryBuffer),
    Gpu(gpu::MemoryBuffer),
}

pub trait ChannelMetadata: Serialize + DeserializeOwned {
    fn used_size(&self) -> usize;
}

pub enum ReceiveRepresentation {
    ExternalShare,
    Materialized,
}

impl Sender {
    pub fn send<M, F>(&self, frame: F, metadata: &M) -> Result<()>
    where
        M: ChannelMetadata,
        F: Into<Frame>;
    pub fn send_map<F>(&self, frame: F, metadata: MessageMeta) -> Result<()>
    where
        F: Into<Frame>;
    pub fn scope(&self) -> CommunicationScope;
}

impl Receiver {
    pub fn recv<M: ChannelMetadata>(&self) -> Result<(Frame, M)>;
    pub fn recv_map(&self) -> Result<(Frame, MessageMeta)>;
    pub fn scope(&self) -> CommunicationScope;
    pub fn receive_representation(&self) -> ReceiveRepresentation;
}
```

Rationale:

- `Frame` stays non-generic and focused on payload backend kind.
- Metadata concerns stay separate from payload concerns.
- `send` and `send_map` can stay ergonomic by accepting any `Into<Frame>` input, so callers can
  pass `cpu::MemoryBuffer`, `gpu::MemoryBuffer`, or an already constructed `Frame`.
- Typed metadata (`recv::<M>()`) is the common ergonomic path.
- Dynamic metadata (`recv_map()`) remains available for schema-less workflows.

## Local Bootstrap Model

Phase 3 local IPC should use an internal listen/connect handshake similar to client/server
transport setup, but that transport detail stays behind the channel builders.

Public intent:

- users provide a shared logical channel identifier
- the library derives the local rendezvous endpoint from that channel id
- one side acts as transport server and performs `listen` / `accept`
- the other side acts as transport client and performs `connect`
- sender-configured metadata encoding is communicated during connection bootstrap
- after connection, message flow stays normal `send` / `recv`

Users should not be required to implement their own pipe/socket bootstrap logic to use local
channels.

If a peer later creates its own sender for reverse application traffic, that reverse channel must
use a distinct `ChannelId`.

```mermaid
sequenceDiagram
    participant SenderBuilder
    participant ReceiverBuilder
    participant LocalTransport

    SenderBuilder->>LocalTransport: listen(channel-id endpoint)
    ReceiverBuilder->>LocalTransport: connect(channel-id endpoint)
    LocalTransport-->>SenderBuilder: accepted Sender
    LocalTransport-->>ReceiverBuilder: connected Receiver
```

### Bootstrap Role vs Message Direction

Bootstrap role should stay separate from message direction:

- `Sender` / `Receiver` describe payload flow
- transport server/client describe endpoint establishment
- `listen` / `accept` / `connect` belong to transport internals

The transport server does not need to always be the channel receiver in the abstract API. The
current Phase 3 default is sender-listen, receiver-connect because that keeps the path open for
future 1 -> many local broadcast without changing the rendezvous model, but that should remain an
implementation choice rather than a public API rule.

### Point-To-Point First

Phase 3 commits only to point-to-point local channels:

- one sender
- one receiver
- ordered delivery per channel

Future fan-out support should be added as a separate semantic mode rather than overloading the
point-to-point channel contract.

### Future Fan-Out Boundary

The local transport should still be implemented so that future fan-out is possible without changing
the public payload API.

Future broadcast-capable local transport would require:

- one logical sender with multiple connected transport peers
- one per-peer handle transfer on each message
- immutable payload-by-contract after `send`

This is especially important for local CPU shared-memory transport:

- Windows would duplicate the shared-memory handle once per connected receiver process
- Unix would send one fd per connected receiver via `SCM_RIGHTS`

Remote fan-out can follow the same semantic split later:

- MPI can support 1 -> many delivery either through one-to-many point-to-point fan-out or collective
  communication such as broadcast
- which MPI mechanism is chosen should remain a transport/runtime decision behind the same higher-level
  channel mode

That is a transport/runtime expansion, not a change to `Frame`, metadata, or typed receive APIs.

### Payload Representation

`Frame` is the public payload type and distinguishes payload backend kind:

- `Frame::Cpu` for CPU-backed payloads
- `Frame::Gpu` for Vulkan-backed GPU payloads

It does not expose whether the receiver got the payload by importing shared backing or by
materializing a local buffer.

Representation is reported through receiver introspection rather than a separate payload enum:

- `ReceiveRepresentation::ExternalShare` means the receiver imported or shared transport backing
- `ReceiveRepresentation::Materialized` means the receiver received a local materialized buffer

Expected consequence:

- local CPU zero-copy receive may still return `Frame::Cpu` while
  `receive_representation()` reports `ExternalShare`
- a materializing receive path may also return `Frame::Cpu`, but
  `receive_representation()` reports `Materialized`

## Channel Allocator Model

Receiver materialization is channel-owned and configured once at receiver creation.

```rust
pub trait ChannelAllocator: Send + Sync {
    /// Fixed output target for materialized payloads.
    fn delivery_target(&self) -> MemoryLocation;
    /// Allocate destination memory using the allocator's fixed-target strategy.
    fn allocate(&self, size: usize) -> Result<Frame>;
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

For local point-to-point transport bootstrap, the runtime should derive a deterministic endpoint
name from `ChannelId` rather than requiring users to exchange raw platform addresses.

## Why No `recv_into` In Core API

- `recv_into` couples channel API to concrete buffer ownership details.
- Conflicts with intra-node non-materialization policy (might require unnecessary alloc)
- Ring/arena/hybrid reuse belongs in allocator strategy and can be implemented without expanding channel API.
- Interop integrations (Torch/NumPy/etc.) should not force transport representation into the
  core channel payload type.

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
  receiver-level representation introspection
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

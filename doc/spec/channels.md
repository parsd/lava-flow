# Layer 2: Channel Semantics

This document defines the unified channel API and how transports are selected for local vs remote communication.

## TL;DR

- One `Channel` API across all phases.
- Transport selection is internal and based on scope + buffer type.
- MPI is a transport detail, not a separate public API.

## API Shape (Unified)

```rust
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
channel.send(frame)?;
let received = channel.recv()?;
```

## Receive APIs (Planned)

To support platform-agnostic receive logic while keeping performance paths explicit, the channel API exposes two
receive variants:

```rust
pub enum RecvPolicy {
    /// Require direct placement into the requested target location.
    Strict,
    /// Allow staging (for example pinned CPU -> GPU copy) when direct placement is unavailable.
    AllowStaging,
    /// Prefer direct placement and allow staging as a fallback.
    Auto,
}

pub enum MetadataEncoding {
    Json,
    Cbor,
    MessagePack,
}

pub struct RecvAllocOptions {
    /// Caller hint for where payload should land if transport supports it.
    pub preferred_location: MemoryLocation,
    /// How fallback/staging behavior is handled.
    pub policy: RecvPolicy,
}

pub struct RecvIntoOptions {
    /// How fallback/staging behavior is handled.
    pub policy: RecvPolicy,
}

impl Channel {
    pub fn recv_alloc(
        &self,
        allocator: &MemoryAllocator,
        options: RecvAllocOptions,
    ) -> Result<ReceivedFrame>;

    pub fn recv_into(
        &self,
        target: &mut MemoryBuffer,
        staging: Option<&mut CpuMemoryBuffer>,
        options: RecvIntoOptions,
    ) -> Result<ReceivedFrame>;

    /// Convenience wrapper using default receive options.
    pub fn recv_alloc_default(
        &self,
        allocator: &MemoryAllocator,
        preferred_location: MemoryLocation,
    ) -> Result<ReceivedFrame>;

    /// Convenience wrapper for strict receive-into behavior.
    pub fn recv_into_strict(
        &self,
        target: &mut MemoryBuffer,
        staging: Option<&mut CpuMemoryBuffer>,
    ) -> Result<ReceivedFrame>;
}
```

Rationale:

- `recv_alloc`: simple platform-agnostic usage where channel allocates destination memory.
- `recv_into`: caller-managed memory path for streaming performance and custom pooling/ring-buffer strategies.
- `recv_alloc_default` / `recv_into_strict`: ergonomic wrappers for common call paths.

Default guidance:

- Prefer `RecvPolicy::Strict` as the default to avoid implicit slow-path behavior.
- Callers opt in to `AllowStaging` or `Auto` when fallback copies are acceptable.

## Transport Selection (Internal)

- **Local + GPU buffer:** Vulkan IPC (external memory handles)
- **Local + CPU buffer:** shared memory
- **Remote:** MPI point-to-point (CPU staging if needed)

```rust
fn select_transport(scope: CommunicationScope, buffer: &dyn MemoryBuffer) -> Transport {
    match (scope, buffer.is_gpu()) {
        (CommunicationScope::Local, true) => Transport::VulkanIpc,
        (CommunicationScope::Local, false) => Transport::CpuSharedMemory,
        (CommunicationScope::Remote, _) => Transport::MpiPointToPoint,
    }
}
```

## Metadata Envelope (Planned)

Payload and metadata are transported separately:

- Payload: `MemoryBuffer` and transport handles.
- Metadata: serialized control-plane envelope.

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

`Channel` also supports typed metadata via serde:

- send typed: `M: Serialize`
- receive typed: `M: DeserializeOwned`
- receive dynamic: `MessageMeta`

Serialization format is configured per channel (for example via `MetadataEncoding`).

## Semantics (All Transports)

- **Ordering:** producer order is preserved per channel
- **Ownership:** sender must not mutate in-flight buffers
- **Backpressure:** bounded queues or transport-level flow control
- **Errors:** surfaced via unified `Result<T, Error>`
- **Synchronization:** in Phase 5+, `Channel` handles external semaphore wait/signal internally

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

# Phase 3: Local Channel Runtime

**Status:** In Progress

## TL;DR

Implement Layer 2 for local communication with directional endpoints, payload-only frames, and separate metadata.
Transport selection remains internal, and receive materialization behavior is configured once on the receiver endpoint.
Phase 3 starts with a synchronous API, keeps raw interprocess handle types private behind channel internals, and
targets point-to-point local channels first.

## Scope

- Directional endpoint API for local scope (`Sender`, `Receiver`)
- Distinct builders (`SenderBuilder`, `ReceiverBuilder`)
- Synchronous send/receive API for the first implementation
- Point-to-point local channels only for the first implementation
- Trait-based channel allocator integration over concrete Layer-1 allocator backends
- Vulkan IPC transport (external memory handles + shared metadata)
- Local CPU shared-memory transport
- Two receive variants:
  - typed default: `recv::<M>() -> (Frame, M)`
  - dynamic map: `recv_map() -> (Frame, MessageMeta)`
- Receiver-owned allocation strategy (no per-recv target hints)
- Lightweight endpoint introspection (`scope()`, `receive_representation()`)
- Local-only tests and benchmarks

## Current Implementation State

Implemented now:

- internal local bootstrap with deterministic endpoint naming from `ChannelId`
- public `Builder::sender(channel_id, my_location, peer_location)` /
  `Builder::receiver(channel_id, my_location, peer_location)` for local scope
- convenience `Builder::local_sender(channel_id)` / `Builder::local_receiver(channel_id)` using
  hostname detection for the common same-host case
- `Sender` / `Receiver` local runtime for point-to-point CPU and GPU shared-memory transport
- versioned tagged local control protocol
- local frame headers for CPU and GPU payloads:
  - CPU: buffer size
  - GPU: buffer size plus logical GPU device id for receive-side import
- explicit receiver import `ImportOk` / `ImportFailed` ACK/NACK
- CPU shared-memory handle transfer and import
- GPU external-memory handle transfer and Vulkan import/export integration
- configurable local protocol size limits:
  - payload cap
  - metadata cap
- receiver-side `build_with_timeout(...)` for startup-tolerant local connect
- sender-side `build_with_timeout(...)` for bounded local accept
- `BuildCancel` support for cancelling blocking sender and receiver construction
- Windows local access control:
  - duplex named pipe
  - explicit current-logon-session DACL
- Unix local access control:
  - private per-user runtime directory
  - secure fallback order:
    - `LAVA_FLOW_RUNTIME_DIR`
    - `XDG_RUNTIME_DIR/lava-flow/`
    - `/run/user/<uid>/lava-flow/`
    - `$HOME/.local/run/lava-flow/`

Not implemented yet:

- remote builder/runtime path
- receiver-side materialization allocator integration
- true inter-process CPU and GPU test coverage
- bootstrap authentication and optional peer-identity validation

## Phase Ordering

Implementation has proceeded in this order:

1. Local point-to-point CPU shared-memory transport implementation with internal listen/connect bootstrap. **Done.**
2. Builder integration around deterministic local endpoint naming derived from `ChannelId`. **Done.**
3. GPU allocator changes required for device-local, importable external-memory allocations. **Done.**
4. Local point-to-point GPU external-handle transfer and Vulkan IPC integration. **Done.**
5. True inter-process tests for CPU and GPU local IPC paths. **Remaining.**
6. Local bootstrap hardening. **Remaining.**
   - optional OS peer validation during connection establishment
   - shared-secret HMAC challenge/response before any handle transfer or message I/O

Rationale:

- CPU transport already has real handle export/import primitives and lower platform risk.
- Local transport bootstrap should be owned by the library rather than pushed to users.
- GPU transport had additional complexity beyond the control plane:
  - device-local/importable allocation requirements
  - Vulkan import path implementation
  - external synchronization follow-up
- Bootstrap authentication is valuable, but it should follow the basic point-to-point transport and
  real inter-process tests so the security layer is built on a stable rendezvous flow.
- Starting with CPU first reduces uncertainty while preserving the same channel API and transport envelope shape.

## Deliverables

- `Builder::sender(...)` and `Builder::receiver(...)` returning distinct builders
- `Sender` / `Receiver` endpoint types
- `ChannelAllocator` trait with fixed-target allocation-only implementations
- No `src/memory/unified.rs` planning; allocator composition stays in traits/builders and existing module boundaries
- Payload frame type (`Frame`) without embedded metadata
- Metadata contract (`Metadata` + `MessageMeta`) with mandatory metadata envelope but no mandatory
  `used_size` field; the transport carries buffer size in the frame header, and applications may add
  valid-byte counts or other interpretation fields to metadata when needed
- Receiver-level `ReceiveRepresentation` (`ExternalShare`, `Materialized`)
- Sender-side metadata serialization configuration (codec selection), propagated during connect
- Local CPU shared-memory transport integration
- Local GPU external-memory transport integration through the common local IPC protocol
- Internal local rendezvous model (`listen` / `accept` / `connect`) hidden behind builders
- Versioned tagged local control protocol with explicit receiver import ACK/NACK
- Configurable local envelope-size limits enforced before metadata allocation or shared-memory
  import
- Integration tests for local IPC paths
- Platform control-plane plan for real inter-process tests:
  - Unix: Unix-domain sockets with `SCM_RIGHTS` for fd transfer
  - Windows: named pipes for coordination plus native handle duplication/transfer for OS handles

## Local Bootstrap Boundary

Phase 3 local channels should use an internal bootstrap flow that is common in shape across Unix and
Windows:

- a deterministic local endpoint name is derived from `ChannelId`
- one side acts as transport server and performs `listen` / `accept`
- the other side acts as transport client and performs `connect`
- after connect, the sender performs per-message handle transfer as needed
- local control messages are versioned so incompatible protocol revisions fail fast

Users should not manage socket paths, pipe names, or connection ordering directly.

```mermaid
flowchart LR
    A[SenderBuilder inputs] --> C[derive_local_endpoint]
    B[ReceiverBuilder inputs] --> C
    C --> D[local listener]
    C --> E[local connector]
    D --> F[accepted local Sender]
    E --> G[connected local Receiver]
```

### Builder Assumption

The builders do not need live peer process objects. They only need shared bootstrap identity that
both processes can know independently:

- `ChannelId`

That identity is sufficient to derive the same local rendezvous endpoint in both processes.

If the peer later creates its own sender for reverse application traffic, that reverse channel must
use a distinct `ChannelId`.

### Why This Matters On Windows

Windows shared-memory handle duplication only needs the receiver process once the local connection is
open. That is acceptable because the actual duplication happens at message-send time, not at
builder-construction time.

### Current Security Baseline

The current local runtime now applies platform-local access control before challenge/response is
introduced:

- Windows:
  - duplex named pipe is created with an explicit DACL
  - default policy is current logon session only
  - opt-in authenticated-users policy uses the Windows Authenticated Users SID, not Everyone
  - the same pipe carries bootstrap, envelopes, and import ACK/NACK traffic
- Unix:
  - local sockets are created under a private per-user runtime directory
  - `LAVA_FLOW_RUNTIME_DIR` is the preferred explicit override for containers/orchestrators
  - otherwise `XDG_RUNTIME_DIR/lava-flow/` is preferred when available
  - if that is unavailable, `/run/user/<uid>/lava-flow/` is used when present
  - final fallback is `$HOME/.local/run/lava-flow/`
  - the selected runtime directory is required to be non-symlinked, owned by the effective user,
    and forced to `0700`
  - opt-in authenticated-users policy uses a socket directly under the system temporary directory,
    with socket mode `0666` and a sticky world-writable parent directory requirement
- Local protocol limits:
  - default payload cap is `1 GiB`
  - default metadata cap is `1 MiB`
  - the local sender/listener and receiver are constructed with explicit limits
  - public builders expose separate local maximum payload and metadata size setters so callers do
    not have to pass two same-typed limit values in one call

This is the currently implemented baseline. It reduces cross-user access and bounds envelope
resource use, but it does not yet authenticate that the connected peer is the intended process.

### Current Phase 3 Default

For local point-to-point transport bootstrap, the current default should be:

- `Sender` acts as transport server
- `Receiver` acts as transport client

That default keeps the bootstrap model compatible with a future local 1 -> many broadcast mode,
where one logical sender may accept multiple transport clients over time.

## Deferred Fan-Out

Multiple receivers for one sender are explicitly out of scope for Phase 3.

Future fan-out support should be introduced as a separate semantic mode, most likely local
broadcast/fan-out, with these runtime implications:

- one sender maintaining multiple connected local peers
- one handle transfer per receiver on each message
- immutable payload contract after `send`

Remote fan-out should remain compatible with the same semantic boundary:

- MPI can support one-to-many delivery through repeated point-to-point sends or collective
  communication such as broadcast
- the concrete MPI mapping should remain a transport/runtime choice rather than changing the public
  channel payload or metadata model

This expansion should reuse the same bootstrap model and public `Frame` / metadata API rather than
introducing a second payload abstraction.

## Deferred Bootstrap Authentication

Bootstrap authentication should be implemented as a later Phase 3 hardening step, not as part of
the initial local transport bring-up.

Recommended shape:

- primary authentication mechanism:
  - nonce-based challenge/response using a shared secret
  - HMAC over a bootstrap transcript including at least:
    - role
    - `ChannelId`
    - both nonces
    - local protocol version
- authentication must complete before any handle transfer or channel message I/O
- optional OS peer validation should be supported as a pre-auth filter:
  - Unix:
    - `SameUser`
    - peer credentials on connected Unix sockets
    - optional expected `ProcessId`
  - Windows:
    - current logon session remains the access-control baseline
    - optional expected `ProcessId` from connected named-pipe peer APIs

Rationale:

- shared-secret HMAC challenge/response is the primary defense against malicious same-user or
  same-session processes that can still reach the endpoint name
- optional `SameUser` / `ProcessId` checks are defense-in-depth and pre-auth filtering, not the
  main authentication mechanism
- `ProcessId` is useful for orchestrated cases where the caller already knows the peer PID, but it
  should stay optional because PIDs are ephemeral and app-supplied

## Encapsulation Boundary

`InterprocessMemoryHandle` should remain private in Phase 3 unless a concrete transport or interop integration cannot be
implemented cleanly without exposing it.

Preferred boundary:

- Layer 1 exposes stable allocators and buffers
- Layer 2 local transports perform handle export/import internally
- Public channel APIs expose `Frame`, metadata, and allocator configuration rather than raw OS handles

If later language interop work requires direct handle access, that should be introduced as a narrower public export/import
API rather than by exposing all transport internals by default.

## Deferred Follow-Up

- Async receiver loops and dispatch integration are explicitly deferred until the synchronous local runtime shape is
  validated in code.
- ADR-010 currently describes a non-blocking API that does not match the sync-first Phase 3 plan; reconcile that after the
  synchronous local runtime is proven out.
- Local fan-out / multi-receiver semantics are deferred until point-to-point local transport is stable.
- Bootstrap authentication is deferred until the basic point-to-point local runtime and true
  inter-process tests are stable.

## Example (API Shape)

```rust
let channel_id = ChannelId::new("image-stream")?;

// Sender process. For local scope, build waits until the receiver peer connects.
let tx = Builder::sender(channel_id.clone(), my_loc.clone(), peer_loc.clone())
    .with_metadata_encoding(MetadataEncoding::Json)
    .build()?;

// Receiver process.
let rx = Builder::receiver(channel_id, my_loc, peer_loc).build()?;

let meta = ImageMeta {
    valid_bytes: Some(payload_bytes),
    width: 1920,
    height: 1080,
};

tx.send(payload_buffer, &meta)?;
let (frame, meta) = rx.recv::<ImageMeta>()?;
let valid_bytes = meta.valid_bytes;

let representation = rx.receive_representation();
```

## Related Docs

- [Channels Spec](../spec/channels.md)
- [Interop Overview](../spec/interop/README.md)
- [ADR-002 GPU API Selection](../adr/002-gpu-api-selection.md)
- [ADR-010 Channel Buffer Strategy](../adr/010-channel-buffer-strategy.md)

## External References

- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)
- [Vulkan External Semaphore](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_semaphore.html)

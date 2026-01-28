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

## External References

- [MPI Standard](https://www.mpi-forum.org/docs/)
- [OpenMPI Documentation](https://www.open-mpi.org/doc/)
- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)

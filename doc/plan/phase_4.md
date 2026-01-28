# Phase 4: MPI Transport

## TL;DR

Implement Layer 2 for remote communication: MPI point-to-point transport with rank resolution and scope detection.
The public `Channel` API remains identical to Phase 3; MPI is an internal transport.

## Scope

- MPI init/finalize wrapper
- Rank resolution from process locations
- MpiTransport send/recv (blocking and optional non-blocking)
- CI-safe tests gated behind `mpirun`

## Deliverables

- `MpiContext` wrapper
- `RankResolver` for hostname -> rank mapping
- `MpiTransport` implementation
- Integration tests runnable with `mpirun -n 2`

## Example (API Shape)

```rust
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
channel.send(frame)?;
let received = channel.recv()?;
```

## Related Docs

- [Channels Spec](../spec/channels.md)
- [ADR-006 MPI for Inter-Node](../adr/006-mpi-for-inter-node.md)
- [ADR-010 Channel Buffer Strategy](../adr/010-channel-buffer-strategy.md)
- [Phase 3](phase_3.md)

## External References

- [MPI Standard](https://www.mpi-forum.org/docs/)
- [OpenMPI Documentation](https://www.open-mpi.org/doc/)
- [MPICH](https://www.mpich.org/documentation/)
- [UCX](https://openucx.org/)

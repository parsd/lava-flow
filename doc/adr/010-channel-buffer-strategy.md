# ADR-010: Channel Buffer Strategy

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

MPI runtime decides buffer size (~4 MB default in OpenMPI). Non-blocking send/recv, user calls `wait_send()` /
`wait_recv()` for completion. No user-configurable buffer size (simplicity).

## Decision

**MPI manages internal buffers. Non-blocking API with explicit wait.**

## Rationale

**Why MPI manages buffers?**

- Simplicity (no user configuration)
- MPI runtime optimizes per topology
- OpenMPI default (~4 MB) sufficient for most workloads

**Why non-blocking?**

- Overlap communication + computation
- Async/await integration (Phase 3+)

## Implementation

```rust
impl Channel {
    pub fn send(&mut self, frame: Frame) -> Result<()> {
        // Non-blocking: returns immediately
        // MPI buffers internally
        self.mpi_isend(frame)?;
        Ok(())
    }

    pub fn wait_send(&mut self) -> Result<()> {
        // Block until send completes
        self.mpi_wait()?;
        Ok(())
    }

    pub fn recv(&mut self) -> Result<Frame> {
        // Polling: returns immediately if ready
        self.mpi_irecv()
    }

    pub fn wait_recv(&mut self) -> Result<Frame> {
        // Block until recv completes
        self.mpi_wait_recv()
    }
}
```

## Consequences

- **Simple API** — No buffer size tuning
- **Overlap compute** — Non-blocking enables pipelining
- **MPI default** — May not be optimal for all workloads (acceptable for MVP)

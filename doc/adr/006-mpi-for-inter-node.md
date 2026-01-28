# ADR-006: MPI for Remote Communication

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Use MPI point-to-point (not collectives) for process-level synchronization across nodes. Proven, stable,
vendor-optimized (UCX), and standard in HPC.

## Decision

**Use MPI (Message Passing Interface) for Remote communication scope.**

## Rationale

**Why MPI?**

- 30+ years of optimization (since 1994)
- Vendor-optimized transports (OpenMPI with UCX = RDMA on InfiniBand/RoCE)
- Process-level sync (not just tensor collectives)
- Standard in HPC (all clusters support it)
- GPU-aware MPI available (Phase 5+)

**Why not NCCL?**

- Collective operations only (AllReduce, Broadcast), not point-to-point
- Tensor-level (not process-level)
- NVIDIA-focused

**Why not Gloo?**

- ML ecosystem only
- Less mature
- Fewer transport optimizations

**Why not custom TCP?**

- Reinventing 30 years of work
- No RDMA to GPU support
- Harder to optimize

## Implementation

```rust
// Phase 4: MPI-based channel
pub struct MpiChannel {
    rank: i32,
    peer_rank: i32,
    tag: i32,
}

impl MpiChannel {
    pub fn send(&mut self, buffer: &[u8]) -> Result<()> {
        unsafe {
            MPI_Send(
                buffer.as_ptr() as *const c_void,
                buffer.len() as c_int,
                MPI_BYTE,
                self.peer_rank,
                self.tag,
                MPI_COMM_WORLD,
            );
        }
        Ok(())
    }

    pub fn recv(&mut self, buffer: &mut [u8]) -> Result<()> {
        unsafe {
            MPI_Recv(
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len() as c_int,
                MPI_BYTE,
                self.peer_rank,
                self.tag,
                MPI_COMM_WORLD,
                MPI_STATUS_IGNORE,
            );
        }
        Ok(())
    }
}
```

## Consequences

 Leverage 30+ years of HPC optimization Vendor RDMA transports (100 Gbps+ on InfiniBand) Linux: Full support, Windows:
 TCP only (no RDMA)

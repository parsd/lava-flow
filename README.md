# lava-flow

**A Rust GPU and CPU communication library providing zero-copy data movement between processes with transparent routing**
**to optimal transports.**

## Quick Facts

- **Language:** Rust (core), C++ FFI and Python bindings
- **License:** MIT
- **Architecture:** Two-layer (Memory & Allocator + Channel Transports)
- **Scope Detection:** Automatic routing (local -> Vulkan IPC or CPU shared memory, remote -> MPI)

## Overview

lava-flow provides a unified abstraction for GPU and CPU memory allocation and inter-process communication:

- **Layer 1 (Memory & Allocator):** GPU (Vulkan) and CPU (4 strategies) behind unified API
- **Layer 2 (Channels):** Producer/consumer semantics with automatic transport selection (local vs remote)
- **Scope Detection:** Determines optimal communication path based on process location (local vs remote)
- **Zero-Copy for local Channels:** Direct GPU memory sharing via Vulkan external memory handles
- **MPI for remote Channels:** Proven, optimized implementation supporting RDMA

Supports GPU-only clusters, CPU-only HPC, mixed systems, and edge devices with **zero code changes**.

## Documentation

### Start Here

- **[Documentation Index](doc/README.md)** - Map of specs, ADRs, and plans
- **[Architecture](doc/spec/architecture.md)** — System design with two-layer model

### Planning & Design

- **[Design Rationale](doc/plan/design_rationale.md)** — What and why; linking the ADRs
- **[Architecture Decision Records](doc/adr/README.md)** — Detail information on architectural decisions
- **[Implementation Plan](doc/plan/README.md)** — Multi-phase implementation roadmap

## Key Concepts

- [Architecture](doc/spec/architecture.md)
- [Memory model and types](doc/spec/memory.md)
- [Channel semantics and transports](doc/spec/channels.md)

## Building

### Phase 1 (Core Types)

```bash
cargo build
cargo test --lib
```

### Phase 2+ (With Vulkan)

```bash
# Requires Vulkan SDK 1.2+
cargo build --all-features
cargo test
```

### Phase 4+ (With MPI)

```bash
# Requires OpenMPI 4.0+ or MPICH 3.3+
cargo build --features=mpi-backend
mpirun -n 2 cargo test
```

## Platform Support

See [Platform Support](doc/platform_support.md) for the full matrix and notes.

## License

MIT License — See [LICENSE](LICENSE) file for details.

## Quick Links

- [Architecture](doc/spec/architecture.md)
- [Implementation Plan](doc/plan/README.md)
- [Design Philosophy](doc/plan/design_rationale.md)
- [Architecture Decision Records](doc/adr/README.md)

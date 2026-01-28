# Implementation Index

This index maps implementation areas to their corresponding specs and ADRs.

## Core Types and Scope

- Spec: [Architecture](spec/architecture.md) (scope detection, layering)
- ADR: [001 Scope Detection](adr/001-scope-detection.md)

## Layer 1 - Memory and Allocator

- Spec: [Memory](spec/memory.md)
- ADRs:
  - [002 GPU API Selection](adr/002-gpu-api-selection.md)
  - [003 External Memory Handle Types](adr/003-external-memory-handle-types.md)
  - [004 Vulkan Version Requirement](adr/004-vulkan-version-requirement.md)
  - [005 CPU Allocation Strategies](adr/005-cpu-allocation-strategies.md)

## Layer 2 - Channels and Transports

- Spec: [Channels](spec/channels.md)
- ADRs:
  - [002 GPU API Selection](adr/002-gpu-api-selection.md)
  - [006 MPI for Inter-Node](adr/006-mpi-for-inter-node.md)`
  - [007 Async Runtime Selection](adr/007-async-runtime-selection.md)
  - [010 Channel Buffer Strategy](adr/010-channel-buffer-strategy.md)

## Interop

- Spec: [Interop](spec/interop/README.md)
- ADRs:
  - [002 GPU API Selection](adr/002-gpu-api-selection.md)
  - [003 External Memory Handle Types](adr/003-external-memory-handle-types.md)

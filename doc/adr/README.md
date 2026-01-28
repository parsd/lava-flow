# Architecture Decision Records (ADRs) — Updated Index

Complete list of architectural decisions for lava-flow with finer granularity.

When to references ADRs:

- **Understanding why:** Read the relevant ADR
- **Proposing changes:** Write new ADR using this template
- **Debugging:** Check related ADRs for assumptions
- **Code review:** Verify compliance with ADR decisions

## Quick Navigation

| ADR | Title | Status | Impact |
| --- | ----- | ------ | ------ |
| [001](001-scope-detection.md) | Scope Detection Strategies | Accepted | Core routing logic |
| [002](002-gpu-api-selection.md) | GPU API Selection | Accepted | Layer 1 foundation |
| [003](003-external-memory-handle-types.md) | External Memory Handle Types | Accepted | Zero-copy mechanism |
| [004](004-vulkan-version-requirement.md) | Vulkan Version Requirement | Accepted | Compatibility |
| [005](005-cpu-allocation-strategies.md) | CPU Memory Allocation Strategies | Accepted | Layer 1 CPU layer |
| [006](006-mpi-for-inter-node.md) | MPI for Remote Communication | Accepted | Layer 2 remote |
| [007](007-async-runtime-selection.md) | Async Runtime Selection | Accepted | Channel implementation |
| [008](008-error-handling-approach.md) | Error Handling Approach | Accepted | Error handling |
| [009](009-no-global-state.md) | No Global State (Design Principle) | Accepted | Architecture |
| [010](010-channel-buffer-strategy.md) | Channel Buffer Strategy | Accepted | Channel buffering |
| [011](011-multi-language-bindings.md) | Multi-Language Bindings (Orthogonal) | Accepted | Language support |

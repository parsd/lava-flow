# Phase 1: Foundation

## TL;DR

Define the core types, errors, and scope detection logic. No GPU APIs or transports yet.

## Scope

- Public types: process names, process locations, channel IDs, communication scope
- Error model and result types
- Scope detection from host/node identity

## Deliverables

- `src/error.rs` with structured errors
- `src/types.rs` with core types and helpers
- Unit tests that lock in naming rules and scope detection

## Example (API Shape)

```rust
use lava_flow::types::{ProcessLocation, CommunicationScope};

let my_loc = ProcessLocation::new("gpu-node-0");
let peer_loc = ProcessLocation::new("gpu-node-1");
let scope = CommunicationScope::from_locations(&my_loc, &peer_loc);
assert_eq!(scope, CommunicationScope::Remote);
```

## Related Docs

- [Spec Index](../spec/README.md)
- [Architecture](../spec/architecture.md)
- [ADR-001: Scope Detection](../adr/001-scope-detection.md)
- [ADR-009: No Global State](../adr/009-no-global-state.md)

## External References

- [thiserror](https://docs.rs/thiserror/latest/thiserror/)
- [serde](https://serde.rs/)
- [hostname crate](https://docs.rs/hostname/latest/hostname/)

# ADR-008: Error Handling Approach

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Use `thiserror` for error types (library interface) and `anyhow` only for internal contexts. Reduces boilerplate from 50
to 5 lines per error type.

## Decision

**Use `thiserror` for public error types.**

## Rationale

**Why thiserror?**

- Idiomatic Rust (derives `std::error::Error`)
- Minimal boilerplate (5 lines vs 50)
- Automatic `Display` implementation
- Source chain support

**Why not custom enums?**

- 50+ lines boilerplate per error type
- Easy to forget `source()` implementation
- Manual `Display` formatting

**Why not anyhow?**

- Type-unsafe (stringly-typed)
- Unsuitable for library APIs
- OK for internal use

**Why not snafu?**

- Overkill (more features than needed)
- More boilerplate than thiserror

## Implementation

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AllocationError {
    #[error("GPU out of memory: requested {requested} bytes")]
    GpuOutOfMemory { requested: usize },

    #[error("NUMA node {node} not available")]
    NumaNodeNotFound { node: u32 },

    #[error("Hugepages not available")]
    HugepagesUnavailable,

    #[error("GPU pinning failed: {reason}")]
    GpuPinningFailed { reason: String },

    #[error("Vulkan error: {0}")]
    Vulkan(#[from] vk::Result),
}
```

## Consequences

 5 lines vs 50 (90% reduction) Idiomatic Rust Type-safe error handling

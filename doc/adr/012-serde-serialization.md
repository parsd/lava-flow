# ADR-012: Serde for Data Serialization

**Status:** Accepted | **Date:** 2026-02-10 | **Supersedes:** None

## TL;DR

Use `serde` as the standard serialization framework for lava-flow data types and metadata. Derive
`Serialize`/`Deserialize` on portable value types and avoid serializing runtime/resource handles directly.

## Problem

lava-flow needs consistent data encoding/decoding for:

- configuration-oriented structs
- topology metadata (for process and scope-related flows)
- interop with future language bindings and test fixtures

Without a standard framework, serialization logic becomes repetitive and inconsistent across modules.

## Decision

Adopt `serde` as the project-wide serialization foundation.

- Use `#[derive(Serialize, Deserialize)]` on eligible public value types.
- Keep non-portable runtime objects (raw OS handles, live Vulkan/MPI resources) out of direct serialization.
- Prefer explicit wrapper structs for serialized metadata boundaries.

## Rationale

- `serde` is the Rust ecosystem standard and integrates broadly.
- Derive-based implementations reduce boilerplate and maintenance cost.
- Strong typing is preserved at API boundaries.
- Aligns with Phase 1 goals for stable core type definitions.

## Alternatives Considered

### Alternative 1: Manual serialization implementations

**Pros:**

- Full control over all formats

**Cons:**

- High boilerplate and review burden
- Easy to introduce divergence between modules

**Rejected:** Too costly for routine metadata types.

### Alternative 2: Format-specific APIs only (e.g., JSON crate directly)

**Pros:**

- Simple for single-format use cases

**Cons:**

- Couples domain types to one format
- Harder to evolve toward binary formats or mixed environments

**Rejected:** Too restrictive for future phases.

## Consequences

### Positive

- Consistent, reusable serialization model across modules
- Faster implementation for new typed metadata
- Better compatibility with tooling and future bindings

### Negative

- Additional dependency surface (`serde`)
- Potential accidental serialization of fields that should remain internal

## Implementation Notes

- Keep serialization focused on stable data contracts.
- Use type boundaries to prevent leaking runtime internals into serialized forms.
- Revisit optional feature-gating for `serde` only if dependency minimization becomes a priority.

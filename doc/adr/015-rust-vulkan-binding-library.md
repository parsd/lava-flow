# ADR-015: Rust Vulkan Binding Library Selection

**Status:** Accepted | **Date:** 2026-02-21 | **Supersedes:** None

## TL;DR

Use `ash` as the Rust Vulkan binding library for Layer-1 GPU memory work.

## Problem

The architecture selects Vulkan as the GPU API (ADR-002), but implementation still needs a Rust binding choice that:

- Exposes Vulkan at low-level fidelity needed for external memory/export handles
- Keeps ownership/lifetime behavior explicit for safety review
- Avoids introducing copyleft licensing risk
- Works on Windows and Linux in the current Phase-2 scope

## Decision

Use `ash` (`0.38.x`) as the Vulkan binding library.

## Rationale

- `ash` provides direct, explicit Vulkan bindings with minimal abstraction.
- The explicit API shape matches this repository's design preference for transparent low-level control.
- It supports required external-memory calls used in Phase 2 (`VK_KHR_external_memory_fd` and
  `VK_KHR_external_memory_win32` paths).
- It is actively used in the Rust ecosystem and integrates cleanly with current error-handling and test strategy.

## Alternatives Considered

### `vulkano`

- Higher-level abstraction and ergonomics.
- Rejected for this phase because the project currently prioritizes explicit control and predictable low-level behavior
  over convenience wrappers.

### Raw FFI-only hand-written bindings

- Maximum control, but high maintenance and higher correctness risk.
- Rejected because `ash` already provides maintained Vulkan bindings without sacrificing explicitness.

## Security Review Notes

- `cargo audit` run on 2026-02-21 reported no vulnerabilities in the current lockfile.
- Unsafe Vulkan boundary usage remains localized to Layer-1 GPU implementation modules and is covered by targeted tests.

## License Review Notes

- `ash`: `MIT OR Apache-2.0`
- `libloading` (transitive): `ISC`
- No copyleft dependency introduced by this decision.

## Consequences

- Adds a stable Vulkan binding dependency for Layer-1 GPU backend implementation.
- Keeps GPU code close to Vulkan semantics, which increases verbosity but improves implementation transparency.
- Future higher-level wrappers can still be introduced on top of this boundary without changing public API guarantees.

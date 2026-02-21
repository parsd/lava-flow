# ADR-014: Platform FFI Dependencies for CPU Shared Memory

**Status:** Accepted | **Date:** 2026-02-12 | **Supersedes:** None

## TL;DR

Use target-specific FFI crates for the CPU shared-memory backend:

- Unix targets: `libc`
- Windows targets: `windows-sys` (`Win32_Foundation`, `Win32_Security`, `Win32_System_Memory`)

This supports real OS-backed shared-memory mappings and exportable OS handles in Phase 2.

## Problem

Phase 2 moved CPU allocation from demo-only `Vec<u8>` storage to real shared memory with handle export.
Rust std does not provide complete cross-platform APIs for:

- creating named shared-memory objects / mapping them
- exporting native handle types needed for interprocess transport semantics

Without OS FFI bindings, the implementation cannot provide real shared-memory behavior.

## Decision

Add target-specific dependencies:

- `libc` on Unix for `shm_open`, `ftruncate`, `mmap`, `munmap`
- `windows-sys` on Windows for `CreateFileMappingW`, `MapViewOfFile`, 
  `UnmapViewOfFile`

Keep these dependencies scoped to their platform targets in `Cargo.toml`.

## Rationale

- Minimal dependency surface for low-level OS APIs.
- Matches current architecture: unified allocator API with platform-specific backend internals.
- Enables explicit interprocess handle representation in `InterprocessMemoryHandle`.
- Keeps runtime behavior deterministic and transparent (no hidden fallback to process-local heap).

### Why `windows-sys` instead of `windows`

For this layer we only need low-level Win32 FFI calls and constants. `windows-sys` is preferred because:

- It exposes raw ABI bindings without projection-layer abstractions, matching the crate's explicit handle/pointer model.
- It has a smaller compile-time and binary-impact footprint for this use case.
- It avoids mixing higher-level ergonomic wrappers into a backend that is intentionally thin and OS-primitive oriented.

The `windows` crate remains a valid option for higher-level API surfaces later (for example COM-heavy or richer WinRT
integration), but it is unnecessary for the current shared-memory backend scope.

## Security Review

### Findings (ordered by severity)

1. **Medium: FFI memory lifecycle errors can cause resource leaks or UB**
   - Risk: incorrect map/unmap or lock/unlock ordering.
   - Mitigation: RAII ownership in `SharedMemoryRegion` with `Drop` cleanup on all paths.

2. **Info: Dependency vulnerability scan clean at decision time**
   - `cargo audit` run on **2026-02-12** returned no vulnerabilities for this lockfile.
   - Command output summary:
     - `Loaded 919 security advisories`
     - `Scanning Cargo.lock for vulnerabilities (28 crate dependencies)`

### License check

- `libc`: permissive (MIT OR Apache-2.0).
- `windows-sys`: permissive (MIT OR Apache-2.0).
- Decision is consistent with repository policy to avoid copyleft dependencies.

## Consequences

### Positive

- Real CPU shared-memory implementation is now possible on Unix and Windows.
- Handle semantics align with ADR-003 direction (OS-native FD/HANDLE style transport handles).
- No unnecessary cross-platform abstraction crate added beyond required FFI bindings.

### Negative

- Introduces unsafe FFI paths that require strict review/testing discipline.
- Slightly larger dependency surface and platform-specific code paths.

## References

- [ADR-003 External Memory Handle Types](003-external-memory-handle-types.md)
- [ADR-005 CPU Allocation Strategies](005-cpu-allocation-strategies.md)
- [Phase 2 Plan](../plan/phase_2.md)

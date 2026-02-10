# ADR-013: Hostname Library for Host Identity

**Status:** Accepted | **Date:** 2026-02-10 | **Supersedes:** None

## TL;DR

Use the `hostname` crate for OS-level hostname lookup in Phase 1 scope detection instead of environment-variable-only
lookup.

## Problem

Phase 1 scope detection depends on host identity (`CommunicationScope::from_locations`). Using only
`HOSTNAME`/`COMPUTERNAME` environment variables is less reliable across service contexts, containers, and scheduler-run
processes.

## Decision

Adopt `hostname` (`0.4.x`) for `ProcessLocation::from_hostname()` to retrieve hostname from OS APIs.

## Rationale

- Better cross-platform behavior (Windows + Unix) than env-var-only fallback.
- Keeps Phase 1 API simple (`from_hostname()` returns `Result<ProcessLocation>`).
- Matches ADR-001 objective: robust automatic scope detection.

## Security Review (security-reviewer)

### Findings (ordered by severity)

1. **Low: Hostname is not a trusted security identity**
   - Exploit scenario: an attacker/process can influence host naming in some environments (e.g., container/pod naming)
     and cause local/remote misclassification.
   - Impact: transport selection could be suboptimal or incorrect for edge deployments.
   - Remediation: treat hostname as topology hint only (not authentication); keep conservative fallback to `Remote` when
     identity is ambiguous/empty; add stronger node identity strategy in future ADR work (ADR-001 Strategy 3).
2. **Info: No known published vulnerabilities found for in-use `hostname` path**
   - Checked versions in this repo (`Cargo.lock`): `hostname 0.4.2` with transitive `cfg-if 1.0.4`,
     `windows-link 0.2.1` (and `libc 0.2.181` on Unix targets).
   - `cargo audit` run on 2026-02-10 completed without findings (`Scanning Cargo.lock for vulnerabilities (14 crate dependencies)`).
   - RustSec package advisory index lookup found no entries for these packages at review time.
   - deps.rs package pages report no known vulnerabilities for the reviewed versions.

### Implementation review notes

- `hostname::get()` path is read-only and uses OS APIs (`gethostname` on Unix, `GetComputerNameExW` on Windows).
- Unsafe usage in crate internals is bounded around FFI calls with explicit buffer sizing and error checks.
- `set` behavior exists but is feature-gated upstream; our usage does not enable this feature.
- Our integration maps lookup failure to `LavaFlowError::HostnameDetection` and does not panic.

### Scope and limitations

- This review is dependency-focused and Phase 1 scoped (no FFI bindings in lava-flow yet).

## Consequences

### Positive

- More reliable host detection than env vars alone.
- Minimal integration surface (`hostname::get()` only).

### Negative

- Adds one runtime dependency plus small transitive dependency surface.
- Hostname-based routing remains heuristic, not a security boundary.

## References

- `hostname` crate docs: <https://docs.rs/hostname/latest/hostname/>
- RustSec advisories index: <https://rustsec.org/advisories/>
- deps.rs `hostname 0.4.2`: <https://deps.rs/crate/hostname/0.4.2>
- deps.rs `cfg-if 1.0.4`: <https://deps.rs/crate/cfg-if/1.0.4>
- deps.rs `windows-link 0.2.1`: <https://deps.rs/crate/windows-link/0.2.1>
- deps.rs `libc 0.2.181`: <https://deps.rs/crate/libc/0.2.181>

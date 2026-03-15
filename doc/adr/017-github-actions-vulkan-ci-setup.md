# ADR-017: GitHub Actions Vulkan CI Setup

**Status:** Accepted | **Date:** 2026-03-15 | **Supersedes:** None

## TL;DR

Run Vulkan-dependent CI on GitHub-hosted runners by installing software Vulkan drivers:

- Ubuntu: Lavapipe
- Windows: SwiftShader

Use `jakoch/install-vulkan-sdk-action` in SHA-pinned form under the repository's CI action pinning policy.

## Problem

Phase 2 adds Vulkan-backed GPU allocation tests, but standard GitHub-hosted runners do not guarantee a hardware Vulkan
device. Without an installed Vulkan ICD, the Vulkan tests skip or fail due to missing runtime/driver support.

The CI setup therefore needs to:

- enable Vulkan instance/device creation on GitHub-hosted Linux and Windows runners
- avoid depending on dedicated GPU hardware
- keep the workflow maintainable across both supported Phase-2 platforms
- limit supply-chain risk from any added workflow dependency

## Decision

Update CI to install a software Vulkan environment on GitHub-hosted runners:

- `ubuntu-24.04` uses Lavapipe
- `windows-2025` uses SwiftShader

Use `jakoch/install-vulkan-sdk-action` pinned to commit
`06218f81a3cbd7dce502fdc666c8db2af725b442` (`v1.4.0` tag at decision time), consistent with [ADR-016](016-github-actions-third-party-action-pinning.md).

## Rationale

- The chosen Vulkan action directly supports the exact CI need for this repository: Vulkan SDK setup plus optional
  Lavapipe and SwiftShader installation.
- A single action keeps Linux and Windows workflow logic aligned and reduces local scripting and registry setup in CI.
- Software drivers are sufficient for current Phase-2 tests, which validate allocator/runtime behavior rather than GPU
  performance characteristics.

## Alternatives Considered

### Custom shell provisioning in workflow steps

- Avoids a third-party action dependency.
- Rejected for now because it would duplicate substantial OS-specific install logic, especially on Windows where ICD
  registration is more awkward.

### Self-hosted or GPU-backed runners

- Provides hardware-backed Vulkan coverage.
- Rejected for current Phase 2 CI because it increases cost and operational complexity, while software Vulkan is
  sufficient for allocator correctness tests.

### Other Vulkan setup actions

- Alternatives exist in GitHub Marketplace and appear more popular.
- Rejected because the chosen action explicitly supports both Lavapipe and SwiftShader installation in one place, which
  is the main requirement for this repository's matrix.

## Security Review

### Findings

1. **Low: software driver installation expands CI trust surface**
   - Risk: CI now downloads and executes additional binaries beyond the Rust toolchain.
   - Mitigation: keep the dependency surface limited to one pinned action and review workflow changes under repository
     review.

### Notes

- The selected action repository is MIT-licensed and publicly documents support for both SwiftShader and Lavapipe.
- The upstream repository's release notes instruct maintainers to force-update the `v1` tag, which reinforces the
  decision not to reference `@v1` directly.
- No specific suspicious behavior or public security advisory was identified during review of the selected Vulkan setup
  action. The remaining concern is standard third-party action trust, not a confirmed red flag.

## Consequences

### Positive

- Vulkan tests can run on standard GitHub-hosted Linux and Windows runners without dedicated GPU hardware.
- The workflow remains compact and cross-platform.

### Negative

- CI depends on software ICD behavior that may differ from vendor GPU drivers.
- Passing CI does not replace later validation on real hardware.

## References

- [jakoch/install-vulkan-sdk-action](https://github.com/jakoch/install-vulkan-sdk-action)
- [Phase 2 Plan](../plan/phase_2.md)
- [ADR-015 Rust Vulkan Binding Library Selection](015-rust-vulkan-binding-library.md)
- [ADR-016 GitHub Actions Third-Party Action Pinning](016-github-actions-third-party-action-pinning.md)

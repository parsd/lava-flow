# ADR-016: GitHub Actions Third-Party Action Pinning

**Status:** Accepted | **Date:** 2026-03-15 | **Supersedes:** None

## TL;DR

Pin all third-party GitHub Actions to immutable commit SHAs.

## Problem

The CI workflow uses several third-party actions. Mutable refs such as tags and branch names add an avoidable
supply-chain risk because they can change after review.

The repository needs an explicit CI policy that:

- keeps third-party workflow dependencies reviewable and immutable
- reduces the risk of silently changed upstream action code
- still allows first-party `actions/*` usage where appropriate

## Decision

Adopt this general rule for repository workflows:

- all third-party GitHub Actions must be pinned to full commit SHAs
- first-party `actions/*` steps may continue to use major-version tags unless there is a repository-specific reason to
  pin them more tightly

Current pinned third-party actions:

- `dtolnay/rust-toolchain` at `631a55b12751854ce901bb631d5902ceb48146f7` (`stable` ref at decision time)
- `Swatinem/rust-cache` at `42dc69e1aa15d09112580998cf2ef0119e2e91ae` (`v2`)
- `actions-rs/clippy-check` at `eaad5cbab12213484acb251837981a39c27de18d` (`v1`)
- `jakoch/install-vulkan-sdk-action` at `06218f81a3cbd7dce502fdc666c8db2af725b442` (`v1.4.0`)

## Rationale

- GitHub recommends pinning third-party actions to a full-length commit SHA because tags are mutable.
- A repository-wide rule is easier to review and enforce than ad hoc exceptions.
- Immutable pins preserve the exact reviewed workflow dependency set over time.

## Alternatives Considered

### Leaving third-party actions on tags or branches

- Simpler to read in workflow diffs.
- Rejected because mutable refs weaken review integrity and are directly discouraged by GitHub security guidance.

## Security Review

### Findings

1. **Medium: third-party action supply-chain risk**
   - Risk: a compromised action repository or moved tag could alter CI behavior.
   - Mitigation: pin all third-party actions to full commit SHAs instead of mutable tags or branches.

2. **Medium: `actions-rs/clippy-check` is archived / legacy**
   - Risk: archived actions are less likely to receive maintenance, dependency updates, or incident response.
   - Mitigation: keep it SHA-pinned for now, but prefer replacing it with direct `cargo clippy` output or a maintained
     checks/annotation action in a later CI cleanup.

3. **Low: pinned SHAs still require periodic maintenance**
   - Risk: pins can become stale and miss upstream fixes.
   - Mitigation: update pins intentionally through reviewed workflow changes rather than silently through moving tags.

### Notes

- GitHub's secure-use guidance states that pinning to a full-length commit SHA is the only immutable way to consume an
  action release.
- No suspicious behavior or published advisories were identified during review for `dtolnay/rust-toolchain` or
  `Swatinem/rust-cache`.
- `actions-rs/clippy-check` did not show a specific malicious indicator, but its archived upstream is a real
  maintenance risk and is the weakest link among the currently pinned third-party actions.
- No specific suspicious behavior or public security advisory was identified during review of
  `jakoch/install-vulkan-sdk-action`. The remaining concern is standard third-party action trust, not a confirmed red
  flag.

## Consequences

### Positive

- CI behavior is more deterministic because third-party action references are immutable.
- The repository now has an explicit CI policy for third-party action pinning.

### Negative

- Pinned SHAs require deliberate maintenance when upstream fixes are needed.
- One pinned CI action (`actions-rs/clippy-check`) should be considered a future replacement candidate.

## References

- [GitHub Secure use reference](https://docs.github.com/en/actions/reference/security/secure-use)

# Agent Instructions

These instructions apply to coding agents when modifying this repository.

## Required Workflow (Every Code Change)

1. **Format first:** run `cargo fmt`.
2. **Lint next:** run `cargo clippy` and fix all warnings.
   - Run both feature modes:
     - `cargo clippy --all-targets --all-features -- -D warnings`
     - `cargo clippy --all-targets --no-default-features -- -D warnings`
3. **Test last:** run tests for the affected component(s) and any components that depend on them.
   - Run both feature modes:
     - `cargo test --all-targets --all-features`
     - `cargo test --all-targets --no-default-features`

If any step cannot be run, explain why and what would be required to run it.

## Cargo Execution

- Run `cargo` commands outside the sandbox.
- `cargo clean` may be used to clean build targets when needed.

## Design Consistency

- Follow the specs under `doc/spec/` and decisions in `doc/adr/`.
- If a change conflicts with a spec/ADR, update the document and highlight that adjustment in your output
  or call out the mismatch.
- For reusable validation/error reasons, prefer shared enums over local string constants.
- Keep production code free of test-only behavior when possible. Put test-specific setup, naming, and isolation in
  test modules, test support helpers, or test call sites rather than hiding it behind `#[cfg(test)]` in production paths.
- Prefer moving conditional implementations into dedicated `cfg`-selected files/modules when a condition needs more than
  one or two `#[cfg(...)]` sites in the same production module.

## Documentation Standards

- Write rustdoc-compatible documentation for all `pub` types and functions.
- For non-public code, document important or complex implementation details where intent is not obvious.

## Coverage Standards

- Strive for 100% line and function coverage for core logic.
- Coverage should be evaluated in both feature modes when feasible:
  - `cargo llvm-cov --workspace --all-features`
  - `cargo llvm-cov --workspace --no-default-features`
- Region coverage may be lower due to compiler/instrumentation granularity; treat it as a guidance metric, not a hard gate.
- Windows and WSL coverage must keep separate CARGO_TARGET_DIRs. Mixed target directories will corrupt the report inputs.
- Keep all-features and no-default-features coverage target directories separate unless using an explicit
  cargo-llvm-cov merge workflow.
- To merge coverage across feature modes on the same platform, use one platform-specific target directory and
  an explicit clean/no-report/no-clean/report sequence:
  - `cargo llvm-cov clean --workspace`
  - `cargo llvm-cov --workspace --all-features --no-report`
  - `cargo llvm-cov --workspace --no-default-features --no-report --no-clean`
  - `cargo llvm-cov report --summary-only`
- Do not merge Windows and WSL coverage artifacts. Run the merge flow separately for each platform with
  distinct `CARGO_TARGET_DIR` values.

## License Policy

- Do not introduce copyleft licenses (for example GPL) into this library or its dependencies.
- LGPL may be accepted only when there is a strong justification, but it should be avoided by default.

## Skills

A skill is a set of local instructions to follow that is stored in a `SKILL.md` file. Below is the list of
skills that can be used in this repository.

### Available skills

- planner: Researches and plans implementation steps using the repository and web sources. Produces plans and
  documentation updates only (no implementation code changes), and creates focused Mermaid diagrams with implementation
  names. (file: .github/agents/planner/SKILL.md)
- code-reviewer: Senior code reviewer focused on correctness, tests, architecture conformance, and business-logic
  locality. (file: .github/agents/code-reviewer/SKILL.md)
- security-reviewer: Senior security reviewer focused on anti-patterns, dependency vulnerabilities/CVEs, and interop
  safety across Rust/C++/Python boundaries. (file: .github/agents/security-reviewer/SKILL.md)

### How to use skills

- Discovery: The list above is the skills available in this repository.
- Trigger rules: If the user names a skill (with `$SkillName` or plain text) OR the task clearly matches
  a skill's description above, use that skill for the turn.
- Missing/blocked: If a named skill path cannot be read, say so briefly and continue with the best fallback.

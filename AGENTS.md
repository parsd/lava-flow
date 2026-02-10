# Agent Instructions

These instructions apply to coding agents when modifying this repository.

## Required Workflow (Every Code Change)

1. **Format first:** run `cargo fmt`.
2. **Lint next:** run `cargo clippy` and fix all warnings.
3. **Test last:** run tests for the affected component(s) and any components that depend on them.

If any step cannot be run, explain why and what would be required to run it.

## Cargo Execution

- Run `cargo` commands outside the sandbox.
- `cargo clean` may be used to clean build targets when needed.

## Design Consistency

- Follow the specs under `doc/spec/` and decisions in `doc/adr/`.
- If a change conflicts with a spec/ADR, update the document and highlight that adjustment in your output
  or call out the mismatch.
- For reusable validation/error reasons, prefer shared enums over local string constants.

## Documentation Standards

- Write rustdoc-compatible documentation for all `pub` types and functions.
- For non-public code, document important or complex implementation details where intent is not obvious.

## Coverage Standards

- Strive for 100% line and function coverage for core logic.
- Region coverage may be lower due to compiler/instrumentation granularity; treat it as a guidance metric, not a hard gate.

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

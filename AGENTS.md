# Agent Instructions

These instructions apply to coding agents when modifying this repository.

## Required Workflow (Every Change)

1. **Format first:** run `cargo fmt`.
2. **Lint next:** run `cargo clippy` and fix all warnings.
3. **Test last:** run tests for the affected component(s) and any components that depend on them.

If any step cannot be run, explain why and what would be required to run it.

## Design Consistency

- Follow the specs under `doc/spec/` and decisions in `doc/adr/`.
- If a change conflicts with a spec/ADR, update the document and highlight that adjustment in your output
  or call out the mismatch.

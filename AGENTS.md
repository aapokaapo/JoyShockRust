# Instructions for coding agents — JoyShockRust

You are implementing a new Rust project, not translating C++ mechanically. Read all `docs/` specifications before changes. The repository's code is authoritative once implemented; keep documentation aligned with it.

## Non-negotiable principles
1. Correctness, low latency and maintainability take precedence over premature optimization.
2. Never claim a feature works without a test; clearly distinguish synthetic tests, compile checks and physical hardware validation.
3. No filesystem, logging, allocation, blocking IPC, global mutable state, or asynchronous executor in the steady-state per-report processing path.
4. Keep physical HID transport parsing separate from normalized controller input, motion math, mappings, and virtual output.
5. Do not drop IMU samples to meet performance targets. Preserve ordering and explicitly handle gaps, duplicates, rollover and invalid reports.
6. Use safe Rust where possible; isolate and document any unsafe code and its invariants.
7. Prefer bounded channels or atomic configuration snapshots, with changes applied at report boundaries. Do not put a shared configuration mutex on every sample.
8. Treat hardware-specific behavior and proprietary controller output as unverified until measured. Respect third-party licenses.
9. Implement a minimal vertical slice and test it before expanding the workspace.
10. Keep code formatted, clippy-clean and tested. Explain decisions and remaining risks.

## Workflow for each phase
- Identify assumptions, inspect upstream and Linux kernel sources, record evidence.
- State scope, acceptance criteria and files to change.
- Implement the smallest useful increment with tests and examples.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` where available.
- Report what passed, what could not be tested, performance observations, and next actions.
- Never silently change locked architecture decisions; propose an ADR first.

## Required documentation
Maintain the root files `FEATURE_MATRIX.md`, `DECISIONS.md`, and `IMPLEMENTATION_PLAN.md` as work proceeds. Add a source link and date to external technical claims.

## Scope and permissions
Do not push commits, modify upstream repositories, install udev rules, change permissions or alter system configuration without explicit user approval. Do not hide devices globally by default.

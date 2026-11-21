# Architecture decision log

## ADR-001 — Linux-first Rust 2024
**Accepted.** New implementation, not direct C++ port. Keep safe Rust and clear module boundaries.

## ADR-002 — hidraw as primary physical input
**Accepted.** No SDL3/JoyShockLibrary dependency in hot input path. udev discovery and kernel-driver coexistence require validation.

## ADR-003 — event-driven + deadlines
**Accepted.** Read on fd readiness; use deadline timers for timed mapping actions. No artificial global polling tick.

## ADR-004 — dedicated controller processing thread
**Accepted provisionally.** One thread per active controller, benchmark scaling and scheduling jitter before changing.

## ADR-005 — pure deterministic motion library
**Accepted.** No I/O, clock access, threads, Tokio or per-sample allocations. Calibration persistence outside library.

## ADR-006 — separate control plane
**Accepted.** Unix socket IPC, TOML, systemd user service. Tokio not required and prohibited from hot path.

## ADR-007 — output and visibility
**Accepted.** uinput initially, UHID researched for protocol emulation. Per-game environment filtering preferred when proven; no claim of universal HID/evdev hiding.

## ADR-008 — provisional performance goals
**Accepted as targets, not evidence.** See `TESTING_AND_PERFORMANCE.md`.

For changes, add a dated ADR with context, alternatives, consequences and validation plan.

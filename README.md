# JoyShockRust — development guide

JoyShockRust is a **new Linux-first Rust controller mapper**, inspired by the behavior of [JoyShockMapper-systemd](https://github.com/aapokaapo/JoyShockMapper-systemd) and [upstream JoyShockMapper](https://github.com/Electronicks/JoyShockMapper). This repository initially contains **planning documents, not a working implementation**.

## Mission
Correct controller behavior first; then low input-to-output latency, low jitter, low CPU/memory overhead, maintainability, and feature parity. Prefer a small working vertical slice to a broad unfinished rewrite.

## Locked decisions
- Rust stable, edition 2024; Linux first.
- Direct **hidraw** controller acquisition; USB DualSense Edge first, Bluetooth later.
- Event-driven report acquisition (`epoll`/nonblocking reads or equivalent), plus deadline-based timers for timed actions. **No global periodic polling loop by default.**
- One dedicated processing thread per active controller initially; benchmark before changing.
- Platform-agnostic, deterministic `joyshockrust-motion` crate; no I/O or runtime dependencies.
- Linux `uinput` for keyboard/mouse/generic gamepad, and investigate `UHID` for protocol-faithful virtual controllers.
- Independent control plane, Unix socket IPC, TOML profiles, systemd user service.
- No Tokio in the hot path; use it in the control plane only if warranted.
- Controller hiding through launch-environment settings is **optional, per-game, backend-specific**, never presumed universal.

## Read order for ChatGPT/Codex
1. `AGENTS.md`
2. `docs/PRODUCT_REQUIREMENTS.md`
3. `docs/ARCHITECTURE.md`
4. `docs/INPUT_HIDRAW.md`
5. `docs/MOTION_CONTRACT.md`
6. `docs/OUTPUT_AND_VISIBILITY.md`
7. `docs/IMPLEMENTATION_PLAN.md`
8. `docs/TESTING_AND_PERFORMANCE.md`
9. `prompts/START_HERE.md`

## First task
**Do Phase 0 research and Phase 1 design before writing a controller engine.** Verify actual USB HID reports, calibration constants, Linux driver interactions, permissions, and feature semantics against source and hardware. Preserve citations and evidence in research notes.

## Status
Design specification; all targets and protocol assumptions require verification on hardware.

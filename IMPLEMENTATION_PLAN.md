# Phased implementation plan

Each phase requires tests, documentation, a runnable demonstration when appropriate, and a clear hardware-validation status. Do not advance on unverified assumptions.

## Phase 0 — research and feasibility
Inspect both JSM repositories and Linux hid-playstation. Map existing callbacks versus polling, report protocols, calibration, device IDs, permissions, mapping semantics and licensing. Produce `docs/REFERENCE_ANALYSIS.md`, `docs/FEATURE_MATRIX.md`, `docs/DECISIONS.md`. Record source URLs/commit SHAs. **Exit:** evidence-backed hardware protocol and risks.

## Phase 1 — minimal workspace and parser
Create minimal Cargo workspace, lint/CI, typed normalized reports, pure parser with fixtures and fuzz tests. **Exit:** decode captured USB DualSense Edge reports correctly without hardware I/O in tests.

## Phase 2 — end-to-end USB vertical slice
Discover hidraw via udev, open/read events, process buttons/sticks and create uinput virtual gamepad/mouse; clean disconnection. **Exit:** physical USB controller produces verified virtual events; no stuck buttons.

## Phase 3 — pure motion engine
Implement deterministic IMU processing, calibration, sensor fusion and gyro-to-mouse with replay fixtures. **Exit:** calibrated gyro aiming verified on real hardware and replay tests.

## Phase 4 — mapping engine
Add remapping, tap/hold/double-tap, chords, layers, trigger modes, stick curves and Flick Stick with deadline scheduler. **Exit:** deterministic state-machine tests and no unnecessary global polling.

## Phase 5 — config, IPC, CLI and systemd
Versioned TOML, snapshots, Unix socket protocol, diagnostics, udev permissions, systemd user service. **Exit:** live reload at sample boundaries and service restart/reconnect tested.

## Phase 6 — compatibility and transports
Bluetooth DualSense Edge, DualSense, additional controllers, virtual output compatibility, optional game-launch visibility helpers. **Exit:** per-device and per-game compatibility evidence.

## Phase 7 — optimize, harden, prepare GUI
Benchmark p50/p95/p99 and jitter, memory, allocations and CPU. Stress hotplug, errors, high report rates, multiple controllers. Stabilize IPC for GUI. **Exit:** measured performance report and documented limitations.

## Change control
Update `docs/DECISIONS.md` before architectural deviations. Never fake hardware results or benchmark numbers.

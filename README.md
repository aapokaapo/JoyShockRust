# JoyShockRust — development guide

JoyShockRust is a **new Linux-first Rust controller mapper**, inspired by the behavior of [JoyShockMapper-systemd](https://github.com/aapokaapo/JoyShockMapper-systemd) and [upstream JoyShockMapper](https://github.com/Electronicks/JoyShockMapper). The initial workspace provides a pure DualSense Edge USB report parser and device timebase.

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
1. [START_HERE.md](START_HERE.md)
2. [AGENTS.md](AGENTS.md)
3. [PRODUCT_REQUIREMENTS.md](PRODUCT_REQUIREMENTS.md)
4. [ARCHITECTURE.md](ARCHITECTURE.md)
5. [INPUT_HIDRAW.md](INPUT_HIDRAW.md)
6. [MOTION_CONTRACT.md](MOTION_CONTRACT.md)
7. [OUTPUT_AND_VISIBILITY.md](OUTPUT_AND_VISIBILITY.md)
8. [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md)
9. [TESTING_AND_PERFORMANCE.md](TESTING_AND_PERFORMANCE.md)

## First task
**Do Phase 0 research and Phase 1 design before writing a controller engine.** Verify actual USB HID reports, calibration constants, Linux driver interactions, permissions, and feature semantics against source and hardware. Preserve citations and evidence in research notes.

## Status

The parser and timebase are covered by synthetic tests and replay of a stationary USB capture. Live hidraw validation remains pending. See the [implementation plan](IMPLEMENTATION_PLAN.md) and [source analysis](docs/REFERENCE_ANALYSIS.md).

## Local checks

From the workspace root with stable Rust, rustfmt and Clippy installed:

```sh
cargo fetch --locked
cargo fmt --all --check
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo test --locked --offline --workspace
cargo run --locked --offline -p joyshockrust-input --example replay
```

The example replays synthetic packets and prints a Cross press and release alongside device time.

## USB capture and live diagnostics

The default test suite replays 300 stationary reports from a wired DualSense
Edge in `crates/joyshockrust-input/tests/fixtures/dualsense_edge_usb.bin`.
The replay checks neutral controls, forward timestamps, and stationary IMU
ranges using nominal sensor scales. To run just the capture test:

```sh
cargo test -p joyshockrust-input --test usb_replay replays_captured_dualsense_edge_usb
```

On Linux, pass the readable hidraw node for the wired controller to the live dump.
It prints decoded controls, raw IMU counts, and the rate inferred from each
forward device timestamp interval.

```sh
cargo run -p joyshockrust-input --example live_usb_dump -- /dev/hidrawN
```

## Virtual gamepad on Linux

Run the first Linux bridge with a readable DualSense Edge USB hidraw node and
write access to `/dev/uinput`.

```sh
cargo run --locked -p joyshockrust-linux -- /dev/hidrawN
```

The bridge exposes **JoyShockRust Virtual Gamepad** with standard buttons,
D-pad, sticks and analog triggers. It releases held controls when the input
session ends. Start a new invocation after reconnecting. Physical validation
is pending, and games can still see the physical controller.

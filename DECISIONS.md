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

## Phase 0 findings and open decisions

Reviewed 2026-11-22. The existing ADRs stand. Source revisions and physical validation requirements are recorded in the [reference analysis](docs/REFERENCE_ANALYSIS.md).

- **Device and interface.** The pinned Linux driver matches USB Sony `054c:0df2` and expects report `0x01` at 64 bytes. The target firmware, actual interface number and endpoint, Edge-specific buttons and physical kernel coexistence still need capture. Select by udev USB ancestry and corroborate with raw fd identity and descriptor evidence. [Linux probe and IDs](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L2823-L2907) · [USB parser branch](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1432-L1453)
- **Timebase.** Preserve raw device ticks and optional receive time separately. Decide cadence ranges and reset/gap thresholds after reviewing continuous captures. Modular wrap is deterministic to test. Missed input and repeated sensor timestamps must be observable. [Linux device time conversion](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1546-L1562)
- **Calibration.** Keep raw sensor counts until the device-specific conversion has been validated. Linux and SDL use different gyro bias and gain calculations. Choose a conversion after checking the USB feature reply, known orientations and reference motions. Keep factory calibration, runtime bias estimation and bias persistence separate. [Linux calibration](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1052-L1112) · [SDL calibration](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/src/joystick/hidapi/SDL_hidapi_ps5.c#L617-L675)
- **Permissions and visibility.** Verify active-seat and headless read access and keep the kernel driver bound. Treat physical/virtual visibility as backend-specific. Device access and filter behavior remain open for the target host. [Fork device ACL tags](https://github.com/aapokaapo/JoyShockMapper-systemd/blob/bb69784488937e0a5e21988b966eccd9f04d504e/dist/linux/50-sony-gamepad.rules#L25-L33) · [systemd active-seat ACL](https://github.com/systemd/systemd/blob/781d9d0789379d1ea1f2ecefb804d41e9c8b6c38/src/udev/udev-builtin-uaccess.c#L108-L163) · [SDL filter](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/include/SDL3/SDL_hints.h#L897-L913)
- **Workspace and licensing.** Begin with a pure types crate and an input parser crate with no external dependencies. Choose the project distribution license before incorporating external implementation or third-party captures. Record provenance for all shared fixtures. See the [workspace proposal](IMPLEMENTATION_PLAN.md#phase-1--minimal-workspace-and-parser) and [licensing notes](docs/REFERENCE_ANALYSIS.md#licensing-and-reuse).

For changes, add a dated ADR with context, alternatives, consequences and validation plan.

## Phase 1 implementation notes

Recorded 2026-11-22. The two library crates use `no_std` and forbid unsafe code. The parser accepts recognized Edge USB frames, reports a structured error for reserved D-pad values, and preserves raw IMU counts, device ticks and the counter byte. Button and stick decoding is independent of sensor time classification. The report layout and calibration questions are documented in the [reference analysis](docs/REFERENCE_ANALYSIS.md#dualsense-edge-usb-input).

Each `Timebase` tracks one continuous device clock segment. Forward modular deltas below half the counter range advance the clock up to the caller's configured limit, carrying fractional nanoseconds. A larger forward delta starts a new segment. Duplicate, backward and ambiguous timestamps leave the last accepted time intact. A known reconnect or device reset starts a new segment explicitly. The expected cadence and suitable gap limit for real hardware remain open.

The stationary USB capture added in `d7da53b` averaged approximately 1 kHz across 299 timestamp intervals when reviewed on 2026-11-22. The replay test allows up to 2 ms between samples for this fixture. Live diagnostics retain a separate, looser gap limit. These limits do not establish a production gap policy. [Capture](crates/joyshockrust-input/tests/fixtures/dualsense_edge_usb.bin) · [Calibration reference](docs/REFERENCE_ANALYSIS.md#calibration-and-units)

## Phase 2 first increment

Recorded 2026-11-22. Use Linux `poll` with read readiness and bounded nonblocking drains for the single explicit hidraw descriptor. Check the opened descriptor with `HIDIOCGRAWINFO`. The Linux crate uses nix for polling and ioctl definitions and evdev for synchronized uinput batches. The virtual device has its own name and virtual bus identity. [hidraw and uinput references](docs/REFERENCE_ANALYSIS.md#readiness-permissions-and-coexistence)

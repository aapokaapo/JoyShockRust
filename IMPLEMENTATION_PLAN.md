# Phased implementation plan

Each phase requires tests, documentation, a runnable demonstration when appropriate, and a clear hardware-validation status. Do not advance on unverified assumptions.

## Phase 0 — research and feasibility
Review both JSM repositories and Linux hid-playstation for scheduling, protocol, calibration, permissions, kernel coexistence, mapping semantics and licensing. The [reference analysis](docs/REFERENCE_ANALYSIS.md) records exact source revisions. The [feature matrix](FEATURE_MATRIX.md) and [decision log](DECISIONS.md) record findings and open items.

Source review is complete as of 2026-11-22. Phase 0 hardware evidence is pending. The exit requires an annotated wired DualSense Edge capture, factory calibration response, and observed interface, permissions and coexistence behavior. An approved Phase 1 can start with synthetic tests while capture collection is pending. Captured-fixture acceptance remains required.

## Phase 1 — minimal workspace and parser
The offline parser milestone covers synthetic replay and the 300-report stationary USB fixture added in `d7da53b`, reviewed 2026-11-22. Captures of individual controls and motion remain pending. The root Cargo workspace uses stable Rust, edition 2024. See the [source-backed USB layout](docs/REFERENCE_ANALYSIS.md#dualsense-edge-usb-input).

| Crate | Initial contents |
|---|---|
| `joyshockrust-core` | Transport-neutral button, D-pad, stick and analog trigger state. Define normalized axes independently of the packet layout. |
| `joyshockrust-input` | Pure USB parser, raw IMU counts and device ticks, structured errors, and a replayable timebase. Depends only on `joyshockrust-core` and Rust's `core` library. Linux device discovery and I/O are Phase 2 work. |

Create `joyshockrust-motion` when there is calibrated data and motion processing to implement. Its first input should follow the [motion contract](MOTION_CONTRACT.md). The output, mapping, daemon, configuration and IPC crates can wait for their consumers.

| Task | Acceptance |
|---|---|
| Scaffold stable CI | On Linux, build and run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. The parser and timebase tests run offline once the toolchain is available. |
| Define the parser boundary | Take model, transport, report bytes and optional monotonic receive time. Reject unsupported identities/transports and invalid ID/length before field extraction. Use a structured invalid-hat error for reserved values 9–15. Ignore fields outside the initial contract. Pure byte parsing uses fixed-size values, with no heap allocation or I/O per report. |
| Decode and normalize controls | Keep typed D-pad directions and distinguish digital L2/R2 from analog travel. Map stick endpoints to −1 and +1 around nominal byte 128, with positive X right and positive Y down. Map triggers to 0–1. Test endpoints, neutral, eight directions, all supported button bits, signed IMU extrema and little-endian order. Confirm nominal center and Edge-specific buttons against physical traces. |
| Preserve sensor time | Return each report's raw gyro/accel counts and device ticks. Use a separate deterministic timebase to classify first sample, forward delta, wrap, repeated timestamp and discontinuity. Carry tick remainders when converting accumulated ticks to nanoseconds. Reject out-of-order or invalid samples for integration while still allowing synchronous button updates from a structurally valid report. Treat long gaps and reconnect/reset as segment boundaries. Test zero, wrap, backwards progression, repeats, long gaps and clock reset. Set cadence-dependent thresholds from capture evidence. |
| Validate robustness | Test empty, short, oversized and wrong-ID reports. Add property/fuzz cases for arbitrary lengths and bytes, including structurally valid frames with malformed field values. Require no panic and deterministic replay of accepted reports and diagnostics. |
| Replay captured USB fixtures | Preserve report boundaries and order. Keep synthetic inputs separate from physical traces. Record firmware, device profile, descriptor/transport, provenance and expected events per capture. Replay rest, individual controls and motion without hardware I/O. Assert raw counts/ticks, input ordering and control changes against labelled actions and corroborating observations. |

Document the future motion boundary before adding fusion. Define calibrated input as monotonic nanoseconds, °/s and g, with explicit status for invalid samples and segment discontinuity. Specify calibration and motion error types and the cases that preserve or reset state. Hardware calibration and bias learning remain Phase 3 tasks.

**Exit:** source-based synthetic checks and captured USB Edge replay both pass. Physical live input-to-output behavior remains a Phase 2 acceptance test.

## Phase 2 — end-to-end USB vertical slice
Discover hidraw via udev, open/read events, process buttons/sticks and create uinput virtual gamepad/mouse; clean disconnection. **Exit:** physical USB controller produces verified virtual events; no stuck buttons.

The first increment accepts a hidraw node explicitly, checks the USB bus and Edge VID/PID through the opened descriptor, and forwards standard gamepad controls to a uniquely named uinput device. It waits for input readiness and discards the virtual device after releasing held controls on disconnect. Udev discovery, automatic reconnection, and a physical input-to-output test remain open.

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

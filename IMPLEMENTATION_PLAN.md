# Phased implementation plan

Each phase requires tests, documentation, a runnable demonstration when appropriate, and a clear hardware-validation status. Do not advance on unverified assumptions.

## Status on 2026-11-22

Phase 6 compatibility is the current development focus. Phases 0–3 established
source evidence, captured USB replay, a Linux USB input/output bridge and
opt-in gyro aiming. Hardware evidence is partial. Residual bias calibration,
per-axis aiming accuracy and timing benchmarks still need validation.
Profiles, service integration and the control plane are Phase 5 work.
See the [minimum usable product requirements](PRODUCT_REQUIREMENTS.md#p0-minimum-usable-product).

## Phase 0 — research and feasibility
Review both JSM repositories and Linux hid-playstation for scheduling, protocol, calibration, permissions, kernel coexistence, mapping semantics and licensing. The [reference analysis](docs/REFERENCE_ANALYSIS.md) records exact source revisions. The [feature matrix](FEATURE_MATRIX.md) and [decision log](DECISIONS.md) record findings and open items.

Source review is complete. The stationary USB capture and subsequent Linux tests provide partial hardware evidence. Annotated control and motion captures, a shareable factory calibration reply, and broader permission and coexistence checks remain open.

## Phase 1 — minimal workspace and parser
The offline parser milestone covers synthetic replay and the 300-report stationary USB fixture added in `d7da53b`, reviewed 2026-11-22. Captures of individual controls and motion remain pending. The root Cargo workspace uses stable Rust, edition 2024. See the [source-backed USB layout](docs/REFERENCE_ANALYSIS.md#dualsense-edge-usb-input).

| Crate | Initial contents |
|---|---|
| `joyshockrust-core` | Transport-neutral button, D-pad, stick and analog trigger state. Define normalized axes independently of the packet layout. |
| `joyshockrust-input` | Pure USB parser, raw IMU counts and device ticks, structured errors, and a replayable timebase. Depends only on `joyshockrust-core` and Rust's `core` library. Linux device discovery and I/O are Phase 2 work. |

Phases 2 and 3 describe the added Linux and motion crates. The [motion contract](MOTION_CONTRACT.md) defines the calibrated input boundary.

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

The Linux bridge accepts a hidraw node explicitly or uses udev discovery with automatic reconnection. It checks the USB bus and Edge VID/PID through the opened descriptor, waits for input readiness, and releases held controls before dropping the uinput device on disconnect. The [uinput and Steam acceptance tests](TESTING_AND_PERFORMANCE.md#virtual-gamepad-acceptance) cover press/release and reconnect. The Fedora uinput press/release test and Steam USB reconnect step passed on 2026-11-22. Other Steam acceptance steps still need recorded results.

## Phase 3 — pure motion engine
Implement deterministic IMU processing, calibration, sensor fusion and gyro-to-mouse with replay fixtures. **Exit:** calibrated gyro aiming verified on real hardware and replay tests.

The motion engine accepts calibrated IMU samples, applies a supplied gyro bias, and computes deterministic aim deltas and a gravity direction. The opt-in Linux gyro mouse reads USB factory calibration, applies the conversion, and submits relative uinput motion. An additional opt-in startup mode estimates residual bias over a stable window on each connection. Synthetic tests cover motion, bias estimation and the calibration-to-mouse calculation. Persistent profiles, auto-bias learning and [remaining physical gyro checks](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance) remain open.

## Phase 4 — mapping engine
Add remapping, tap/hold/double-tap, chords, layers, trigger modes, stick curves and Flick Stick with deadline scheduler. **Exit:** deterministic state-machine tests and no unnecessary global polling.

Digital holds and toggles passed the user's hardware check. The next increment adds analog L2/R2 gating with separate engagement and release thresholds. Automated tests cover hysteresis, invalid thresholds and fresh session state. The analog gate passed the user's hardware test. The tap/hold gyro binding classifies a press at the 200 ms deadline using monotonic report arrival time. Its effects apply at report boundaries. Double-tap gyro toggling also uses monotonic report times and a 300 ms release-to-release window. A two-button gyro chord is available with either press order. The user reported that the gyro chord worked on hardware. Both virtual sticks now support an optional radial power curve, covered by synthetic tests. The user reported that the stick curve worked as expected. Virtual gamepad button remapping supports simultaneous rules and shared output buttons. The user reported that button remapping worked on hardware. Multiple modifiers can each select a button layer over the base remaps. The selected layer remains active until its modifier is released. A fresh modifier press selects the next layer. Flick Stick emits experimental right-stick turns through the virtual mouse. Initial turn, angle wrap and gap handling are synthetic-tested. Physical game calibration and general deadline-driven output remain open.

## Phase 5 — config, IPC, CLI and systemd
Versioned TOML, snapshots, Unix socket protocol, diagnostics, udev permissions, systemd user service. **Exit:** live reload at sample boundaries and service restart/reconnect tested.

The first Phase 5 increment loads a versioned TOML profile at startup and validates it before opening devices. An offline profile check and a device diagnostics command are available. Diagnostics reports udev candidates and open errors for hidraw and uinput. A systemd user unit can launch a startup profile. The user verified startup and USB reconnect in a Fedora user session. SIGHUP reload validates the profile before replacing settings and applies them between report batches. The user verified valid and invalid reloads on Fedora. Read-only IPC reports USB session and gyro configuration status through the service socket. The user verified that USB status changes on Fedora. The status snapshot now reports the last reload result, and the service stops retrying an invalid startup profile. The first write command reloads the current profile and returns its result to the caller. The systemd user unit uses this command for reload. The user verified synchronous reload success and rejection on Fedora. The user verified valid and invalid preset installation on Fedora. Per-controller gyro calibration persistence remains planned.

## Phase 6 — compatibility and transports
Bluetooth DualSense Edge, DualSense, additional controllers, virtual output compatibility, optional game-launch visibility helpers. **Exit:** per-device and per-game compatibility evidence.

Opt-in Xbox Series X|S and Xbox 360 uinput identities use xpad button, hat
and analog trigger layouts. Automated coverage exercises USB report mapping,
including the Series trigger range and Share button. The user confirmed
Xbox Series output on hardware. Per-game compatibility still needs evidence.
[Xbox output acceptance](TESTING_AND_PERFORMANCE.md#xbox-output-acceptance)

The opt-in Bluetooth path discovers a paired DualSense Edge through udev and
checks the descriptor bus and model. The parser verifies full input and
factory calibration CRCs. The user verified Bluetooth on hardware.
Automatic transport selection prefers an accessible USB node when opening a
session and falls back to Bluetooth. The user verified switching after
disconnect. The Linux bridge also supports standard DualSense `0ce6` on
both transports. The common layout and model-specific button handling
have synthetic coverage. The user verified both models on hardware.
[Standard DualSense acceptance](TESTING_AND_PERFORMANCE.md#standard-dualsense-acceptance)

The DualSense and Edge parsers decode both touch contacts on USB and
Bluetooth. Version 1 profiles can assign independent touch and click
gamepad bindings to a grid of up to 25 regions. Synthetic reports verify
two contacts, boundary crossings and click release. Hardware validation
remains open. [Touchpad grid acceptance](TESTING_AND_PERFORMANCE.md#touchpad-grid-acceptance)
Gyro bindings can also use any grid cell as a digital input. Synthetic
reports cover entering and leaving the cell with either contact and keep
physical clicks independent of the touch input.
[Touchpad gyro acceptance](TESTING_AND_PERFORMANCE.md#touchpad-gyro-acceptance)

Opt-in rumble forwards uinput effects to the physical controller through
evdev. The event loop serves feedback while controller input is idle.
The user verified that forwarding survives reconnection over USB and
Bluetooth. Version 1 profiles can set the transport, output identity and
rumble for a user service. The user verified USB and Bluetooth pulse
tests with the profile. Changes to those settings apply on restart.
[Rumble forwarding acceptance](TESTING_AND_PERFORMANCE.md#rumble-forwarding-acceptance)

Battery and charging state are decoded on both transports and exposed in
the session status. The reading clears on disconnect. The user confirmed
the new fields on Fedora. [Battery status acceptance](TESTING_AND_PERFORMANCE.md#battery-status-acceptance)

The optional SDL launcher applies a per-process ignore list for supported
physical DualSense IDs. Automated tests cover argument forwarding, existing
filters and launch errors. Named game and backend checks remain open.
[SDL visibility acceptance](TESTING_AND_PERFORMANCE.md#per-game-sdl-visibility-acceptance)

Profiles define independent horizontal and vertical gyro scales. A CLI
request can also apply a preset to the running service for the current
session. The startup file remains available through the existing reload
command. [Live gyro sensitivity acceptance](TESTING_AND_PERFORMANCE.md#live-gyro-sensitivity-acceptance)

The profile scales gyro and Flick Stick with a per-game factor. The
calibration CLI uses the controller’s right stick to drive a temporary
mouse and calculates the factor from whole camera turns. It saves the
result to the selected profile. The user verified measurement and saving
after four full in-game turns. The angular accuracy check after applying
the profile remains open. [Game calibration acceptance](TESTING_AND_PERFORMANCE.md#game-sensitivity-calibration-acceptance)

## Phase 7 — optimize, harden, prepare GUI
Benchmark p50/p95/p99 and jitter, memory, allocations and CPU. Stress hotplug, errors, high report rates, multiple controllers. Stabilize IPC for GUI. **Exit:** measured performance report and documented limitations.

The first increment adds per-connection HID decoder counts and an
approximate host-side decoded report rate to the service status reply.
Synthetic input covers successful and rejected reports and connection reset.
USB and Bluetooth measurements are pending.
[Report statistics acceptance](TESTING_AND_PERFORMANCE.md#report-statistics-acceptance)

One-shot benchmarks now measure processing time and host report intervals
through the real bridge. The same run reports device-clock interval
percentiles and counts repeated timestamps, out-of-order samples, gaps
and continuous wraps. USB and Bluetooth baselines, CPU and memory
measurements, and contention and reconnect experiments remain open.
Desktop controls are deferred until the broader GUI work.
[Live benchmarks](TESTING_AND_PERFORMANCE.md#live-bridge-benchmark)

The benchmark can also terminate at a collection deadline and report a
partial result. Synthetic tests cover a silent but connected input.
The report includes wall time, process CPU time, CPU utilization and
peak resident memory. Physical idle and active baselines remain open.

The service counts full read batches. A synthetic burst test checks that
the readiness loop answers status queries between batches and preserves
decoded report order despite malformed reports mixed into the burst.
[Report statistics acceptance](TESTING_AND_PERFORMANCE.md#report-statistics-acceptance)

Discovery now tracks disconnected HID instances by sysfs path. Synthetic
rescans cover stale entries, multiple disconnected candidates and reuse
of a hidraw node name by the same model and transport.
[Rapid reconnect acceptance](TESTING_AND_PERFORMANCE.md#rapid-reconnect-acceptance)

Process-lifetime counters expose started and ended controller sessions.
They allow repeated hotplug runs to verify transitions alongside the
per-session input and output counters.

The Linux test suite asserts zero heap allocations for steady-state USB
report processing through mapped gamepad event encoding and for gyro
integration through mouse event encoding. The checks run without hardware.

The status socket reports virtual gamepad and mouse batch counts for the
active connection. Synthetic tests cover output suppression and reset.
[Report statistics acceptance](TESTING_AND_PERFORMANCE.md#report-statistics-acceptance)

## Change control
Update `docs/DECISIONS.md` before architectural deviations. Never fake hardware results or benchmark numbers.

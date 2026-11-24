# Phased implementation plan

Each phase requires tests, documentation, a runnable demonstration when appropriate, and a clear hardware-validation status. Do not advance on unverified assumptions.

## Current status

Updated 2026-11-24 against `d761a87`. The desktop tray and editor
(Phase 8) are implemented and are the current acceptance focus. Device and
game compatibility (Phase 6) and measured performance (Phase 7) still have
open work. Phase numbers group related deliverables. They do not imply
that every acceptance check in an earlier phase has passed.

| Area | Shipped | Next acceptance step |
|---|---|---|
| Desktop controls | Tray lifecycle and telemetry, independent controller/profile selection, guided calibration, and a GTK/Wayland editor with live previews and exclusive per-button routing. | Verify the latest editor on Fedora with a physical controller. Check keyboard capture, output isolation, tray profile restore, and calibration navigation. |
| Motion and calibration | Device-axis and fusion aim, stationary bias storage, RWC, gyro response and selectable Flick Stick. | Retest rest drift and slow aiming with acceleration on/off in both aim modes. Verify IMU telemetry, calibrated turn angles and haptic cues. |
| Compatibility | One active DualSense or Edge, USB/Bluetooth, uinput identities, rumble and optional SDL filtering. | Exercise controller switching, repeat hotplug and record named native SDL and Proton game results. |
| Performance | Report counters, zero-allocation tests on two synthetic paths, and a live benchmark CLI. | Record USB/Bluetooth idle, active and contention baselines with the host, firmware and transport identified. |

Use the [current acceptance priorities](TESTING_AND_PERFORMANCE.md#current-acceptance-priorities)
for the physical test sequence and the [feature matrix](FEATURE_MATRIX.md)
for evidence by feature.

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

The motion crate provides deterministic device-axis aim, gravity-referenced
fusion aim and stationary bias estimation. The Linux adapter applies factory
calibration, selects the aim path, and emits relative mouse events. Guided and
hold-triggered calibration can store a correction per controller. Replay
covers the math and calibration-to-mouse path. Physical follow-up covers
drift with acceleration, slow movement, and aim across controller poses.
[Motion contract](MOTION_CONTRACT.md) · [Gyro acceptance](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

## Phase 4 — mapping engine
Add remapping, tap/hold/double-tap, chords, layers, trigger modes, stick curves and Flick Stick with deadline scheduler. **Exit:** deterministic state-machine tests and no unnecessary global polling.

Digital and analog gyro activation, base remaps, a single active hold
layer, radial stick curves and calibrated Flick Stick are implemented.
Keyboard and mouse gesture deadlines run in the readiness loop, including
when hidraw is idle. The editor can assign one output family per physical
button, suppressing passthrough for explicit keyboard or mouse routes.
Existing TOML profiles keep their previous routing until edited. The user
verified early gyro activation, remaps, layers and stick curves. Current
output isolation and Flick Stick tuning remain physical acceptance items.
[Remap layer acceptance](TESTING_AND_PERFORMANCE.md#remap-layer-acceptance) · [Keyboard and mouse acceptance](TESTING_AND_PERFORMANCE.md#keyboard-and-mouse-profile-acceptance)

## Phase 5 — config, IPC, CLI and systemd
Versioned TOML, snapshots, Unix socket protocol, diagnostics, udev permissions, systemd user service. **Exit:** live reload at sample boundaries and service restart/reconnect tested.

TOML v1 startup and live profile loads validate before replacing active
settings. The control socket supports reload, load, unload and output
toggles. Separate status queries show connection and reload results.
The user's Fedora tests covered user-service startup, USB reconnect,
valid and rejected reloads, synchronous systemd reload results and
atomic preset installation. Controller preferences, the selected profile
and stationary calibration use separate persistence paths.
[Profile reload acceptance](TESTING_AND_PERFORMANCE.md#profile-reload-acceptance) · [User service](README.md#systemd-user-service)

## Phase 6 — compatibility and transports
Bluetooth DualSense Edge, DualSense, additional controllers, virtual output compatibility, optional game-launch visibility helpers. **Exit:** per-device and per-game compatibility evidence.

Opt-in Xbox Series X|S and Xbox 360 uinput identities use xpad button, hat
and analog trigger layouts. Automated coverage exercises USB report mapping,
including the Series trigger range and Share button. The user confirmed
Xbox Series output on hardware. Per-game compatibility still needs evidence.
[Xbox output acceptance](TESTING_AND_PERFORMANCE.md#xbox-output-acceptance)

The Bluetooth path discovers a paired DualSense Edge through udev and
checks the descriptor bus and model. The parser verifies full input and
factory calibration CRCs. The user verified Bluetooth on hardware.
Automatic transport selection first looks for the remembered eligible
controller. Without a match it prefers USB candidates before Bluetooth. The user verified switching after
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
Bluetooth. Version 1 profiles can set the output identity and rumble for
a user service. Controller transport is configured separately. The user
verified USB and Bluetooth pulse tests. Rumble settings apply on restart.
Gamepad identity can change via a live profile load.
[Rumble forwarding acceptance](TESTING_AND_PERFORMANCE.md#rumble-forwarding-acceptance)

Battery and charging state are decoded on both transports and exposed in
the session status. The reading clears on disconnect. The user confirmed
the new fields on Fedora. [Battery status acceptance](TESTING_AND_PERFORMANCE.md#battery-status-acceptance)

The optional SDL launcher applies a per-process ignore list for supported
physical DualSense IDs. Automated tests cover argument forwarding, existing
filters and launch errors. The user verified the Steam controller list
with the filter. Named game and backend checks remain open.
[SDL visibility acceptance](TESTING_AND_PERFORMANCE.md#per-game-sdl-visibility-acceptance)

Profiles define independent horizontal and vertical gyro scales. A CLI
request can also apply a preset to the running service, which remembers its selection for
service startup. The startup file remains available through the existing reload
command. [Live gyro sensitivity acceptance](TESTING_AND_PERFORMANCE.md#live-gyro-sensitivity-acceptance)

The profile scales gyro and Flick Stick with a per-game factor. The
calibration CLI uses the controller’s right stick to drive a temporary
mouse and calculates the factor from whole camera turns. It saves the
result to the selected profile. The user verified measurement and saving
after four full in-game turns. The angular accuracy check after applying
the profile remains open. [Game calibration acceptance](TESTING_AND_PERFORMANCE.md#game-sensitivity-calibration-acceptance)

## Phase 7 — performance and robustness
Benchmark p50/p95/p99 and jitter, memory, allocations and CPU. Stress hotplug, errors, high report rates and switching among connected candidates with one active controller. **Exit:** measured performance report and documented limitations.

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
[Live benchmarks](TESTING_AND_PERFORMANCE.md#live-bridge-benchmark)

The benchmark can also terminate at a collection deadline and report a
partial result. Synthetic tests cover a silent but connected input.
The report includes wall time, process CPU time, CPU utilization and
peak resident memory. Physical idle and active baselines remain open.

The service counts full read batches. A synthetic burst test checks that
the readiness loop answers status queries between batches and preserves
decoded report order despite malformed reports mixed into the burst.
Status also reports separate keyboard and mouse button batch counts for
the active session, including scheduled gesture output.
[Report statistics acceptance](TESTING_AND_PERFORMANCE.md#report-statistics-acceptance)

Discovery now tracks disconnected HID instances by sysfs path. Synthetic
rescans cover stale entries, multiple disconnected candidates and reuse
of a hidraw node name by the same model and transport.
[Rapid reconnect acceptance](TESTING_AND_PERFORMANCE.md#rapid-reconnect-acceptance)

Auto-discovery also recovers when a controller disconnects during
session setup. A synthetic descriptor test checks that setup errors
still propagate while the controller descriptor is live.

The independent controller settings file can constrain discovery, manual
sessions, live benchmarks and game calibration by model and transport.
Discovery prefers the last saved HID ID and falls back to another
eligible device or waits for hotplug. Diagnostics show exclusions from
the selected filter. Same-model and cross-transport hardware checks remain open.
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

Discovery changes controller selection only through the dedicated CLI
command and closes the existing virtual devices before opening a new
controller. Profile loads keep the current selection. The process
handles a single active controller session. Status shows the effective
filter while waiting, and a headless integration test verifies that
selection changes and profile loads take effect independently.
The user service starts without a profile file and exposes controller
selection and preset loading immediately. A manually started bridge can
enable the same control socket without a startup profile.
The CLI also supports unloading a profile to restore default mappings
during the current session.
Status identifies the active mapping source independently of the result of
the last profile command.
Controller inventory lists connected nodes independently of the service.
Status exposes the active node for comparison with the inventory.
The virtual gamepad can be enabled or disabled through the control socket
while retaining the physical controller session and other outputs.
An opt-in uinput integration test exercises this path without a physical
controller. Kernel device verification is pending on a host with uinput.

Keyboard profiles map digital buttons and D-pad directions to tap, hold
and double-tap keys. Analog trigger travel can drive the same gestures
with independent press and release thresholds per binding. Gestures can
also target mouse buttons through a dedicated virtual pointer. Touchpad
grid contacts, region clicks, and two-input chords can feed these gestures.
Each gesture can emit a keyboard shortcut or a modified mouse click. Profiles can
set the hold threshold and double-tap window per binding.
Tap and double-tap can emit a one-step vertical or horizontal mouse scroll.
Hold actions can repeat wheel steps at a configurable rate.
One- or two-finger touchpad swipes can emit wheel steps at independent axis
distances. Profiles can reverse either axis independently of wheel steps
from mapped buttons or lock each swipe to the axis selected at the
first wheel step. Synthetic tests cover axis selection and reset.
One-finger swipes can drive relative pointer motion with independent
sensitivity and direction per axis and an optional radial start
threshold. Two-finger scrolling takes over when a second finger lands.
The pointer profile maps single- and two-finger taps to configurable
mouse buttons, defaulting to left and right. Both use a configurable duration and
radial travel limit. Synthetic tests cover staggered contacts,
threshold boundaries and cancellation by touch scrolling.
An opt-in session grab can suppress native evdev touchpad events from the
active controller while HID mappings continue. The grab can follow the
active profile or be requested at startup. Hardware validation is pending.
An opt-in uinput test covers the timed mouse tap and held-key cleanup with
both virtual devices and synthetic USB reports.
The opt-in touchpad round trip replays USB reports and checks pointer
motion, timed left and right clicks, and locked-axis scrolling through
the virtual evdev mouse. The default suite runs the same trace through
the USB and Bluetooth decoders and mapper for both controller models.
The desktop touchpad preset combines these mappings with a physical
click-to-drag binding and can be loaded into a running service.
Deadline and output handling have synthetic coverage. The bridge supports
keyboard and mouse profiles with the virtual gamepad disabled. Analog
stick bindings support direction keys,
a radial deadzone with an optional release radius and optional angular
hysteresis at direction boundaries. Inner and outer stick zones have
separate engagement and release thresholds and support hold, toggle,
and one pulse per continuous movement.
The keyboard walk preset combines WASD with a held Shift in the inner
zone. Synthetic USB replay checks its key states. An opt-in uinput test
checks the same sequence through the virtual keyboard.
Flick Stick calibration is covered by output
counts before and after a scaling change.
[Keyboard profile acceptance](TESTING_AND_PERFORMANCE.md#keyboard-and-mouse-profile-acceptance)
[Live controller selection acceptance](TESTING_AND_PERFORMANCE.md#live-controller-selection-acceptance)

## Phase 8 — desktop controls

The desktop tray starts and stops the existing systemd user bridge.
XDG autostart gives the user a login-start preference. The tray shows
controller telemetry and offers independent controller and profile
selectors.

The tray opens a Wayland real-world calibration window for a chosen
profile. The bridge releases its virtual outputs while retaining the live
profile and controller selection. The window uses the existing Flick
Stick measurement and validates and saves the selected profile through
the bridge CLI. The window follows readiness and sweep progress as they
happen and asks for the destination profile after measurement. It
preselects the active file when available. The tray resumes the bridge
after the window exits. Both calibration windows accept controller
navigation while focused, including numeric adjustment and profile
selection and a live result preview in the real-world calibration window.
Flick Stick can snap to cardinal axes within a configurable angular
threshold for the initial turn, with independently scaled held steering
plus selectable stick, engagement and duration. The Sticks editor previews
physical and adjusted input and plots both angular responses. RWC measurement
uses the physical stick angle and full steering sensitivity.
Gyro aim has a minimum movement speed and optional bounded
speed-dependent gain that can be toggled per profile. The default minimum
speed filters small residual bias after calibration. The Gyro editor shows
the speed response and calibrated IMU readings from the active controller.

The tray launches a native profile editor. The editor creates, validates,
saves and applies TOML profiles. Its visual controller layout selects
physical controls and selects an exclusive keyboard, mouse or gamepad
output per physical button in the inspector. Keyboard pickers capture
one key in the editor window. Esc, a click or a 10-second timeout ends
the inline prompt. Mouse buttons use dropdowns and **Unbind** clears
the current action.
Advanced TOML covers the full profile schema.
The Sticks and Gyro pages expose stepped sensitivity controls, typed gyro values, radial
response previews and editable game calibration values. The GTK 4 editor
uses libadwaita navigation and follows the system appearance. The tray
summarizes controller telemetry on one row, omits USB battery status,
and labels the active controller and profile submenus. It can enable and
disable gamepad and gyro mouse individually, with a master switch
that retains both selections. The
bridge restores the previous active profile on service startup. The
user units order a simultaneous restart and tie desktop shutdown to
the graphical session. The Gyro page
supports gyro-only and sensor fusion aim modes, gyro activation bindings
and separate or linked sensitivity adjustment. The editor has a separate
picker for opening and creating profile drafts.

The Touchpad page edits grid dimensions and per-cell gamepad, keyboard
and mouse actions for contact and click independently. It also lets a
cell serve as the gyro input. Resizing checks cell references in gamepad,
gyro and keyboard bindings.

The Sticks page edits keyboard direction keys and inner and outer zones
independently for each stick. The mapping dialog validates the draft with
the bridge before returning to the profile.

The tray can also guide stationary gyro calibration and retain the
correction per controller across restarts. Synthetic tests cover saving,
cancellation and application of the correction.

Profiles can bind the shared five-second stationary calibration to a physical button hold. A strong double pulse marks completion. A separate cue announces the virtual gamepad and gyro mouse becoming ready.
USB actuator feedback marks calibration boundaries and RWC sweep steps.
A standalone CLI test plays the USB cues and counts detents from a physical
right-stick sweep through the existing PCM path.
The editor exposes virtual gamepad identity and separate partial and full
keyboard or mouse trigger actions. Each trigger can keep both stages active
at full travel. Live profile application replaces the virtual gamepad when
its type changes.

Next, run the [current acceptance priorities](TESTING_AND_PERFORMANCE.md#current-acceptance-priorities)
against the current branch on Fedora/Wayland.

## Change control
Update `DECISIONS.md` before architectural deviations. Never fake hardware results or benchmark numbers.

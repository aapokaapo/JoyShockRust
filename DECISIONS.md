# Architecture decision log

The dated phase entries record decisions and implementation at the time.
Use [Architecture](ARCHITECTURE.md) for current runtime behavior and the
[implementation plan](IMPLEMENTATION_PLAN.md#current-status) for current
acceptance work.

## ADR-001 — Linux-first Rust 2024
**Accepted.** New implementation, not direct C++ port. Keep safe Rust and clear module boundaries.

## ADR-002 — hidraw as primary physical input
**Accepted.** No SDL3/JoyShockLibrary dependency in hot input path. udev discovery and kernel-driver coexistence require validation.

## ADR-003 — event-driven + deadlines
**Accepted.** Read on fd readiness; use deadline timers for timed mapping actions. No artificial global polling tick.

## ADR-004 — dedicated controller processing thread
**Accepted provisionally.** One owner for active controller state. The current bridge uses one active session in its readiness loop. Measure scheduling and latency before changing ownership.

## ADR-005 — pure deterministic motion library
**Accepted.** No I/O, clock access, threads, Tokio or per-sample allocations. Calibration persistence outside library.

## ADR-006 — separate control plane
**Accepted.** Unix socket IPC, TOML and systemd user service. The tray and editor are separate clients. The current bridge serves status and control descriptors in its readiness loop. Tokio stays outside that path.

## ADR-007 — output and visibility
**Accepted.** uinput initially, UHID researched for protocol emulation. Per-game environment filtering preferred when proven; no claim of universal HID/evdev hiding.

## ADR-008 — provisional performance goals
**Accepted as targets, not evidence.** See `TESTING_AND_PERFORMANCE.md`.

## Phase 0 findings and open decisions

Reviewed 2026-11-22. The existing ADRs stand. Source revisions and physical validation requirements are recorded in the [reference analysis](docs/REFERENCE_ANALYSIS.md).

- **Device and interface.** The pinned Linux driver matches USB Sony `054c:0df2` and expects report `0x01` at 64 bytes. The target firmware, actual interface number and endpoint, Edge-specific buttons and physical kernel coexistence still need capture. Select by udev USB ancestry and corroborate with raw fd identity and descriptor evidence. [Linux probe and IDs](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L2823-L2907) · [USB parser branch](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1432-L1453)
- **Timebase.** Preserve raw device ticks and optional receive time separately. Decide cadence ranges and reset/gap thresholds after reviewing continuous captures. Modular wrap is deterministic to test. Missed input and repeated sensor timestamps must be observable. [Linux device time conversion](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1546-L1562)
- **Calibration.** Keep raw sensor counts until the device-specific conversion has been validated. Linux and SDL use different gyro bias and gain calculations. Choose a conversion after checking the USB feature reply, known orientations and reference motions. Keep factory calibration, runtime bias estimation and bias persistence separate. [Linux calibration](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1052-L1112) · [SDL calibration](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/src/joystick/hidapi/SDL_hidapi_ps5.c#L617-L675)
- **Permissions and visibility.** Verify active-seat and headless read access and keep the kernel driver bound. Treat physical/virtual visibility as backend-specific. Device access and filter behavior remain open for the target host. [systemd active-seat ACL](https://github.com/systemd/systemd/blob/781d9d0789379d1ea1f2ecefb804d41e9c8b6c38/src/udev/udev-builtin-uaccess.c#L108-L163) · [SDL filter](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/include/SDL3/SDL_hints.h#L897-L913)
- **Workspace and licensing.** Begin with a pure types crate and an input parser crate with no external dependencies. Choose the project distribution license before incorporating external implementation or third-party captures. Record provenance for all shared fixtures. See the [workspace proposal](IMPLEMENTATION_PLAN.md#phase-1--minimal-workspace-and-parser) and [licensing notes](docs/REFERENCE_ANALYSIS.md#licensing-and-reuse).

For changes, add a dated ADR with context, alternatives, consequences and validation plan.

## Live editor input previews

2026-11-24. The editor polls the existing bridge status socket while the
corresponding preview is mapped. The bridge retains only the latest touch
positions for display, using the same decoded reports as the touch mappings.
The editor draws those positions over its draft grid. This keeps the preview
independent of profile activation and avoids a second hidraw reader.
The touch snapshot distinguishes finger release from connection loss.
The touchpad preview projects zone boundaries and contact positions
onto the same tapered surface so both follow the illustrated pad.
Buttons and Touchpad share the bundled DualSense SVG. Touchpad crops the
artwork in view-box coordinates so the grid stays aligned when resized.
The editor applies valid dimensions to the draft when the row or column
value changes and retains the live preview across those edits.
Response charts reuse the bridge's stick travel and projected gyro speed
and apply the editor's draft curve settings to those inputs.

## D-pad remapping

2026-11-24. The bridge expands the physical hat into cardinal directions
at the mapping boundary. It combines mapped outputs into the virtual hat
after processing base rules, layers and touchpad targets. Opposite
directions cancel on their axis.

## Analog and adaptive triggers

2026-11-24. Per-trigger profile settings define two analog stages with
separate pull and release points. The editor uses one continuous action
per stage and retains legacy threshold strings and gesture bindings.
The bridge sends opt-in resistance on connection and on every profile
apply, including an identical reapply. It addresses only configured
actuators and actuators it needs to clear. Trigger-only HID reports leave
rumble and lights under kernel control. DualSense and Edge use the same
trigger framing in the reference implementation reviewed 2026-11-24.
The CLI can test either side without a JoyShockRust profile. The user
confirmed resistance on DualSense and DualSense Edge. On Edge, the
active onboard profile's **Trigger Effect Intensity** was **Off** during
the failed test. Enabling it restored resistance.
[Linux report framing](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1235-L1306) · [DualSense payload reference](https://github.com/x-iso/FlexInput/blob/6efeb8945f2edcb4591c7920d02915b6579cee8a/crates/devices/src/gyro.rs#L2418-L2457)
[DualSense and Edge detection](https://github.com/nowrep/dualsensectl/blob/d3ae2fa272e1bf9a5c08a0ded3f374b40f7c73ac/main.c#L346-L364) · [Trigger reports](https://github.com/nowrep/dualsensectl/blob/d3ae2fa272e1bf9a5c08a0ded3f374b40f7c73ac/main.c#L795-L882)

2026-11-24. The editor sends trigger previews asynchronously through the
local control socket. The bridge writes the selected side in the
control event, leaving the profile, virtual device state and last
reload status alone. Closing the editor or switching drafts requests
restoration of the active profile's effects. A new controller session
starts with the active profile.

2026-11-24. Trigger travel uses the bridge's raw normalized input
snapshot. Polling starts when the chart is mapped, including when the
user selects a trigger in an inspector that is already visible. The
Partial band shows suppression from the Full Pull threshold when stage
overlap is disabled. The focused editor sends the selected trigger's draft
thresholds through the control socket and refreshes them while the
chart is mapped. The bridge applies the existing hysteresis at report
cadence and queues a detent only on a stage transition. The preview
expires when the editor stops refreshing it. It uses the controller
haptics worker with USB PCM and evdev rumble fallback.

## Optional desktop device access

Reviewed 2026-11-24. Ship separate hidraw and uinput fallback rules for
active-seat installations where diagnostics reports a permission error.
The hidraw rule matches the HID parent name for the supported Sony
products on USB or Bluetooth. The kernel uses a bus, vendor and product
prefix when naming those devices. Both rules rely on the distribution's
seat-late ACL processing and leave device mode assignment to the
distribution. Keep the optional runtime touchpad grab as the way to
avoid duplicate desktop pointer input. [Linux HID naming](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-core.c#L2944-L2948) · [systemd seat-late rule](https://github.com/systemd/systemd/blob/781d9d0789379d1ea1f2ecefb804d41e9c8b6c38/rules.d/73-seat-late.rules.in#L10-L16) · [Device access setup](README.md#optional-device-access-rules)

## Project license

2026-11-23. JoyShockRust uses MIT for its own source and documentation.
Retain upstream notices for any reused material and document its source
before importing it. [Project license](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.md)

## Phase 1 implementation notes

Recorded 2026-11-22. The two library crates use `no_std` and forbid unsafe code. The parser accepts recognized Edge USB frames, reports a structured error for reserved D-pad values, and preserves raw IMU counts, device ticks and the counter byte. Button and stick decoding is independent of sensor time classification. The report layout and calibration questions are documented in the [reference analysis](docs/REFERENCE_ANALYSIS.md#dualsense-edge-usb-input).

Each `Timebase` tracks one continuous device clock segment. Forward modular deltas below half the counter range advance the clock up to the caller's configured limit, carrying fractional nanoseconds. A larger forward delta starts a new segment. Duplicate, backward and ambiguous timestamps leave the last accepted time intact. A known reconnect or device reset starts a new segment explicitly. The expected cadence and suitable gap limit for real hardware remain open.

The stationary USB capture added in `d7da53b` averaged approximately 1 kHz across 299 timestamp intervals when reviewed on 2026-11-22. The replay test allows up to 2 ms between samples for this fixture. Live diagnostics retain a separate, looser gap limit. These limits do not establish a production gap policy. [Capture](crates/joyshockrust-input/tests/fixtures/dualsense_edge_usb.bin) · [Calibration reference](docs/REFERENCE_ANALYSIS.md#calibration-and-units)

## Phase 2 first increment

Recorded 2026-11-22. Use Linux `poll` with read readiness and bounded nonblocking drains for the single explicit hidraw descriptor. Check the opened descriptor with `HIDIOCGRAWINFO`. The Linux crate uses nix for polling and ioctl definitions and evdev for synchronized uinput batches. The virtual device has its own name and virtual bus identity. [hidraw and uinput references](docs/REFERENCE_ANALYSIS.md#readiness-permissions-and-coexistence)

Automatic mode subscribes to udev hidraw events before enumerating and filters candidates by their physical USB parent. The opened descriptor must independently match the wired Edge identity. An input session owns one virtual gamepad. When the session ends, discovery resumes and the next session creates a fresh gamepad. An explicit device path remains a single-session diagnostic option. [hidraw discovery reference](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/Documentation/hid/hidraw.rst#L23-L54)

## Phase 3 first increment

Recorded 2026-11-22. The pure engine integrates calibrated device-axis angular velocity into aim deltas and estimates gravity by correcting the gyro prediction with near-1 g acceleration. The sensor factory conversion and Linux mouse output stay outside the crate. Runtime bias estimation remains open. A stationary flag alone is insufficient for learning because it can also classify slow deliberate rotation as stationary. [Motion contract](MOTION_CONTRACT.md)

The experimental USB path uses the SDL HIDAPI convention of subtracting the factory gyro bias and scaling by the endpoint spans. It requires a structurally valid feature report and keeps the default gamepad mode available separately. The Linux mouse accumulates fractional movement within each segment and clears it across gaps. Report validation and the conversion are synthetic-tested. Physical direction, gain and remaining drift need measurement. [Calibration sources](docs/REFERENCE_ANALYSIS.md#calibration-and-units)

The `--calibrate-gyro` mode uses an explicit stationary window at the start of each USB session. It withholds mouse motion until the window completes, then applies a fixed residual bias. The mode keeps profile storage and automatic bias learning separate. The stationary thresholds are provisional pending hardware tests. [Calibration contract](MOTION_CONTRACT.md#calibration)

## Phase 4 first increment

Recorded 2026-11-22. Optional gyro bindings use digital button states from each parsed report. The toggle starts inactive on each connection and flips on a new press after release. Mouse movement is emitted only for intervals with aim enabled at both ends. Inactive reports discard fractional movement. IMU timing and stationary calibration continue independently of the gate. [Gyro acceptance](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

The analog gyro gate uses normalized trigger travel and separate engage/release thresholds to avoid chatter around a single cutoff. Defaults are 45% engage and 35% release. [Gyro acceptance](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

The tap/hold gyro binding compares report arrival time with a 200 ms monotonic deadline. A short release flips the latch. A long hold temporarily enables aim and preserves the latch on release. The gate affects pointer output at report boundaries. Actions that must fire without new input still need an event-loop timer. [Gyro acceptance](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

Double-tap gyro toggling counts complete short taps. Each press must be shorter than 200 ms, and the second release must arrive within 300 ms of the first. A long press cancels a pending pair. Timing uses monotonic report arrival and each USB session starts with an empty gesture state. [Gyro acceptance](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

The gyro chord holds aim active while two distinct digital buttons are down. Button order is irrelevant. It uses the same report-boundary gate as a single hold. [Gyro acceptance](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

The optional stick curve raises the radial magnitude to a power inside the unit circle and scales both components together. Center, full travel and diagonal corners retain their original values. The curve affects virtual gamepad output after HID decoding. [Stick curve acceptance](TESTING_AND_PERFORMANCE.md#stick-response-curve-acceptance)

Button remaps act on the physical button snapshot and publish a single virtual state. Rules apply simultaneously, so swaps work without chaining. Output stays pressed while any mapped or passthrough source is held. Analog axes and physical gyro bindings keep their own input state. [Remap acceptance](TESTING_AND_PERFORMANCE.md#button-remap-acceptance)

Only one remap layer is active at a time. The selected layer remains active until its modifier is released. Other modifiers are mapped as ordinary buttons. Layer entry requires a new press. Simultaneous eligible presses use declaration order. Base rules provide the fallback for buttons absent from the selected layer. Each input report rebuilds the target button state, so entering or leaving the layer while a source is held updates both the old and new virtual buttons in the same event batch. The modifier can still be forwarded as a normal gamepad button. [Layer acceptance](TESTING_AND_PERFORMANCE.md#remap-layer-acceptance)

The generic uinput gamepad emits Linux `BTN_X` for physical Square and `BTN_Y` for Triangle. Linux aliases those codes to `BTN_NORTH` and `BTN_WEST` respectively. SDL's Linux fallback uses X/Y for generic devices and position names for Sony devices. The USB parser retains the physical face positions. Sources checked 2026-11-22. [Linux event codes](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/include/uapi/linux/input-event-codes.h#L380-L389) · [SDL mapping](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/src/joystick/linux/SDL_sysjoystick.c#L2342-L2374)

Flick Stick uses the selected physical stick and device report timing. It defaults to the right stick, a 100 ms initial turn and 90% engagement, follows the shortest angular change while held, and re-arms within 25% radial travel. It clears queued motion on timestamp gaps and keeps the selected virtual stick centered. An angular axial threshold snaps directions close to each cardinal axis and blends continuously back to the physical angle. Only the initial flick uses this response. Held steering tracks physical angle changes from the starting position with a separate multiplier. Calibration sweeps use the physical angle and full sensitivity so small corrections remain possible near each axis. The Sticks editor uses the runtime angle calculation and reads the active controller directly while the preview is visible. The Gyro editor reads the latest IMU snapshot over the existing status socket. The bridge stores the raw sample and active bias during report processing and converts them to physical units when serving that socket. The mouse sensitivity is configurable because host and game settings determine the resulting turn. [Flick Stick acceptance](TESTING_AND_PERFORMANCE.md#flick-stick-acceptance)

## Phase 5 first increment

Recorded 2026-11-22. TOML profiles have a required schema version and reject unknown fields. Startup validates profile values with the same button and curve validators used by the CLI. The process owns one resolved configuration for all USB sessions. Profile persistence remains future work. [Profile example](profiles/dualsense-edge.example.toml)

Device diagnostics shares automatic USB Edge enumeration and fd identity verification with normal startup. It checks uinput access by opening the builder and exits before creating a device. [Diagnostic acceptance](TESTING_AND_PERFORMANCE.md#device-diagnostics-acceptance)

The optional systemd user unit starts the installed bridge in discovery mode with a startup profile under the user manager's configuration directory. It preserves the operator's user identity and reports startup and reconnect status through the journal. [User service acceptance](TESTING_AND_PERFORMANCE.md#user-service-acceptance)

Profiles can be reloaded with SIGHUP. Linux polls a signalfd alongside hidraw or udev readiness, and the session validates new settings between report batches. Rejection preserves the previous profile. A mapping change resets layer selection, and gyro option changes prepare a replacement mouse before dropping the current one. [Reload acceptance](TESTING_AND_PERFORMANCE.md#profile-reload-acceptance)

The user service publishes a read-only snapshot on a Unix stream socket in its private runtime directory. Each connection receives one versioned status response and closes. The bridge serves a bounded number of status connections per readiness wake. [Service status acceptance](TESTING_AND_PERFORMANCE.md#service-status-acceptance)

Updated 2026-11-22. The user unit uses `Type=notify`. The bridge sends
`READY=1` after binding the status socket and starting the udev monitor, so a
status query can connect as soon as the service start command completes.
[systemd startup notification](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#Type=)

Updated 2026-11-22. An invalid or unreadable startup profile exits with
`EX_CONFIG` (78), which the user unit excludes from automatic retries.
The version 1 snapshot includes the most recent reload result. The process
records success only after applying the profile, and a rejected reload keeps
the previous settings. [systemd restart settings](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#RestartPreventExitStatus=)

Updated 2026-11-22. The user unit requests reload through a dedicated
Unix socket in its private runtime directory. One connection requests one
reload and receives its result after the bridge has applied or rejected the
profile. The unit uses the client's exit status to report reload success or
failure. Direct SIGHUP remains available. [systemd reload settings](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#ExecReload=)

Updated 2026-11-22. Profile installation validates and stages the same
source bytes before renaming them into place. The default destination
follows the user configuration directory. The installed file has private
permissions. Profile installation and live reload are separate operations.
[XDG configuration directory](https://specifications.freedesktop.org/basedir-spec/latest/)

Updated 2026-11-22. The optional Xbox 360 output advertises USB input ID
`045e:028e` and analog-only triggers. It shares the normalized controller
state and event batching with the default virtual gamepad. The output
identity is chosen when the bridge starts and stays fixed across profile
reloads. [Linux xpad device ID](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/input/joystick/xpad.c#L153-L156) · [Linux xpad event layout](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/input/joystick/xpad.c#L908-L956)

Updated 2026-11-22. Xbox Series X|S output advertises wired USB input ID
`045e:0b12` and input version `0507`. It follows xpad's 0–1023 analog
trigger range and emits `KEY_RECORD` for Share from the DualSense Edge
touchpad click. [Linux xpad device ID](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/input/joystick/xpad.c#L165-L167) · [Linux xpad axis ranges](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/input/joystick/xpad.c#L1913-L1929) · [Linux SDL controller database](https://github.com/mdqinc/SDL_GameControllerDB/blob/4e87e3c6c2574f49e1cdf8526b6f829b61e5ccb3/gamecontrollerdb.txt#L1933)

Updated 2026-11-22. Bluetooth discovery is opt-in and identifies the hidraw
candidate through its HID parent's ID. The opened descriptor is checked again
before decoding. The bridge accepts full Bluetooth input reports and
validates their input CRC. Factory feature replies use the separate feature
CRC seed. Both paths reuse the USB common report layout and calibration
math. [Linux HID identity](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-core.c#L2870-L2877) · [Linux Bluetooth input](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1432-L1453) · [Linux feature CRC](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L783-L816)

Updated 2026-11-22. Automatic transport selection tries USB candidates first,
then Bluetooth candidates. The active descriptor determines report framing
and factory calibration until the session ends. The next transport starts a
new session with fresh virtual devices. The status snapshot includes the
active transport. [Automatic transport acceptance](TESTING_AND_PERFORMANCE.md#automatic-transport-acceptance)

Updated 2026-11-22. Both Sony DualSense models are identified from the open
hidraw descriptor. Model selection controls interpretation of the extra
button bits, while the common controls and sensor counts use the same
decoder. USB remains first in automatic mode. On a given transport,
discovery tries the Edge first. [Linux HID IDs](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-ids.h#L1257-L1267) · [Linux device match](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L2898-L2906)

Updated 2026-11-22. Opt-in rumble routes uinput effects to the physical
controller's evdev node under the same HID parent. Linux hid-playstation
owns transmission to the controller. Virtual effect IDs map to
session-owned physical effects, which are removed on erase or session end.
The virtual gamepad advertises rumble when a writable physical node is
available. [Linux DualSense playback](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1621-L1640) · [Linux effect ownership and erase](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/input/ff-core.c#L124-L187)

Updated 2026-11-22. Rumble discovery filters sibling input nodes by their
force feedback bitmap before opening them, then checks for `FF_RUMBLE`
with evdev. The optional udev rule uses the existing active-seat ACL
mechanism and matches physical DualSense or Edge event nodes with force
feedback capabilities. [systemd active-seat ACL](https://github.com/systemd/systemd/blob/781d9d0789379d1ea1f2ecefb804d41e9c8b6c38/src/udev/udev-builtin-uaccess.c#L108-L163)

Updated 2026-11-22. The wired Edge rumble diagnostic can send a single
legacy USB pulse directly on a verified Edge hidraw node. The diagnostic
sends a zero-amplitude report on completion or ordinary termination. The
bridge still forwards rumble through evdev. [Linux report format and rumble flags](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1290-L1327) · [SDL legacy rumble](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/src/joystick/hidapi/SDL_hidapi_ps5.c#L692-L710)

Updated 2026-11-22. Each rumble-enabled session retries a missing or
permission-denied physical evdev node for up to one second before creating
its virtual gamepad. This allows for udev initialization and active-seat ACL
application after hidraw discovery. [Rumble reconnect acceptance](TESTING_AND_PERFORMANCE.md#rumble-forwarding-acceptance)

Updated 2026-11-22. The optional version 1 `[bridge]` table stores transport,
virtual gamepad mode and rumble settings. Existing profiles keep their
defaults. Command-line transport and gamepad overrides are resolved at
startup. Reload rejects changes to virtual-device capabilities.
[Bridge profile settings](README.md#toml-profiles)

Updated 2026-11-22. The common DualSense input payload supplies battery
state at byte 52, which is USB report byte 53 and Bluetooth report byte 54.
The bridge uses the Linux midpoint estimate for known charge states and
reports an unavailable percentage on error states. The status snapshot
holds the latest reading for the current session.
[Linux input layout](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L235-L258) · [Linux battery decoding](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1582-L1611)

Updated 2026-11-22. The launcher applies SDL's VID/PID ignore hint at
process launch. It leaves system device access in place and reports
inherited allow-list conflicts. SDL's two lists have explicit precedence,
so a successful launch alone supplies no evidence of a per-game result.
[SDL filter precedence](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/src/joystick/SDL_gamepad.c#L2742-L2758)

Updated 2026-11-22. Independent gyro sensitivity is applied after angular
integration and before fractional pointer accumulation. Sensitivity-only
changes preserve the motion timebase, calibration and virtual mouse.
A CLI preset load sends up to 64 KiB of TOML over a companion datagram
socket in the service runtime directory. The running bridge validates and
applies it with the same transactional reload path. The startup profile
remains the source for ordinary reload and restart.
[Live gyro sensitivity acceptance](TESTING_AND_PERFORMANCE.md#live-gyro-sensitivity-acceptance)

Updated 2026-11-22. The gyro profile's game factor is
`real_world_calibration / in_game_sens`. It multiplies each gyro axis and
the independent Flick Stick scale. Defaults preserve existing profiles.
The calibration command measures a known relative mouse displacement and
computes `counts / (360 × measured turns) × in_game_sens`. It assumes the
same mouse input settings used to measure and play.
[JoyShockMapper scaling](https://github.com/Electronicks/JoyShockMapper/blob/e148efe0b6ff4ad2ce0fc8c2560fa1028097b1a7/JoyShockMapper/src/main.cpp#L1166-L1174) · [Calibration acceptance](TESTING_AND_PERFORMANCE.md#game-sensitivity-calibration-acceptance)

Updated 2026-11-22. Stick calibration counts the signed horizontal mouse
movement between two confirmations at the same on-screen landmark. The
operator supplies the number of complete turns. The command edits only
the two calibration fields and replaces the profile atomically after
validating it. [Game calibration instructions](README.md#game-sensitivity-calibration)

Updated 2026-11-22. The version 1 status reply adds decoded and rejected HID
report totals for the active connection and a host-time average decoded
report rate. The input loop aggregates counts per read batch and records
the first successful batch time once per connection. A status query
computes the rate over elapsed host time, including idle periods. The
counters persist across profile reloads and reset on disconnect.
[Report statistics acceptance](TESTING_AND_PERFORMANCE.md#report-statistics-acceptance)

Updated 2026-11-22. The opt-in bridge benchmark brackets the normal
report callback with monotonic clock reads. It preallocates sample storage
and sorts after closing the virtual devices. The interval distribution
uses the same host timestamps. Normal runs skip benchmark timing and
storage. [Live benchmarks](TESTING_AND_PERFORMANCE.md#live-bridge-benchmark)

Updated 2026-11-22. The benchmark classifies device timestamps with the
shared input timebase and a 50 ms forward limit, matching the current
motion defaults. It records device intervals after timing the report
callback and preallocates sample storage for them. The timebase advances
independently of the configured gyro, and diagnostics leave chronological
report delivery intact. [DualSense sensor timestamp](docs/REFERENCE_ANALYSIS.md#dualsense-edge-usb-input)

Updated 2026-11-22. Status counts read batches that consume all 32 report
slots, using totals already accumulated by the readiness loop. Valid
and rejected HID reports both count toward the batch budget. The metric
observes userspace bursts and resets on disconnect.
[hidraw read and queue behavior](docs/REFERENCE_ANALYSIS.md#readiness-permissions-and-coexistence)

Updated 2026-11-22. Auto-discovery retains disconnected hidraw sysfs paths
until a scan no longer lists them. Monitor events trigger a new scan.
The open path checks the descriptor's model and transport and re-resolves
its device number through udev to check the observed sysfs path. Linux
assigns a new HID instance ID during device registration, so a replacement
can be selected when it reuses the same hidraw name.
[Linux HID device naming](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-core.c#L2943-L2952)

Updated 2026-11-22. One-shot benchmarks use an absolute monotonic
collection deadline. Poll derives its remaining wait from the deadline
after interruptions, and timeout uses the normal virtual-output cleanup
path. Ordinary bridge runs wait for readiness without a deadline.
[Live benchmarks](TESTING_AND_PERFORMANCE.md#live-bridge-benchmark)

Updated 2026-11-23. Status counts successful synchronized uinput output
batches separately for the gamepad, gyro mouse, keyboard and mouse
button device. The keyboard and mouse button counts include deadline
events and releases during profile reload. Counters reset with the HID
session.
[Report statistics acceptance](TESTING_AND_PERFORMANCE.md#report-statistics-acceptance)

Updated 2026-11-22. The live benchmark samples `getrusage(RUSAGE_SELF)`
on both sides of the report loop. CPU values are interval deltas. Peak RSS
comes from the final process high-water mark and includes sample storage.
The loop itself adds no resource queries.
[Linux getrusage](https://man7.org/linux/man-pages/man2/getrusage.2.html)

Updated 2026-11-22. The status socket counts controller sessions as they
enter and leave the connected state. These counts persist across
reconnects and reloads within the bridge process. They update only at
session transitions.
[Rapid reconnect acceptance](TESTING_AND_PERFORMANCE.md#rapid-reconnect-acceptance)

Updated 2026-11-22. The Linux tests use a test-only thread-local allocation
counter. It measures the steady-state USB decoding and mapping path and
gyro integration with synthetic event sinks. Release builds use the
standard allocator.
[Allocation counter](https://docs.rs/allocation-counter/0.8.1/allocation_counter/)

Updated 2026-11-22. If a session in auto-discovery fails, the bridge polls
the still-open physical HID descriptor once without waiting. A hangup or
error flag permits reconnect recovery and the original error is logged.
Other failures propagate. This covers detach races before the read loop
starts, including factory calibration reads.
[Linux hidraw poll](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hidraw.c#L263-L274)

Updated 2026-11-22. The optional bridge model setting narrows the
transport filter to standard DualSense or DualSense Edge. Discovery
applies the filter at enumeration and validates the opened descriptor.
The default accepts both models. Model and transport share the
controller selection boundary.
[Rapid reconnect acceptance](TESTING_AND_PERFORMANCE.md#rapid-reconnect-acceptance)

Updated 2026-11-22. Profile-aware diagnostics enumerate supported HID
controllers, show the same model and transport filter as startup, and
probe access for matching nodes. Excluded inventory entries are identified
from udev metadata. [Device diagnostics](README.md#device-diagnostics)

Updated 2026-11-22. Profile selection can optionally require a six-octet
controller ID. The bridge reads `HIDIOCGRAWUNIQ` from the opened
descriptor because the Linux DualSense driver sets the HID unique string
to the controller address. The setting is checked after model and
transport, regardless of the reused hidraw node name. Diagnostics display
actual addresses only through an explicit opt-in.
[DualSense driver](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1755-L1760) · [hidraw API](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/include/uapi/linux/hidraw.h)

Updated 2026-11-22. The common DualSense payload supplies two contact slots.
Grid bindings are opt-in in version 1 profiles. Either active slot can
hold a touch binding. The mechanical click selects the first active slot's
binding on the press and holds the selection through release. A bound
click takes precedence over the ordinary click mapping. Grid targets merge
with remapped gamepad buttons after remapping, so shared targets remain
held until all sources release. Grid edits are applied at the existing
profile reload boundary.
[Linux contact and click decoding](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L1564-L1581) · [JSM grid processing](https://github.com/Electronicks/JoyShockMapper/blob/e148efe0b6ff4ad2ce0fc8c2560fa1028097b1a7/JoyShockMapper/src/main.cpp#L185-L208)

Updated 2026-11-22. Gyro bindings can select a touch grid cell with
`touch:ROW:COLUMN`. Either decoded contact can hold that input. The gyro
gate uses the physical HID report and the same grid geometry as gamepad
bindings. Profile validation resolves the cell even when it has no
gamepad binding. [Touch region gyro input](README.md#touch-region-as-a-gyro-input)

Updated 2026-11-23. The independent controller configuration stores model,
transport and the last identifiable controller. Discovery prefers the
remembered device, falls back to its first usable candidate, and waits
on udev when none is available. The saved ID is a preference. An explicit
live ID filter can require a particular controller for the current run.
Profiles control mappings and virtual outputs. Legacy controller fields
in version 1 profiles are parsed for compatibility and ignored.
[Live controller selection acceptance](TESTING_AND_PERFORMANCE.md#live-controller-selection-acceptance)

Updated 2026-11-22. Status version 1 includes the effective model and
transport filter and whether an individual controller ID is required.
The status payload omits the actual HID ID. Discovery updates the
filter snapshot between sessions, before it serves the next query.
The status reply includes the active device node path. The standalone
inventory uses the diagnostics scan and shows HID IDs only on request.
[Live controller selection acceptance](TESTING_AND_PERFORMANCE.md#live-controller-selection-acceptance)

Updated 2026-11-23. The CLI can update the current HID ID, model, and
transport filters. Only a changed selection triggers the single-session
handoff and closes the active outputs before discovery resumes. The command
preserves the active profile and records no profile reload outcome.
The control socket also accepts selections and inline presets when no
startup profile path is configured. The systemd unit treats a missing
startup file as default mappings and still reports an invalid file as a
configuration error.

Profile unload applies default mappings using the existing live update
path. It releases held keyboard and mouse buttons and leaves the startup
virtual gamepad configuration and current controller filter in effect.
Status reports the active source separately from the last reload outcome.
Only an applied profile update changes the source. It survives controller
disconnects and switches.

Virtual gamepad emulation can be toggled at runtime independently of the
profile. Disabling releases the virtual controls and closes the gamepad
and force-feedback descriptors. Enabling recreates the configured gamepad
identity and starts publishing from the next HID report. The readiness
loop refreshes its feedback descriptor before polling again.
The status socket reports the current virtual gamepad sysfs path when the
kernel supplies one. An opt-in Linux integration test observes the actual
device lifecycle with synthetic HID input and production control commands.
[Live controller selection acceptance](TESTING_AND_PERFORMANCE.md#live-controller-selection-acceptance)

Updated 2026-11-22. Keyboard bindings consume physical button states
and D-pad directions. The controller readiness loop schedules gesture
deadlines and key releases in the same thread. Key state and tap
storage are allocated at device setup. The keyboard advertises Linux
keyboard capabilities so udev recognizes it even with few bindings.
A startup profile can suppress virtual gamepad creation while retaining
keyboard and mouse output.
[systemd keyboard classification](https://github.com/systemd/systemd/blob/781d9d0789379d1ea1f2ecefb804d41e9c8b6c38/src/udev/udev-builtin-input_id.c#L366-L371)

Updated 2026-11-23. Keyboard stick mappings use radial boundaries and
eight directional sectors. Inner and outer zones each use hysteresis
and can be combined with direction keys. The inner zone follows the
stick's radial deadzone state and engages below its entry radius.
A shared key stays held until all of its sources release.
Held zone keys are pressed before direction keys
when both change on the same report. Toggle mode requests a key pulse on
entry and exit and keeps a release interval between pulses. If the zone
changes again before the next pulse can start, the latest zone determines
whether another press is needed. Keep-sprint mode pulses once and
re-arms when the stick has released all of its mapped direction keys.
Pulsed zone keys have a single binding. Changes from one input report
are emitted together, with zone pulses following movement presses.
Direction keys can also use a smaller release radius near center. This
preserves continuous movement in `keep_sprint` mode through minor stick
motion near the deadzone boundary.
An optional angular margin separates each direction's engagement and
release boundaries. The ratios are computed at profile load and angular
state resets when the stick reaches its radial release threshold.

Keyboard trigger bindings use normalized travel and share the gyro
trigger gate's threshold behavior. Each distinct threshold pair tracks
its own pressed state and feeds the existing keyboard gesture logic.
Digital L2 and R2 bindings continue to read their report bits.

Keyboard and trigger gestures can also target the five standard mouse
buttons. Mouse button bindings use a dedicated virtual pointer so they
work without gyro and can be reloaded without rebuilding gyro state.
Each virtual device receives only its own event batch.

Keyboard and mouse gestures can read touchpad grid cells directly. Touch
reads both live contacts. A region click latches the first active
contact on the mechanical press edge until release, matching gamepad
region selection. Deadlines reuse the last report's contact state.

Keyboard and mouse chords are symmetric pairs of physical inputs. Each
member retains its own trigger hysteresis. Both must be active for the
chord's gesture, and standalone bindings remain independent.

Keyboard gesture outputs accept either a single target or a set of
keyboard and mouse buttons. A gesture activates its set together and shares key
ownership with other bindings. Ctrl, Shift, Alt and Meta precede regular
key presses and follow regular key releases. Across devices, mouse
releases precede the keyboard update and mouse presses follow it.

Updated 2026-11-23. Tap and double-tap can emit one relative wheel step
on the button mouse. Wheel actions use gesture timing and generate no key
release event. Wheel capabilities are advertised for profiles that use them.
[Linux relative events](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/Documentation/input/event-codes.rst)

Hold scroll actions reuse the gesture deadline loop. Each wake can deliver
one step for the binding and schedules the next step from the current
time. A release cancels its deadline.

Touchpad swipe scrolling uses relative motion of one tracked contact or
the midpoint of two tracked contacts.
The mapper retains partial wheel distances in each configured axis and
resets them as soon as the tracked contact or pair changes. Two-finger
tracking identifies contacts independently of their report slot order.
The feature shares the virtual wheel with mapped gestures.
Each axis can invert its emitted touch scroll steps independently.
The setting applies to both finger counts and leaves mapped wheel
actions in their configured directions.
An opt-in axis lock selects an axis at the first emitted touch wheel
step and keeps it for the current tracked contact or pair. Simultaneous
steps select by absolute step count, with vertical taking ties.

Touchpad pointer motion uses one tracked contact and preserves fractional
counts independently per axis. Adding a second contact clears its baseline,
so two-finger scrolling and single-finger pointing can share the pad.
Pointer direction can be inverted per axis. The profile resolves the
sign into the sensitivity when it initializes the pointer mapper.
An optional radial threshold suppresses initial pointer motion until
the contact moves far enough from its starting point. The crossing
report starts motion without replaying suppressed movement. A contact
change or two-finger gesture resets the threshold.

One-finger left clicks and two-finger right clicks can be enabled
independently. The tap window runs from first contact to final lift.
The mapper tracks each finger by contact ID and retains its original
position across report slot swaps and staggered lifts. Excess travel,
contact replacement, a wheel step from touch scrolling or a mechanical
press cancels the candidate until all contacts lift. Clicks share the
pulse deadline used by other mapped buttons.
Both tap counts use the profile's time window and per-finger radial
travel limit. They default to 200 ms and 40 touchpad units.
Each tap count can override its mouse button using the same five
button codes supported by keyboard-profile bindings.
The opt-in desktop preset combines pointing, tapping and scrolling
with an immediate held left button on mechanical pad press. The
existing held-binding path supplies dragging and cleanup.

Updated 2026-11-23. The opt-in native touchpad grab is scoped to an evdev
node under the active hidraw device's HID parent. The opened node must
have the driver touchpad name, button-pad property and both multitouch
axes. The session owns the grab and closing the descriptor releases it.
The keyboard profile can request the grab independently of its mappings.
Loading or unloading a profile applies its request between report batches.
The CLI flag keeps the grab requested across profile changes.
[Linux touchpad registration](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L930-L950) · [evdev grab lifetime](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/input/evdev.c)

Keyboard bindings can override hold and double-tap deadlines independently.
Both timers use the existing monotonic report and readiness clock. The
default timings and immediate behavior of hold-only bindings are retained.
[Keyboard profile acceptance](TESTING_AND_PERFORMANCE.md#keyboard-and-mouse-profile-acceptance)
## Desktop profile selection

The tray profile submenu controls the loaded profile. The editor's own
picker controls the file being edited and asks before discarding a draft.
Save & Apply remains the action that applies the draft to the bridge.
When run with an optional startup profile, the bridge remembers successful
live profile selections in a separate file next to the startup profile.
An explicit unload is remembered as default mappings. The startup file
is still the target of a startup profile reload and serves as fallback
if a remembered profile cannot be opened or validated.
Gamepad and gyro mouse selections are process-local and independent.
The master switch suppresses both outputs while retaining the selections.
Status exposes the master, individual choices, and effective output states.

## Desktop process lifecycle

2026-11-23. The input bridge stays supervised by systemd in the user
session. The separate tray process owns the StatusNotifierItem menu and
uses the bridge's existing control and status interfaces. Put the login
preference in XDG autostart, which launches the same tray unit as the
application menu. Systemd serializes launches of the tray unit. The
tray's Quit action stops the bridge before its menu disappears. Both
services stop with the graphical session. A tray restart is ordered
after a simultaneous bridge restart, so the tray can start the bridge
when opened without propagating a stop job during a tray restart.
Desktop polling and command execution stay outside the input process.
[systemd.unit](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html)
· [graphical-session.target](https://www.freedesktop.org/software/systemd/man/latest/systemd.special.html#graphical-session.target)
[XDG Autostart](https://specifications.freedesktop.org/autostart-spec/latest/)
· [StatusNotifierItem](https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/)

2026-11-24. The desktop launcher uses the editor's bundled DualSense SVG.
The user installation places it in the `hicolor` fallback theme.
[Icon Theme Specification](https://specifications.freedesktop.org/icon-theme-spec/latest/)

The running profile editor uses the desktop entry's application ID
so Wayland shells can associate its window with the installed icon.
[XDG Shell application ID](https://wayland.app/protocols/xdg-shell#xdg_toplevel:request:set_app_id)

For tray RWC calibration, the bridge ends its active output session
before acknowledging the pause and keeps serving control requests.
Discovery resumes with the existing profile and controller filter.
The tray waits for the GTK calibration window, then resumes the bridge.
The calibration window opens the selected hidraw node read-only and
watches it through a bounded nonblocking source on the GTK main loop.
It handles navigation presses only while focused. Options remains
available to the measurement worker. The review screen uses the same
RWC calculation as the save command.
The window runs the same Flick Stick sweep as the interactive CLI.
Measurement emits line-delimited status on readiness and Options
transitions. The GTK window drains these messages asynchronously before
reaping the worker. Measurement and saving are separate commands. The
save command validates
and atomically updates the profile. Closing the window during measurement
stops and waits for the measurement process before the tray resumes
normal outputs.

The tray also offers controller gyro calibration. A dedicated measurement
reads the selected controller without creating a virtual pointer, using
factory calibration and the same five-second stationary window as gyro
startup and the controller shortcut.
Saving is explicit and stores the residual bias by stable controller ID.
The bridge reads that correction when opening gyro output. A saved
correction takes precedence over optional session-start estimation.

2026-11-23. An opt-in physical hold binding starts the stationary
estimator after the button is released and the start cue has time to
settle. The report loop hands off persistence and feedback through
bounded queues. Calibration cues and RWC detents use native USB
actuator PCM. An audio worker finds the playback device under the
same USB parent as the active controller. It sends 48 kHz four-channel
PCM and keeps the audible channels silent. Bluetooth actuator streaming
is pending hardware validation. RWC detents use the angular sweep
before conversion to mouse counts. The diagnostic command uses the same
PCM player and angular detector for standalone testing.
Gyro calibration completion uses a longer, higher-amplitude double pulse.
A separate ready cue follows activation of both virtual outputs. A worker
handles these cues using USB PCM or evdev rumble so playback stays outside
report processing.

[USB audio descriptor](https://github.com/nondebug/dualsense/blob/02338358b6b4be2d64dbf661ae2a47842156cfcf/lsusb-descriptor-info.txt#L143-L156) · [Actuator channel routing](https://github.com/nondebug/dualsense/blob/02338358b6b4be2d64dbf661ae2a47842156cfcf/web-haptics-polyfill/README.md#L19-L24)

The editor is a separate native window. Its controller diagram
selects a physical input, and the inspector edits keyboard and mouse
gestures and base gamepad remaps. Advanced TOML preserves access to
the rest of the profile schema. The editor uses the bridge CLI for
validation and applying profiles. New profiles use a no-clobber save.
Existing profiles use the bridge's validated atomic replacement.
The desktop editor uses the native Wayland window backend.
2026-11-23. The physical-control selection and target-picker pattern
was inspired by the DualSense Tester profile page.
[DualSense Tester mapping page](https://github.com/daidr/dualsense-tester/blob/5bc7ecee9f9bd09c7a562b5f0f2cbc0c35c710f9/src/router/DualSenseEdge/views/_Profile/pages/CustomButtonMapping.vue)

The editor groups buttons, stick response and gyro aim into separate
pages. The visual response preview shows the same radial power curve
that the bridge uses for virtual gamepad axes. Each stick may override
the shared profile exponent. The game calibration fields write only
when both values and their ratio pass validation.
[DualSense Tester stick settings](https://github.com/daidr/dualsense-tester/blob/5bc7ecee9f9bd09c7a562b5f0f2cbc0c35c710f9/src/router/DualSenseEdge/views/_Profile/pages/JoystickSensitivity.vue)

2026-11-23. The editor uses GTK 4 and libadwaita for adaptive navigation,
preference rows, feedback and dialogs. Only the controller outline and
curve plots use custom drawing. The toolkit handles system appearance.
[GNOME Human Interface Guidelines](https://developer.gnome.org/hig/patterns/index.html)

The editor’s Touchpad page updates the existing grid and region TOML.
Resizing rejects a dimension change if it would invalidate a cell
referenced by gamepad, keyboard or gyro bindings. Touch and click reuse
the keyboard gesture editor. Gyro uses the profile's single activation
binding. Moving a simple touch binding keeps the existing sensitivity
and calibration settings. Button, trigger and chord bindings remain
editable in Advanced TOML. Saving uses the bridge's existing validation.

The stick keyboard dialog writes into the existing `keyboard.sticks`
records and validates the proposed profile through the bridge. Direction
keys and both zone radii are edited together. The bridge is responsible
for enforcing radius ordering and the requirement for a movement key
when a zone uses keep sprint. The dialog keeps existing deadzone and
direction hysteresis settings.

The profile editor can choose the virtual gamepad identity. A live
change replaces the virtual gamepad and preserves the physical input
session. Profiles without a type use the startup identity. Analog
partial-only trigger gestures use two hysteresis gates and suppress
partial tap actions if the same pull reaches the full stage by default.
The per-trigger `keyboard.keep_partial` setting keeps the lower gate
active through full travel when enabled.

The Gyro tab sets the aim reference frame per profile. Device-axis
gyro aim is the default. Fusion uses an estimate of gravity to project
the bias-corrected gyro rates before pointer scaling. Live changes reset
the reference frame and timing without reopening the virtual mouse.

The Gyro tab edits the profile’s single activation binding. Its button,
touch, chord and trigger choices use the bridge’s existing profile schema.
The touchpad’s cell editor edits the same binding for single touch inputs.

The gyro editor offers 0–20 sliders for fine adjustment and text fields
for the full 0–100 profile range. Opening a profile or typing a value
above 20 keeps the recorded sensitivity and caps only the slider position.
Linking uses the horizontal value as the starting point and edits both
TOML values together. The link switch is part of the editor session. The gyro acceleration
switch is saved with the profile. Disabling it retains the configured
curve and keeps applying the minimum-speed cutoff. The default cutoff
of 0.2°/s suppresses small residual bias after calibration. An explicit
zero retains unfiltered slow aim. Speed-dependent gain uses the average
speed over the same device-time interval as the integrated aim delta,
so the gain is independent of which end of the interval is faster.

The status reply includes a display filename and, when known, the
active profile's absolute path as encoded bytes. RWC uses the exact path
to preselect a destination even when profiles share a filename. Live
loads carry path metadata and the TOML bytes. The bridge still accepts
legacy name-only and name-free loads. Rejections preserve the current
profile identity, and unloading clears it.

The Buttons editor stores exclusive keyboard or mouse routing in
`keyboard.button_output`. Older profiles retain their original routing
until an output type is selected for a physical button. A claimed source
is filtered before virtual gamepad remapping so another source can still
send the same virtual target. Trigger claims cover both the digital button
and the analog axis. D-pad claims preserve any remaining component of a
diagonal. Chord and layer activation still read physical input.

The editor captures one physical keyboard key in the focused Wayland window.
The GTK Wayland backend adds 8 to the Linux keycode, so the editor removes
that offset and serializes the evdev key name already accepted by the bridge.
Esc, a click in the window or a 10-second timeout dismisses the capture
without changing the binding. **Unbind** clears it explicitly.
[GTK 4.18.6 Wayland key delivery, checked 2026-11-24](https://github.com/GNOME/gtk/blob/4.18.6/gdk/wayland/gdkseat-wayland.c#L1423-L1433)

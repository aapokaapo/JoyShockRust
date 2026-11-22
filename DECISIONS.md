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

Experimental Flick Stick uses physical right-stick direction and device report timing. It starts an initial 100 ms turn at 90% radial travel, follows the shortest angular change while held, and re-arms below 75%. It clears queued motion on timestamp gaps and keeps the virtual right stick centered. The mouse sensitivity is configurable because host and game settings determine the resulting turn. [Flick Stick acceptance](TESTING_AND_PERFORMANCE.md#flick-stick-acceptance)

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
startup. Reload rejects changes to this table since the discovery loop
and virtual-device capabilities are selected at startup.
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

Updated 2026-11-22. Status counts successful synchronized uinput output
batches separately for the gamepad and mouse. Each output reports
whether it emitted a batch. The status counters update only when a batch
is emitted and reset with the HID session.
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
The default accepts both models. Model changes use the same
restart boundary as transport changes.
[Rapid reconnect acceptance](TESTING_AND_PERFORMANCE.md#rapid-reconnect-acceptance)

Updated 2026-11-22. Profile-aware diagnostics enumerate supported HID
controllers, show the same model and transport filter as startup, and
probe access for matching nodes. Excluded inventory entries are identified
from udev metadata. [Device diagnostics](README.md#device-diagnostics)

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

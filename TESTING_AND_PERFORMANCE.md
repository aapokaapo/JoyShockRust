# Testing, instrumentation and performance

## Shared-library refactor verification

2026-11-25. The locked offline workspace suite passed 247 tests and
doctests, with 16 ignored by default. The example suite passed the
legacy vibration wire test. Five ignored editor tests passed separately
on headless Wayland, covering trigger bindings, profile selection,
touchpad regions, gyro activation and sensitivity. The workspace suite
also passed the steady-state zero-allocation replay.

For the release-build smoke comparison, the same temporary harness
decoded the captured DualSense Edge USB reports and mapped them with
a stick curve, a button remap, a digital trigger stage and synthetic
touch contacts. A block contains 64 reports. The harness discarded 100
warm-up blocks and retained 10,000 blocks per run. Twelve retained runs
per revision alternated order on one CPU on an AMD EPYC 9V74 host
with Linux 6.18.57 and Rust 1.100.0. The baseline was `99c95f8`
and the candidate was `2696ba4`. Both produced the same output
checksum in every run.

| Metric | Baseline | Candidate |
|---|---:|---:|
| Median run p50 (ns per report) | 79 | 79 |
| Median run p95 (ns per report) | 80 | 80 |
| Median run p99 (ns per report) | 111 | 109.5 |
| Median run p99.9 (ns per report) | 715.5 | 687 |
| Largest observed block average (ns per report) | 30,733 | 28,939 |
| Median user CPU per run (ms) | 55.54 | 55.52 |
| Median system CPU per run (ms) | 0 | 0 |
| Median peak RSS per run (KiB) | 10,624 | 10,624 |

The run-level p99 ranges overlapped (91–126 ns baseline and 89–136 ns
candidate). Timing is for the average report in each block. CPU and
RSS include the test harness. The run covers in-process input decoding
and gamepad mapping. End-to-end USB and Bluetooth latency, kernel
virtual output, haptics, and status polling under contention still need
measurement on a physical test host.

## Initial targets (provisional, one controller)
| Metric | Target | Definition |
|---|---|---|
| Idle CPU | <0.5% of one core | Daemon idle, controller connected; document sampling window |
| Active CPU | <2% of one core | Representative gameplay, one controller |
| Resident memory | <20 MB | Headless daemon RSS |
| Processing latency | p99 <0.5 ms | From normalized input availability to output submission; excludes physical I/O |
| Hot-path allocations | 0 | Steady-state per-report processing |
| Input cadence | Preserve device reports | No intentional gyro sample dropping |

These are **engineering targets**, not established measurements. Measure USB and Bluetooth separately and distinguish per-report processing, scheduling delay, HID acquisition and kernel output delivery.

## Test pyramid
- Unit: parsers, units, timebase rollover, mapping transitions, fusion math.
- Property/fuzz: malformed HID reports, state machine invariants, bounded timers.
- Replay: timestamped IMU and button traces; same input yields same output.
- Integration: virtual output events, permissions, hotplug, IPC reload.
- Hardware-in-loop: wired DualSense Edge first, then Bluetooth and others.
- Compatibility: Steam/Proton/native game discovery and filtering.

## Current acceptance priorities

Use the latest build from the `test` branch. For the revisions
through `d761a87`, workspace tests and eight headless Wayland editor tests
passed. Those editor tests exercise GTK controls and synthetic events.
Use the sequence below to verify the current physical behavior. Record
the commit, controller model, USB/Bluetooth, Fedora/kernel, and game and
input backend where relevant.

| Order | Check | Pass condition |
|---|---|---|
| 1 | Buttons and keyboard capture | Choose Keyboard, Mouse and Virtual gamepad in turn for Cross. Each route emits only its configured family. Esc, click and timeout preserve an existing binding. Unbind clears it. Check L2 partial/full actions and D-pad diagonals. |
| 2 | Gyro and IMU | With the same saved gyro bias, compare both aim modes with acceleration enabled and disabled. At rest the cursor stays still and deliberate slow rotation moves it. Tilt and rotate to verify the live axes. Disconnecting clears the reading. |
| 3 | Both calibration flows and Flick Stick | Navigate the windows with the controller. Save stationary bias and verify reuse on reconnect. Check the haptic cues and USB detents. Measure several whole turns for RWC, apply the saved profile and verify an approximately 90° flick. Test both stick selections. |
| 4 | Tray and session lifecycle | Check controller and profile selection independently, output switches, profile restore, login startup and Quit. Repeat hotplug with a held button and check that the old virtual device releases and disappears. |
| 5 | Compatibility and performance | Repeat core checks on Bluetooth and the other controller model. Test one named SDL game and one Proton game, and record idle, active and contention benchmarks separately. |

Earlier hardware evidence includes USB uinput press/release and Steam
reconnect, standard DualSense and Edge support, USB/Bluetooth rumble
and reconnect, Steam controller visibility with the SDL filter, and one
successful four-turn RWC measure/save. These results precede the current
editor changes. The detailed procedures below retain their recorded
scope. [Desktop test procedure](#desktop-tray-acceptance) · [Benchmark procedure](#live-bridge-benchmark)

## Virtual gamepad acceptance

The kernel round-trip test feeds a synthetic Cross press into the USB parser,
submits it through uinput and checks the virtual evdev node for exactly one
press and release, each followed by `SYN_REPORT`. Run on Linux with writable
`/dev/uinput` and permission to read the virtual evdev node.

```sh
cargo test --locked -p joyshockrust-linux uinput_cross_press_and_release -- --ignored
```

If the test reports `Permission denied` on the virtual `/dev/input/eventN`
node, run the test executable via `sudo` on an x86-64 Fedora development
machine. Cargo still runs and builds as your normal user.

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER=sudo \
  cargo test --locked -p joyshockrust-linux uinput_cross_press_and_release -- --ignored
```

To test live gamepad toggling without a physical controller, run the session
test with the same uinput and evdev permissions. It feeds synthetic USB
reports into the live bridge, toggles the gamepad through the command
socket, and checks the Linux device and button state before and after.

```sh
cargo test --locked -p joyshockrust-linux uinput_live_gamepad_toggle -- --ignored
```

Steam acceptance uses a wired DualSense Edge and the current build.

1. Unplug the controller and start `cargo run --locked -p joyshockrust-linux`.
   Confirm the process waits for a controller.
2. Plug in the controller and select **JoyShockRust Virtual Gamepad** in
   Steam's controller settings. Open **Test Device Inputs**.
3. Press and release Cross, then exercise a D-pad direction, a stick and a
   trigger. Confirm the selected virtual device registers the matching
   controls and each returns to neutral on release.
4. Hold Cross and unplug the controller. Confirm the virtual device is removed
   and the process waits again. Reconnect without restarting it. Confirm one
   virtual device returns in Steam, begins neutral and registers another
   Cross press and release.

Record the commit, Linux kernel, Steam client version and result for each
step when running this acceptance test.

On 2026-11-22, the user reported that step 4 passed on Fedora with a wired
DualSense Edge. The separate uinput round-trip test also passed on Fedora
using the sudo test runner (1 passed, 0 failed). The unprivileged run ended
with a permission error while opening the virtual evdev node.

## USB gyro mouse acceptance

On Linux, use a readable wired DualSense Edge hidraw node and writable
`/dev/uinput`. Close other gyro-to-mouse mappers and start the bridge:

```sh
cargo run --locked -p joyshockrust-linux -- --gyro-mouse
```

1. Connect the controller and check for `JoyShockRust Gyro Mouse ready`.
   A factory calibration error indicates a failed feature read or an invalid
   reply. Note the exact message and hidraw path.
2. Set the controller down for 10 seconds and observe the pointer. Record
   any drift. Rotate it slowly left/right and then up/down. Record the
   pointer direction, continuity, and whether the other axis moved.
3. Unplug and reconnect without restarting the bridge. Check that the
   gyro mouse starts again and moves the pointer on rotation without an
   initial jump. Check that the virtual gamepad still receives buttons.

Record the commit, Linux kernel, controller firmware if known, and results.
The test observes physical response. The synthetic suite separately covers
calibration conversion, fractional mouse movement and timestamp gaps.

On Fedora with commit `647dcf6`, the user reported that the pointer stayed
still at rest, that both pointer axes were inverted, and that USB reconnect
restored pointer control without a jump. The bridge also printed duplicate
waiting messages. The axis sign and wait logging fixes need a hardware retest.

On Fedora with commit `6be09fa`, the USB reconnect log showed one waiting
message and the gyro mouse started again. Pointer direction was not recorded
for that run.

To test residual bias calibration, add `--calibrate-gyro` to the command.
Keep the controller moving briefly, then set it down. Confirm the pointer
stays still during calibration and the completion message appears after
at least a second of stable input. After completion, check for drift and
confirm that slow deliberate rotation still moves the pointer. Repeat after
USB reconnect to verify that a fresh calibration is requested.

For the Phase 4 hold gate, run with `--gyro-mouse --gyro-hold-l2`.
Rotate with L2 released and check that the pointer stays still. Hold L2
while rotating and check that motion starts without a jump. Release L2
while rotating and check that the pointer stops. Press L2 again to check
that inactive motion did not carry over. Repeat after reconnect.

Run again with `--gyro-mouse --gyro-toggle r3`. Confirm aim starts off,
turns on with one R3 press, stays on through release, and turns off with
the next press. Reconnect and confirm aim starts off again.

The user confirmed that digital hold and toggle worked as expected on the
Phase 4 test branch. The user also reported that the analog trigger gate
worked as expected at the next milestone.

Start with `--gyro-mouse --gyro-trigger l2:0.60:0.40`. While rotating slowly,
squeeze L2 through 60% travel to enable aim. Ease back to around 50% and
confirm aim stays on. Release below 40% to turn it off. At around 50% again,
aim should stay off until L2 crosses 60%. Repeat with `r2:0.60:0.40` to check
the other trigger.

For tap/hold, run with `--gyro-mouse --gyro-tap-hold r3`. Aim starts off.
Tap R3 briefly to turn it on, then tap again to turn it off. Hold R3
while rotating the controller. Aim should begin after 200 ms and stop on
release. Repeat a long hold with aim already latched on. Aim should stay on
after that release. Reconnect and confirm a fresh session starts off.

For double-tap, run with `--gyro-mouse --gyro-double-tap r3`. Confirm one
tap leaves aim unchanged. Two short taps within 300 ms should toggle once.
Wait more than 300 ms between taps and press for 200 ms or longer in separate
attempts. Both should leave aim unchanged. Reconnect with R3 held, release
it, and confirm that aim is still off.

The user reported that both tap/hold and double-tap worked as expected on the Phase 4 USB test branch.

For the chord gate, run with `--gyro-mouse --gyro-chord l1,r1`. While
rotating, test L1 and R1 separately, then hold both in either order. Aim
should move only while both are down. Release either button while rotating
to check that motion stops, then press it again and check for a jump.

The user reported that the two-button gyro chord worked as expected.

## Stick response curve acceptance

Start with `cargo run --locked -p joyshockrust-linux -- --stick-curve 2`.
Open **Test Device Inputs** for **JoyShockRust Virtual Gamepad** in Steam.
Move each stick about halfway along one axis. The displayed virtual movement
should be about one quarter, before any Steam filtering. Try a diagonal and
verify its direction stays the same. Full travel and return to center should
match the linear mode. Compare with a separate run using `--stick-curve 1`.

The user reported that the stick response curve worked as expected.

## Button remap acceptance

Run `cargo run --locked -p joyshockrust-linux -- --remap-button cross=circle --remap-button circle=cross`.
In Steam's **Test Device Inputs**, press and release each button. Confirm
that physical Cross appears as virtual Circle and physical Circle as virtual
Cross. Hold both, then release one at a time. Verify each virtual button
releases independently. To test a shared target, rerun with only
`--remap-button cross=circle` and verify the virtual Circle stays held
until both physical buttons have been released.

The user reported that virtual button remapping worked as expected.

## Remap layer acceptance

Run:

```sh
cargo run --locked -p joyshockrust-linux -- \
  --remap-button cross=triangle \
  --layer-remap l1:cross=circle --layer-remap l1:l3=r3 \
  --layer-remap r1:cross=square
```

In Steam's **Test Device Inputs**, hold physical Cross throughout. It should
start as virtual Triangle. Hold L1 to switch to Circle, then press R1 while
still holding L1. Cross should remain Circle, L3 should still map to R3,
and R1 should show its ordinary virtual state. Release L1 while holding R1
and check that Cross returns to Triangle. Release and press R1 again to
switch Cross to Square. Finally release all buttons and confirm they are up.

The user reported that the Square/Triangle correction and layer behavior
worked as expected after the generic gamepad face-button fix.

## Flick Stick acceptance

Run with `--gyro-mouse --gyro-hold l2 --flick-stick 4` and keep L2 released
to isolate the stick. Start with the right stick centered. In a game that
accepts relative mouse motion, flick right and left separately. Confirm that
the camera turns in the expected direction and the turn finishes after you
recenter. Hold the stick near the outside and rotate it across the rear
axis. Confirm the camera follows smoothly without a full-turn jump. Set
the counts-per-degree value for the game's sensitivity before measuring
the angles. Confirm the left stick still drives the virtual gamepad.
Set the axial threshold to 12° and flick just left and right of forward.
Verify both initial flicks snap to forward. Without centering, turn the
stick through a shaded sector and check that each movement remains
proportional, even within the sector. Pull the stick inward to about half
travel and rotate it, then return it to the outside. Check that movement
continues as steering until the stick returns near center. Compare
steering at 0, 0.5 and 1 with the same initial flick. Measure RWC with the
same profile. Verify small corrections near forward still use the full
measurement sensitivity.
Set the threshold to 0° and repeat to compare the response. Open
**Sticks → Flick Stick** with the bridge running. Move the active right
stick near a sector edge and change the threshold in the editor.
Check that both previews follow the stick, that the adjusted marker
snaps on initial flick within a shaded sector, and that both the sector
and held steering response update before saving.
Switch controllers or reconnect and check that the preview follows the
active controller. Set **Flick Stick input** to **Left stick**, lower
**Engagement** and increase **Flick duration**. Check that the preview
shows the new ring, the left stick starts a turn earlier and completes
the turn over the longer interval, and the right virtual stick still
responds. Compare both solid response curves against their dashed
linear references as the threshold and steering multiplier change.

The user reported that Flick Stick worked as expected.

## TOML profile acceptance

Validate `profiles/dualsense-edge.example.toml` with `--check-profile`,
then start the bridge with `--profile` on a wired DualSense Edge.
The example sets Cross to Triangle, L1+Cross to Circle and R1+Cross to
Square. Its gyro mouse should move only while L2 is held. After stopping
the bridge, copy the example and change its version to 2. Check that
`--check-profile` rejects the copy.

The user reported that the gyro-except-L2 profile worked as expected.

## Device diagnostics acceptance

Run `cargo run --locked -p joyshockrust-linux -- --diagnose` with the wired
Edge connected. Expect its hidraw path to be listed as readable and
`/dev/uinput: read-write open OK` for a user who can run the bridge. Run
again after unplugging and check that the node disappears from the list.
The command checks opening and hidraw identity. Virtual output events are
covered separately by the uinput acceptance test.

On 2026-11-22, the user tested commit `03a7f40` on Fedora. With USB
connected, diagnostics reported a readable wired DualSense Edge at
`/dev/hidraw6`. After unplugging, diagnostics reported no wired Edge nodes.
`/dev/uinput` opened in read-write mode in both runs.

Check the optional access rules statically with
`udevadm verify dist/udev/*.rules` on a host with that subcommand. On a
local desktop host without equivalent rules, run diagnostics over USB
and Bluetooth for each supported controller. If a device reports
`Permission denied`, install the corresponding fallback and rerun
diagnostics as the same desktop user. Verify readable hidraw and
read-write uinput access separately. Check the virtual output with the
kernel uinput acceptance test. The ACL checks require physical device
nodes and an active seat. [Optional access setup](README.md#optional-device-access-rules)

## User service acceptance

Install and start the unit using the [service instructions](README.md#systemd-user-service).
With USB disconnected, check that the service is active and its journal
reports that it is waiting. Connect the Edge, check the journal for the
connection, and verify the profile drives the virtual gamepad in Steam.
Unplug and reconnect while the service is active and check that the virtual
gamepad returns. Finally stop the service and check that its virtual
devices are gone. The user's Fedora run of `ac0df46` verified startup
while disconnected, device detection, and gyro mouse recreation at reconnect.

## Xbox output acceptance

Stop the user service if it is active, then run
`cargo run --locked -p joyshockrust-linux -- --gamepad xbox-series` with a
DualSense Edge connected by USB. Open Steam's Test Device Inputs for the
virtual controller. Check Cross/A, Circle/B, Square/X and Triangle/Y, the
D-pad, both sticks and both analog triggers. In consumers that expose Xbox
Share, check that clicking the DualSense Edge touchpad presses Share.
Unplug and reconnect to
verify the device returns with the same identity. In a Proton game, record
the game, Proton version, Steam Input setting and whether a single press
performs one action. The physical controller is still visible to games.

For a kernel-level assertion of the virtual device ID, trigger range and
Cross events, run
`cargo test --locked -p joyshockrust-linux uinput_xbox_series_identity_and_cross_press -- --ignored`
with the evdev permissions described in [Virtual gamepad acceptance](#virtual-gamepad-acceptance).
For Xbox 360 compatibility, repeat with `--gamepad xbox360` and the
`uinput_xbox360_identity_and_cross_press` test.

The user reported that the Xbox Series output worked as expected on Fedora
with `0ff794e`.

## Bluetooth DualSense Edge acceptance

Turn off the existing user service before running the bridge manually.
Disconnect USB, pair the Edge over Bluetooth and run
`cargo run --locked -p joyshockrust-linux -- --diagnose --bluetooth`. Verify
that diagnostics lists a readable Bluetooth hidraw node. Start the bridge
with `--bluetooth --gamepad xbox-series` and check the face buttons, sticks
and gradual trigger travel in Steam. Disconnect Bluetooth, reconnect the
controller and check that the virtual device returns. Repeat with
`--gyro-mouse` to check stationary pointer behavior and directional motion
after factory calibration. The automated tests use synthetic frames and
independent CRC values for both Bluetooth input and calibration. The user
reported that Bluetooth input, gyro and reconnect worked with `c7c79b9`.

## Automatic transport acceptance

Start the bridge with `--auto-transport --gamepad xbox-series` and connect
over Bluetooth with USB unplugged. Check the connection log for Bluetooth
and test inputs in Steam. Disconnect Bluetooth, then connect USB while the
bridge is still running. Check for a wired connection and test the virtual
controller again. Unplug USB and reconnect Bluetooth to exercise the reverse
transition. If both connections are visible at startup, the bridge tries USB
first. When there is already a session, it keeps using it until disconnect.
With a status socket configured, check that `--status` reports the active
transport and reports `none` while waiting. The user reported that automatic
transport selection worked with `704d33a`.

## Standard DualSense acceptance

Connect a standard DualSense with product ID `0ce6` by USB and run
`cargo run --locked -p joyshockrust-linux -- --diagnose --auto-transport`.
Check that it is listed as a readable wired DualSense. Run the bridge
with `--auto-transport --gamepad xbox-series --gyro-mouse` and check face
buttons, sticks, triggers and gyro on the virtual devices. Confirm
Square maps to X and Triangle to Y. Disconnect USB, connect the controller
over Bluetooth, and repeat. The bridge should log the model and transport
for each session. With an Edge, repeat the same steps and check a remap
using an Fn button or paddle as the source.

The user verified the standard DualSense and DualSense Edge with `3a0bc7a`.

## Rumble forwarding acceptance

Stop the user service if it is active. Run the bridge manually with
`--auto-transport --gamepad xbox-series --rumble` and check for
`Rumble forwarding ready`. In another terminal run
`cargo run --locked -p joyshockrust-linux --example rumble_test`. Expect
one 750 ms vibration pulse on the physical controller. The example opens
the JoyShockRust virtual event node and uploads, plays and stops the effect.
Append `-- /dev/input/eventN` when several virtual gamepads are present.
If the controller stays still, start the bridge with
`JOYSHOCKRUST_RUMBLE_TRACE=1` and repeat. Save the upload and playback
lines. Then run
`cargo run --locked -p joyshockrust-linux --example rumble_test -- --physical /dev/input/eventN`
with the physical node from `Rumble forwarding ready` and compare whether
the same pulse is felt. A successful write confirms kernel acceptance.
The user's wired Edge test on `993abc2` accepted uploads and playback
writes through both paths without physical vibration. To check whether
device gain suppresses the pulse, close other controller applications and
repeat the physical test with `--reset-gain`. This restores the device-wide
gain to the Linux default of 100% before the pulse. Compare the direct
result over Bluetooth if USB still produces no vibration. The user later
confirmed that the standard DualSense vibrated through both the virtual and
direct tests. The Edge's vibration had been disabled in the controller
settings.

To compare the Edge's legacy USB mode, stop the bridge and any other
controller clients, leave the Edge connected over USB, and use the hidraw
node shown by `--diagnose`. The test opens that node for writing and sends
one pulse and a stop report.

```sh
cargo run --locked -p joyshockrust-linux --example rumble_test -- --edge-usb-legacy /dev/hidrawN
```

Check that vibration stops when the effect ends, the client exits or the
controller disconnects. For in-game compatibility, record the game, Proton
version and Steam Input setting.
Disconnect and reconnect the controller with the same bridge process
running. Wait for a new `Rumble forwarding ready` line and repeat the
pulse test. The bridge allows up to one second for the rumble evdev node
and its permissions to become available. Repeat over the other transport.
If permissions prevent opening the physical node, check that the bridge names the force feedback
node in its error and that normal input still works. Install the optional
[rumble udev rule](README.md#virtual-gamepad-on-linux) in an active local
session, reconnect and repeat the test. The ignored `uinput_rumble_gamepad_advertises_ff_rumble`
test checks the virtual node's capabilities with a writable `/dev/uinput`
and readable virtual event node. The user confirmed that rumble forwarding
worked after reconnecting over both USB and Bluetooth with `ecbda68`.

## Bridge startup profile acceptance

Add the [bridge settings](README.md#toml-profiles) to the active service
profile and restart the user service. Check that the Xbox Series gamepad
appears in Steam and use `rumble_test` to check a pulse. Swap transports
without restarting the service. The user confirmed that the pulse test
worked over USB and Bluetooth with the bridge profile on `d987b1c`.
Change only `rumble` in `[bridge]` and request
a reload. Expect a restart-required error and the original gamepad to
retain its capabilities. Restore the setting, make a gyro or mapping
change and check that a profile reload applies it.

## Battery status acceptance

Run the updated user service with automatic transport selection. Connect
the controller over USB and run
`$HOME/.local/bin/joyshockrust-linux --status`. Expect a percentage and a
charge state consistent with the controller. Depending on the controller's
charge and power settings, it can report `charging` or `full`. Unplug USB and
disconnect Bluetooth so the bridge waits. Both battery fields should
then be `none`. Reconnect over Bluetooth without a charging cable and
check for `discharging`. Reload a mapping profile and check that the reading
remains available during the same session. The user confirmed the battery
status with `ff56188`.

## Per-game SDL visibility acceptance

The user reported that Steam started with the SDL filter showed only the
virtual Xbox Series controller. Named game and Proton results remain open.

Keep the Xbox Series virtual gamepad running. Choose a native SDL game with
an input screen or a repeatable action. Use a temporary profile remap such
as Cross to Circle so input from the physical controller and the virtual
gamepad can be distinguished. Compare a baseline launch with
`joyshockrust-linux --launch-sdl -- GAME`. In Steam, enter the launcher
path followed by `--launch-sdl -- %command%` in the game's launch options.
Press Cross once. Record whether the game receives only the remapped B
action, along with the game build, SDL version and Steam Input setting.
A Proton comparison should include the Proton version.

## Live gyro sensitivity acceptance

Run the updated service with gyro configured. In a temporary copy of the
active profile, set `sensitivity_x = 12.0` and `sensitivity_y = 2.0` in
the `[gyro]` table. Run `joyshockrust-linux --load-profile PATH` and expect a
success response. Compare equal rotations on each axis. Change only the
vertical sensitivity and load the preset again. Confirm that horizontal
aim retains its sensitivity and that calibration continues. Check that a
negative sensitivity is rejected and that the last valid profile remains
active. Set a minimum speed and 2× acceleration in the editor. Compare
slow turns below the cutoff, turns between the two acceleration
thresholds, and fast turns above the upper threshold. Turn the
acceleration switch off and verify the boost stops while the cutoff
still applies. Turn it back on and check the curve settings are retained.
A replay with uneven report timing checks that equal and opposite small
rotations produce zero net pointer movement with acceleration enabled. Run
`joyshockrust-linux --reload-profile` to restore the startup profile.

A manual bridge needs `--profile PATH --reload-socket SOCKET`. Pass the same
`SOCKET` as the optional final argument to `--load-profile`.

## Game sensitivity calibration acceptance

The user completed four camera turns at a game sensitivity of `1.0` and
confirmed that the tool printed and saved `real_world_calibration = 44.34375`
and `in_game_sens = 1.0` to the profile.

Run the [game calibration tool](README.md#game-sensitivity-calibration) in
a game with a visible landmark. Use the right stick to complete several
full camera turns and enter the whole-turn count. Verify that the tool
prints and saves the calibration values while retaining the other profile
settings and comments. Load the saved profile. With
`sensitivity_x = 1.0`, compare a physical horizontal turn with the camera
turn. If Flick Stick is enabled, set `flick_stick = 1.0` and verify that a
90-degree stick flick turns the camera by roughly 90 degrees. Record the
game and its raw-input and mouse sensitivity settings.

## Profile reload acceptance

Install the updated executable and unit. Reload the systemd user manager
and restart the service once so the new executable is running. With USB
connected, edit the profile to change a gamepad mapping, then run
`systemctl --user reload joyshockrust.service`. Expect the command to
succeed and verify that the mapping changes in Steam on the existing virtual
gamepad. Change the profile version to an unsupported value and reload
again. Expect the command to fail, the journal to report the rejection and
the last working mapping to remain active. Restore the version when done. The same reload
command works when USB is unplugged. The user's Fedora run of `5d13eda`
confirmed successful reloads and rejection of profile version 2 with the
same bridge process running. The user's Fedora run of `4800d3b` also
confirmed the systemd reload command reports success, failure and recovery.

## Profile installation acceptance

The user verified valid and invalid installs on Fedora with `c703382`.

Install a valid preset to a separate file with
`joyshockrust-linux --install-profile SOURCE DESTINATION`. Compare the
files to check that comments and formatting match. Try to install a preset
with version 2 to the same destination. Expect a nonzero exit and confirm
that the destination still contains the first preset. For a live test,
install a chosen preset as the service profile and reload the service.
Check the mapping or gyro binding on the existing virtual devices.

## Service status acceptance

Install the updated executable and unit and restart the service. As soon
as the restart returns, run `$HOME/.local/bin/joyshockrust-linux --status`
with USB disconnected. Repeat after
plugging the Edge in, and after unplugging it. Expect `controller=waiting`,
`controller=connected` and `controller=waiting`, respectively. Check that
`gyro_configured` matches the profile's gyro section. A successful reload
that adds or removes the section changes the field. An invalid reload
preserves it. After the reload command returns, check that `last_reload`
is `applied` or `rejected`, respectively. After stopping the
service, the status command should fail to connect. The user confirmed that
the USB status switched from waiting to connected on Fedora.

To check startup error handling, set the profile version to 2 and restart.
Expect the start command to fail and the service status to show exit code
78. Verify in the journal that the service stays stopped beyond the normal
restart delay. Restore the version to 1, start the service again and check
that `last_reload=not_attempted`. The user's Fedora run of `1e4b78c`
confirmed exit status `78/CONFIG`, no automatic retry, and a successful
manual restart after restoring version 1.

## Touchpad grid acceptance

Add the [two-column example](README.md#touchpad-regions) to a copy of a
working version 1 profile. Load the copy with
`joyshockrust-linux --load-profile PATH` while the updated service is
running. Open Steam's **Test Device Inputs** for the virtual gamepad. Expect
the following gamepad buttons (Xbox A, B, X and Y).

1. Rest a finger on the left half. Cross (A) should be held. Slide into the
   right half without clicking. Cross should release and Circle (B) should
   be held until you lift your finger.
2. Put one finger on each half. Cross and Circle should both be held.
   Lifting either finger should release the corresponding button.
3. Click and hold on the left half. Square (X) should be held alongside
   Cross. Slide to the right half while holding the click. Square should
   remain held while the touch binding changes to Circle. Release the
   click, then click on the right to check Triangle (Y).
4. Repeat on Bluetooth with `transport = "auto"` in `controller.toml`.
   Disconnect while a target is held and reconnect with the touchpad
   clear. The virtual gamepad should return with the targets released.

Use `joyshockrust-linux --reload-profile` to restore the startup profile.
Record the model, transport and result for each step.

## Touchpad gyro acceptance

Load the [right-half gyro profile](README.md#touch-region-as-a-gyro-input)
into the running bridge. Rotate the controller while touching the left
half, then slide your finger to the right half while still rotating.
Gyro aim should start when your finger enters the right half and stop
when you slide back or lift your finger. Repeat with one finger resting
on the left half and a second finger operating the right half. Change
the mode to `disable-while-held` and reload the profile to verify the
reverse behavior. Repeat over Bluetooth with an automatic-transport
bridge.

## Report statistics acceptance

With the updated service running, query `joyshockrust-linux --status`
before connecting a controller. Expect zero decoded and rejected reports
and `report_rate_hz=none`. Connect via USB and run the command twice,
several seconds apart. Expect the decoded count to increase and a positive
rate once there is at least one second of observation. Check that the
decoded count continues increasing after a valid profile reload.
Disconnect and check that the values reset. Repeat over Bluetooth with
`transport = "auto"` in `controller.toml`. Record the model, transport,
kernel and both status snapshots for each connection.

The rate estimates how many HID reports the host decodes per second after
the first successful read batch. The rejected count covers only
reports that reached the parser and failed decoding.

`report_full_batches` counts each pass through the readiness loop that
read 32 reports. Both valid and rejected reports count
toward the budget. An increasing count shows bursts reaching the
userspace batch limit. Measuring the depth of the kernel queue or
physical packet loss requires separate evidence. The count resets after a
disconnect.

Read `uinput_gamepad_batches` before and after pressing and releasing a
button. With the gyro mouse enabled, rotate the controller and check that
`uinput_mouse_batches` increases. A report produces at most one batch per
output, even when several controls or both mouse axes change. Automatic
gyro and Flick Stick output share the mouse counter. Both counters reset
on disconnect and persist across a profile reload. They count normal
report output during the session.

With a keyboard and mouse profile, record `uinput_keyboard_batches` and
`uinput_mouse_button_batches`. Tap a single keyboard binding and wait
for its release. Expect two more keyboard batches. Repeat with a mouse
button binding and check the mouse-button counter independently of the
gyro mouse counter. Reload the profile and confirm the counters retain
their values, then disconnect and check that both reset.

## Rapid reconnect acceptance

Start the bridge in auto-discovery mode and record its process ID.
Disconnect and reconnect the same controller several times in quick
succession, first over USB and then Bluetooth. Check that each established
connection produces one virtual gamepad and that the process ID remains
the same. With `--status`, verify that input counters begin again for
each connection. If a second supported controller is present, leave it
connected and verify the bridge selects it after the active one disconnects.
The udev selection tests cover reuse of an existing hidraw name by a new
HID instance of the same model and bus.

During the run, `controller_sessions_ended` should increase by one for
each established session that ends, and `controller_sessions_started`
by one when the next controller session starts. The counts differ by one
while connected and match while waiting. Reload the profile during a
connection and check that both counts stay the same. A service restart
starts a fresh count.

With both DualSense models connected, set `model` in `controller.toml` to
`"dualsense"`. Run `joyshockrust-linux --diagnose --profile PATH` and
check that the Edge is listed as `excluded by controller filter`. Restart the
service and check that `--status` shows
`model=dualsense`. Disconnect the standard DualSense and check that the
service waits while the Edge remains connected. Reconnect the selected
model, then repeat with `model = "dualsense-edge"`. With
`transport = "auto"`, try each model over both USB and Bluetooth.

For two controllers of the same model, use
`--diagnose --auto-transport --show-controller-id` to obtain their IDs.
Pass `--controller-id ID` to diagnostics and verify it excludes the other
controller. Use `--select-controller ID` with the running service, confirm
that the intended controller produces input, then repeat over Bluetooth. Record which selection matched on each transport. The optional
diagnostic ID is the controller's hardware address. Omit it from shared
test logs.

With gyro enabled, also disconnect during the interval between the
`Connected` log line and virtual device setup. If setup fails after
the detach, expect a disconnect diagnostic and the same process to
return to discovery. After reconnect, verify gamepad and gyro output
resume. The descriptor-based recovery is covered synthetically.

## Live controller selection acceptance

Run `--list-controllers` with both controllers connected. Check their
model, transport and node paths. Add `--show-controller-id` to check the
IDs, and compare `active_hidraw` from `--status` with the list. Verify the
field changes on selection and reads `none` while waiting.

While connected, start with a profile that enables both gyro mouse and
virtual gamepad. Hold a mapped gamepad button and run `--set-gamepad off`.
Check that the gamepad disappears, gyro still moves the pointer, and
`active_hidraw` and the session counts stay unchanged. Release the
physical button, run `--set-gamepad on`, and verify it appears again with
released controls. If rumble is configured, run the pulse test again.
Repeat across a disconnect and check that `virtual_gamepad_enabled`
retains its selected value.

Use the discovery-mode user service and record its PID. Use
`--select-controller ID` to select the second controller. Include
`--controller-model MODEL` and a transport flag if needed. Verify the
old virtual gamepad disappears, the process PID stays the same, and
the new controller uses the same bindings. Start with the second
controller disconnected to verify the service waits for it.

While connected, load a preset that changes a binding and specifies
legacy controller fields. Confirm the new binding applies and the
current virtual gamepad remains connected. Repeat with
`--reload-profile` and while waiting for the selected controller.
Use `--status` to verify that the selection fields and session counters
stay the same through both kinds of profile updates. Use
`--select-controller any` to clear the pin and check that
`controller_id_configured=false`.

On a fresh user configuration without a startup profile, start the user
service. Confirm `--select-controller` works before loading a profile.
Then load a preset and check that the selected controller stays active.
Run `--unload-profile` while a mapped key is held. Verify that the key
releases, gyro output ends, and the same controller remains selected.
If a startup profile is configured, run `--reload-profile` and verify
the profile's mappings work again.
Use `--status` to check that `profile_source` reports `loaded` after a
preset load, `defaults` after unload, and `startup` after a startup-file
reload. Try a rejected load and verify the previous source remains active.

To verify automatic startup selection, select a controller with both
connected and restart the service. Confirm it chooses the same controller.
Stop the service, disconnect that controller, and start again. Confirm
that the other controller is selected and becomes the saved preference.
Stop and disconnect both, then start again. Check `controller=waiting`
and connect a controller to verify activation without a service restart.

## Keyboard and mouse profile acceptance

Run the kernel round-trip test with writable `/dev/uinput` and readable
virtual evdev nodes. It feeds synthetic USB input to the keyboard and mouse
mapper and checks that a tap waits for the double-tap window, that the
mouse click reaches the correct device, and that cleanup clears a held key.

```sh
cargo test --locked -p joyshockrust-linux uinput_keyboard_and_mouse_gestures -- --ignored
cargo test --locked -p joyshockrust-linux uinput_usb_touchpad_pointer_and_gestures -- --ignored
```

The second test replays touch contacts through the USB parser and
checks the virtual mouse event stream for the pointer start threshold,
single- and two-finger clicks, and axis-locked scrolling. It uses
synthetic reports and needs no physical controller.
The regular test suite also runs the same sequence before uinput for
DualSense and DualSense Edge over both USB and Bluetooth. It checks the
mouse events and release deadlines without requiring virtual devices.

Stop the user service while testing a standalone bridge.

```sh
cargo run --locked -p joyshockrust-linux -- --profile profiles/gyro-keyboard.toml
```

1. Open a key-event viewer for **JoyShockRust Virtual Keyboard**. Tap
   Cross once and expect Space after the double-tap window. Double-tap
   Cross and expect R. Hold Cross and expect Left Shift to stay down
   until release.
2. Move the left stick slightly from center and verify that the deadzone
   keeps the keys released. Tilt it partway up and check that W is held.
   Tilt it fully up and check that Left Shift joins W. Back off slowly
   past the release threshold and check that W stays held after Shift
   releases. Tilt diagonally up-left and verify W and A. Push further
   in that direction to add Shift, then return to center and check that
   all three keys release.
3. Verify the pointer responds to gyro and Flick Stick. Check the game
   has one input mode and that JoyShockRust created a keyboard and mouse.
4. Disconnect while a key is held. Verify that the key clears and the
   bridge can reconnect. Repeat over Bluetooth.
5. For a game with toggle sprint, test with a copy of the profile. Add
   `mode = "toggle"` to `outer` and change the Cross `hold` key to
   `KEY_LEFTCTRL`. With sprint initially off, hold the stick diagonally
   up-left and move into the outer zone. Verify that W and A stay down
   and Shift presses and releases once. Stay in the outer zone, then
   back off past the release threshold while keeping W and A down.
   Verify one more Shift press and release and confirm that the game
   switches back to walking. Repeat with a return to center.
6. In a game that ends sprint when movement stops, repeat with
   `mode = "keep_sprint"`. A move into the outer zone should send one
   Shift press. Back off, change direction and push fully again while
   moving. Verify continued sprint with no further Shift press. Center
   the stick and verify that the movement keys release and sprint ends.
   The next movement into the outer zone should send a new Shift press.
7. Add a keyboard binding with `trigger = "l2:0.35:0.25"` and
   `hold = "KEY_Q"`. Slowly squeeze L2 until Q activates, back off
   slightly without releasing Q, and then release L2 fully to clear Q.
   Confirm R2 alone leaves Q released. Repeat over Bluetooth.
8. Add `trigger = "r2:0.6:0.4"` and `hold = "BTN_LEFT"` in a new
   keyboard binding. Confirm **JoyShockRust Button Mouse** sends a left
   press at the threshold and a release as R2 backs off. With gyro
   enabled, move the pointer while holding R2 and check that a desktop
   drag works. Switch profiles or disconnect while R2 is held and
   confirm the mouse button releases.
9. Add the [keyboard and mouse touchpad example](README.md#touchpad-regions-as-keyboard-and-mouse-inputs).
   Touch the left half and confirm Tab follows the touch. Lift the finger,
   press down on the right half and verify the left mouse button stays
   held while the pressed finger slides across the grid.
   Release the pad and confirm the mouse button releases. Repeat over
   Bluetooth.
10. Set `chord = ["l1", "trigger:r2:0.6:0.4"]` and `hold = "BTN_RIGHT"`
    in a keyboard binding. Press L1 and R2 in both orders. Confirm the
    right mouse button is held only when both are active, including when
    R2 starts above 0.6 and backs off slightly before L1 is pressed.
11. Add a binding with `button = "create"` and
    `tap = ["KEY_LEFTCTRL", "KEY_TAB"]`. Tap Create with a browser
    focused and check that it switches tabs once. In a key-event viewer,
    check that Ctrl presses before Tab and releases after Tab.
12. Set `hold_ms = 500` and `double_tap_ms = 400` on the Cross binding.
    Verify that holding Cross starts Shift after the longer threshold.
    Verify that two short taps within the window send R and that a
    single short tap sends Space when the window expires.
13. Set `tap = ["KEY_LEFTSHIFT", "BTN_LEFT"]` on a spare button. Check
    that tapping it performs Shift+click in an application that supports
    it. In a key-event viewer, check the Shift press before the mouse
    press and the mouse release before the Shift release.
14. Set `deadzone = 0.3` and `deadzone_release = 0.2` on the left stick.
    Start from center and tilt the stick just under 30% to check that no
    direction key activates. Push beyond 30%, then back off slowly. The
    direction keys should remain held until the radius reaches 20%.
15. Set `direction_hysteresis_deg = 5.0` on the left stick. Tilt straight
    up, then move gradually toward the upper-left diagonal. Observe W
    alone until A engages. Back off toward straight up and verify that
    A remains held through the same angle before releasing closer to up.
16. Bind D-pad Up and Down to opposite wheel directions. In a scrollable
    window, tap each direction and verify one wheel step per tap. Bind a
    horizontal scroll action to a double tap and verify it emits only
    after the second short release.
17. Set `hold = { scroll = "right" }` and `scroll_repeat_ms = 80` on a
    spare input. Hold it still and verify wheel steps continue until
    release. Add a tap action and verify short taps send that action
    while repeat scrolling starts after the hold threshold.
18. Set `vertical_step = 120` and `horizontal_step = 180` under
    `[keyboard.touch_scroll]`. Swipe slowly in each direction and verify
    wheel events arrive after the configured travel. Lift and reposition
    the finger and verify scrolling resumes only after a new swipe.
19. Set `fingers = 2` in the same table. Swipe with one finger and verify
    that it produces no wheel steps. Add a second finger, move both in the
    same direction and check scrolling. Lift one finger and reposition it
    before swiping again. The new pair should start without a jump.
20. Add `[keyboard.touch_pointer]` while two-finger scrolling is enabled.
    In the **JoyShockRust Button Mouse** event stream, verify that one
    finger emits pointer movement and two fingers emit wheel steps. Lift
    one finger, then continue moving the other. Pointer motion should
    resume from the remaining finger's new baseline without a jump.
21. Run the bridge with the touch pointer profile and `--grab-touchpad`.
    Verify that the native touchpad event stream is quiet while the
    JoyShockRust pointer still moves. Check the physical gamepad buttons
    still work. With a status socket, check `native_touchpad_grabbed=true`.
    Stop the bridge, confirm native touchpad input resumes, and repeat
    on Bluetooth.
22. Start the service without the CLI grab flag. Load a profile with
    `[keyboard].grab_touchpad = true` and confirm both touchpad grab fields
    in `--status` become true. Unload the profile and confirm both clear,
    native touchpad events resume, and the same controller stays active.
23. Set `tap_to_click = true` under `[keyboard.touch_pointer]`. Verify a
    brief touch produces one left click when the finger lifts. Swipe,
    hold the finger still, add a second finger, and press the physical
    pad in separate attempts. None should produce the virtual tap click.
24. Also set `two_finger_tap_to_click = true` under
    `[keyboard.touch_pointer]`. With two-finger scrolling enabled, tap
    with two fingers and lift them in quick succession. Confirm one
    right click after the final lift. Swipe with two fingers and confirm
    scrolling produces no click. Turn off `tap_to_click` and confirm
    the right click still works.
25. In `[keyboard.touch_scroll]`, set `invert_vertical = true` and
    repeat the same vertical and horizontal swipes. Verify only vertical
    wheel events change sign and button-bound wheel actions keep their
    direction. Set `invert_vertical = false` and `invert_horizontal = true`
    to check the other axis.
26. Set `axis_lock = true` in `[keyboard.touch_scroll]` with both axes
    configured. Start a vertical swipe and turn it horizontally without
    lifting. Check that only vertical steps appear. End the swipe and
    start horizontally. Check that horizontal steps now appear.
27. In `[keyboard.touch_pointer]`, enable both tap actions and set
    `tap_window_ms = 500` and `tap_max_travel = 10`. Try brief stationary
    taps with one and two fingers, then try holding for over half a second
    or moving either finger before lifting. Confirm only the taps within
    the configured time and travel limits produce clicks.
28. Set `motion_threshold = 10` in `[keyboard.touch_pointer]`. Check that
    small initial motion during a tap does not move the pointer. Make a
    deliberate single-finger swipe and verify the pointer moves after the
    threshold without jumping. Lift the finger and check that the next
    contact starts with the threshold again.
29. Enable both tap gestures in `[keyboard.touch_pointer]` and set
    `tap_button = "BTN_MIDDLE"` and `two_finger_tap_button = "BTN_SIDE"`.
    Check that each tap produces one press and release of the selected
    mouse button in the virtual mouse event stream.
30. Set `invert_x = true` in `[keyboard.touch_pointer]` and repeat the
    same diagonal swipe. Check that only horizontal pointer motion
    changes direction. Then set `invert_x = false` and `invert_y = true`
    and check the vertical axis. Two-finger scrolling should keep its
    configured direction.
31. Load `profiles/touchpad-desktop.toml` in the running service and
    verify `native_touchpad_grab_requested=true` with `--status`. Press
    the physical pad over a window title bar and move one finger.
    Confirm the window moves while the pad is pressed and releases
    with the physical click. Unload the profile and verify that the
    profile's touchpad grab request clears.
32. Load `profiles/keyboard-walk.toml` and tilt up-left slightly. Verify Shift, W and A
    are held. Tilt farther to release Shift while W and A stay held.
    Back off until Shift engages again, then center the stick and check
    that all keys release. For a toggle-walk game, repeat with
    `mode = "toggle"` and check for one Shift pulse per zone transition.

The kernel test for this preset needs writable `/dev/uinput` and a
readable virtual evdev node. It drives the stick with synthetic USB
reports.

```sh
cargo test --locked -p joyshockrust-linux uinput_keyboard_walk_preset -- --ignored
```

The automated Flick Stick test verifies two calibrated 90° turns with
different game sensitivity values. For a physical check, calibrate a
copy of the profile for your game and compare a 90° right flick against
an in-game landmark.

## Desktop tray acceptance

Open both calibration windows using the tray. Use the physical
controller to start, save and close the gyro workflow. In game
calibration, verify D-pad left/right changes sensitivity by 0.1,
L1/R1 changes it by 0.01, and D-pad up/down moves focus. While the game
has focus, verify Cross and Circle leave the calibration window alone
and Options still controls measurement. Refocus calibration, change the
whole-turn count with the D-pad and check the RWC preview changes while
the in-game sensitivity stays constant. Choose a profile and save with
Cross.
Check Circle cancels and the tray resumes the bridge.

Install the tray and bridge using the [combined desktop setup](README.md#install-the-service-tray-and-editor-together).
Run JoyShockRust from the applications menu in a Wayland session with a
StatusNotifierItem host. Verify the menu with no controller connected,
then connect a supported controller over USB and Bluetooth. Check the
connection icon and decoded report rate match `joyshockrust-linux
--status` and that the battery indicator only appears over Bluetooth.
Under **Active controller**, open the submenu and check its title updates
after selection. Under **Active profile**, load another profile and check
that its name updates while the selected controller stays active.
Restart both services with `systemctl --user restart joyshockrust.service
joyshockrust-tray.service` and verify that both are active and the same
profile comes back. Restart the tray alone and verify the bridge
keeps its PID. Repeat the combined restart
with **Default mappings** selected. Toggle **Gamepad emulation** and
**Gyro mouse** separately, then turn off **Gamepad and gyro mouse**.
Check both outputs stop and the two selections are grayed out. Turn on
the master and verify each previous selection takes effect. Load
another profile with the master off and verify the outputs stay off.

Click **Profile editor** and check it opens the active profile. Choose a
different file in the editor's dropdown and check that the tray still
shows the previous active profile. Edit a setting, choose **New profile**,
then use **Keep Editing** and confirm the draft remains. Choose **New
profile** again, discard the draft, and name the new profile. Switch the
desktop between light and dark appearance and resize the editor to verify
the page switcher moves between the header and bottom bar. Use Tab and
the keyboard to select controls and change bindings. Edit a setting and
close the window to check the discard confirmation. Select Cross
on the controller diagram and choose **Keyboard**. Click **Bind key…**
on Tap and press Space, then bind Left Shift on Hold. Open Tap again
and look for **Hit ESC or Click anywhere to cancel** in place of the
bind button. Click elsewhere in the editor and check that Space is
still assigned. Repeat and cancel with Esc. Start again and wait 10
seconds. The original binding should still be assigned when the prompt
disappears. Select another control and return to Cross
to verify both choices remain visible. Use **Unbind** to clear
Hold and confirm Tap still sends Space. Save and confirm Cross no longer
registers on the virtual gamepad. Change Cross to **Mouse** and bind
**Left mouse** to tap. Check that only a click occurs. Switch to
**Virtual gamepad**, choose a target, save and verify that only the
configured virtual button registers. Reopen the profile to verify
the choice. Resize the window and check every diagram button has its
own clickable area. In
the **Advanced** page, enter invalid TOML syntax and check that validation or
saving displays an error while the saved profile remains usable.

Choose **Xbox Series X|S** under **Virtual gamepad type**, save and apply,
then check `virtual_gamepad_kind=xbox-series` and the replacement device
in Steam. In the L2 inspector choose **Keyboard**, set the **Partial pull**
action to Q and the **Full pull** action to E. Check that Q is held partway
and E takes over at full travel. Change the two pull and release points
under **Trigger travel** and verify the transitions move with them.
Enable **Keep partial action
at full pull** for L2 and save. Verify Q stays held when E engages, then
ease off until E releases at 85% and Q releases at 35%. Check that R2
still switches stages with its default setting.

On the Buttons tab, select L2 and choose **Same as controller** for
Full Pull. Verify that only the travel limit is editable and that a
limit of 60 produces 100% virtual L2 at 60% physical travel. Confirm
the gray marker shows 30% and the blue marker shows 50% at 30% travel.
Hold at or above the limit and check that the haptic tick plays once.
Release below 59% and pull back to the limit to check that it plays again.
Select a
non-trigger Full Pull target, then assign Partial Pull. Verify that a
Full Pull setting of 80 activates at 48% physical travel. Check the
same relative threshold with a keyboard key and a mouse button. Toggle
**Keep partial action at full pull** and confirm the Partial band stays
colored through the travel limit only when the switch is on. With L2
selected in the focused editor, slowly pull and release L2.
Check that the marker follows the pull and that a short haptic tick
plays at each solid line on the way down and at each dashed line on the
way back. Hold the trigger between a stage's two lines and check that
small movements do not repeat the tick. With the trigger held, edit a
valid threshold and check that the chart updates immediately. Select
R2 and repeat. Switch away from the editor and confirm the preview
ticks stop. Repeat with USB and Bluetooth.

On USB and Bluetooth, select **Resistance** under **Adaptive trigger**
for one side. Change the start zone and strength. Check each change
before saving and verify the other side stays as it was. Select **Off**
and confirm that normal travel returns immediately. Close the editor
and discard the draft to verify the active profile's effects are
restored. Open the editor again, use **Save & Apply**, and confirm the
saved effect is applied on reconnect. Repeat on DualSense and DualSense
Edge.

The user confirmed Edge resistance after enabling **Trigger Effect
Intensity** in the active onboard profile. The earlier USB test
reported successful HID writes on both sides with this setting **Off**
and produced no physical resistance. For a repeat test, set both
travel sliders to the longest position, stop the bridge and run the
[standalone adaptive trigger test](README.md#desktop-tray) once per
side. Record the transport, whether the report write succeeded and
whether you felt resistance. Repeat on the other transport if available.

In **Gyro → Aim input**, switch between **Gyro aim** and **Sensor fusion
aim** using **Save & Apply**. Hold the controller approximately flat and
rotate about vertical to compare horizontal aim. Move the controller in
a straight line without rotating it and check the cursor stays still.
Switch modes again while the controller rests and check for a jump.
In **Live calibrated IMU**, check that angular velocity approaches zero
at rest, the acceleration magnitude is about 1 g, and the three axes
respond to rotation and tilt. Disconnect and confirm the values clear.
In **Gyro response**, change the minimum speed and acceleration
multiplier and compare the configured curve with the dashed linear
reference.

In **Gyro → Gyro activation**, select **Disable while held** and L2,
apply the profile, then check that holding and releasing L2 stops and
resumes aim. Configure a touch region, a chord and an analog trigger in
turn. Reopen the profile to check that the selected mode and inputs persist.

In **Sticks**, move each response slider one step and verify its own
preview updates. Save and check the two per-stick values in
**Advanced**. In **Gyro**, move X and Y separately and check each slider
stops at 20. Check that both tracks have the same length and alignment.
Enable **Link horizontal and vertical** and verify that the shared
slider and entry update both axes. Disable the link and verify that
changes to one axis leave the other at its last shared value.
Type a value above 20 with a decimal comma and verify it
survives saving and reopening. Confirm that invalid text prevents saving.
Enter RWC and in-game sensitivity and verify they also round-trip.

For the left stick, open **Keyboard mapping**. Bind Up, Left, Down and
Right to W, A, S and D. Set inner **Key** to **Left Shift** in **Hold**
mode at 0.5 and outer **Key** to **R** in **Keep sprint** mode at 0.8.
Leave the release fields empty. Use **Update Mapping** and **Save &
Apply**. Verify diagonal motion sends two keys, Shift is held only near
the center while moving, and R pulses once per continuous movement.
Try an inner radius below the deadzone and confirm **Update Mapping**
shows a validation error while the dialog stays open.

Run the GTK dialog test with a built bridge from a Wayland session.

```sh
cargo build --locked -p joyshockrust-linux
JOYSHOCKRUST_TEST_BRIDGE="$PWD/target/debug/joyshockrust-linux" \
  cargo test --locked -p joyshockrust-editor stick_keyboard_dialog_validates_and_updates_the_profile -- --ignored
```

In **Touchpad**, set a grid with one row and two columns. Select the
right cell and assign **Touch** to Cross and **Click** to Square. Try
reducing the grid to one column and confirm the editor rejects the change.
Save and apply the two-column grid, reopen it, and check the two actions
on the virtual gamepad while touching and clicking the right half.
Under **Keyboard and mouse · Touch**, set **Tap** to **Space**. Under
**Keyboard and mouse · Click**, set **Hold** to **Left mouse**. Under
**Gyro → Gyro activation**, choose **Enable while held** and select
**Touch region 1:2** as the input. Save and apply.
Tap the right half and check for a Space press. Hold a click there and
check for a held mouse button. Move the controller while touching each
half and verify gyro aim only activates on the right. Reopen the
profile and verify all three settings. The widget test runs without a
controller in a Wayland session.

```sh
cargo test --locked -p joyshockrust-editor touchpad_widgets_edit_the_selected_cell -- --ignored
```

Choose **Calibrate game sensitivity (RWC)** with a named profile active. While the
Wayland window is open, check `outputs_paused=true` and verify the
regular virtual outputs are removed. Start measurement and confirm the
window shows **Ready**. Press Options with the stick off-center to see
the centering prompt. Center the stick and press Options to see
**Measuring**. With the stick still centered, press Options again and
check for the no-movement prompt. Perform one or more complete turns and
press Options with the stick centered to finish the sweep.
Enter the total number of turns in the window. Verify the active file
is preselected. Select a different destination and verify that **Save
Calibration** displays the result and updates the selected profile. Select
**Done** and verify `outputs_paused=false`, the controller reconnects,
and the previous live profile remains active. Select the calibrated
profile to apply the new values. Repeat, cancel during measurement,
and verify the calibration mouse disappears and the profile stays
unchanged.

The Wayland widget test pauses the simulated worker at each progress
update and checks that the window responds before the worker exits. It
also checks Save and Cancel.

```sh
cargo test --locked -p joyshockrust-editor calibration_window_completes_and_cancels -- --ignored
```

Choose **Calibrate Controller Gyro** with a controller connected. Place
the controller on a stable surface and start measurement. Check that
confirmation appears after five continuous seconds of steady readings.
Move the controller just before the window completes and verify it
needs a fresh five-second interval. Cancel at the confirmation step. Check that the earlier correction is preserved and
that virtual outputs resume. Repeat and save, then open a gyro-enabled
profile and compare stationary drift and deliberate slow aim. Verify that
the bridge logs the saved correction after reconnect and that a second
controller keeps its own correction. With the default minimum speed,
leave the controller on the surface and check the cursor stays still,
then turn it slowly to confirm that deliberate aim still moves it.

Bind **Gyro → Controller gyro calibration → Hold button** to PS. Verify
a tap leaves calibration idle. Hold PS for 1.5 seconds, then release it
and set the controller down. Verify the single start pulse and the
stronger, longer double completion pulse on USB and Bluetooth. Toggle
one output off and back on and verify that the single ready pulse occurs
when both outputs are available. Repeat over USB with the controller’s
audio device occupied to check the evdev fallback. Repeat while moving for 15 seconds
and verify the previous correction remains active. Run RWC over USB and
rotate slowly around the right stick edge. Check that tactile steps
follow rotation in either direction and cease when the stick centers.

For diagnosis without the game, run `joyshockrust-linux --test-haptics`
on the USB device. Verify that the selected PCM matches the controller
and report which effects are perceptible. Run with `--pattern sweep`
to compare the count of detected stick detents with the felt clicks.
The automated CLI test checks the actuator-channel PCM and propagation
of an ALSA playback failure using a simulated player. On the reported
Fedora setup, moving the system's audio output away from the controller
freed the ALSA playback device. The test then completed PCM playback
with both DualSense and DualSense Edge.

Enable **Start with system**, log out and back in, and confirm the tray
and bridge start. Clear the setting and repeat to confirm manual
launch is needed. Use **Quit JoyShockRust** and verify both user units
are inactive.

## Benchmarks
Use a repeatable workload and document CPU model, kernel, Rust version, power profile, controller firmware/transport, report rate, and whether debug/release. Record p50, p95, p99, max, samples, and any drops. Include CPU contention, idle and reconnect cases. Avoid misleading comparisons between processing-only latency and physical end-to-end latency.

### Live bridge benchmark

Build once in release mode so compilation is outside the measurement.
Stop the user service during the run to keep one virtual output active,
then connect the controller before starting the benchmark.

```sh
cargo build --locked --release -p joyshockrust-linux
systemctl --user stop joyshockrust.service
profile="${XDG_CONFIG_HOME:-$HOME/.config}/joyshockrust/profile.toml"
target/release/joyshockrust-linux --benchmark-reports 10000 --profile "$profile" --auto-transport
```

Run once with the pad resting, then again while moving it and exercising
its controls. Repeat with USB unplugged and Bluetooth connected. Check
`benchmark_transport` on each run, and restart the user service when
finished.

`benchmark_wall_ns` spans the report loop, including waits. The
`benchmark_cpu_user_us` and `benchmark_cpu_system_us` fields report
process CPU time during the same window. `benchmark_cpu_percent` is the
sum divided by wall time, with 100% representing one fully occupied CPU
core. `benchmark_peak_rss_kib` is the process's peak resident memory up
to the end of collection and includes the benchmark's sample buffers.
Optionally use `/usr/bin/time -v` for a whole-command measurement.

`processing_*_ns` spans the decoded report callback through virtual
output submission when controls change. `host_interval_*_ns` measures
spacing between callback starts, including queueing and scheduling
effects. Both distributions show p50, p95, p99 and max. The processing
target of 0.5 ms corresponds to `processing_p99_ns=500000`. The benchmark
measures the service's userspace work and reports parser rejections
separately. To test an early disconnect, unplug or power off the controller
before the target count. Expect `benchmark_end=disconnected`, a partial
summary and a nonzero exit.

Collection defaults to a 600-second limit after the bridge opens its
virtual outputs. Set `--benchmark-timeout SECONDS` to use a different
limit between 1 and 3600 seconds. To exercise the deadline on a connected
controller, use `--benchmark-reports 100000 --benchmark-timeout 1`.
Expect `benchmark_end=timeout`, the results collected so far and a
nonzero exit. Successful runs print `benchmark_end=complete`.

`device_interval_*_ns` uses the controller's IMU clock for the same run.
It summarizes forward intervals of at most 50 ms, including continuous
32-bit wraps. The benchmark counts repeated timestamps, backward or
ambiguous timestamps, and gaps over 50 ms separately. Each gap starts a
fresh device-clock segment. Compare this distribution with
`host_interval_*_ns` to examine variation in both clocks. The counters
describe timestamps on decoded reports. Determining physical packet loss
requires separate evidence.

## Observability
Counters for reports received/rejected, timestamp gaps, output writes, queue/backlog, reconnects and timer deadlines. Diagnostics must be buffered or collected outside the hot path; no synchronous per-sample logging.

The Linux test suite checks the zero-heap-allocation target with synthetic
USB reports through readiness polling, decoding, stick curves, remaps,
touchpad bindings and gamepad event encoding. A second measurement
covers gyro integration and relative mouse event encoding. Both checks
use in-memory output sinks and start after configuration and device
setup. Physical driver and IPC paths have separate acceptance tests.

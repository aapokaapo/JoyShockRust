# Testing, instrumentation and performance

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
independent CRC values for both Bluetooth input and calibration.

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

## Benchmarks
Use a repeatable workload and document CPU model, kernel, Rust version, power profile, controller firmware/transport, report rate, and whether debug/release. Record p50, p95, p99, max, samples, and any drops. Include CPU contention, idle and reconnect cases. Avoid misleading comparisons between processing-only latency and physical end-to-end latency.

## Observability
Counters for reports received/rejected, timestamp gaps, output writes, queue/backlog, reconnects and timer deadlines. Diagnostics must be buffered or collected outside the hot path; no synchronous per-sample logging.

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

## Benchmarks
Use a repeatable workload and document CPU model, kernel, Rust version, power profile, controller firmware/transport, report rate, and whether debug/release. Record p50, p95, p99, max, samples, and any drops. Include CPU contention, idle and reconnect cases. Avoid misleading comparisons between processing-only latency and physical end-to-end latency.

## Observability
Counters for reports received/rejected, timestamp gaps, output writes, queue/backlog, reconnects and timer deadlines. Diagnostics must be buffered or collected outside the hot path; no synchronous per-sample logging.

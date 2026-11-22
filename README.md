# JoyShockRust — development guide

JoyShockRust is a **new Linux-first Rust controller mapper**, inspired by the behavior of [JoyShockMapper-systemd](https://github.com/aapokaapo/JoyShockMapper-systemd) and [upstream JoyShockMapper](https://github.com/Electronicks/JoyShockMapper). The initial workspace provides a pure DualSense Edge USB report parser and device timebase.

## Mission
Correct controller behavior first; then low input-to-output latency, low jitter, low CPU/memory overhead, maintainability, and feature parity. Prefer a small working vertical slice to a broad unfinished rewrite.

## Locked decisions
- Rust stable, edition 2024; Linux first.
- Direct **hidraw** controller acquisition for DualSense and DualSense Edge over USB or Bluetooth.
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

## Status

Development has started **Phase 6**, controller and game compatibility. The USB parser replays a
300-report capture. The Linux virtual gamepad, USB reconnect, gyro mouse
startup, and digital hold and toggle gates have user-reported hardware tests.
The analog trigger gate also passed the user's hardware check. Tap/hold and
double-tap have also passed the user's hardware check. The user also verified
Xbox Series virtual output, Edge Bluetooth input and automatic transport
switching. The user also verified standard DualSense input on hardware.

See the [implementation plan](IMPLEMENTATION_PLAN.md), [test evidence](TESTING_AND_PERFORMANCE.md)
and [source analysis](docs/REFERENCE_ANALYSIS.md).

## Local checks

From the workspace root with stable Rust, rustfmt and Clippy installed. The
Linux bridge also needs `pkg-config` and the libudev development files
(`libudev-dev` on Debian/Ubuntu or `systemd-devel` on Fedora).

Install the formatter for the Cargo toolchain you use:

- Fedora-packaged Cargo: `sudo dnf install rustfmt`.
- Rustup-managed Cargo: `rustup component add rustfmt` from the workspace root.

The Fedora package `fmt` is a C++ formatting library. Use
`cargo fmt --version` to verify that Cargo can find rustfmt.

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

## TOML profiles

Start with the [example profile](profiles/dualsense-edge.example.toml):

```sh
cargo run --locked -p joyshockrust-linux -- --check-profile profiles/dualsense-edge.example.toml
cargo run --locked -p joyshockrust-linux -- --profile profiles/dualsense-edge.example.toml
```

Profiles require `version = 1`. The optional `[gyro]` table enables the gyro
mouse. The optional `[gyro.binding]` table takes `mode` and `input`. Modes are
`hold`, `disable-while-held`, `toggle`, `tap-hold`, `double-tap`, `chord`
and `trigger`. Input names a digital button, a chord, or an analog trigger,
depending on the mode. The [gyro-except-L2 profile](profiles/gyro-except-l2.toml)
keeps gyro aim enabled until the digital L2 button is pressed.
Use `joyshockrust-linux --install-profile SOURCE [DESTINATION]` to validate
a saved preset and copy it into place, preserving its comments and formatting.
The default destination is `$XDG_CONFIG_HOME/joyshockrust/profile.toml`, or
`$HOME/.config/joyshockrust/profile.toml` when `XDG_CONFIG_HOME` is unset or
relative. The command creates parent directories as needed and replaces an
existing regular file. Symbolic link destinations are rejected. Reload the
running service to apply the installed profile.

The profile is loaded at startup. Send SIGHUP to the running bridge PID
to reload it. Invalid edits leave the last valid configuration active. Choose the profile or the equivalent CLI
mapping flags for that run. You can still pass a hidraw path with `--profile`.

A profile can also select the bridge's startup options. Add this table to
the active service profile to use automatic transport selection, Xbox
Series output and rumble.

```toml
[bridge]
transport = "auto"
gamepad = "xbox-series"
rumble = true
```

The transport options are `usb`, `bluetooth` and `auto`. The gamepad options
are `generic`, `xbox360` and `xbox-series`. Profiles without this table use
USB, generic output and rumble off. Restart JoyShockRust when changing
`[bridge]` settings. The reload command still applies mapping and gyro
changes during a session when the bridge settings stay the same. The
existing transport and gamepad flags override their profile values.
`--rumble` enables rumble for the run.

## Device diagnostics

Run `cargo run --locked -p joyshockrust-linux -- --diagnose` to list wired
DualSense Edge hidraw nodes and check access to them and `/dev/uinput`.
Append a hidraw path to check that node directly. The command opens the
devices as the current user and exits without creating virtual outputs.

## systemd user service

Stop any manually running bridge before starting the service. Install the
binary and the [user unit](dist/systemd/joyshockrust.service) from this
checkout. The sample profile is copied only if the destination is absent.

```sh
cargo install --locked --path crates/joyshockrust-linux --root "$HOME/.local"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}"
profile="$config_dir/joyshockrust/profile.toml"
if [ ! -e "$profile" ]; then
  install -Dm644 profiles/dualsense-edge.example.toml "$profile"
fi
install -Dm644 dist/systemd/joyshockrust.service "$config_dir/systemd/user/joyshockrust.service"
systemctl --user daemon-reload
systemctl --user start joyshockrust.service
journalctl --user -u joyshockrust.service -f
```

The service uses the user manager's configuration directory. Edit
`joyshockrust/profile.toml` there and run
`systemctl --user reload joyshockrust.service` to load the changes. The
command waits for the result and exits with an error if the bridge rejects
the profile. The journal includes the rejection reason. Reload
keeps the virtual gamepad. Changing gyro settings may recreate the mouse
and restart its calibration. If the profile is invalid or unreadable at
startup, the service stops. Correct the profile, check it with
`$HOME/.local/bin/joyshockrust-linux --check-profile "$profile"`, then start
the service again.
Use `systemctl --user enable joyshockrust.service` to start it with the
user manager, or `systemctl --user stop joyshockrust.service` to stop it.
The service needs the same hidraw and uinput access as a manual run.

Query the running service with `$HOME/.local/bin/joyshockrust-linux --status`.
The command prints `controller=waiting` or `controller=connected` and
`gyro_configured=true` or `gyro_configured=false`. The `transport` field
reports `usb`, `bluetooth` or `none`. The `model` field reports
`dualsense`, `dualsense_edge` or `none`. The gyro field describes
the loaded profile. A hold or toggle binding can still suppress mouse output.
The `last_reload` field reports `not_attempted` on startup, then `applied`
or `rejected` after the bridge handles a reload. A bridge started without a
profile reports `unavailable`. To request the same reload directly, run
`$HOME/.local/bin/joyshockrust-linux --reload-profile`.
The status and reload sockets are under `$XDG_RUNTIME_DIR/joyshockrust/`
and are removed when the service stops.

## Virtual gamepad on Linux

Run the Linux bridge with read access to a DualSense or DualSense Edge USB hidraw node and
write access to `/dev/uinput`. With no arguments it uses udev discovery and
waits for the next connection when the controller is unplugged. Pass a
specific node to run a single session.

```sh
cargo run --locked -p joyshockrust-linux
cargo run --locked -p joyshockrust-linux -- /dev/hidrawN
```

The bridge exposes **JoyShockRust Virtual Gamepad** with standard buttons,
D-pad, sticks and analog triggers. It releases held controls when the input
session ends. Games can still see the physical controller. See the
[uinput and Steam acceptance tests](TESTING_AND_PERFORMANCE.md#virtual-gamepad-acceptance).

For modern Xbox output, pass `--gamepad xbox-series` to expose
**JoyShockRust Virtual Xbox Series X|S Gamepad**. It advertises the wired
Xbox Series input ID, scales analog triggers to 0–1023, and maps the
DualSense Edge touchpad click to Xbox Share. Use `--gamepad xbox360` when
testing Xbox 360 compatibility. Both modes support TOML profiles, including
bindings that use the physical trigger buttons. The output mode applies
for the entire bridge run. Steam Input and Proton need hardware testing.

Add `--rumble` to forward game vibration to the connected physical
controller. The bridge uses the force feedback evdev node belonging to
the same controller, which must be writable. Look for
`Rumble forwarding ready` in the log. On each connection, the bridge
allows up to one second for the physical evdev node and its permissions.
If the node is still unavailable, the bridge reports the reason and starts
the virtual gamepad without rumble.
For a permission error in a local desktop session, the optional
[udev rule](dist/udev/70-joyshockrust-rumble.rules) grants active-seat
access to the force feedback nodes of supported DualSense controllers.

```sh
sudo install -Dm644 dist/udev/70-joyshockrust-rumble.rules /etc/udev/rules.d/70-joyshockrust-rumble.rules
sudo udevadm control --reload-rules
```

Disconnect and reconnect the controller to apply the rule. With the bridge
running and rumble ready, run the following in another terminal to request a
short pulse without launching a game.

```sh
cargo run --locked -p joyshockrust-linux --example rumble_test
```

If several JoyShockRust virtual gamepads are present, append
`-- /dev/input/eventN` to select the virtual event node. The test sends the
effect and you verify the physical pulse. If the pulse is missing, set
`JOYSHOCKRUST_RUMBLE_TRACE=1` when starting the bridge to log uploads and
playback writes. The example's `--physical /dev/input/eventN` option uses
the physical node from `Rumble forwarding ready` to exercise the kernel
driver directly.
[Rumble forwarding acceptance](TESTING_AND_PERFORMANCE.md#rumble-forwarding-acceptance)

Add `--stick-curve 2` to soften small movements on both virtual gamepad
sticks. The radial curve preserves direction, center, and full travel.
Exponent 1 is linear. Values below 1 amplify small movement and values
above 1 reduce it. The accepted range is 0.25–4.

Use repeatable `--remap-button SOURCE=TARGET` options to change virtual
gamepad buttons, for example `--remap-button cross=circle --remap-button circle=cross`.
Rules use physical button names and apply simultaneously. Gyro bindings
continue to read physical buttons. Remapping L2 or R2 changes its digital
button. Analog trigger travel is forwarded independently.

Layer rules use a separate, repeatable `--layer-remap MODIFIER:SOURCE=TARGET`
option. For example, use `--layer-remap l1:cross=circle --layer-remap r1:cross=square`.
Rules sharing a modifier belong to the same layer. At most one layer is
active. Once selected, it stays active until its modifier is released.
Other modifiers act like ordinary buttons while it is active. A modifier
held across layer exit must be pressed again to select its layer.
If multiple new modifiers arrive in the same report while no layer is
active, the last-declared layer wins. Unmapped buttons use base rules. The modifier still follows its own button mapping.

Replace the former `--remap-layer l1 --remap-button cross=circle` syntax
with `--layer-remap l1:cross=circle`.

## DualSense and DualSense Edge over Bluetooth

Pair the controller in the Linux Bluetooth settings, then select the
Bluetooth bridge explicitly. USB discovery remains the default.

```sh
cargo run --locked -p joyshockrust-linux -- --diagnose --bluetooth
cargo run --locked -p joyshockrust-linux -- --bluetooth --gamepad xbox-series
```

Append `--gyro-mouse` to use the controller's Bluetooth factory calibration
for pointer output. Bluetooth discovery accepts full reports with a valid
checksum and waits for reconnection after the device disconnects.

Use `--auto-transport` to accept either connection in one run. When opening
a session, the bridge tries USB first and then Bluetooth. It keeps the chosen
connection until it disconnects and creates fresh virtual devices for the
next session. When both models are available on the same transport, the
bridge tries the Edge first. The standard DualSense uses USB product ID
`0ce6`. `--diagnose --auto-transport` lists candidates for both transports.

## Experimental gyro mouse

On Linux, enable relative pointer output from a DualSense or DualSense Edge:

```sh
cargo run --locked -p joyshockrust-linux -- --gyro-mouse
```

The bridge reads the controller's factory calibration and creates
**JoyShockRust Gyro Mouse** alongside the virtual gamepad. You can append
`/dev/hidrawN` to select one device explicitly. The initial sensitivity is
four relative counts per degree on both axes. To measure residual gyro bias,
add `--calibrate-gyro`, put the controller on a stable surface, and wait for
`Gyro rest calibration complete`. Pointer output starts when the calibration
window completes. The bias is measured again on each reconnect.
Add `--gyro-hold l2` (or `--gyro-hold-l2`) to move the pointer only while
L2 is held. Alternatively, use `--gyro-toggle r3` to switch gyro aiming on
and off with each R3 press. A toggle starts off on every connection.
For analog travel, use `--gyro-trigger l2` or `--gyro-trigger r2:0.60:0.40`.
The default trigger engages at 45% travel and releases at 35%. Custom
thresholds use `SIDE:PRESS:RELEASE` in the range 0–1, with the release
threshold below the press threshold. Select one binding per session.
Any binding can be combined with `--calibrate-gyro`. Use
`--gyro-tap-hold r3` to toggle on a tap or enable aim temporarily on a
hold of at least 200 ms. Releasing a long hold restores the latched state.
Use `--gyro-double-tap r3` to toggle only after two short taps. Each tap
must be shorter than 200 ms and the releases at most 300 ms apart.
Use `--gyro-chord l1,r1` to aim while holding both buttons, in either order.

Supported button names are `cross`, `circle`, `square`, `triangle`,
`l1`, `r1`, `l2`, `r2`, `l3`, `r3`, `create`, `options`, `ps`,
`touchpad`, `mute`, `fn-left`, `fn-right`, `paddle-left` and
`paddle-right`. L2 and R2 use their digital button states.

[USB gyro test](TESTING_AND_PERFORMANCE.md#usb-gyro-mouse-acceptance)

## Experimental Flick Stick

Add `--flick-stick 4` with `--gyro-mouse` to use the right stick for
horizontal mouse turns. The argument sets relative counts per degree and
accepts values from 0.01 to 100. Flicking from center turns toward the stick
angle over 100 ms. Rotating the held stick follows its angle changes.
The virtual gamepad's right-stick axes are centered while this mode is
enabled. Gyro gating still controls gyro input independently.
See the [Flick Stick test](TESTING_AND_PERFORMANCE.md#flick-stick-acceptance).

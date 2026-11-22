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
and `trigger`. Input names a digital button, a touch region, a chord, or an
analog trigger, depending on the mode. The [gyro-except-L2 profile](profiles/gyro-except-l2.toml)
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

Set horizontal and vertical gyro sensitivity independently in the
`[gyro]` table.

```toml
[gyro]
sensitivity_x = 12.0
sensitivity_y = 2.0
```

Each axis defaults to 4 counts per degree and accepts values from 0 to 100.
A value of 0 disables gyro motion on that axis. A sensitivity-only change
keeps the existing gyro mouse and calibration. Flick Stick has its own
sensitivity setting.

With the user service running, load a preset into the current session.

```sh
"$HOME/.local/bin/joyshockrust-linux" --load-profile profiles/gyro-except-l2.toml
```

This replaces the live mappings and gyro settings as a complete profile.
The service replies after applying or rejecting it. The profile can omit
`[bridge]` to keep the current controller selection and output settings. The loaded preset remains active
until another load, a reload of the startup profile or a service restart.
Use `--install-profile` to persist a preset for the next start.

A profile can also select the bridge's controller and output options. Add this table to
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
USB, generic output and rumble off. Restart JoyShockRust to change
`gamepad`, `virtual_gamepad` or `rumble`. With automatic discovery, reload the profile to
change `transport`, `model` or `controller_id` in the same process.
The bridge releases and closes the current virtual devices before
opening the selected controller. If it is absent, the bridge waits for
it. An explicit hidraw path and a one-shot benchmark keep their original
selection. The transport and gamepad CLI flags override their profile values.
`--rumble` enables rumble for the run.

Set `model = "dualsense"` or `model = "dualsense-edge"` in `[bridge]`
to select a controller model. If the chosen model is absent, automatic
discovery waits for it. The default `model = "any"` accepts either model.
The model selection also applies to benchmarks and game calibration
that use the profile.

To select one individual controller, run
`joyshockrust-linux --diagnose --auto-transport --show-controller-id`
and copy that controller's reported ID into `[bridge]`.

```toml
[bridge]
transport = "auto"
model = "dualsense-edge"
controller_id = "00:11:22:33:44:55"
```

Use your controller's value in place of the example. The ID comes from
Linux's HID driver. A configured ID requires an exact match, with case
ignored in hexadecimal digits. If the ID is unavailable, choose a model
filter without an ID. Use `systemctl --user reload joyshockrust.service`
or `--load-profile PATH` to change the active selection in discovery mode.

To change only the live ID filter, use `joyshockrust-linux
--select-controller ID` or pass `any` to clear the ID filter.
This keeps the current transport, model, mappings and gyro settings.
The new ID applies until another selection command, a profile load that
sets the selection, or a process restart. `--reload-profile` restores
the startup selection. An optional final argument selects the reload
socket.

### Keyboard and mouse profiles

The [gyro and keyboard example](profiles/gyro-keyboard.toml) enables gyro
mouse and Flick Stick, maps the left stick to WASD, and assigns three gestures
on Cross to three keys. `[bridge].virtual_gamepad = false` runs the
profile with keyboard and mouse output.

```toml
[[keyboard.bindings]]
button = "cross"
tap = "KEY_SPACE"
hold = "KEY_LEFTSHIFT"
double_tap = "KEY_R"
```

Targets are Linux `KEY_*` names. Each binding reads a physical button or
a direction such as `dpad-up`. A diagonal activates both matching
directions. A hold-only binding presses its key immediately. A binding
with a tap action commits to a hold after 200 ms. Two short releases
less than 300 ms apart trigger the double-tap key. A single tap waits
for that window when a double-tap binding is configured. Tap outputs
hold the virtual key briefly so games can observe the press.

Use `[[keyboard.sticks]]` to bind an analog stick. This example uses
`KEY_LEFTSHIFT` in the outer zone and keeps the current WASD direction
held while entering or leaving that zone.

```toml
[[keyboard.sticks]]
side = "left"
deadzone = 0.2
up = "KEY_W"
left = "KEY_A"
down = "KEY_S"
right = "KEY_D"
outer = { key = "KEY_LEFTSHIFT", threshold = 0.8, release = 0.7 }
```

`side` accepts `left` or `right`. Radius is measured from the center,
with 1.0 representing full deflection along an axis. Inside the deadzone,
all stick keys release. Diagonals can hold two direction keys. The outer
key engages at `threshold` and releases at `release`. The separate
release threshold prevents flickering near the boundary. A profile can
omit `outer` for direction keys alone.

If the game uses the same key to toggle sprint on and off, add
`mode = "toggle"` to `outer`. This sends a short key press each time
the stick enters or leaves the outer zone, including a return to center.
WASD follows the stick throughout. Start with sprint off in the game.
JoyShockRust tracks the stick zone, so games that cancel sprint on their
own can fall out of sync. A toggle key must be exclusive to its zone.
In `profiles/gyro-keyboard.toml`, change or remove the Cross `hold`
binding if Left Shift is the toggle key.

Use `mode = "keep_sprint"` when the game ends sprint by itself once the
movement keys are released. The outer zone sends one short press per
continuous movement. Backing off or changing direction while a stick
direction remains mapped keeps that sprint going. Releasing all mapped
stick directions re-arms the next sprint, including on return to center.
This mode also requires an exclusive sprint key. The default mode is
`hold`.

Keyboard mappings can be reloaded. Changing the virtual gamepad setting
requires a restart. If the game also detects the physical gamepad, use
the applicable [per-game visibility settings](#per-game-sdl-controller-visibility).

### Touchpad regions

Add a `[touchpad]` grid to a version 1 profile for DualSense or DualSense
Edge over USB or Bluetooth. This example splits the surface into two halves
and gives each half a separate touch and click binding.

```toml
[touchpad]
columns = 2
rows = 1

[[touchpad.regions]]
column = 1
row = 1
touch = "cross"
click = "square"

[[touchpad.regions]]
column = 2
row = 1
touch = "circle"
click = "triangle"
```

Rows and columns start at 1 in the top-left corner. The grid supports up to
25 cells, and a 1×1 grid covers the whole surface. Each region can define
`touch`, `click`, or both. Targets use the same virtual gamepad button names
as button remaps. Digital `l2` and `r2` targets are available with generic
output.

Both contacts can hold their respective touch targets. Moving across a
boundary updates the held touch buttons. Pressing the pad also holds the
click target from the first active contact slot until the pad is released.
If that cell has no click binding or there is no active contact, the
ordinary `touchpad` button mapping applies. Region targets go directly to
the virtual gamepad and may share a target with a physical button. Region
changes can be applied with `--load-profile` or a service reload.
[Touchpad acceptance test](TESTING_AND_PERFORMANCE.md#touchpad-grid-acceptance)

#### Touch region as a gyro input

This version 1 profile enables gyro aim while either finger touches the
right half of the pad.

```toml
version = 1

[touchpad]
rows = 1
columns = 2

[gyro.binding]
mode = "hold"
input = "touch:1:2"
```

`touch:ROW:COLUMN` selects a cell in the same grid used for gamepad
bindings. Both coordinates start at 1. A cell can control gyro with or
without its own `[[touchpad.regions]]` entry. Leaving the cell or lifting
the last finger releases the input. Use `disable-while-held` to turn gyro
aim off during the touch. Touch regions also work with `toggle`, `tap-hold`,
`double-tap` and `chord`. For example, `input = "l1,touch:1:2"` with
`mode = "chord"` requires both inputs. The existing `touchpad` input
selects the physical click.

## Device diagnostics

Run `cargo run --locked -p joyshockrust-linux -- --diagnose` to list wired
DualSense Edge hidraw nodes and check access to them and `/dev/uinput`.
Append a hidraw path to check that node directly. The command opens the
devices as the current user and exits without creating virtual outputs.

Use `--diagnose --profile PATH` to check a specific profile before
starting or restarting the bridge. It prints the effective controller
filter and labels supported controllers outside that filter as
`excluded by profile`. An optional `--bluetooth` or
`--auto-transport` overrides the profile's transport for this check.
Add `--show-controller-id` when you need to see individual HID IDs.

Use `--benchmark-reports COUNT` for a bounded live bridge benchmark. It
reports processing, host and device report interval percentiles plus
timestamp discontinuity counts after handling the requested count.
Use `--benchmark-timeout SECONDS` to set the report collection deadline.
The summary also reports CPU time and the process's peak resident memory.
[Live benchmark instructions](TESTING_AND_PERFORMANCE.md#live-bridge-benchmark)

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
`dualsense`, `dualsense_edge` or `none`. The `selected_transport` and
`selected_model` fields show the effective filter even when the bridge
is waiting. `controller_id_configured` shows whether the filter requires
an individual HID ID. The gyro field describes
the loaded profile. A hold or toggle binding can still suppress mouse output.
The `battery_state` and `battery_percent` fields report the controller's
latest battery reading. The percentage is an estimate in coarse steps.
A missing reading is shown as `none`, including when the controller is
disconnected.
The `reports_decoded` and `reports_rejected` fields count HID decoder
results for the current connection. The `report_rate_hz` field estimates
decoded reports per second using host time after the first successful read
batch. It shows `none` until at least a second has elapsed and a later
batch has supplied a valid report. The counters reset to zero on disconnect.
The `report_full_batches` field counts batches that read all 32 report
slots, including any parser rejections.
`uinput_gamepad_batches` and `uinput_mouse_batches` count synchronized
event batches successfully submitted by the bridge for this connection.
`controller_sessions_started` and `controller_sessions_ended` count
session transitions since the bridge process started.
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

## Per-game SDL controller visibility

Keep the JoyShockRust bridge running and launch a native SDL game with
`joyshockrust-linux --launch-sdl -- GAME [ARGS...]`. The launcher passes
the supported physical DualSense IDs to SDL's ignore list for that process.
For a Steam title, set its launch options to
`/absolute/path/to/joyshockrust-linux --launch-sdl -- %command%`.
The game's input backend determines whether the hint takes effect.
[Per-game visibility details](OUTPUT_AND_VISIBILITY.md#physical-controller-visibility)

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
`/dev/hidrawN` to select one device explicitly. The default sensitivity is
four relative counts per degree on both axes. A TOML profile can set each
axis independently. To measure residual gyro bias,
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

## Game sensitivity calibration

Use the right stick to turn the camera through whole rotations. Enable raw
mouse input in the game when available. Stop the normal JoyShockRust bridge
for the measurement so its gyro or gamepad output cannot affect the turn.

```sh
joyshockrust-linux --calibrate-game --profile /path/to/game.toml
```

1. Enter the game's mouse sensitivity at the terminal.
2. Focus the game, aim at a landmark and press the controller's Options
   button with the right stick centered.
3. Tilt the right stick up and rotate it while counting full camera turns.
   Use several turns for better precision. Return to the same landmark,
   center the stick and press Options again.
4. Return to the terminal and enter the number of complete turns. The command
   prints and saves `real_world_calibration` and `in_game_sens` in the
   profile's `[gyro]` table.

The command uses the profile's transport. Append a `/dev/hidrawN` path
to select one controller explicitly. The calibration mouse runs only
for the measurement. For a known number of counts and measured turns,
the calculator form remains available.

```sh
joyshockrust-linux --calibrate-game 9000 2.0 1.25
```

```toml
[gyro]
real_world_calibration = 40.0
in_game_sens = 2.0
sensitivity_x = 1.0
sensitivity_y = 1.0
# flick_stick = 1.0
```

The factor `real_world_calibration / in_game_sens` scales both gyro axes
and optional Flick Stick output. Both fields default to `1.0`. With the
factor calibrated, `sensitivity_x = 1.0` is a starting point for a 1:1
horizontal turn. Tune the vertical axis separately. If the game exposes
no numeric sensitivity, use `1.0` and recalibrate after a game setting
change. Load the resulting preset with `--load-profile`.
[JoyShockMapper scaling](https://github.com/Electronicks/JoyShockMapper/blob/e148efe0b6ff4ad2ce0fc8c2560fa1028097b1a7/JoyShockMapper/src/main.cpp#L1166-L1174)

## Experimental Flick Stick

Add `--flick-stick 4` with `--gyro-mouse` to use the right stick for
horizontal mouse turns. The argument sets relative counts per degree and
accepts values from 0.01 to 100. Flicking from center turns toward the stick
angle over 100 ms. Rotating the held stick follows its angle changes.
The virtual gamepad's right-stick axes are centered while this mode is
enabled. Gyro gating still controls gyro input independently.
See the [Flick Stick test](TESTING_AND_PERFORMANCE.md#flick-stick-acceptance).

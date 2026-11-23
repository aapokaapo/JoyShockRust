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
Workspace checks include the editor and need the GTK 4 and libadwaita
development packages listed under [Desktop tray](#desktop-tray).

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
The service replies after applying or rejecting it. Loading a preset or
reloading the startup profile keeps the current controller selection.
The profile can omit `[bridge]` to retain the running output settings.
The loaded preset remains active until another profile command or a
service restart. Use `--unload-profile` to restore default mappings and
turn off profile-driven gyro and keyboard or mouse button output. The
service keeps the selected controller and startup virtual gamepad settings.
Use `--reload-profile` to restore the startup profile from disk.
Use `--install-profile` to persist a preset for the next start.

Use `[bridge]` for virtual output settings such as Xbox Series output
and rumble.

```toml
[bridge]
gamepad = "xbox-series"
rumble = true
```

The gamepad options are `generic`, `xbox360` and `xbox-series`. The profile
sets the startup identity, rumble and `virtual_gamepad` default. To switch
virtual gamepad emulation for the current process, use
`joyshockrust-linux --set-gamepad off` or `--set-gamepad on`. The command
keeps the physical controller session, gyro and keyboard mappings active.
The selected state persists through reconnects and profile loads.

### Controller selection

The controller settings file is
`${XDG_CONFIG_HOME:-$HOME/.config}/joyshockrust/controller.toml`.
Its optional `transport` and `model` fields default to `"auto"` and
`"any"` so USB and Bluetooth DualSense and DualSense Edge are eligible.
Values for `transport` are `"usb"`, `"bluetooth"` and `"auto"`.
Values for `model` are `"dualsense"`, `"dualsense-edge"` and `"any"`.
Move any legacy model or transport restriction you still want from the
profile to this file. Old profile keys are accepted for compatibility
and have no effect on discovery.

The bridge saves the last selected controller's HID ID as
`last_controller_id` when it can read the ID. At startup it prefers that
controller if it is available, otherwise it uses the first usable
controller in discovery order. Without a match it stays running, reports
`controller=waiting`, and connects when an eligible controller appears.
Only one controller is active at a time.

Run `joyshockrust-linux --list-controllers --show-controller-id` to list
connected controllers and find their HID IDs. Compare the listed hidraw
paths with `active_hidraw` in `--status` to identify the current device.
Use `--select-controller ID` to select a device
in a running discovery service or use `any` to clear the live ID filter.
Add a model or transport flag to change those filters in the same command.

```sh
joyshockrust-linux --select-controller any --controller-model dualsense-edge --auto-transport
```

The command retains the loaded profile. Changed model and transport
filters are saved to the controller settings file. An explicit ID filter
waits for that controller for the current run. The next process treats
the last active ID as a preference and can fall back to another controller.
Controller selection also works before a profile is loaded. For a manual
run, start the bridge with `--reload-socket PATH` and pass that path as
the final argument to `--select-controller` or `--load-profile`.
To override the settings at startup for a single run, use `--usb`, `--bluetooth`,
`--auto-transport`, `--controller-model MODEL` or `--controller-id ID`.

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
directions. A hold-only binding presses its key immediately. When a
binding combines tap and hold, a release before `hold_ms` counts as a
tap and a longer press activates the hold. Two short releases less
than `double_tap_ms` apart trigger the double-tap action. A single tap
waits for that window when a double-tap binding is configured. Both
settings are optional positive integers in milliseconds and default to
200 and 300 respectively. Tap outputs hold the virtual key briefly so
games can observe the press.

Give `tap`, `hold`, or `double_tap` an array of distinct keyboard keys or
mouse buttons to send a shortcut or modified click. Ctrl, Shift, Alt and
Meta are pressed first and released last. An output that another active
binding still holds stays down.

```toml
[[keyboard.bindings]]
button = "create"
tap = ["KEY_LEFTCTRL", "KEY_TAB"]
```

```toml
[[keyboard.bindings]]
button = "r1"
tap = ["KEY_LEFTSHIFT", "BTN_LEFT"]
```

Use `trigger` in place of `button` for an analog L2 or R2 binding.
The optional suffix sets the engagement and release thresholds on a
scale from 0 to 1. The defaults are 0.45 and 0.35. Bindings at different
thresholds can overlap, so a deeper pull can add a second key.

```toml
[[keyboard.bindings]]
trigger = "l2:0.35:0.25"
hold = "KEY_Q"
```

Button and trigger gestures also accept `BTN_LEFT`, `BTN_RIGHT`,
`BTN_MIDDLE`, `BTN_SIDE` and `BTN_EXTRA`. For example, hold the left
mouse button while pulling R2 past its configured threshold.

```toml
[[keyboard.bindings]]
trigger = "r2:0.6:0.4"
hold = "BTN_LEFT"
```

These gestures are sent by **JoyShockRust Button Mouse**, which also
works in profiles with gyro disabled. Gyro and Flick Stick movement
continue through **JoyShockRust Gyro Mouse** when enabled.

Set `tap` or `double_tap` to a scroll action to emit one mouse wheel
step. Directions are `up`, `down`, `left` and `right`.

```toml
[[keyboard.bindings]]
button = "dpad-up"
tap = { scroll = "up" }

[[keyboard.bindings]]
button = "dpad-down"
tap = { scroll = "down" }
```

Use `hold` to repeat a wheel step while the binding stays pressed.
`scroll_repeat_ms` sets the interval in milliseconds and defaults to 100.
A hold-only binding starts immediately. A binding that also has a tap or
double-tap action starts scrolling at its `hold_ms` threshold.

```toml
[[keyboard.bindings]]
button = "dpad-right"
hold = { scroll = "right" }
scroll_repeat_ms = 80
```

Use `[keyboard.touch_scroll]` to scroll by swiping the touchpad. Each
configured axis emits one wheel step per specified distance in touchpad
coordinate units. Omit an axis to disable it. In one-finger mode, the
bridge follows the same finger until it lifts.
Set `fingers = 2` to scroll from the midpoint between two touches. Lifting
either finger ends the swipe. The default is `fingers = 1`.
Use `invert_vertical` and `invert_horizontal` to reverse the corresponding
touch swipe direction. Both default to `false`. Button-bound wheel actions
keep their configured directions.
Set `axis_lock = true` to scroll along only one axis per swipe. The first
wheel step selects the axis. If both axes reach a step in the same
report, the one with more steps wins, with ties favoring vertical.
Without the lock, a diagonal swipe can scroll on both axes.

```toml
[keyboard.touch_scroll]
fingers = 2
vertical_step = 120
horizontal_step = 180
invert_vertical = true
axis_lock = true
```

Use `[keyboard.touch_pointer]` to move the virtual pointer with one
finger. The sensitivity on each axis defaults to 0.5 counts per touchpad
unit. Allowed values are 0–100, and 0 disables movement on that axis.
Set `invert_x = true` or `invert_y = true` to reverse the corresponding
pointer axis. These settings affect pointer movement only and both
default to `false`.
Set `motion_threshold` to suppress small initial movements. The pointer
begins moving when the finger exceeds that distance from its starting
position. It uses the current report's motion and discards earlier
movement. The threshold is in touchpad units and defaults to 0 (off).
Combine this with `fingers = 2` under `[keyboard.touch_scroll]` to move
the pointer with one finger and scroll with two.

```toml
[keyboard.touch_pointer]
sensitivity_x = 0.5
sensitivity_y = 0.5
motion_threshold = 10
tap_to_click = true
two_finger_tap_to_click = true
tap_window_ms = 200
tap_max_travel = 40
```

`tap_to_click` enables one-finger clicks, with `BTN_LEFT` as the default
output. `two_finger_tap_to_click` enables two-finger clicks, with
`BTN_RIGHT` as the default output. Both settings are off by default
and can be enabled independently. Use `tap_button` or
`two_finger_tap_button` to change the corresponding output to
`BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE`, `BTN_SIDE` or `BTN_EXTRA`.
For example, `two_finger_tap_button = "BTN_MIDDLE"` sends a middle click
when two-finger taps are enabled. Both tap gestures use
`tap_window_ms`, a positive whole number that defaults to 200. A tap
must be shorter than this window from first contact to final lift.
Each finger must stay within `tap_max_travel` touchpad units of its
starting point. This radial distance defaults to 40 and can be set to 0
to require stationary contacts. A scroll step or mechanical press on the
pad cancels the tap.

If the desktop also uses the physical controller touchpad, set
`grab_touchpad = true` under `[keyboard]` in the profile. Loading or
unloading the profile updates the reservation immediately. The CLI flag
`--grab-touchpad` requests the reservation for the entire run. The bridge
reserves only the active controller's native touchpad evdev node. Touch
input still reaches the profile through hidraw.

```toml
[keyboard]
grab_touchpad = true

[keyboard.touch_pointer]
```

`native_touchpad_grab_requested` shows the effective request and
`native_touchpad_grabbed=true` confirms the reservation. If access fails,
the bridge logs the reason and continues serving the profile.

Load the [desktop touchpad profile](profiles/touchpad-desktop.toml) for
one-finger pointing, left and right tap clicks, and two-finger scrolling.
Press the physical pad and move one finger to drag. This profile requests
the native touchpad grab.

```sh
"$HOME/.local/bin/joyshockrust-linux" --load-profile profiles/touchpad-desktop.toml
```

Use `--unload-profile` to exit the desktop profile. To switch virtual
gamepad output independently, use `--set-gamepad off` or `--set-gamepad on`.

Use `chord` in place of `button` or `trigger` to require two active
inputs. Either press order works. The second input completes the chord,
and releasing either input ends it. Separate bindings for the member
inputs run independently. Chords accept buttons, D-pad directions and
touchpad regions. Use a `trigger:` prefix for analog travel.

```toml
[[keyboard.bindings]]
chord = ["l1", "trigger:r2:0.6:0.4"]
hold = "BTN_RIGHT"
```

Use `[[keyboard.sticks]]` to bind an analog stick. This example uses
`KEY_LEFTSHIFT` in the outer zone and keeps the current WASD direction
held while entering or leaving that zone.

```toml
[[keyboard.sticks]]
side = "left"
deadzone = 0.2
deadzone_release = 0.15
direction_hysteresis_deg = 5.0
up = "KEY_W"
left = "KEY_A"
down = "KEY_S"
right = "KEY_D"
outer = { key = "KEY_LEFTSHIFT", threshold = 0.8, release = 0.7 }
```

`side` accepts `left` or `right`. Radius is measured from the center,
with 1.0 representing full deflection along an axis. Direction keys engage
above `deadzone` and release at or below `deadzone_release`. The release
value can be anywhere from zero to `deadzone` and defaults to `deadzone`.
Diagonals can hold two direction keys. Set `direction_hysteresis_deg` to
reduce switching near the straight-to-diagonal boundary. For example,
5° adds the second key at 27.5° from a straight axis and holds it until
the angle falls below 17.5°. The default is 0°. Valid values are below
22.5°. The outer
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

For a game that runs by default and holds Shift to walk, add an
`inner` zone to the same `[[keyboard.sticks]]` table as the WASD
bindings.

```toml
[[keyboard.sticks]]
side = "left"
deadzone = 0.2
up = "KEY_W"
left = "KEY_A"
down = "KEY_S"
right = "KEY_D"
inner = { key = "KEY_LEFTSHIFT", threshold = 0.5, release = 0.6 }
```

Load the complete [keyboard walk profile](profiles/keyboard-walk.toml)
into the running service.

```sh
"$HOME/.local/bin/joyshockrust-linux" --load-profile profiles/keyboard-walk.toml
```

When the stick leaves its deadzone, the inner zone engages at or below
`threshold`. It stays engaged until the radius reaches `release` or
movement returns to the deadzone. If omitted, `release` defaults to
`threshold + 0.1`, capped at 1.0. These values must satisfy
`deadzone < threshold < release <= 1`. WASD continues to follow the
stick across the zone boundary, including diagonals.

Use `mode = "toggle"` if Shift toggles walking. This pulses Shift on both
entry and exit. Use `mode = "keep_sprint"` if one press should last until
movement stops and the game resets the mode on its own. This pulses once
per continuous movement. Pulse keys must be exclusive to their zones.
An `inner` and an `outer` binding can coexist on the same stick. Each
has its own thresholds and timing.

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

#### Touchpad regions as keyboard and mouse inputs

Use `touch:ROW:COLUMN` or `click:ROW:COLUMN` as `button` in a keyboard
binding. Either finger can activate a touch input. A click input uses
the first active contact at the moment the pad is pressed and keeps
that region until release. Tap, hold and double-tap work for both.
The grid can be shared with gyro and gamepad mappings.

```toml
[touchpad]
rows = 1
columns = 2

[[keyboard.bindings]]
button = "touch:1:1"
hold = "KEY_TAB"

[[keyboard.bindings]]
button = "click:1:2"
hold = "BTN_LEFT"
```

## Device diagnostics

Run `cargo run --locked -p joyshockrust-linux -- --diagnose` to list wired
DualSense Edge hidraw nodes and check access to them and `/dev/uinput`.
Append a hidraw path to check that node directly. The command opens the
devices as the current user and exits without creating virtual outputs.

Use `--diagnose --profile PATH` to check a specific profile before
starting or restarting the bridge. It prints the effective controller
filter and labels supported controllers outside that filter as
`excluded by profile`. An optional `--bluetooth` or
`--auto-transport` overrides the controller settings file's transport for this check.
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
checkout. The sample profile is optional. The command below copies it
only if the destination is absent.

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

The service starts with default mappings when the startup profile is
absent. Controller selection and `--load-profile` are available in that
state. To set up a startup profile, create or edit
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

### Desktop tray

Use the desktop tray and its GTK 4 profile editor from a Wayland session.
Building the editor requires GTK 4 and libadwaita 1.5 or newer. Install
`gtk4-devel libadwaita-devel` on Fedora, or `libgtk-4-dev libadwaita-1-dev`
on Debian and Ubuntu.

Install the tray beside the bridge binary and its launcher beside the
desktop's other applications. Keep the bridge user unit installed.

```sh
cargo install --locked --path crates/joyshockrust-tray --root "$HOME/.local"
cargo install --locked --path crates/joyshockrust-editor --root "$HOME/.local"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}"
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm644 dist/systemd/joyshockrust-tray.service "$config_dir/systemd/user/joyshockrust-tray.service"
install -Dm644 dist/desktop/io.github.aapokaapo.JoyShockRust.desktop "$data_dir/applications/io.github.aapokaapo.JoyShockRust.desktop"
systemctl --user daemon-reload
```

Open **JoyShockRust** from the application menu or run
`systemctl --user start joyshockrust-tray.service`. The tray starts the
bridge, shows its current controller, connection, decoded report rate,
and battery state, and shows the charge percentage while discharging.
Use **Active controller** to choose a connected controller with an HID ID,
or select **Automatic**. Place named TOML profiles in
`$config_dir/joyshockrust/profiles/` for the **Profiles** submenu.
The tray shows the active profile's filename, including profiles loaded
from the command line.
**Default mappings** unloads the profile. **Quit JoyShockRust** stops the
bridge and closes the tray.

Use **Profile editor** to open a named profile or choose **New profile**.
The editor follows the system’s light or dark appearance. The page
switcher moves to the bottom when the window is narrow.
Select a physical control on the DualSense diagram or choose a DualSense
Edge control below it. The inspector shows the selected control's tap,
hold and double-tap keyboard or mouse actions, along with its base virtual
gamepad remap when available. Choose **No action** to clear a gesture or
**Same as controller** to remove the base remap. Use the **Advanced**
page for TOML settings such as chords and layers.

In **Touchpad**, set the number of rows and columns, select a grid
cell, and assign separate virtual gamepad buttons to **Touch** and
**Click**. Choose **No action** to clear a mapping. Clear or update the
bindings in any cells you want to remove before shrinking the grid.
The **Keyboard and mouse** groups assign tap, hold and double-tap actions
separately for touch and click.

In **Gyro activation**, choose **Enable while touching** or another mode
to use the selected cell as the profile's gyro input. Choosing a mode in
another cell moves the touch binding. Clearing the binding leaves
configured gyro aim always on. Physical button, analog trigger and chord
gyro bindings can be edited in **Advanced**.

The **Sticks** page previews each virtual stick's response curve. Each
exponent ranges from 0.25 to 4.00 in 0.05 increments. An exponent of 1.00
is linear. A lower exponent increases response near the center. The
profile keys are `stick_curve_left` and `stick_curve_right` and each
falls back to the existing `stick_curve` when omitted. Under each stick,
open **Keyboard mapping** to assign direction keys and configure the
inner and outer zones. Each zone offers **Hold**, **Toggle** and **Keep
sprint**. Set the engage radius as a fraction of full deflection and
leave the release radius empty to use the bridge default. Select
**Update Mapping** to validate the settings and return to the profile,
then **Save & Apply** to load them into the running bridge.

The **Gyro** page has separate horizontal and vertical sensitivity
sliders, from 0 to 100 counts per degree in 0.1 increments. Edit RWC
and in-game sensitivity in the **Game calibration** text fields.
Changing these settings creates a `[gyro]` section if needed, which
enables the profile's gyro mouse.

Open the profile actions menu to **Validate Profile** or **Save** to
disk. **Save & Apply** writes the profile and loads it into the running
bridge. Use Ctrl+S to save or Ctrl+Shift+S to save and apply.

Select **Calibrate game sensitivity (RWC)** while a controller is connected. The tray pauses virtual outputs and opens the Wayland
calibration window. Enter the game's mouse sensitivity and select **Start
Measurement**. Wait for **Ready**, then focus the game, aim at a landmark
and press Options with the right stick centered. The window shows when
measurement starts and whether the stick needs to be centered or more
movement is needed. Sweep the stick around its edge until the camera has
completed one or more whole turns and has returned to the landmark. Center
the stick and press Options again. Return to the calibration window and
enter the number of complete turns. The active profile is preselected if
its file is available. Choose another profile or use **Browse Profiles…**
to find a different file. Select **Save Calibration** to store the result.

Select **Done** to resume the bridge with the same in-memory profile and
controller selection. To apply the saved calibration, select the
updated profile in **Profiles**. If the tray was interrupted and the
bridge is still paused, use **Resume virtual outputs** or run
`joyshockrust-linux --set-outputs resume`.

Check **Start when I log in** to enable desktop login startup. If you
previously enabled the bridge unit directly, run
`systemctl --user disable joyshockrust.service` once so the tray controls
login startup. The tray requires a StatusNotifierItem host. Some desktops,
including GNOME setups, may need an AppIndicator extension.

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
event batches successfully submitted by the gamepad and gyro mouse.
`uinput_keyboard_batches` and `uinput_mouse_button_batches` count batches
from the keyboard and button mouse, including wheel steps. Both include timer-driven events and
releases sent on profile reload. All four counters reset on disconnect.
`controller_sessions_started` and `controller_sessions_ended` count
session transitions since the bridge process started.
The `last_reload` field reports `not_attempted` when a startup profile path
is configured, or `unavailable` when no path has been set. It reports
`applied` or `rejected` after a profile update. To reload the startup file directly, run
`$HOME/.local/bin/joyshockrust-linux --reload-profile`.
The `profile_source` field shows which mappings are active. Its values
are `defaults`, `command_line`, `startup` and `loaded`. A rejected load
leaves the previously active source in effect.
The `active_hidraw` field reports the device path for the active session
and reads `none` while waiting for a controller.
`virtual_gamepad_enabled` shows the effective emulation setting, including
while the controller is disconnected.
`virtual_gamepad_sysfs` shows the current virtual gamepad's Linux sysfs
path when available and reads `none` when the device is absent.
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

## Controller gyroscope calibration

With a controller connected, choose **Calibrate Controller Gyro** from the
tray. Put the controller on a flat, stable surface and select **Start Gyro
Calibration**. Keep it still until the measurement finishes, then select
**Save Gyro Calibration** and **Done**. The bridge resumes and uses the
correction whenever it opens gyro output for that controller. Calibration
is stored by controller ID in `$XDG_CONFIG_HOME/joyshockrust/gyro-calibration/`
or under `~/.config` if XDG_CONFIG_HOME is unset. Cancel before saving to
discard the measurement. A saved correction takes precedence over the
optional session-start stationary calibration.

In the profile editor, open **Gyro → Controller gyro calibration** and
choose **PS button** under **Hold button**. Save and apply the profile.
Hold PS for 1.5 seconds, release it, and leave the controller still. The
new correction applies immediately and is saved for this controller.
The shortcut reads the physical button, and the usual button mapping
still runs. To customize the hold duration in TOML, use:

```toml
[gyro.calibration_hold]
button = "ps"
hold_ms = 1500
```

For advanced haptics, connect the controller by USB and install
`alsa-utils` for `aplay`. The controller's four-channel ALSA playback
device must be available. A single pulse marks the start and a double
pulse marks completion. During RWC measurement, sweeping the stick
produces short tactile detents every 15 degrees of relative stick
rotation. Camera movement remains continuous. Bluetooth calibration
is available with visual status or bridge logs.

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

The command uses the separate controller settings. Append a `/dev/hidrawN` path
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

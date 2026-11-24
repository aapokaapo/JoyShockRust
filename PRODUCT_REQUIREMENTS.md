# Product requirements

## Users and platform

JoyShockRust targets Linux gamers using a DualSense or DualSense Edge.
The primary desktop test environment is Fedora on Wayland with
Steam/Proton. The bridge also runs without the desktop companions when the
user has the required device access.

## Release scope

- Run in the background with a tray control and optional login startup.
  Offer a headless user service and CLI for diagnostics and profile control.
- Use one active physical controller over USB or Bluetooth. On startup,
  prefer the remembered controller, fall back to an eligible device, or
  wait for hotplug. Switch controllers without coupling the choice to the
  active profile.
- Output a virtual gamepad and optional relative mouse or keyboard. Keep
  output lifetime, release events and reconnect behavior predictable.
  Offer Xbox Series X|S compatibility and retain generic and Xbox 360 choices.
- Support controller and keyboard/mouse mappings, including gestures,
  layers, analog trigger stages, touch zones, and per-stick direction keys
  with inner and outer zones. The editor selects one output type for a
  physical button, with independent gestures within that type.
- Offer device-axis gyro aim, gravity-referenced fusion aim, per-axis
  sensitivity and Flick Stick. Guide stationary gyro calibration and
  per-game real-world calibration, with visible feedback and controller
  navigation.
- Let users create, edit and apply TOML profiles. In the tray, show
  controller status and report rate, and battery information on Bluetooth.
  Offer separate gyro mouse and gamepad switches plus a shared master switch.
- Treat rumble, calibration haptics and input visibility as
  transport- or game-dependent compatibility features.

## Acceptance and future scope

The [implementation plan](IMPLEMENTATION_PLAN.md) tracks current work and
release priorities. The [feature matrix](FEATURE_MATRIX.md) shows the
evidence per feature, including where hardware or game tests are still
needed.

Simultaneous active controllers, additional controller families, native
DualSense HID emulation, system-wide device hiding and cross-platform
support are outside the current release scope. Use the
[visibility contract](OUTPUT_AND_VISIBILITY.md) for the per-launch SDL option.

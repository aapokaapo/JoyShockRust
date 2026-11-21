# Product requirements

## Users and platform
Linux gamers using controllers with gyroscopes, especially DualSense Edge over USB on Fedora/Wayland with Steam/Proton. The daemon must also work without a desktop session where permissions permit. GUI comes later.

## P0 (minimum usable product)
- Discover and connect one wired DualSense Edge using hidraw.
- Read and normalize buttons, sticks, triggers, gyroscope and accelerometer with validated reports.
- Perform gyro calibration and persistent profile load/save outside the motion crate.
- Map gyro to relative mouse movement; basic buttons/sticks to virtual output.
- Handle disconnect/reconnect cleanly, without stuck buttons.
- Load TOML profile, provide basic CLI diagnostics and run as a systemd user service.
- Produce deterministic replay tests and timing benchmarks.

## P1
- Advanced mappings: tap/hold/double-tap, layers, chords, trigger thresholds, response curves, gyro gating, flick stick.
- Per-game profiles and reload, USB DualSense, Bluetooth Edge, multi-controller support.
- Virtual Xbox-compatible output where games recognize it; compatibility testing.

## P2
- Other Sony/Nintendo controllers, touchpad, feedback/rumble, battery, additional virtual controller profiles, GUI.

## Out of scope for first release
- Full proprietary DualSense emulation without evidence and interoperability tests.
- Universal physical-controller hiding through environment variables.
- Root-required daemon as default; custom real-time scheduling as default.
- Cross-platform abstractions that compromise the Linux implementation.

## Feature parity
Use `FEATURE_MATRIX.md` to track original JSM behavior, Rust implementation status and test evidence; do not equate conceptual similarity with parity.

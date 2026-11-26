# Pure deterministic motion crate contract

`joyshockrust-motion` must compile and test without Linux-specific dependencies, Tokio, device access, filesystem or IPC.

## Implemented API

`MotionEngine` consumes `ImuSample` values in degrees per second and g.
The caller applies the device's factory conversion to raw counts before
calling motion. A separately supplied stationary gyro bias is subtracted
once. See the [API and example](crates/joyshockrust-motion/src/lib.rs).

Use elapsed nanoseconds within one continuous input segment. The first
sample establishes a baseline. Later samples integrate angular velocity
with the trapezoidal rule. Horizontal aim uses device Y, vertical aim
uses device X, and signed sensitivities control the output direction.
The bounds of 2048 °/s and 4 g per axis are provisional input guards.
Non-finite or out-of-range samples, non-increasing timestamps, and gaps
above the configured limit return errors and preserve the previous state.
The caller ends a segment with `reset_segment`, which preserves the
supplied bias. [Nominal range source reviewed 2026-11-22](https://github.com/torvalds/linux/blob/7d0a66e4bb9081d75c82ec4957c50034cb0ea449/drivers/hid/hid-playstation.c#L166-L170)

Gyro aim uses only the calibrated gyro channels. Sensor fusion aim estimates
gravity by predicting it from gyro rotation and correcting it with
accelerometer direction when the magnitude is near 1 g and it agrees
with the predicted hemisphere. The first usable accelerometer sample
initializes the estimate. Before that, fusion aim uses the device axes.
Fusion projects yaw onto the estimated up axis and pitch onto the
controller's right axis in the horizontal plane. Accelerometer changes
alone produce no pointer delta.

`PointerMotion` uses input's `ImuStream` for timestamp classification and
factory conversion. It applies the selected aim mode, threshold,
acceleration and per-axis sensitivity with game calibration. It composes
the resulting mouse counts with Flick Stick before the Linux adapter
submits relative X/Y events in one uinput batch. Linux supplies the
activation decisions for both paths for every report.

Gyro and Flick Stick each accumulate fractional counts for their own
motion. Flick Stick can skip a span of reports during calibration and
re-anchors using its own last accepted timestamp. Keeping these rules
preserves the report on which each count is emitted. Changing aim mode
clears gyro integration and fractional state while retaining the bias.
The combined replay compares exact per-report counts with the previous
pipeline. [Pointer implementation](crates/joyshockrust-motion/src/pointer.rs)

## Determinism
Identical initial state, configuration and ordered samples must produce identical results on a defined build/numeric environment. No system-clock reads, RNG, I/O, threads or hidden global state. Explicitly define behavior for duplicate/out-of-order timestamps, long gaps, wraparound and invalid samples. Bit-identical cross-architecture floating-point output is not initially promised.

## Calibration
`StationaryCalibration` estimates residual bias from factory-converted samples
while the user keeps the controller still. It needs at least five uninterrupted seconds and
64 samples of stable gyro and accelerometer readings. A gap above 50 ms,
invalid reading or motion outside the quality limits discards the window.
The accepted bias is used until a new calibration is applied. The Linux
bridge can estimate bias at session start when requested and no saved
calibration is available. Guided calibration and the physical hold
shortcut can persist the correction by controller ID. Subsequent sessions
prefer that correction. Continuous automatic bias learning remains future
work. [Linux calibration workflow](README.md#controller-gyroscope-calibration)

## Performance
No steady-state heap allocations. Benchmarks must cover high sample rates and worst-case input sequences; mathematical accuracy and stability are more important than micro-optimizations.

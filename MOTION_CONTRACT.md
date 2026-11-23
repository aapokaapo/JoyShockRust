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

The engine predicts the gravity direction from gyro rotation and blends
in accelerometer direction when its magnitude is near 1 g and it agrees
with the predicted hemisphere. The first usable accelerometer sample
initializes the estimate. The stationary flag uses magnitude and angular
velocity thresholds. It is informational and leaves aim integration intact.

## Planned extensions
1. Validate finite values, timestamp ordering and plausible ranges.
2. Apply static/device calibration and learned gyro bias.
3. Detect stillness using calibrated sensor noise and accelerometer conditions.
4. Update fusion state, gravity estimate and orientation.
5. Transform axes and reference frame; apply sensitivity independently for X/Y.
6. Apply carefully measured smoothing/deadzones and emit motion deltas.
7. Return updated calibration state/proposals without persisting them.

## Determinism
Identical initial state, configuration and ordered samples must produce identical results on a defined build/numeric environment. No system-clock reads, RNG, I/O, threads or hidden global state. Explicitly define behavior for duplicate/out-of-order timestamps, long gaps, wraparound and invalid samples. Bit-identical cross-architecture floating-point output is not initially promised.

## Calibration
`StationaryCalibration` estimates residual bias from factory-converted samples
while the user keeps the controller still. It needs at least one second and
64 samples of stable gyro and accelerometer readings. A gap above 50 ms,
invalid reading or motion outside the quality limits discards the window.
Once accepted, the bias is fixed for the session. The Linux adapter withholds
pointer output during calibration and starts a new window after reconnect.
Bias persistence and bounded auto-bias learning remain planned.

## Performance
No steady-state heap allocations. Benchmarks must cover high sample rates and worst-case input sequences; mathematical accuracy and stability are more important than micro-optimizations.

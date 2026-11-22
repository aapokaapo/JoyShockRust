# Pure deterministic motion crate contract

`joyshockrust-motion` must compile and test without Linux-specific dependencies, Tokio, device access, filesystem or IPC.

## Core API sketch
```rust
pub struct ImuSample {
    pub timestamp_ns: u64,
    pub gyro_dps: [f32; 3],
    pub accel_g: [f32; 3],
}

pub struct MotionConfig {
    pub sensitivity_x: f32,
    pub sensitivity_y: f32,
    pub auto_calibration: bool,
}

pub struct MotionOutput {
    pub aim_delta: [f32; 2],
    pub angular_velocity: [f32; 3],
    pub gravity: [f32; 3],
    pub stationary: bool,
}

pub struct MotionEngine { /* private owned state */ }
// MotionEngine::new(config, calibration) -> Self
// MotionEngine::process(&mut self, sample) -> Result<MotionOutput, MotionError>
```
This sketch describes the future motion API. The first parser passes raw counts to a device-specific conversion stage. That stage uses validated factory coefficients and passes calibrated degrees per second and g to motion. Persisted manual gyro bias belongs to the profile and enters motion as three finite values in degrees per second. Motion applies that bias once and keeps runtime bias estimates in its own state.

Use elapsed nanoseconds within a continuous input segment. The caller forwards only accepted sensor samples for integration and explicitly resets the motion timebase when a new segment starts. The first sample establishes a baseline and produces no integrated aim delta. Proposed sample errors are non-finite values, out-of-range values and non-increasing timestamps. Each leaves the previous accepted motion state intact. A segment reset clears elapsed-time and fusion history while retaining validated calibration. Define the physical range limits alongside device calibration using the captured fixture data.

## Processing stages
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
Support manual stationary calibration with quality gates, robust noise estimation, persistent profile format owned by config crate, and bounded auto-bias learning. Do not allow automatic calibration to erase genuine slow aim movement. Provide synthetic traces for stationary drift, slow intentional motion, fast motion, gravity changes, vibration and report gaps.

## Performance
No steady-state heap allocations. Benchmarks must cover high sample rates and worst-case input sequences; mathematical accuracy and stability are more important than micro-optimizations.

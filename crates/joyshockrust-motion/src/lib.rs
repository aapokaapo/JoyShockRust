//! Deterministic motion processing for calibrated, timestamped IMU samples.
//!
//! ```
//! use joyshockrust_motion::{Calibration, ImuSample, MotionConfig, MotionEngine};
//!
//! let mut motion = MotionEngine::new(MotionConfig::default(), Calibration::default()).unwrap();
//! let mut sample = ImuSample {
//!     timestamp_ns: 0,
//!     gyro_dps: [0.0, 100.0, 0.0],
//!     accel_g: [0.0, 0.0, 1.0],
//! };
//! assert_eq!(motion.process(sample).unwrap().aim_delta, [0.0, 0.0]);
//! sample.timestamp_ns = 10_000_000;
//! assert_eq!(motion.process(sample).unwrap().aim_delta, [1.0, 0.0]);
//! ```

const MAX_GYRO_DPS: f32 = 2048.0;
const MAX_ACCEL_G: f32 = 4.0;
const MAX_BIAS_DPS: f32 = 10.0;
const MAX_SENSITIVITY: f32 = 10_000.0;
const GRAVITY_CORRECTION_SECONDS: f64 = 0.5;

pub mod aim_response;
pub mod game_calibration;
pub mod sensitivity;
mod stationary_calibration;
pub mod stick_response;
pub use stationary_calibration::StationaryCalibration;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AimMode {
    #[default]
    Gyro,
    SensorFusion,
}

impl AimMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gyro => "gyro",
            Self::SensorFusion => "sensor-fusion",
        }
    }
}

impl std::str::FromStr for AimMode {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gyro" => Ok(Self::Gyro),
            "sensor-fusion" => Ok(Self::SensorFusion),
            _ => Err("gyro.aim_mode must be gyro or sensor-fusion"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImuSample {
    pub timestamp_ns: u64,
    pub gyro_dps: [f32; 3],
    pub accel_g: [f32; 3],
}

#[derive(Debug, Clone, Copy)]
pub struct MotionConfig {
    /// Signed horizontal aim units per degree of rotation about device Y.
    pub sensitivity_x: f32,
    /// Signed vertical aim units per degree of rotation about device X.
    pub sensitivity_y: f32,
    /// Largest interval the engine will integrate, in nanoseconds.
    pub max_gap_ns: u64,
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self {
            sensitivity_x: 1.0,
            sensitivity_y: 1.0,
            max_gap_ns: 50_000_000,
        }
    }
}

/// Stationary gyro offset, in degrees per second in device axis order.
#[derive(Debug, Default, Clone, Copy)]
pub struct Calibration {
    pub gyro_bias_dps: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionOutput {
    pub aim_delta: [f32; 2],
    pub angular_velocity: [f32; 3],
    /// Unit vector in the direction of rest acceleration, in device axes.
    pub gravity: Option<[f32; 3]>,
    pub stationary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionError {
    InvalidConfig,
    InvalidCalibration,
    NonFiniteSample,
    SampleOutOfRange,
    NonIncreasingTimestamp,
    Gap { elapsed_ns: u64 },
}

struct PreviousSample {
    timestamp_ns: u64,
    angular_velocity: [f64; 3],
}

struct IntegrationStep {
    dt: f64,
    average_gyro: [f64; 3],
    angular_velocity: [f64; 3],
}

pub struct MotionEngine {
    config: MotionConfig,
    calibration: Calibration,
    previous: Option<PreviousSample>,
    gravity: Option<[f64; 3]>,
}

impl MotionEngine {
    pub fn new(config: MotionConfig, calibration: Calibration) -> Result<Self, MotionError> {
        if config.max_gap_ns == 0
            || config.max_gap_ns > 1_000_000_000
            || ![config.sensitivity_x, config.sensitivity_y]
                .into_iter()
                .all(|value| value.abs() <= MAX_SENSITIVITY)
        {
            return Err(MotionError::InvalidConfig);
        }
        if !in_range(calibration.gyro_bias_dps, MAX_BIAS_DPS) {
            return Err(MotionError::InvalidCalibration);
        }
        Ok(Self {
            config,
            calibration,
            previous: None,
            gravity: None,
        })
    }

    /// Stationary bias currently applied to the calibrated gyro.
    pub fn calibration(&self) -> Calibration {
        self.calibration
    }

    /// Clear time and gravity history while retaining the supplied gyro bias.
    pub fn reset_segment(&mut self) {
        self.previous = None;
        self.gravity = None;
    }

    /// Process one sample. An error preserves the last accepted state.
    /// For a new device-time segment, call [`Self::reset_segment`] before retrying.
    pub fn process(&mut self, sample: ImuSample) -> Result<MotionOutput, MotionError> {
        if !sample
            .gyro_dps
            .into_iter()
            .chain(sample.accel_g)
            .all(f32::is_finite)
        {
            return Err(MotionError::NonFiniteSample);
        }
        if !in_range(sample.gyro_dps, MAX_GYRO_DPS) || !in_range(sample.accel_g, MAX_ACCEL_G) {
            return Err(MotionError::SampleOutOfRange);
        }

        let step = self.integrate(sample.timestamp_ns, sample.gyro_dps)?;
        let magnitude = self.update_gravity(&step, Some(sample.accel_g));
        Ok(MotionOutput {
            aim_delta: self.device_aim(&step),
            angular_velocity: step.angular_velocity.map(|a| a as f32),
            gravity: self.gravity.map(|gravity| gravity.map(|a| a as f32)),
            stationary: (0.95..=1.05).contains(&magnitude) && length(step.angular_velocity) <= 0.1,
        })
    }

    /// Integrate calibrated gyro rates in the controller's own axes.
    pub fn process_gyro_aim(
        &mut self,
        timestamp_ns: u64,
        gyro_dps: [f32; 3],
    ) -> Result<[f32; 2], MotionError> {
        let step = self.integrate(timestamp_ns, gyro_dps)?;
        Ok(self.device_aim(&step))
    }

    /// Frame gyro aim using gravity estimated from gyro and accelerometer.
    /// Uses device axes until the first trustworthy gravity measurement.
    pub fn process_fused_aim(&mut self, sample: ImuSample) -> Result<[f32; 2], MotionError> {
        let step = self.integrate(sample.timestamp_ns, sample.gyro_dps)?;
        let previous_gravity = self.gravity;
        let accel = in_range(sample.accel_g, MAX_ACCEL_G).then_some(sample.accel_g);
        self.update_gravity(&step, accel);
        let Some(gravity) = self.gravity else {
            return Ok(self.device_aim(&step));
        };
        let gravity = previous_gravity
            .and_then(|previous| normalized(std::array::from_fn(|i| previous[i] + gravity[i])))
            .unwrap_or(gravity);
        let yaw = dot(step.average_gyro, gravity);
        // Project onto the controller's right axis in the horizontal plane.
        // The pitch contribution fades as this axis approaches vertical.
        let pitch = step.average_gyro[0] - gravity[0] * yaw;
        Ok(self.scale_aim(&step, [yaw, pitch]))
    }

    fn device_aim(&self, step: &IntegrationStep) -> [f32; 2] {
        self.scale_aim(step, [step.average_gyro[1], step.average_gyro[0]])
    }

    fn scale_aim(&self, step: &IntegrationStep, rates: [f64; 2]) -> [f32; 2] {
        let sensitivity = [self.config.sensitivity_x, self.config.sensitivity_y];
        std::array::from_fn(|i| (rates[i] * step.dt * f64::from(sensitivity[i])) as f32)
    }

    fn integrate(
        &mut self,
        timestamp_ns: u64,
        gyro_dps: [f32; 3],
    ) -> Result<IntegrationStep, MotionError> {
        if !gyro_dps.into_iter().all(f32::is_finite) {
            return Err(MotionError::NonFiniteSample);
        }
        if !in_range(gyro_dps, MAX_GYRO_DPS) {
            return Err(MotionError::SampleOutOfRange);
        }
        let angular_velocity = std::array::from_fn(|axis| {
            f64::from(gyro_dps[axis]) - f64::from(self.calibration.gyro_bias_dps[axis])
        });
        let (dt, average_gyro) = if let Some(previous) = &self.previous {
            let elapsed_ns = timestamp_ns
                .checked_sub(previous.timestamp_ns)
                .filter(|&elapsed| elapsed > 0)
                .ok_or(MotionError::NonIncreasingTimestamp)?;
            if elapsed_ns > self.config.max_gap_ns {
                return Err(MotionError::Gap { elapsed_ns });
            }
            let average = std::array::from_fn(|axis| {
                (angular_velocity[axis] + previous.angular_velocity[axis]) * 0.5
            });
            (elapsed_ns as f64 * 1e-9, average)
        } else {
            (0.0, angular_velocity)
        };

        self.previous = Some(PreviousSample {
            timestamp_ns,
            angular_velocity,
        });
        Ok(IntegrationStep {
            dt,
            average_gyro,
            angular_velocity,
        })
    }

    fn update_gravity(&mut self, step: &IntegrationStep, accel: Option<[f32; 3]>) -> f64 {
        let measured = accel.unwrap_or_default().map(f64::from);
        let magnitude = length(measured);
        let measured_gravity = (0.9..=1.1)
            .contains(&magnitude)
            .then(|| measured.map(|a| a / magnitude));
        self.gravity = match self.gravity {
            Some(gravity) => {
                let predicted = rotate_into_device_frame(gravity, step.average_gyro, step.dt);
                if let Some(measured) =
                    measured_gravity.filter(|&measured| dot(predicted, measured) > 0.0)
                {
                    let gain = -(-step.dt / GRAVITY_CORRECTION_SECONDS).exp_m1();
                    let corrected =
                        std::array::from_fn(|i| predicted[i] + gain * (measured[i] - predicted[i]));
                    let norm = length(corrected);
                    Some(corrected.map(|a| a / norm))
                } else {
                    Some(predicted)
                }
            }
            None => measured_gravity,
        };
        magnitude
    }
}

fn in_range(values: [f32; 3], max: f32) -> bool {
    values.into_iter().all(|value| value.abs() <= max)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(vector: [f64; 3]) -> f64 {
    dot(vector, vector).sqrt()
}

fn normalized(vector: [f64; 3]) -> Option<[f64; 3]> {
    let norm = length(vector);
    (norm > 1e-6).then(|| vector.map(|value| value / norm))
}

fn rotate_into_device_frame(gravity: [f64; 3], gyro_dps: [f64; 3], dt: f64) -> [f64; 3] {
    let speed = length(gyro_dps);
    if speed == 0.0 || dt == 0.0 {
        return gravity;
    }
    let [x, y, z] = gyro_dps.map(|velocity| velocity / speed);
    let [gx, gy, gz] = gravity;
    let cross = [y * gz - z * gy, z * gx - x * gz, x * gy - y * gx];
    let (sin, cos) = (-speed.to_radians() * dt).sin_cos();
    let axis = [x, y, z];
    let parallel = dot(axis, gravity);
    std::array::from_fn(|i| gravity[i] * cos + cross[i] * sin + axis[i] * parallel * (1.0 - cos))
}

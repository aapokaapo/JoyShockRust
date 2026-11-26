//! Profile sensitivity in counts per degree before game calibration.

pub const DEFAULT_GYRO_SENSITIVITY: f32 = 4.0;
pub const MAX_GYRO_SENSITIVITY: f32 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GyroSensitivity([f32; 2]);

impl Default for GyroSensitivity {
    fn default() -> Self {
        Self([DEFAULT_GYRO_SENSITIVITY; 2])
    }
}

impl GyroSensitivity {
    pub fn axes(self) -> [f32; 2] {
        self.0
    }

    pub fn from_profile(x: Option<f32>, y: Option<f32>) -> Result<Self, &'static str> {
        let [default_x, default_y] = Self::default().0;
        let scales = [x.unwrap_or(default_x), y.unwrap_or(default_y)];
        if !(0.0..=MAX_GYRO_SENSITIVITY).contains(&scales[0]) {
            return Err("gyro.sensitivity_x must be between 0 and 100 counts per degree");
        }
        if !(0.0..=MAX_GYRO_SENSITIVITY).contains(&scales[1]) {
            return Err("gyro.sensitivity_y must be between 0 and 100 counts per degree");
        }
        Ok(Self(scales))
    }
}

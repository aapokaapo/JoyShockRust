use std::time::Duration;

use super::{Calibration, ImuSample, length};

const MAX_GAP_NS: u64 = 50_000_000;
const MIN_SAMPLES: u32 = 64;

/// Estimate a session's residual gyro bias while the user holds the
/// controller stationary. Input has already had factory calibration applied.
#[derive(Default)]
pub struct StationaryCalibration {
    window: Option<Window>,
}

struct Window {
    first: ImuSample,
    last_ns: u64,
    samples: u32,
    mean: [f64; 3],
}

impl StationaryCalibration {
    pub const SAMPLE_DURATION: Duration = Duration::from_secs(5);

    /// Discard the current window at a device-clock discontinuity.
    pub fn reset(&mut self) {
        self.window = None;
    }

    /// Return a bias after the full window of consistent samples.
    /// Motion, invalid input and interrupted timing restart the window.
    pub fn observe(&mut self, sample: ImuSample) -> Option<Calibration> {
        if !(0.9..=1.1).contains(&length(sample.accel_g.map(f64::from)))
            || !(0.0..=3.0).contains(&length(sample.gyro_dps.map(f64::from)))
        {
            self.reset();
            return None;
        }
        if let Some(window) = &mut self.window {
            let gap = sample.timestamp_ns.checked_sub(window.last_ns);
            if !gap.is_some_and(|gap| (1..=MAX_GAP_NS).contains(&gap))
                || distance(sample.accel_g, window.first.accel_g) > 0.06
                || distance(sample.gyro_dps, window.first.gyro_dps) > 1.0
            {
                self.reset();
                return None;
            }
            window.last_ns = sample.timestamp_ns;
            window.samples += 1;
            for (mean, reading) in window.mean.iter_mut().zip(sample.gyro_dps) {
                *mean += (f64::from(reading) - *mean) / f64::from(window.samples);
            }
            if window.samples >= MIN_SAMPLES
                && Duration::from_nanos(sample.timestamp_ns - window.first.timestamp_ns)
                    >= Self::SAMPLE_DURATION
            {
                let bias = window.mean.map(|axis| axis as f32);
                self.reset();
                return Some(Calibration {
                    gyro_bias_dps: bias,
                });
            }
        } else {
            self.window = Some(Window {
                first: sample,
                last_ns: sample.timestamp_ns,
                samples: 1,
                mean: sample.gyro_dps.map(f64::from),
            });
        }
        None
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f64 {
    length(std::array::from_fn(|i| f64::from(a[i]) - f64::from(b[i])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resting(ms: u64) -> ImuSample {
        ImuSample {
            timestamp_ns: ms * 1_000_000,
            gyro_dps: [0.5, -0.25, 0.125],
            accel_g: [0.0, 1.0, 0.0],
        }
    }

    #[test]
    fn averages_noise_over_a_full_stationary_window() {
        let mut calibration = StationaryCalibration::default();
        for index in 0..500 {
            let mut sample = resting(index * 10);
            sample.gyro_dps[0] += if index % 2 == 0 { 0.125 } else { -0.125 };
            assert!(calibration.observe(sample).is_none());
        }
        assert_eq!(
            calibration.observe(resting(5000)).unwrap().gyro_bias_dps,
            resting(0).gyro_dps,
        );
    }

    #[test]
    fn gaps_motion_and_invalid_samples_discard_the_incomplete_window() {
        for interruption in [
            resting(4900), // repeated timestamp
            resting(4970), // gap
            ImuSample {
                gyro_dps: [3.5, 0.0, 0.0],
                ..resting(4910)
            },
            ImuSample {
                gyro_dps: [2.0, 0.0, 0.0],
                ..resting(4910)
            },
            ImuSample {
                accel_g: [0.0, 0.8, 0.6],
                ..resting(4910)
            },
            ImuSample {
                accel_g: [f32::NAN, 1.0, 0.0],
                ..resting(4910)
            },
        ] {
            let mut calibration = StationaryCalibration::default();
            for ms in (0..=4900).step_by(10) {
                assert!(calibration.observe(resting(ms)).is_none());
            }
            assert!(calibration.observe(interruption).is_none());
            for ms in (5000..10000).step_by(10) {
                assert!(calibration.observe(resting(ms)).is_none());
            }
            assert!(calibration.observe(resting(10000)).is_some());
        }
    }
}

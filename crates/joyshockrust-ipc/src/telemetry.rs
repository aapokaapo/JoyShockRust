//! Value codecs for the existing status socket fields.

use std::fmt::{self, Display, Formatter};

pub mod preview;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImuReading {
    pub gyro_dps: [f32; 3],
    pub accel_g: [f32; 3],
}

impl ImuReading {
    pub fn parse(value: &str) -> Option<Self> {
        let mut fields = value.split(',');
        let mut values = [0.0; 6];
        for item in &mut values {
            *item = fields
                .next()?
                .parse::<f32>()
                .ok()
                .filter(|x| x.is_finite())?;
        }
        let [gx, gy, gz, ax, ay, az] = values;
        fields.next().is_none().then_some(Self {
            gyro_dps: [gx, gy, gz],
            accel_g: [ax, ay, az],
        })
    }
}

impl Display for ImuReading {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let [gx, gy, gz] = self.gyro_dps;
        let [ax, ay, az] = self.accel_g;
        write!(f, "{gx:.4},{gy:.4},{gz:.4},{ax:.4},{ay:.4},{az:.4}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GyroSpeed(pub f64);

impl GyroSpeed {
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.parse::<f64>().ok()?;
        (value.is_finite() && value >= 0.0).then_some(Self(value))
    }
}

impl Display for GyroSpeed {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4}", self.0)
    }
}

fn parse_pair(value: &str, max: f64) -> Option<[f64; 2]> {
    let (left, right) = value.split_once(',')?;
    let pair = [left.parse().ok()?, right.parse().ok()?];
    pair.iter().all(|x| (0.0..=max).contains(x)).then_some(pair)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickTravel(pub [f64; 2]);

impl StickTravel {
    pub fn from_sticks(sticks: [joyshockrust_core::Stick; 2]) -> Self {
        Self(sticks.map(|stick| 100.0 * f64::from(stick.x).hypot(f64::from(stick.y)).min(1.0)))
    }

    pub fn parse(value: &str) -> Option<Self> {
        parse_pair(value, 100.0).map(Self)
    }
}

impl Display for StickTravel {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let [left, right] = self.0;
        write!(f, "{left:.4},{right:.4}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriggerTravel(pub [f64; 2]);

impl TriggerTravel {
    pub fn parse(value: &str) -> Option<Self> {
        parse_pair(value, 1.0).map(Self)
    }
}

impl Display for TriggerTravel {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let [left, right] = self.0;
        write!(f, "{left:.6},{right:.6}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchPoints(pub [Option<(u16, u16)>; 2]);

impl TouchPoints {
    pub fn parse(value: &str) -> Option<Self> {
        let mut points = [None; 2];
        if !value.is_empty() {
            for (index, point) in value.split(';').enumerate() {
                let (x, y) = point.split_once(',')?;
                *points.get_mut(index)? = Some((x.parse().ok()?, y.parse().ok()?));
            }
        }
        Some(Self(points))
    }
}

impl Display for TouchPoints {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (index, (x, y)) in self.0.into_iter().flatten().enumerate() {
            if index != 0 {
                f.write_str(";")?;
            }
            write!(f, "{x},{y}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_preserve_wire_units_precision_and_contact_order() {
        let imu = ImuReading::parse("-1,2,3,0,1.01,-0.5").unwrap();
        assert_eq!(imu.gyro_dps, [-1.0, 2.0, 3.0]);
        assert_eq!(
            imu.to_string(),
            "-1.0000,2.0000,3.0000,0.0000,1.0100,-0.5000"
        );
        assert_eq!(GyroSpeed::parse("12.04").unwrap().to_string(), "12.0400");
        assert_eq!(
            StickTravel::parse("50,100").unwrap().to_string(),
            "50.0000,100.0000"
        );
        assert_eq!(
            TriggerTravel::parse("0.5,1").unwrap().to_string(),
            "0.500000,1.000000"
        );
        assert_eq!(
            TouchPoints::parse("960,540;0,1079").unwrap().to_string(),
            "960,540;0,1079"
        );
        assert_eq!(TouchPoints::parse(""), Some(TouchPoints([None; 2])));
        assert_eq!(TouchPoints([None, Some((960, 540))]).to_string(), "960,540");
    }

    #[test]
    fn rejects_malformed_telemetry() {
        for invalid in ["1,2,3", "1,2,3,4,5,6,7", "NaN,2,3,4,5,6"] {
            assert!(ImuReading::parse(invalid).is_none());
        }
        assert!(GyroSpeed::parse("inf").is_none());
        assert!(StickTravel::parse("101,0").is_none());
        assert!(TriggerTravel::parse("0,1.01").is_none());
        assert!(TouchPoints::parse("1,2;3,4;5,6").is_none());
    }
}

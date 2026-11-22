use std::{
    error::Error,
    ffi::OsString,
    io::{self, IsTerminal},
    thread,
    time::Duration,
};

use evdev::{EventType, InputEvent, RelativeAxisCode};

use crate::gyro_mouse;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameCalibration(f64);

impl Default for GameCalibration {
    fn default() -> Self {
        Self(1.0)
    }
}

impl GameCalibration {
    pub fn new(real_world_calibration: f64, in_game_sens: f64) -> Result<Self, &'static str> {
        if !positive(real_world_calibration) {
            return Err("gyro.real_world_calibration must be finite and greater than zero");
        }
        if !positive(in_game_sens) {
            return Err("gyro.in_game_sens must be finite and greater than zero");
        }
        let factor = real_world_calibration / in_game_sens;
        // Keep effective pointer scales representable even at the maximum
        // gyro and Flick Stick sensitivities and a large angle per report.
        if !positive(factor) || factor > 10_000.0 {
            return Err(
                "real_world_calibration / in_game_sens must be greater than 0 and at most 10000",
            );
        }
        Ok(Self(factor))
    }

    pub fn scale(self, sensitivity: f64) -> f64 {
        sensitivity * self.0
    }
}

fn positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn calculate(counts: i32, in_game_sens: f64, turns: f64) -> Result<f64, &'static str> {
    if !positive(turns) {
        return Err("measured turns must be finite and greater than zero");
    }
    let real_world_calibration = (f64::from(counts) / 360.0 / turns) * in_game_sens;
    GameCalibration::new(real_world_calibration, in_game_sens)?;
    Ok(real_world_calibration)
}

pub fn run(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    const USAGE: &str =
        "usage: joyshockrust-linux --calibrate-game COUNTS IN_GAME_SENS [MEASURED_TURNS]";
    let [Some(counts), Some(in_game_sens), turns, None] =
        [args.next(), args.next(), args.next(), args.next()]
    else {
        return Err(USAGE.into());
    };
    let counts: i32 = counts.to_str().ok_or("invalid mouse count")?.parse()?;
    if !(1..=1_000_000).contains(&counts) {
        return Err("mouse count must be between 1 and 1000000".into());
    }
    let in_game_sens: f64 = in_game_sens
        .to_str()
        .ok_or("invalid in-game sensitivity")?
        .parse()?;
    if !positive(in_game_sens) {
        return Err("in-game sensitivity must be finite and greater than zero".into());
    }
    let turns = match turns {
        Some(turns) => turns.to_str().ok_or("invalid measured turns")?.parse()?,
        None => measure_turns(counts)?,
    };
    let calibration = calculate(counts, in_game_sens, turns)?;
    println!("real_world_calibration = {calibration:?}\nin_game_sens = {in_game_sens:?}");
    Ok(())
}

fn measure_turns(counts: i32) -> Result<f64, Box<dyn Error>> {
    let input = io::stdin();
    if !input.is_terminal() {
        return Err("pass MEASURED_TURNS when running without an interactive terminal".into());
    }
    let mut output = gyro_mouse::open_output("JoyShockRust Game Calibration Mouse")?;
    eprintln!("Focus the game and aim at a landmark. The turn starts in 5 seconds.");
    thread::sleep(Duration::from_secs(5));
    for delta in movement_steps(counts) {
        if delta != 0 {
            output.emit(&[InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_X.0,
                delta,
            )])?;
        }
        thread::sleep(Duration::from_millis(10));
    }
    eprintln!(
        "Sent {counts} horizontal counts. Return here and enter the measured turns, for example 1.25."
    );
    let mut turns = String::new();
    input.read_line(&mut turns)?;
    Ok(turns.trim().parse()?)
}

fn movement_steps(counts: i32) -> impl Iterator<Item = i32> {
    (1..=100).map(move |step| counts * step / 100 - counts * (step - 1) / 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_recovers_the_game_factor_for_different_turn_counts() {
        let calibration = calculate(9000, 2.0, 1.25).unwrap();
        assert!((calibration - 40.0).abs() < 1e-10);
        let effective = GameCalibration::new(calibration, 2.0).unwrap();
        assert!((effective.scale(1.0) - 20.0).abs() < 1e-10);
        assert!((effective.scale(3.0) - 60.0).abs() < 1e-10);
        assert!(calculate(9000, 2.0, 0.0).is_err());
        assert!(GameCalibration::new(1.0, f64::INFINITY).is_err());
    }

    #[test]
    fn movement_preserves_exact_counts_for_small_and_large_turns() {
        for counts in [1, 37, 100, 14_401, 1_000_000] {
            let steps: Vec<_> = movement_steps(counts).collect();
            assert_eq!(steps.iter().sum::<i32>(), counts);
            assert!(
                steps
                    .iter()
                    .all(|&delta| (0..=(counts as u32).div_ceil(100) as i32).contains(&delta))
            );
        }
    }
}

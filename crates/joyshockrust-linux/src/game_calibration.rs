use std::{
    error::Error,
    ffi::OsString,
    io::{self, IsTerminal},
    ops::ControlFlow,
    path::Path,
    thread,
    time::Duration,
};

use evdev::{EventType, InputEvent, RelativeAxisCode};

use joyshockrust_core::{Button, Stick};

use crate::{
    SessionEvent, discovery, flick_stick::FlickStick, forward_reports, gyro_mouse, profile,
};

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

pub fn run(args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let mut args = args.peekable();
    if args.next_if(|arg| arg == "--profile").is_some() {
        let profile = args
            .next()
            .ok_or("expected a profile path after --profile")?;
        let hidraw = args.next();
        if args.next().is_some() {
            return Err(
                "usage: joyshockrust-linux --calibrate-game --profile PATH [/dev/hidrawN]".into(),
            );
        }
        return calibrate_with_stick(Path::new(&profile), hidraw.as_deref().map(Path::new));
    }
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

fn calibrate_with_stick(path: &Path, hidraw: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let path = path.canonicalize()?;
    let settings = profile::load(&path)?;
    let input = io::stdin();
    if !input.is_terminal() {
        return Err("stick calibration requires an interactive terminal".into());
    }

    let in_game_sens = prompt_value(
        "Enter the game's mouse sensitivity greater than zero.",
        |text| text.parse::<f64>().ok().filter(|&value| positive(value)),
    )?;

    let filter = settings.bridge.unwrap_or_default().controller_filter();
    let (mut controller, identity) = match hidraw {
        Some(path) => discovery::open_controller(path, filter)?,
        None => discovery::open_first_controller(filter)?,
    };
    let mut output = gyro_mouse::open_output("JoyShockRust Game Calibration Mouse")?;
    let mut sweep = StickSweep::new((40.0 / in_game_sens).clamp(0.25, 10_000.0));
    let mut measured_counts = None;
    eprintln!("Focus the game, aim at a landmark and press Options with the stick centered.");
    eprintln!(
        "Tilt the right stick up and rotate it until the camera completes one or more whole turns."
    );
    eprintln!("Count the turns. At the landmark, center the stick and press Options again.");

    forward_reports(&mut controller, identity, None, None, None, None, |event| {
        let SessionEvent::Report(report) = event else {
            return Ok(ControlFlow::Continue(()));
        };
        let (delta, confirmation) = sweep.advance(
            report.controls.right_stick,
            report.imu.device_ticks,
            report.controls.buttons.contains(Button::Start),
        );
        if delta != 0 {
            output.emit(&[InputEvent::new(
                EventType::RELATIVE.0,
                RelativeAxisCode::REL_X.0,
                delta,
            )])?;
        }
        match confirmation {
            Confirmation::None => {}
            Confirmation::Started => eprintln!("Measuring the camera turn."),
            Confirmation::CenterStick => eprintln!("Center the right stick, then press Options."),
            Confirmation::NoMotion => {
                eprintln!("No horizontal movement recorded. Turn the camera before confirming.")
            }
            Confirmation::Finished(counts) => {
                measured_counts = Some(counts);
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    })?;
    drop(output);
    let counts = i32::try_from(
        measured_counts.ok_or("controller disconnected before calibration finished")?,
    )?;
    let turns = prompt_value(
        "Return to this terminal and enter the number of complete turns.",
        |text| text.parse::<u32>().ok().filter(|&turns| turns > 0),
    )?;
    let calibration = calculate(counts, in_game_sens, f64::from(turns))?;
    println!("real_world_calibration = {calibration:?}\nin_game_sens = {in_game_sens:?}");
    profile::save_game_calibration(&path, calibration, in_game_sens)?;
    println!("Saved calibration to {}", path.display());
    Ok(())
}

fn prompt_value<T>(prompt: &str, parse: impl Fn(&str) -> Option<T>) -> io::Result<T> {
    loop {
        eprintln!("{prompt}");
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "calibration cancelled",
            ));
        }
        if let Some(value) = parse(input.trim()) {
            return Ok(value);
        }
        eprintln!("Invalid number. Try again.");
    }
}

struct StickSweep {
    flick: FlickStick,
    measuring: bool,
    options_pressed: bool,
    counts: i64,
}

#[derive(Debug, PartialEq, Eq)]
enum Confirmation {
    None,
    Started,
    CenterStick,
    NoMotion,
    Finished(u64),
}

impl StickSweep {
    fn new(scale: f64) -> Self {
        Self {
            flick: FlickStick::new(scale),
            measuring: false,
            options_pressed: true,
            counts: 0,
        }
    }

    fn advance(&mut self, stick: Stick, ticks: u32, options: bool) -> (i32, Confirmation) {
        let pressed = options && !self.options_pressed;
        self.options_pressed = options;
        let delta = if self.measuring {
            self.flick.advance(stick, ticks)
        } else {
            0
        };
        self.counts += i64::from(delta);
        let confirmation = if !pressed {
            Confirmation::None
        } else if !self.flick.is_settled(stick) {
            Confirmation::CenterStick
        } else if !self.measuring {
            self.measuring = true;
            self.flick.advance(stick, ticks);
            Confirmation::Started
        } else if self.counts == 0 {
            Confirmation::NoMotion
        } else {
            Confirmation::Finished(self.counts.unsigned_abs())
        };
        (delta, confirmation)
    }
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
    fn stick_measurement_sums_two_full_rotations_and_uses_whole_turns() {
        let up = Stick { x: 0.0, y: -1.0 };
        let right = Stick { x: 1.0, y: 0.0 };
        let down = Stick { x: 0.0, y: 1.0 };
        let left = Stick { x: -1.0, y: 0.0 };
        let center = Stick::default();
        let mut sweep = StickSweep::new(20.0);
        let mut ticks = 0;
        let mut advance = |stick, options| {
            ticks += 30_000;
            sweep.advance(stick, ticks, options)
        };

        assert_eq!(advance(up, true), (0, Confirmation::None));
        advance(center, false);
        assert_eq!(advance(center, true), (0, Confirmation::Started));
        assert_eq!(advance(up, true), (0, Confirmation::None));
        for direction in [right, down, left, up].into_iter().cycle().take(8) {
            assert_eq!(advance(direction, false), (1800, Confirmation::None));
        }
        assert_eq!(advance(up, true), (0, Confirmation::CenterStick));
        for _ in 0..3 {
            advance(center, false);
        }
        let (delta, confirmation) = advance(center, true);
        assert_eq!(delta, 0);
        assert_eq!(confirmation, Confirmation::Finished(14_400));
        assert_eq!(calculate(14_400, 2.0, 2.0).unwrap(), 40.0);
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

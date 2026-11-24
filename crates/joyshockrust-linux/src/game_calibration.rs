use std::{
    error::Error,
    ffi::OsString,
    io::{self, IsTerminal, Write},
    ops::ControlFlow,
    path::Path,
    thread,
    time::Duration,
};

use evdev::{EventType, InputEvent, RelativeAxisCode};

use joyshockrust_core::{Button, Stick};
use joyshockrust_motion::aim_response::validate_flick_axial_threshold;

use crate::{
    SessionEvent, discovery,
    flick_stick::FlickStick,
    forward_reports, gyro_mouse,
    haptics::{Cue, Haptics},
    profile,
};

pub use joyshockrust_motion::game_calibration::GameCalibration;
use joyshockrust_motion::game_calibration::{calculate, positive};

pub fn run(args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let mut args = args.peekable();
    if args.next_if(|arg| arg == "--measure-stick").is_some() {
        let sensitivity = parse_arg(args.next(), "invalid in-game sensitivity")?;
        let hidraw = args.next().ok_or("expected a controller node")?;
        let parent: i32 = parse_arg(args.next(), "invalid calibration window process ID")?;
        // Older editors supplied the profile's threshold. Keep accepting the
        // argument while measuring with the physical angle in all cases.
        if let Some(arg) = args.next() {
            validate_flick_axial_threshold(parse_arg(
                Some(arg),
                "invalid Flick Stick axial threshold",
            )?)?;
        }
        if args.next().is_some() || !positive(sensitivity) || parent <= 1 {
            return Err(
                "expected sensitivity, controller node and calibration window process ID".into(),
            );
        }
        crate::gyro_calibration::guard_window_process(parent)?;
        let mut stdout = io::stdout().lock();
        let counts = measure_with_stick(sensitivity, Some(Path::new(&hidraw)), |progress| {
            writeln!(stdout, "{}", progress.token())?;
            stdout.flush()
        })?;
        writeln!(stdout, "counts {counts}")?;
        return Ok(());
    }
    if args.next_if(|arg| arg == "--save-measurement").is_some() {
        let path = args.next().ok_or("expected a profile path")?;
        let counts: i32 = parse_arg(args.next(), "invalid mouse count")?;
        let sensitivity = parse_arg(args.next(), "invalid in-game sensitivity")?;
        let turns: u32 = parse_arg(args.next(), "invalid whole turn count")?;
        if args.next().is_some() {
            return Err("expected profile, mouse count, sensitivity and whole turns".into());
        }
        let calibration = save_measurement(Path::new(&path), counts, sensitivity, turns)?;
        println!("{calibration:?}");
        return Ok(());
    }
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

fn parse_arg<T: std::str::FromStr>(
    arg: Option<OsString>,
    error: &'static str,
) -> Result<T, &'static str> {
    arg.and_then(|arg| arg.to_str()?.parse().ok()).ok_or(error)
}

fn save_measurement(
    path: &Path,
    counts: i32,
    sensitivity: f64,
    turns: u32,
) -> Result<f64, Box<dyn Error>> {
    let calibration = calculate(counts, sensitivity, f64::from(turns))?;
    profile::save_game_calibration(&path.canonicalize()?, calibration, sensitivity)?;
    Ok(calibration)
}

fn calibrate_with_stick(path: &Path, hidraw: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let path = path.canonicalize()?;
    profile::load(&path)?;
    let input = io::stdin();
    if !input.is_terminal() {
        return Err("stick calibration requires an interactive terminal".into());
    }

    let in_game_sens = prompt_value(
        "Enter the game's mouse sensitivity greater than zero.",
        |text| text.parse::<f64>().ok().filter(|&value| positive(value)),
    )?;

    let counts = measure_with_stick(in_game_sens, hidraw, |progress| {
        match progress {
            Progress::Ready => {
                eprintln!(
                    "Focus the game, aim at a landmark and press Options with the stick centered."
                );
                eprintln!(
                    "Tilt the right stick up and rotate it until the camera completes one or more whole turns."
                );
                eprintln!(
                    "Count the turns. At the landmark, center the stick and press Options again."
                );
            }
            Progress::Started => eprintln!("Measuring the camera turn."),
            Progress::CenterStick => eprintln!("Center the right stick, then press Options."),
            Progress::NoMotion => {
                eprintln!("No horizontal movement recorded. Turn the camera before confirming.")
            }
        }
        Ok(())
    })?;
    let turns = prompt_value(
        "Return to this terminal and enter the number of complete turns.",
        |text| text.parse::<u32>().ok().filter(|&turns| turns > 0),
    )?;
    let calibration = save_measurement(&path, counts, in_game_sens, turns)?;
    println!("real_world_calibration = {calibration:?}\nin_game_sens = {in_game_sens:?}");
    println!("Saved calibration to {}", path.display());
    Ok(())
}

#[derive(Clone, Copy)]
enum Progress {
    Ready,
    Started,
    CenterStick,
    NoMotion,
}

impl Progress {
    fn token(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Started => "started",
            Self::CenterStick => "center-stick",
            Self::NoMotion => "no-motion",
        }
    }
}

fn measure_with_stick(
    in_game_sens: f64,
    hidraw: Option<&Path>,
    mut progress: impl FnMut(Progress) -> io::Result<()>,
) -> Result<i32, Box<dyn Error>> {
    let config = crate::controller_config::ControllerConfig::load();
    let filter = config.filter();
    let (mut controller, identity) = match hidraw {
        Some(path) => discovery::open_controller(path, filter)?,
        None => {
            let (file, candidate) = discovery::open_first_controller(filter, config.preferred())?;
            (file, candidate.identity)
        }
    };
    let mut output = gyro_mouse::open_output("JoyShockRust Game Calibration Mouse")?;
    let haptics = Haptics::open(&controller, identity.transport);
    let mut sweep = StickSweep::new((40.0 / in_game_sens).clamp(0.25, 10_000.0));
    let mut measured_counts = None;
    progress(Progress::Ready)?;

    forward_reports(
        &mut controller,
        identity,
        None,
        None,
        None,
        crate::ReportTimers::default(),
        |event| {
            let SessionEvent::Report(report) = event else {
                return Ok(ControlFlow::Continue(()));
            };
            let (delta, confirmation, detent) = sweep.advance(
                report.controls.right_stick,
                report.imu.device_ticks,
                report.controls.buttons.contains(Button::Start),
            );
            if detent && let Some(haptics) = &haptics {
                haptics.send(Cue::Detent);
            }
            if delta != 0 {
                output.emit(&[InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_X.0,
                    delta,
                )])?;
            }
            match confirmation {
                Confirmation::None => {}
                Confirmation::Started => {
                    if let Some(haptics) = &haptics {
                        haptics.send(Cue::CalibrationStart);
                    }
                    progress(Progress::Started)?;
                }
                Confirmation::CenterStick => progress(Progress::CenterStick)?,
                Confirmation::NoMotion => progress(Progress::NoMotion)?,
                Confirmation::Finished(counts) => {
                    measured_counts = Some(counts);
                    return Ok(ControlFlow::Break(()));
                }
            }
            Ok(ControlFlow::Continue(()))
        },
    )?;
    let counts = measured_counts.ok_or("controller disconnected before calibration finished")?;
    if let Some(haptics) = haptics {
        haptics.finish(Cue::CalibrationDone);
    }
    Ok(i32::try_from(counts)?)
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
    dial: Dial,
}

#[derive(Default)]
pub(crate) struct Dial {
    travel: f64,
}

impl Dial {
    pub(crate) fn rotate(&mut self, degrees: Option<f64>) -> bool {
        const STEP: f64 = 15.0;
        let Some(degrees) = degrees else {
            self.travel = 0.0;
            return false;
        };
        self.travel += degrees;
        if self.travel.abs() >= STEP {
            self.travel %= STEP;
            return true;
        }
        false
    }
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
            flick: FlickStick::new(scale)
                .with_axial_threshold(0.0)
                .with_steering_sensitivity(1.0),
            measuring: false,
            options_pressed: true,
            counts: 0,
            dial: Dial::default(),
        }
    }

    fn advance(&mut self, stick: Stick, ticks: u32, options: bool) -> (i32, Confirmation, bool) {
        let pressed = options && !self.options_pressed;
        self.options_pressed = options;
        let mut detent = false;
        let delta = if self.measuring {
            self.flick.advance_with_rotation(stick, ticks, |degrees| {
                detent = self.dial.rotate(degrees);
            })
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
        (delta, confirmation, detent)
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

        assert_eq!(advance(up, true), (0, Confirmation::None, false));
        advance(center, false);
        assert_eq!(advance(center, true), (0, Confirmation::Started, false));
        assert_eq!(advance(up, true), (0, Confirmation::None, false));
        for direction in [right, down, left, up].into_iter().cycle().take(8) {
            assert_eq!(advance(direction, false), (1800, Confirmation::None, true));
        }
        assert_eq!(advance(up, true), (0, Confirmation::CenterStick, false));
        for _ in 0..3 {
            advance(center, false);
        }
        let (delta, confirmation, detent) = advance(center, true);
        assert!(!detent);
        assert_eq!(delta, 0);
        assert_eq!(confirmation, Confirmation::Finished(14_400));
        assert_eq!(calculate(14_400, 2.0, 2.0).unwrap(), 40.0);
    }

    #[test]
    fn measurement_can_make_fine_adjustments_next_to_forward() {
        let mut sweep = StickSweep::new(20.0);
        let center = Stick::default();
        sweep.advance(center, 0, false);
        assert_eq!(sweep.advance(center, 30_000, true).1, Confirmation::Started);
        sweep.advance(Stick { x: 0.0, y: -1.0 }, 60_000, false);
        let (x, forward) = 2.0_f32.to_radians().sin_cos();
        let (counts, _, _) = sweep.advance(Stick { x, y: -forward }, 90_000, false);
        assert!((39..=40).contains(&counts));
    }

    #[test]
    fn dial_uses_relative_steps_and_resets_when_the_stick_recenters() {
        let mut dial = Dial::default();
        assert!(!dial.rotate(Some(10.0)));
        assert!(!dial.rotate(Some(-2.0)));
        assert!(dial.rotate(Some(8.0)));
        assert!(!dial.rotate(Some(-14.0)));
        assert!(dial.rotate(Some(-3.0)));
        assert!(!dial.rotate(None));
        assert!(!dial.rotate(Some(-10.0)));
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

use std::{
    error::Error,
    ffi::OsString,
    io::{self, Write},
    ops::ControlFlow,
    path::Path,
};

use joyshockrust_core::{Button, Controls, DPad};

use crate::{ReportTimers, SessionEvent, discovery, forward_reports};

pub fn run(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let node = args.next().ok_or("expected a controller node")?;
    let parent = args
        .next()
        .and_then(|arg| arg.to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid > 1)
        .ok_or("expected a calibration window process ID")?;
    if args.next().is_some() {
        return Err("expected only a controller node and a calibration window process ID".into());
    }
    nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGKILL)?;
    if nix::unistd::getppid().as_raw() != parent {
        return Err("calibration window has closed".into());
    }
    let (mut input, identity) = discovery::open_controller(
        Path::new(&node),
        discovery::ControllerFilter {
            transport: discovery::Selection::Auto,
            ..Default::default()
        },
    )?;
    let mut previous = None;
    let mut stdout = io::stdout().lock();
    forward_reports(
        &mut input,
        identity,
        None,
        None,
        None,
        ReportTimers::default(),
        |event| {
            if let SessionEvent::Report(report) = event
                && let Some(command) = command(&mut previous, report.controls)
            {
                writeln!(stdout, "{command}")?;
                stdout.flush()?;
            }
            Ok(ControlFlow::Continue(()))
        },
    )?;
    Ok(())
}

fn direction(dpad: DPad) -> Option<&'static str> {
    match dpad {
        DPad::Up | DPad::UpLeft | DPad::UpRight => Some("up"),
        DPad::Down | DPad::DownLeft | DPad::DownRight => Some("down"),
        DPad::Left => Some("left"),
        DPad::Right => Some("right"),
        DPad::Neutral => None,
    }
}

fn command(previous: &mut Option<Controls>, current: Controls) -> Option<&'static str> {
    let previous = previous.replace(current)?;
    let pressed = |button| current.buttons.contains(button) && !previous.buttons.contains(button);
    if pressed(Button::East) {
        return Some("back");
    }
    let current_direction = direction(current.dpad);
    if current_direction.is_some() && current_direction != direction(previous.dpad) {
        return current_direction;
    }
    [
        (Button::South, "accept"),
        (Button::LeftShoulder, "fine-left"),
        (Button::RightShoulder, "fine-right"),
    ]
    .into_iter()
    .find_map(|(button, command)| pressed(button).then_some(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_emits_fresh_presses_and_leaves_options_for_measurement() {
        let mut previous = None;
        let mut controls = Controls::default();
        controls.buttons.insert(Button::South);
        assert_eq!(command(&mut previous, controls), None);
        assert_eq!(command(&mut previous, controls), None);
        controls.buttons.remove(Button::South);
        controls.buttons.insert(Button::Start);
        assert_eq!(command(&mut previous, controls), None);
        controls.buttons.insert(Button::South);
        assert_eq!(command(&mut previous, controls), Some("accept"));
        controls.dpad = DPad::Down;
        assert_eq!(command(&mut previous, controls), Some("down"));
        controls.dpad = DPad::DownRight;
        assert_eq!(command(&mut previous, controls), None);
        controls.dpad = DPad::Right;
        assert_eq!(command(&mut previous, controls), Some("right"));
        controls.buttons.insert(Button::East);
        assert_eq!(command(&mut previous, controls), Some("back"));
    }
}

#[cfg(target_os = "linux")]
mod discovery;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
mod gyro_mouse;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod hidraw;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf};

    const USAGE: &str = "usage: joyshockrust-linux [--gyro-mouse] [/dev/hidrawN]";
    let mut gyro_mouse = false;
    let mut path = None;
    for arg in env::args_os().skip(1) {
        if arg == "--help" {
            println!("{USAGE}");
            return Ok(());
        }
        if arg == "--gyro-mouse" && !gyro_mouse {
            gyro_mouse = true;
        } else if path.is_none() && !arg.as_encoded_bytes().starts_with(b"-") {
            path = Some(PathBuf::from(arg));
        } else {
            return Err(USAGE.into());
        }
    }
    if let Some(path) = path {
        let mut input = discovery::open_wired_edge(&path)
            .map_err(|err| format!("could not use {}: {err}", path.display()))?;
        serve_session(&mut input, gyro_mouse)?;
    } else {
        discovery::run(|input| serve_session(input, gyro_mouse))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn serve_session(input: &mut std::fs::File, gyro_mouse: bool) -> std::io::Result<()> {
    let mut mouse = if gyro_mouse {
        let mouse = gyro_mouse::GyroMouse::open(input)
            .map_err(|err| std::io::Error::other(format!("could not start gyro mouse: {err}")))?;
        eprintln!("JoyShockRust Gyro Mouse ready");
        Some(mouse)
    } else {
        None
    };
    let mut output = gamepad::open()
        .map_err(|err| std::io::Error::other(format!("could not create uinput gamepad: {err}")))?;
    let mut state = gamepad::GamepadState::default();
    let result = forward_reports(input, |report| {
        state.publish(report.controls, |events| output.emit(events))?;
        if let Some(mouse) = &mut mouse {
            mouse.publish(report.imu)?;
        }
        Ok(())
    });
    let release = state.publish(joyshockrust_core::Controls::default(), |events| {
        output.emit(events)
    });
    result.and(release)
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("joyshockrust-linux requires Linux hidraw and uinput");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn forward_reports(
    input: &mut std::fs::File,
    mut publish: impl FnMut(joyshockrust_input::InputReport) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use joyshockrust_input::{DeviceModel, Transport, parse_report};
    use nix::{
        errno::Errno,
        poll::{PollFd, PollFlags, PollTimeout, poll},
    };
    use std::{
        io::{self, Read},
        os::fd::AsFd,
    };

    // One read consumes one complete HID report. A larger buffer preserves
    // length validation when a node delivers a report larger than 64 bytes.
    let mut report = [0; 16_384];
    loop {
        let mut fds = [PollFd::new(input.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
        let ready = fds[0].revents().unwrap_or_else(PollFlags::empty);
        if ready.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Ok(());
        }
        if !ready.contains(PollFlags::POLLIN) {
            continue;
        }
        for _ in 0..32 {
            let len = match input.read(&mut report) {
                Ok(0) => return Ok(()),
                Ok(len) => len,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err)
                    if matches!(err.raw_os_error(), Some(nix::libc::EIO | nix::libc::ENODEV)) =>
                {
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            if let Ok(report) = parse_report(
                DeviceModel::DualSenseEdge,
                Transport::Usb,
                &report[..len],
                None,
            ) {
                publish(report)?;
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        fs::File,
        io,
        os::fd::OwnedFd,
        os::unix::net::{UnixDatagram, UnixStream},
    };

    use joyshockrust_core::Button;

    #[test]
    fn readiness_loop_ends_when_input_disconnects() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(sender);
        let mut input = File::from(OwnedFd::from(receiver));
        super::forward_reports(&mut input, |_| {
            panic!("disconnected input published controls")
        })
        .unwrap();
    }

    #[test]
    fn readiness_loop_decodes_complete_reports_in_order() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender.send(&[1; 65]).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[8] = 0x28;
        sender.send(&frame).unwrap();
        frame[8] = 8;
        sender.send(&frame).unwrap();

        let mut input = File::from(OwnedFd::from(receiver));
        let mut presses = Vec::new();
        let result = super::forward_reports(&mut input, |report| {
            presses.push(report.controls.buttons.contains(Button::South));
            if presses.len() == 2 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(presses, [true, false]);
    }
}

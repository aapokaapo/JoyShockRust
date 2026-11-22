#[cfg(target_os = "linux")]
mod discovery;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod hidraw;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf};

    let mut args = env::args_os().skip(1);
    let path = match (args.next(), args.next()) {
        (None, None) => return Ok(discovery::run(serve_session)?),
        (Some(path), None) => PathBuf::from(path),
        _ => return Err("usage: joyshockrust-linux [/dev/hidrawN]".into()),
    };
    let mut input = discovery::open_wired_edge(&path)
        .map_err(|err| format!("could not use {}: {err}", path.display()))?;
    Ok(serve_session(&mut input)?)
}

#[cfg(target_os = "linux")]
fn serve_session(input: &mut std::fs::File) -> std::io::Result<()> {
    let mut output = gamepad::open()
        .map_err(|err| std::io::Error::other(format!("could not create uinput gamepad: {err}")))?;
    let mut state = gamepad::GamepadState::default();
    let result = forward_reports(input, |controls| {
        state.publish(controls, |events| output.emit(events))
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
    mut publish: impl FnMut(joyshockrust_core::Controls) -> std::io::Result<()>,
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
                publish(report.controls)?;
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
        let result = super::forward_reports(&mut input, |controls| {
            presses.push(controls.buttons.contains(Button::South));
            if presses.len() == 2 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(presses, [true, false]);
    }
}

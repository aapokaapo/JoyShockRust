#[cfg(target_os = "linux")]
mod diagnostics;
#[cfg(target_os = "linux")]
mod discovery;
#[cfg(target_os = "linux")]
mod flick_stick;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
mod gyro_gate;
#[cfg(target_os = "linux")]
mod gyro_mouse;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod hidraw;
#[cfg(target_os = "linux")]
mod profile;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod status;
#[cfg(target_os = "linux")]
mod systemd;

#[cfg(target_os = "linux")]
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf, process::ExitCode};

    use gyro_gate::{Binding, TriggerGate};
    use joyshockrust_core::Button;

    const USAGE: &str = concat!(
        "usage: joyshockrust-linux --profile PATH [/dev/hidrawN]\n",
        "       joyshockrust-linux --check-profile PATH\n",
        "       joyshockrust-linux --install-profile SOURCE [DESTINATION]\n",
        "       joyshockrust-linux --diagnose [/dev/hidrawN]\n",
        "       joyshockrust-linux --status [SOCKET]\n",
        "       joyshockrust-linux --reload-profile [SOCKET]\n",
        "bridge options: [--gamepad generic|xbox360] [--status-socket SOCKET] [--reload-socket SOCKET]\n",
        "usage: joyshockrust-linux [--stick-curve EXPONENT] [--remap-button SOURCE=TARGET]... [--layer-remap MODIFIER:SOURCE=TARGET]... ",
        "[--gyro-mouse [--calibrate-gyro] [--flick-stick COUNTS_PER_DEGREE] [BINDING]] [/dev/hidrawN]\n",
        "BINDING: --gyro-hold BUTTON | --gyro-toggle BUTTON | --gyro-tap-hold BUTTON | ",
        "--gyro-double-tap BUTTON | --gyro-chord BUTTON,BUTTON | --gyro-hold-l2 | ",
        "--gyro-trigger SIDE[:PRESS:RELEASE]",
    );
    #[derive(PartialEq)]
    enum ProfileMode {
        Run,
        Check,
        Install,
    }

    let mut gyro_mouse = false;
    let mut options = gyro_mouse::Options::default();
    let mut path = None;
    let mut profile_path = None;
    let mut diagnose = false;
    let mut status_query = false;
    let mut reload_query = false;
    let mut reload_path = None;
    let mut status_path = None;
    let mut gamepad_kind = None;
    let mut stick_curve = None;
    let mut remaps = gamepad::ButtonRemaps::default();
    let mut args = env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        if arg == "--gyro-mouse" && !gyro_mouse {
            gyro_mouse = true;
        } else if arg == "--calibrate-gyro" && !options.calibrate {
            options.calibrate = true;
        } else if matches!(
            arg.to_str(),
            Some(
                "--gyro-hold-l2"
                    | "--gyro-hold"
                    | "--gyro-toggle"
                    | "--gyro-tap-hold"
                    | "--gyro-double-tap"
                    | "--gyro-chord"
                    | "--gyro-trigger"
            )
        ) {
            if !matches!(options.binding, Binding::Always) {
                return Err("select only one gyro binding".into());
            }
            options.binding = if arg == "--gyro-hold-l2" {
                Binding::Hold(Button::LeftTrigger)
            } else if arg == "--gyro-chord" {
                let spec = args.next().ok_or("expected two gyro chord buttons")?;
                gyro_gate::chord_by_name(spec.to_str().ok_or("invalid gyro chord specification")?)?
            } else if arg == "--gyro-trigger" {
                let spec = args
                    .next()
                    .ok_or("expected an analog trigger specification")?;
                Binding::AnalogTrigger(TriggerGate::parse(
                    spec.to_str()
                        .ok_or("invalid analog trigger specification")?,
                )?)
            } else {
                let name = args.next().ok_or("expected a gyro binding button name")?;
                let button = name
                    .to_str()
                    .and_then(gyro_gate::button_by_name)
                    .ok_or_else(|| {
                        format!("unknown gyro binding button: {}", name.to_string_lossy())
                    })?;
                match arg.to_str() {
                    Some("--gyro-hold") => Binding::Hold(button),
                    Some("--gyro-tap-hold") => Binding::TapHold(button),
                    Some("--gyro-double-tap") => Binding::DoubleTap(button),
                    _ => Binding::Toggle(button),
                }
            };
        } else if arg == "--diagnose" && !diagnose {
            diagnose = true;
        } else if arg == "--status" && !status_query {
            status_query = true;
        } else if arg == "--reload-profile" && !reload_query {
            reload_query = true;
        } else if arg == "--reload-socket" && reload_path.is_none() {
            reload_path = Some(PathBuf::from(
                args.next().ok_or("expected a reload socket path")?,
            ));
        } else if arg == "--status-socket" && status_path.is_none() {
            status_path = Some(PathBuf::from(
                args.next().ok_or("expected a status socket path")?,
            ));
        } else if matches!(
            arg.to_str(),
            Some("--profile" | "--check-profile" | "--install-profile")
        ) {
            if profile_path.is_some() {
                return Err("select only one profile".into());
            }
            let file = args.next().ok_or("expected a profile path")?;
            let mode = match arg.to_str() {
                Some("--check-profile") => ProfileMode::Check,
                Some("--install-profile") => ProfileMode::Install,
                _ => ProfileMode::Run,
            };
            profile_path = Some((PathBuf::from(file), mode));
        } else if arg == "--gamepad" && gamepad_kind.is_none() {
            let value = args
                .next()
                .ok_or("expected generic or xbox360 after --gamepad")?;
            gamepad_kind = Some(match value.to_str() {
                Some("generic") => gamepad::OutputKind::Generic,
                Some("xbox360") => gamepad::OutputKind::Xbox360,
                _ => return Err("expected generic or xbox360 after --gamepad".into()),
            });
        } else if arg == "--flick-stick" {
            if options.flick_stick.is_some() {
                return Err("select only one Flick Stick sensitivity".into());
            }
            let value = args
                .next()
                .ok_or("expected counts per degree after --flick-stick")?;
            let scale = value
                .to_str()
                .and_then(|value| value.parse::<f64>().ok())
                .ok_or("invalid Flick Stick sensitivity")?;
            options.flick_stick = Some(gyro_mouse::validate_flick_scale(scale)?);
        } else if arg == "--stick-curve" {
            if stick_curve.is_some() {
                return Err("select only one stick curve".into());
            }
            let value = args.next().ok_or("expected a stick curve exponent")?;
            stick_curve = Some(gamepad::StickCurve::parse(
                value.to_str().ok_or("invalid stick curve exponent")?,
            )?);
        } else if arg == "--remap-button" {
            let spec = args
                .next()
                .ok_or("expected SOURCE=TARGET after --remap-button")?;
            remaps.add(spec.to_str().ok_or("invalid button remap specification")?)?;
        } else if arg == "--layer-remap" {
            let spec = args
                .next()
                .ok_or("expected MODIFIER:SOURCE=TARGET after --layer-remap")?;
            remaps.add_layer(spec.to_str().ok_or("invalid layer remap specification")?)?;
        } else if arg == "--remap-layer" {
            return Err("use --layer-remap MODIFIER:SOURCE=TARGET for each layer rule".into());
        } else if path.is_none() && !arg.as_encoded_bytes().starts_with(b"-") {
            path = Some(PathBuf::from(arg));
        } else {
            return Err(USAGE.into());
        }
    }
    if (options.calibrate
        || options.flick_stick.is_some()
        || !matches!(options.binding, Binding::Always))
        && !gyro_mouse
    {
        return Err("gyro options require --gyro-mouse".into());
    }
    let mapping_options = gyro_mouse || stick_curve.is_some() || !remaps.is_empty();
    let bridge_options = status_path.is_some() || reload_path.is_some() || gamepad_kind.is_some();
    if status_query || reload_query {
        if (status_query && reload_query)
            || diagnose
            || profile_path.is_some()
            || bridge_options
            || mapping_options
        {
            return Err("select one socket command with only an optional socket path".into());
        }
        if status_query {
            status::query(path.as_deref(), std::io::stdout().lock())?;
        } else if let Err(err) = reload::request(path.as_deref()) {
            eprintln!("{err}");
            return Ok(ExitCode::FAILURE);
        }
        return Ok(ExitCode::SUCCESS);
    }
    if diagnose {
        if profile_path.is_some() || bridge_options || mapping_options {
            return Err("--diagnose takes only an optional hidraw path".into());
        }
        diagnostics::run(path.as_deref(), std::io::stdout().lock())?;
        return Ok(ExitCode::SUCCESS);
    }
    let (mut runtime, reload) = if let Some((profile_path, mode)) = profile_path {
        if mapping_options {
            return Err("use either a TOML profile or command-line mapping options".into());
        }
        if mode == ProfileMode::Install {
            if bridge_options {
                return Err("--install-profile takes a source and an optional destination".into());
            }
            let destination = match path {
                Some(path) => path,
                None => profile::default_path()?,
            };
            profile::install(&profile_path, &destination)?;
            println!("Installed profile: {}", destination.display());
            return Ok(ExitCode::SUCCESS);
        }
        if mode == ProfileMode::Check && (path.is_some() || bridge_options) {
            return Err("--check-profile takes only a profile path".into());
        }
        let profile = match profile::load(&profile_path) {
            Ok(profile) => profile,
            Err(err) => {
                eprintln!("{}: {err}", profile_path.display());
                // EX_CONFIG tells the systemd unit to wait for a profile fix.
                return Ok(ExitCode::from(78));
            }
        };
        if mode == ProfileMode::Check {
            println!("Valid profile: {}", profile_path.display());
            return Ok(ExitCode::SUCCESS);
        }
        (
            profile,
            Some(reload::Reload::new(profile_path, reload_path.as_deref())?),
        )
    } else {
        if reload_path.is_some() {
            return Err("--reload-socket requires --profile".into());
        }
        (
            profile::RuntimeOptions {
                gyro: gyro_mouse.then_some(options),
                stick_curve,
                remaps,
            },
            None,
        )
    };
    let status = status_path
        .as_deref()
        .map(|path| {
            status::StatusSocket::bind(path)
                .map_err(|err| format!("could not bind {}: {err}", path.display()))
        })
        .transpose()?;
    let status = status.as_ref();
    if let Some(status) = status {
        status.update(false, runtime.gyro.is_some());
    }
    let reload = reload.as_ref();
    let gamepad_kind = gamepad_kind.unwrap_or_default();
    if let Some(path) = path {
        let mut input = discovery::open_wired_edge(&path)
            .map_err(|err| format!("could not use {}: {err}", path.display()))?;
        serve_session(&mut input, &mut runtime, reload, status, gamepad_kind)?;
    } else {
        discovery::run(reload, status, |event| match event {
            discovery::Event::Connected(input) => {
                serve_session(input, &mut runtime, reload, status, gamepad_kind)
            }
            discovery::Event::Reload(reload) => {
                if let Some(next) = reload.load() {
                    runtime = next;
                    if let Some(status) = status {
                        status.update(false, runtime.gyro.is_some());
                    }
                    reload.applied();
                }
                Ok(())
            }
        })?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(target_os = "linux")]
fn open_mouse(
    input: &std::fs::File,
    options: Option<gyro_mouse::Options>,
) -> std::io::Result<Option<gyro_mouse::GyroMouse>> {
    let Some(options) = options else {
        return Ok(None);
    };
    let mouse = gyro_mouse::GyroMouse::open(input, options)
        .map_err(|err| std::io::Error::other(format!("could not start gyro mouse: {err}")))?;
    eprintln!("JoyShockRust Gyro Mouse ready");
    if options.calibrate {
        eprintln!("Set the controller down and keep it still for gyro calibration");
    }
    Ok(Some(mouse))
}

#[cfg(target_os = "linux")]
fn serve_session(
    input: &mut std::fs::File,
    runtime: &mut profile::RuntimeOptions,
    reload: Option<&reload::Reload>,
    status: Option<&status::StatusSocket>,
    gamepad_kind: gamepad::OutputKind,
) -> std::io::Result<()> {
    let mut mouse = open_mouse(input, runtime.gyro)?;
    let reload_input = reload.map(|_| input.try_clone()).transpose()?;
    let mut output = gamepad::open(gamepad_kind)
        .map_err(|err| std::io::Error::other(format!("could not create uinput gamepad: {err}")))?;
    let mut state = gamepad::GamepadState::new(gamepad_kind);
    let mut layer_state = gamepad::RemapState::default();
    if let Some(status) = status {
        status.update(true, runtime.gyro.is_some());
    }
    let result = forward_reports(input, reload, status, |event| {
        let report = match event {
            SessionEvent::Report(report) => report,
            SessionEvent::Reload(reload) => {
                let Some(next) = reload.load() else {
                    return Ok(());
                };
                if runtime.gyro != next.gyro {
                    let input = reload_input.as_ref().expect("reload descriptor");
                    match open_mouse(input, next.gyro) {
                        Ok(next_mouse) => mouse = next_mouse,
                        Err(err) => {
                            reload.rejected(&err);
                            return Ok(());
                        }
                    }
                }
                if runtime.remaps != next.remaps {
                    layer_state = gamepad::RemapState::default();
                }
                *runtime = next;
                if let Some(status) = status {
                    status.update(true, runtime.gyro.is_some());
                }
                reload.applied();
                return Ok(());
            }
        };
        let mut controls = runtime
            .stick_curve
            .map_or(report.controls, |curve| curve.apply(report.controls));
        if runtime
            .gyro
            .is_some_and(|options| options.flick_stick.is_some())
        {
            controls.right_stick = Default::default();
        }
        state.publish(runtime.remaps.apply(controls, &mut layer_state), |events| {
            output.emit(events)
        })?;
        if let Some(mouse) = &mut mouse {
            mouse.publish(report)?;
        }
        Ok(())
    });
    if let Some(status) = status {
        status.update(false, runtime.gyro.is_some());
    }
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
enum SessionEvent<'a> {
    Report(joyshockrust_input::InputReport),
    Reload(reload::Request<'a>),
}

#[cfg(target_os = "linux")]
fn forward_reports(
    input: &mut std::fs::File,
    reload: Option<&reload::Reload>,
    status: Option<&status::StatusSocket>,
    mut publish: impl FnMut(SessionEvent<'_>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use joyshockrust_input::{DeviceModel, Transport, parse_report};
    use nix::poll::PollFlags;
    use std::{
        io::{self, Read},
        os::fd::AsFd,
    };

    // One read consumes one complete HID report. A larger buffer preserves
    // length validation when a node delivers a report larger than 64 bytes.
    let mut report = [0; 16_384];
    loop {
        let (ready, request) = reload::wait(input.as_fd(), reload, status)?;
        if let Some(request) = request {
            publish(SessionEvent::Reload(request))?;
        }
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
                publish(SessionEvent::Report(report))?;
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
        super::forward_reports(&mut input, None, None, |_| {
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
        let result = super::forward_reports(&mut input, None, None, |event| {
            let super::SessionEvent::Report(report) = event else {
                unreachable!()
            };
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

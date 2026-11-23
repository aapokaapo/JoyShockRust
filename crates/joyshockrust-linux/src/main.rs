#[cfg(target_os = "linux")]
mod benchmark;
#[cfg(target_os = "linux")]
mod controller_config;
#[cfg(target_os = "linux")]
mod diagnostics;
#[cfg(target_os = "linux")]
mod discovery;
#[cfg(target_os = "linux")]
mod flick_stick;
#[cfg(target_os = "linux")]
mod game_calibration;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
mod gyro_calibration;
#[cfg(target_os = "linux")]
mod gyro_gate;
#[cfg(target_os = "linux")]
mod gyro_mouse;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod hidraw;
#[cfg(target_os = "linux")]
mod keyboard;
#[cfg(target_os = "linux")]
mod launch;
#[cfg(all(test, target_os = "linux"))]
mod live_uinput_test;
#[cfg(target_os = "linux")]
mod profile;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod rumble;
#[cfg(target_os = "linux")]
mod status;
#[cfg(target_os = "linux")]
mod systemd;
#[cfg(target_os = "linux")]
mod touchpad;

#[cfg(target_os = "linux")]
use hidraw::Identity;
#[cfg(target_os = "linux")]
use joyshockrust_input::Transport;
#[cfg(target_os = "linux")]
use std::ops::ControlFlow;

#[cfg(target_os = "linux")]
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf, process::ExitCode, time::Duration};

    use gyro_gate::{Binding, TriggerGate};
    use joyshockrust_core::Button;

    const USAGE: &str = concat!(
        "usage: joyshockrust-linux [--profile PATH|--profile-if-present PATH] [/dev/hidrawN]\n",
        "       joyshockrust-linux --check-profile PATH\n",
        "       joyshockrust-linux --install-profile SOURCE [DESTINATION]\n",
        "       joyshockrust-linux --diagnose [--profile PATH] [--show-controller-id] [CONTROLLER_OPTIONS] [/dev/hidrawN]\n",
        "       joyshockrust-linux --list-controllers [--show-controller-id] [CONTROLLER_OPTIONS]\n",
        "       joyshockrust-linux --status [SOCKET]\n",
        "       joyshockrust-linux --reload-profile [SOCKET]\n",
        "       joyshockrust-linux --load-profile PATH [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --unload-profile [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --set-gamepad on|off [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --set-outputs pause|resume [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --select-controller ID|any [--controller-model MODEL] [--usb|--bluetooth|--auto-transport] [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --launch-sdl -- COMMAND [ARGS...]\n",
        "       joyshockrust-linux --calibrate-game --profile PATH [/dev/hidrawN]\n",
        "       joyshockrust-linux --calibrate-game COUNTS IN_GAME_SENS [MEASURED_TURNS]\n",
        "       joyshockrust-linux --benchmark-reports COUNT [--benchmark-timeout SECONDS] [--profile PATH] [--bluetooth|--auto-transport] [/dev/hidrawN]\n",
        "bridge options: [--usb|--bluetooth|--auto-transport] [--controller-model any|dualsense|dualsense-edge] [--controller-id ID|any]\n",
        "                [--gamepad generic|xbox360|xbox-series] [--rumble] [--grab-touchpad] [--status-socket SOCKET] [--reload-socket SOCKET]\n",
        "usage: joyshockrust-linux [--stick-curve EXPONENT] [--remap-button SOURCE=TARGET]... [--layer-remap MODIFIER:SOURCE=TARGET]... ",
        "[--gyro-mouse [--calibrate-gyro] [--flick-stick COUNTS_PER_DEGREE] [BINDING]] [/dev/hidrawN]\n",
        "BINDING: --gyro-hold BUTTON | --gyro-toggle BUTTON | --gyro-tap-hold BUTTON | ",
        "--gyro-double-tap BUTTON | --gyro-chord BUTTON,BUTTON | --gyro-hold-l2 | ",
        "--gyro-trigger SIDE[:PRESS:RELEASE]",
    );
    #[derive(PartialEq)]
    enum ProfileMode {
        Run,
        RunIfPresent,
        Check,
        Install,
    }

    let mut gyro_mouse = false;
    let mut options = gyro_mouse::Options::default();
    let mut path = None;
    let mut profile_path = None;
    let mut diagnose = false;
    let mut list_controllers = false;
    let mut show_controller_id = false;
    let mut benchmark_target = None;
    let mut benchmark_timeout = None;
    let mut status_query = false;
    let mut reload_query = false;
    let mut load_profile_path = None;
    let mut unload_profile = false;
    let mut set_gamepad = None;
    let mut set_outputs_paused = None;
    let mut controller_selection = None;
    let mut reload_path = None;
    let mut status_path = None;
    let mut gamepad_kind = None;
    let mut rumble_enabled = false;
    let mut grab_touchpad = false;
    let mut transport = None;
    let mut controller_model = None;
    let mut controller_id = None;
    let mut stick_curve = None;
    let mut remaps = gamepad::ButtonRemaps::default();
    let mut args = env::args_os().skip(1).peekable();
    if args.next_if(|arg| arg == "--launch-sdl").is_some() {
        eprintln!("{}", launch::run(args));
        return Ok(ExitCode::FAILURE);
    }
    if args.next_if(|arg| arg == "--gyro-calibration").is_some() {
        gyro_calibration::run(args)?;
        return Ok(ExitCode::SUCCESS);
    }
    if args.next_if(|arg| arg == "--calibrate-game").is_some() {
        game_calibration::run(args)?;
        return Ok(ExitCode::SUCCESS);
    }
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
                Binding::Hold(Button::LeftTrigger.into())
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
                    Some("--gyro-hold") => Binding::Hold(button.into()),
                    Some("--gyro-tap-hold") => Binding::TapHold(button.into()),
                    Some("--gyro-double-tap") => Binding::DoubleTap(button.into()),
                    _ => Binding::Toggle(button.into()),
                }
            };
        } else if arg == "--diagnose" && !diagnose {
            diagnose = true;
        } else if arg == "--list-controllers" && !list_controllers {
            list_controllers = true;
        } else if arg == "--show-controller-id" && !show_controller_id {
            show_controller_id = true;
        } else if arg == "--benchmark-reports" && benchmark_target.is_none() {
            let value = args
                .next()
                .ok_or("expected a report count after --benchmark-reports")?;
            benchmark_target = Some(
                value
                    .to_str()
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|count| (2..=benchmark::MAX_REPORTS).contains(count))
                    .ok_or("benchmark report count must be between 2 and 100000")?,
            );
        } else if arg == "--benchmark-timeout" && benchmark_timeout.is_none() {
            let value = args
                .next()
                .ok_or("expected seconds after --benchmark-timeout")?;
            benchmark_timeout = Some(
                value
                    .to_str()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|seconds| (1..=benchmark::MAX_TIMEOUT_SECONDS).contains(seconds))
                    .map(Duration::from_secs)
                    .ok_or("benchmark timeout must be between 1 and 3600 seconds")?,
            );
        } else if arg == "--status" && !status_query {
            status_query = true;
        } else if arg == "--reload-profile" && !reload_query {
            reload_query = true;
        } else if arg == "--load-profile" && load_profile_path.is_none() {
            load_profile_path = Some(PathBuf::from(
                args.next()
                    .ok_or("expected a profile path after --load-profile")?,
            ));
        } else if arg == "--unload-profile" && !unload_profile {
            unload_profile = true;
        } else if arg == "--set-gamepad" && set_gamepad.is_none() {
            set_gamepad = Some(
                match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("expected on or off after --set-gamepad".into()),
                },
            );
        } else if arg == "--select-controller" && controller_selection.is_none() {
            let value = args.next().ok_or("expected a controller HID ID or any")?;
            controller_selection = Some(reload::parse_controller_selection(
                value
                    .to_str()
                    .ok_or("expected a controller HID ID or any")?,
            )?);
        } else if arg == "--set-outputs" && set_outputs_paused.is_none() {
            set_outputs_paused = Some(
                match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
                    Some("pause") => true,
                    Some("resume") => false,
                    _ => return Err("expected pause or resume after --set-outputs".into()),
                },
            );
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
            Some("--profile" | "--profile-if-present" | "--check-profile" | "--install-profile")
        ) {
            if profile_path.is_some() {
                return Err("select only one profile".into());
            }
            let file = args.next().ok_or("expected a profile path")?;
            let mode = match arg.to_str() {
                Some("--check-profile") => ProfileMode::Check,
                Some("--install-profile") => ProfileMode::Install,
                Some("--profile-if-present") => ProfileMode::RunIfPresent,
                _ => ProfileMode::Run,
            };
            profile_path = Some((PathBuf::from(file), mode));
        } else if matches!(
            arg.to_str(),
            Some("--usb" | "--bluetooth" | "--auto-transport")
        ) {
            if transport.is_some() {
                return Err("select only one transport option".into());
            }
            transport = Some(match arg.to_str() {
                Some("--usb") => discovery::Selection::Usb,
                Some("--bluetooth") => discovery::Selection::Bluetooth,
                _ => discovery::Selection::Auto,
            });
        } else if arg == "--controller-model" && controller_model.is_none() {
            let value = args.next().ok_or("expected a controller model")?;
            controller_model = Some(discovery::ModelSelection::parse(
                value.to_str().ok_or("invalid controller model")?,
            )?);
        } else if arg == "--controller-id" && controller_id.is_none() {
            let value = args.next().ok_or("expected a controller HID ID or any")?;
            controller_id = Some(reload::parse_controller_selection(
                value
                    .to_str()
                    .ok_or("expected a controller HID ID or any")?,
            )?);
        } else if arg == "--rumble" && !rumble_enabled {
            rumble_enabled = true;
        } else if arg == "--grab-touchpad" && !grab_touchpad {
            grab_touchpad = true;
        } else if arg == "--gamepad" && gamepad_kind.is_none() {
            let value = args
                .next()
                .ok_or("expected generic, xbox360 or xbox-series after --gamepad")?;
            gamepad_kind = Some(match value.to_str() {
                Some("generic") => gamepad::OutputKind::Generic,
                Some("xbox360") => gamepad::OutputKind::Xbox360,
                Some("xbox-series") => gamepad::OutputKind::XboxSeries,
                _ => return Err("expected generic, xbox360 or xbox-series after --gamepad".into()),
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
    let bridge_options = status_path.is_some()
        || reload_path.is_some()
        || gamepad_kind.is_some()
        || rumble_enabled
        || grab_touchpad;
    let selection_options =
        transport.is_some() || controller_model.is_some() || controller_id.is_some();
    let socket_commands = u8::from(status_query)
        + u8::from(reload_query)
        + u8::from(load_profile_path.is_some())
        + u8::from(unload_profile)
        + u8::from(set_gamepad.is_some())
        + u8::from(set_outputs_paused.is_some())
        + u8::from(controller_selection.is_some());
    if show_controller_id && !diagnose && !list_controllers {
        return Err("--show-controller-id requires --diagnose or --list-controllers".into());
    }
    if benchmark_timeout.is_some() && benchmark_target.is_none() {
        return Err("--benchmark-timeout requires --benchmark-reports".into());
    }
    if benchmark_target.is_some()
        && (diagnose
            || list_controllers
            || socket_commands > 0
            || profile_path
                .as_ref()
                .is_some_and(|(_, mode)| matches!(mode, ProfileMode::Check | ProfileMode::Install)))
    {
        return Err("--benchmark-reports requires a controller bridge run".into());
    }
    if socket_commands > 0 {
        if socket_commands > 1
            || diagnose
            || list_controllers
            || profile_path.is_some()
            || bridge_options
            || controller_id.is_some()
            || (controller_selection.is_none() && selection_options)
            || mapping_options
        {
            return Err("select one socket command and provide its required arguments".into());
        }
        if status_query {
            status::query(path.as_deref(), std::io::stdout().lock())?;
        } else {
            let result = if let Some(source) = load_profile_path.as_deref() {
                reload::load_profile(source, path.as_deref())
            } else if unload_profile {
                reload::unload_profile(path.as_deref())
            } else if let Some(enabled) = set_gamepad {
                reload::set_gamepad(enabled, path.as_deref())
            } else if let Some(paused) = set_outputs_paused {
                reload::set_outputs_paused(paused, path.as_deref())
            } else if let Some(selection) = controller_selection {
                reload::select_controller(selection, controller_model, transport, path.as_deref())
            } else {
                reload::request(path.as_deref())
            };
            if let Err(err) = result {
                eprintln!("{err}");
                return Ok(ExitCode::FAILURE);
            }
            if let Some(source) = load_profile_path {
                println!("Loaded profile: {}", source.display());
            } else if unload_profile {
                println!("Profile unloaded");
            } else if let Some(enabled) = set_gamepad {
                println!("Virtual gamepad {}", if enabled { "on" } else { "off" });
            } else if controller_selection.is_some() {
                println!("Controller selection updated");
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if diagnose || list_controllers {
        if list_controllers
            && (diagnose
                || profile_path.is_some()
                || path.is_some()
                || bridge_options
                || mapping_options)
        {
            return Err(
                "--list-controllers accepts controller selection options and --show-controller-id"
                    .into(),
            );
        }
        if bridge_options
            || mapping_options
            || profile_path
                .as_ref()
                .is_some_and(|(_, mode)| *mode != ProfileMode::Run)
        {
            return Err(
                "--diagnose accepts an optional profile, controller selection options and hidraw path".into(),
            );
        }
        if let Some((path, _)) = &profile_path
            && let Err(err) = profile::load(path)
        {
            eprintln!("{}: {err}", path.display());
            return Ok(ExitCode::from(78));
        }
        let mut filter = controller_config::ControllerConfig::load().filter();
        if let Some(transport) = transport {
            filter.transport = transport;
        }
        if let Some(model) = controller_model {
            filter.model = model;
        }
        if let Some(id) = controller_id {
            filter.controller_id = id;
        }
        if list_controllers {
            diagnostics::list_controllers(filter, show_controller_id, std::io::stdout().lock())?;
            return Ok(ExitCode::SUCCESS);
        }
        diagnostics::run(
            path.as_deref(),
            filter,
            show_controller_id,
            std::io::stdout().lock(),
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let default_runtime = || profile::RuntimeOptions {
        gyro: gyro_mouse.then_some(options),
        stick_curve,
        remaps,
        ..profile::RuntimeOptions::default()
    };
    use status::ProfileSource;
    let (mut runtime, startup_profile, profile_source) = if let Some((profile_path, mode)) =
        profile_path
    {
        if mapping_options {
            return Err("use either a TOML profile or command-line mapping options".into());
        }
        if mode == ProfileMode::Install {
            if bridge_options || selection_options {
                return Err("--install-profile takes a source and an optional destination".into());
            }
            let destination = match path {
                Some(path) => path,
                None => profile::default_path()?,
            };
            if let Err(err) = profile::install(&profile_path, &destination) {
                eprintln!("{err}");
                return Ok(ExitCode::FAILURE);
            }
            println!("Installed profile: {}", destination.display());
            return Ok(ExitCode::SUCCESS);
        }
        if mode == ProfileMode::Check && (path.is_some() || bridge_options || selection_options) {
            return Err("--check-profile takes only a profile path".into());
        }
        let (profile, source) = match profile::load(&profile_path) {
            Ok(profile) => (profile, ProfileSource::Startup),
            Err(err)
                if mode == ProfileMode::RunIfPresent
                    && err
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
            {
                (default_runtime(), ProfileSource::Defaults)
            }
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
        (profile, Some(profile_path), source)
    } else {
        let source = if mapping_options {
            ProfileSource::CommandLine
        } else {
            ProfileSource::Defaults
        };
        (default_runtime(), None, source)
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
        status.update(None, runtime.gyro.is_some());
        let path = startup_profile
            .as_deref()
            .filter(|_| matches!(profile_source, ProfileSource::Startup));
        status.set_profile(
            profile_source,
            path.and_then(std::path::Path::file_name),
            path,
        );
        status.set_touchpad_policy(grab_touchpad || runtime.grab_touchpad, false);
    }
    let mut controller_config = controller_config::ControllerConfig::load();
    let controller_defaults = controller_config.filter();
    let filter = discovery::ControllerFilter {
        transport: transport.unwrap_or(controller_defaults.transport),
        model: controller_model.unwrap_or(controller_defaults.model),
        controller_id: controller_id.flatten(),
    };
    let configured_bridge = runtime.bridge.unwrap_or_default();
    let mut bridge = profile::BridgeOptions {
        gamepad: gamepad_kind.unwrap_or(configured_bridge.gamepad),
        rumble: rumble_enabled || configured_bridge.rumble,
        ..configured_bridge
    };
    let reload = if startup_profile.is_some() || reload_path.is_some() {
        Some(reload::Reload::new(
            startup_profile,
            reload_path.as_deref(),
            runtime.bridge.unwrap_or(bridge),
            path.is_none() && benchmark_target.is_none(),
        )?)
    } else {
        None
    };
    let reload = reload.as_ref();
    if let Some(status) = status {
        status.set_filter(filter);
        status.set_gamepad_enabled(bridge.virtual_gamepad);
    }
    if let Some(target) = benchmark_target {
        let (mut input, identity, path) = if let Some(path) = path {
            let (input, identity) = discovery::open_controller(&path, filter)
                .map_err(|err| format!("could not use {}: {err}", path.display()))?;
            (input, identity, path)
        } else {
            let (input, candidate) =
                discovery::open_first_controller(filter, controller_config.preferred())?;
            (input, candidate.identity, candidate.path)
        };
        let mut measurement = benchmark::Benchmark::new(
            target,
            benchmark_timeout.unwrap_or(benchmark::DEFAULT_TIMEOUT),
        );
        let result = serve_session(
            &mut input,
            &mut runtime,
            reload,
            status,
            SessionDevice {
                identity,
                filter,
                path: &path,
                grab_touchpad,
                outputs_paused: None,
            },
            &mut bridge,
            Some(&mut measurement),
        );
        measurement.write(identity, result.is_err(), std::io::stdout().lock())?;
        result?;
        if !measurement.complete() {
            if measurement.timed_out {
                eprintln!("Benchmark time limit reached before collecting all requested reports");
            } else {
                eprintln!("Controller disconnected before benchmark completed");
            }
            return Ok(ExitCode::FAILURE);
        }
    } else if let Some(path) = path {
        let (mut input, identity) = discovery::open_controller(&path, filter)
            .map_err(|err| format!("could not use {}: {err}", path.display()))?;
        if let Ok(Some(id)) = hidraw::controller_id(&input) {
            controller_config.remember(id);
        }
        serve_session(
            &mut input,
            &mut runtime,
            reload,
            status,
            SessionDevice {
                identity,
                filter,
                path: &path,
                grab_touchpad,
                outputs_paused: None,
            },
            &mut bridge,
            None,
        )?;
    } else {
        let outputs_paused = std::cell::Cell::new(false);
        discovery::run(
            filter,
            controller_config,
            reload,
            status,
            &outputs_paused,
            |event| match event {
                discovery::Event::Connected(input, identity, filter, path) => serve_session(
                    input,
                    &mut runtime,
                    reload,
                    status,
                    SessionDevice {
                        identity,
                        filter,
                        path,
                        grab_touchpad,
                        outputs_paused: Some(&outputs_paused),
                    },
                    &mut bridge,
                    None,
                ),
                discovery::Event::Reload(reload, current_filter) => {
                    let selection = match reload.resolve(current_filter) {
                        Some(reload::Update::Profile(next)) => {
                            runtime = *next;
                            if let Some(status) = status {
                                status.update(None, runtime.gyro.is_some());
                                status.set_touchpad_policy(
                                    grab_touchpad || runtime.grab_touchpad,
                                    false,
                                );
                            }
                            None
                        }
                        Some(reload::Update::Selection(selection)) => Some(selection),
                        Some(reload::Update::Gamepad(enabled)) => {
                            bridge.virtual_gamepad = enabled;
                            if let Some(status) = status {
                                status.set_gamepad_enabled(enabled);
                            }
                            None
                        }
                        Some(reload::Update::OutputsPaused(paused)) => {
                            outputs_paused.set(paused);
                            if let Some(status) = status {
                                status.set_outputs_paused(paused);
                            }
                            None
                        }
                        None => return Ok(None),
                    };
                    reload.applied(status);
                    Ok(selection)
                }
            },
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(target_os = "linux")]
fn open_mouse(
    input: &std::fs::File,
    transport: Transport,
    options: Option<gyro_mouse::Options>,
) -> std::io::Result<Option<gyro_mouse::GyroMouse>> {
    let Some(options) = options else {
        return Ok(None);
    };
    let mouse = gyro_mouse::GyroMouse::open(input, transport, options)
        .map_err(|err| std::io::Error::other(format!("could not start gyro mouse: {err}")))?;
    eprintln!("JoyShockRust Gyro Mouse ready");
    if options.calibrate {
        eprintln!("Set the controller down and keep it still for gyro calibration");
    }
    Ok(Some(mouse))
}

#[cfg(target_os = "linux")]
struct OpenGamepad {
    device: evdev::uinput::VirtualDevice,
    rumble: Option<rumble::Rumble>,
    feedback: Option<std::os::fd::OwnedFd>,
}

#[cfg(target_os = "linux")]
fn open_gamepad(
    input: &std::fs::File,
    bridge: profile::BridgeOptions,
) -> std::io::Result<OpenGamepad> {
    use std::os::fd::AsFd;

    let rumble = if bridge.rumble {
        match rumble::Rumble::open(input) {
            Ok(rumble) => Some(rumble),
            Err(err) => {
                eprintln!("Rumble forwarding unavailable: {err}");
                None
            }
        }
    } else {
        None
    };
    let output = gamepad::open(bridge.gamepad, rumble.is_some())
        .map_err(|err| std::io::Error::other(format!("could not create uinput gamepad: {err}")))?;
    let feedback = rumble
        .as_ref()
        .map(|_| output.as_fd().try_clone_to_owned())
        .transpose()?;
    Ok(OpenGamepad {
        device: output,
        rumble,
        feedback,
    })
}

#[cfg(target_os = "linux")]
struct SessionDevice<'a> {
    identity: Identity,
    filter: discovery::ControllerFilter,
    path: &'a std::path::Path,
    grab_touchpad: bool,
    outputs_paused: Option<&'a std::cell::Cell<bool>>,
}

#[cfg(target_os = "linux")]
fn serve_session(
    input: &mut std::fs::File,
    runtime: &mut profile::RuntimeOptions,
    reload: Option<&reload::Reload>,
    status: Option<&status::StatusSocket>,
    SessionDevice {
        identity,
        filter,
        path,
        grab_touchpad,
        outputs_paused,
    }: SessionDevice<'_>,
    bridge: &mut profile::BridgeOptions,
    mut measurement: Option<&mut benchmark::Benchmark>,
) -> std::io::Result<Option<discovery::ControllerFilter>> {
    use std::{
        cell::{Cell, RefCell},
        time::Instant,
    };

    let mut mouse = open_mouse(input, identity.transport, runtime.gyro)?;
    let mut keyboard = keyboard::Keyboard::open(runtime.keyboard.as_ref(), status)?;
    let keyboard_deadline = Cell::new(None);
    let reload_input = reload.map(|_| input.try_clone()).transpose()?;
    let (mut output, mut rumble, feedback) = if bridge.virtual_gamepad {
        let OpenGamepad {
            device,
            rumble,
            feedback,
        } = open_gamepad(input, *bridge)?;
        (Some(device), rumble, feedback)
    } else {
        (None, None, None)
    };
    let feedback = RefCell::new(feedback);
    let mut state = gamepad::GamepadState::new(bridge.gamepad);
    let mut layer_state = gamepad::RemapState::default();
    let mut touch_click = touchpad::ClickState::default();
    let touchpad_requested = grab_touchpad || runtime.grab_touchpad;
    let mut native_touchpad = touchpad_requested
        .then(|| touchpad::try_grab_native(input))
        .flatten();
    if let Some(status) = status {
        status.update(Some(identity), runtime.gyro.is_some());
        status.set_active_hidraw(path);
        status.set_gamepad_device(output.as_mut());
        status.set_touchpad_policy(touchpad_requested, native_touchpad.is_some());
    }
    let resource_start = measurement
        .as_ref()
        .and_then(|_| benchmark::ResourceSnapshot::capture());
    let mut switch = None;
    let result = forward_reports(
        input,
        identity,
        reload,
        status,
        Some(&feedback),
        ReportTimers {
            measurement: measurement.as_deref_mut(),
            keyboard: Some(&keyboard_deadline),
            ..Default::default()
        },
        |event| {
            let report = match event {
                SessionEvent::Report(report) => report,
                SessionEvent::KeyboardDeadline(now) => {
                    keyboard_deadline.set(None);
                    if let Some(keyboard) = &mut keyboard {
                        keyboard_deadline.set(keyboard.tick(now)?);
                    }
                    return Ok(ControlFlow::Continue(()));
                }
                SessionEvent::Feedback => {
                    if let (Some(rumble), Some(output)) = (&mut rumble, &mut output) {
                        rumble.service(output)?;
                    }
                    return Ok(ControlFlow::Continue(()));
                }
                SessionEvent::Reload(reload) => {
                    let current_filter = filter;
                    let next = match reload.resolve(current_filter) {
                        Some(reload::Update::Profile(next)) => next,
                        Some(reload::Update::Selection(selection))
                            if selection != current_filter =>
                        {
                            switch = Some((Some(selection), reload));
                            return Ok(ControlFlow::Break(()));
                        }
                        Some(reload::Update::Selection(_)) => {
                            reload.applied(status);
                            return Ok(ControlFlow::Continue(()));
                        }
                        Some(reload::Update::OutputsPaused(paused)) => {
                            if paused && outputs_paused.is_some() {
                                switch = Some((None, reload));
                                return Ok(ControlFlow::Break(()));
                            }
                            reload.applied(status);
                            return Ok(ControlFlow::Continue(()));
                        }
                        Some(reload::Update::Gamepad(enabled)) => {
                            if enabled != bridge.virtual_gamepad {
                                if enabled {
                                    let input = reload_input.as_ref().expect("reload descriptor");
                                    match open_gamepad(input, *bridge) {
                                        Ok(OpenGamepad {
                                            device,
                                            rumble: forwarder,
                                            feedback: descriptor,
                                        }) => {
                                            output = Some(device);
                                            rumble = forwarder;
                                            feedback.replace(descriptor);
                                        }
                                        Err(err) => {
                                            reload.rejected(&err);
                                            return Ok(ControlFlow::Continue(()));
                                        }
                                    }
                                } else {
                                    feedback.take();
                                    if let Some(mut device) = output.take()
                                        && let Err(err) = state.publish(
                                            joyshockrust_core::Controls::default(),
                                            |events| device.emit(events),
                                        )
                                    {
                                        eprintln!(
                                            "Could not release virtual gamepad before removing it: {err}"
                                        );
                                    }
                                    rumble = None;
                                    state = gamepad::GamepadState::new(bridge.gamepad);
                                    layer_state = gamepad::RemapState::default();
                                    touch_click = touchpad::ClickState::default();
                                }
                                bridge.virtual_gamepad = enabled;
                                if let Some(status) = status {
                                    status.set_gamepad_enabled(enabled);
                                    status.set_gamepad_device(output.as_mut());
                                }
                            }
                            reload.applied(status);
                            return Ok(ControlFlow::Continue(()));
                        }
                        None => return Ok(ControlFlow::Continue(())),
                    };
                    let replacement_keyboard = if runtime.keyboard != next.keyboard {
                        match keyboard::Keyboard::open(next.keyboard.as_ref(), status) {
                            Ok(device) => Some(device),
                            Err(err) => {
                                reload.rejected(&err);
                                return Ok(ControlFlow::Continue(()));
                            }
                        }
                    } else {
                        None
                    };
                    if let (Some(mouse), Some(current), Some(next)) =
                        (&mut mouse, runtime.gyro, next.gyro)
                        && (gyro_mouse::Options {
                            sensitivity: next.sensitivity,
                            game_calibration: next.game_calibration,
                            ..current
                        }) == next
                    {
                        mouse.set_scaling(next);
                    } else if runtime.gyro != next.gyro {
                        let input = reload_input.as_ref().expect("reload descriptor");
                        match open_mouse(input, identity.transport, next.gyro) {
                            Ok(next_mouse) => mouse = next_mouse,
                            Err(err) => {
                                reload.rejected(&err);
                                return Ok(ControlFlow::Continue(()));
                            }
                        }
                    }
                    if runtime.remaps != next.remaps {
                        layer_state = gamepad::RemapState::default();
                    }
                    if runtime.touchpad != next.touchpad {
                        touch_click = touchpad::ClickState::default();
                    }
                    if let Some(replacement) = replacement_keyboard {
                        if let Some(previous) = &mut keyboard
                            && let Err(err) = previous.release()
                        {
                            reload.rejected(&err);
                            return Err(err);
                        }
                        keyboard = replacement;
                        keyboard_deadline.set(None);
                    }
                    *runtime = *next;
                    let touchpad_requested = grab_touchpad || runtime.grab_touchpad;
                    if !touchpad_requested {
                        native_touchpad.take();
                    } else if native_touchpad.is_none() {
                        let input = reload_input.as_ref().expect("reload descriptor");
                        native_touchpad = touchpad::try_grab_native(input);
                    }
                    if let Some(status) = status {
                        status.update(Some(identity), runtime.gyro.is_some());
                        status.set_touchpad_policy(touchpad_requested, native_touchpad.is_some());
                    }
                    reload.applied(status);
                    return Ok(ControlFlow::Continue(()));
                }
            };
            if let Some(status) = status {
                status.observe_battery(report.battery);
            }
            if let Some(keyboard) = &mut keyboard {
                let now = Instant::now();
                if keyboard_deadline
                    .get()
                    .is_some_and(|deadline| deadline <= now)
                {
                    // A deadline can fall between two reports in the same read batch.
                    keyboard.tick(now)?;
                }
                keyboard_deadline.set(keyboard.publish(report, now)?);
            }
            let gamepad_batch = if let Some(output) = &mut output {
                let controls =
                    map_gamepad_controls(runtime, &report, &mut layer_state, &mut touch_click);
                state.publish(controls, |events| output.emit(events))?
            } else {
                false
            };
            let mouse_batch = if let Some(mouse) = &mut mouse {
                mouse.publish(report)?
            } else {
                false
            };
            if let Some(status) = status
                && (gamepad_batch || mouse_batch)
            {
                status.observe_outputs(gamepad_batch, mouse_batch);
            }
            Ok(ControlFlow::Continue(()))
        },
    );
    if let Some(measurement) = measurement {
        measurement.finish_resources(resource_start);
    }
    let release_gamepad = if let Some(output) = &mut output {
        state
            .publish(joyshockrust_core::Controls::default(), |events| {
                output.emit(events)
            })
            .map(|_| ())
    } else {
        Ok(())
    };
    let release_keyboard = if let Some(keyboard) = &mut keyboard {
        keyboard.release()
    } else {
        Ok(())
    };
    let result = result.and(release_gamepad).and(release_keyboard);
    drop(native_touchpad);
    if let Some((selection, request)) = switch {
        if let Err(err) = &result {
            request.rejected(err);
        } else {
            // Close all old virtual outputs before acknowledging the new filter.
            drop(feedback);
            drop(output);
            drop(mouse);
            drop(keyboard);
            drop(rumble);
            if let Some(status) = status {
                status.update(None, runtime.gyro.is_some());
            }
            if selection.is_none()
                && let Some(outputs_paused) = outputs_paused
            {
                outputs_paused.set(true);
                if let Some(status) = status {
                    status.set_outputs_paused(true);
                }
            }
            request.applied(status);
            return Ok(Some(selection.unwrap_or(filter)));
        }
    }
    if let Some(status) = status {
        status.update(None, runtime.gyro.is_some());
    }
    result.map(|_| None)
}

#[cfg(target_os = "linux")]
fn map_gamepad_controls(
    runtime: &profile::RuntimeOptions,
    report: &joyshockrust_input::InputReport,
    layer_state: &mut gamepad::RemapState,
    touch_click: &mut touchpad::ClickState,
) -> joyshockrust_core::Controls {
    let mut controls = runtime
        .stick_curve
        .map_or(report.controls, |curve| curve.apply(report.controls));
    if runtime
        .gyro
        .is_some_and(|options| options.flick_stick.is_some())
    {
        controls.right_stick = Default::default();
    }
    let touch_buttons = runtime
        .touchpad
        .as_ref()
        .map_or_else(Default::default, |grid| {
            grid.map(&mut controls.buttons, report.touch_contacts, touch_click)
        });
    let mut controls = runtime.remaps.apply(controls, layer_state);
    controls.buttons = controls.buttons.union(touch_buttons);
    controls
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("joyshockrust-linux requires Linux hidraw and uinput");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
enum SessionEvent<'a> {
    Report(joyshockrust_input::InputReport),
    Feedback,
    Reload(reload::Request<'a>),
    KeyboardDeadline(std::time::Instant),
}

#[cfg(target_os = "linux")]
const REPORT_BATCH_LIMIT: u64 = 32;

#[cfg(target_os = "linux")]
#[derive(Default)]
struct ReportTimers<'a> {
    measurement: Option<&'a mut benchmark::Benchmark>,
    stop_at: Option<std::time::Instant>,
    keyboard: Option<&'a std::cell::Cell<Option<std::time::Instant>>>,
}

#[cfg(target_os = "linux")]
fn forward_reports<'a>(
    input: &mut std::fs::File,
    identity: Identity,
    reload: Option<&'a reload::Reload>,
    status: Option<&status::StatusSocket>,
    feedback: Option<&std::cell::RefCell<Option<std::os::fd::OwnedFd>>>,
    timers: ReportTimers<'_>,
    mut publish: impl FnMut(SessionEvent<'a>) -> std::io::Result<ControlFlow<()>>,
) -> std::io::Result<()> {
    use joyshockrust_input::parse_report;
    use nix::poll::PollFlags;
    use std::{
        io::{self, Read},
        os::fd::{AsFd, AsRawFd},
        time::Instant,
    };

    // One read consumes one complete HID report. A larger buffer preserves
    // length validation when a node delivers a larger report than expected.
    let mut report = [0; 16_384];
    let mut measurement = timers.measurement;
    let benchmark_deadline = measurement
        .as_ref()
        .map(|measurement| Instant::now() + measurement.timeout);
    loop {
        let deadline = benchmark_deadline
            .into_iter()
            .chain(timers.keyboard.and_then(std::cell::Cell::get))
            .chain(timers.stop_at)
            .min();
        let ((ready, request, mut feedback_ready), polled_feedback) = {
            let feedback = feedback.map(std::cell::RefCell::borrow);
            let descriptor = feedback
                .as_deref()
                .and_then(Option::as_ref)
                .map(AsFd::as_fd);
            (
                reload::wait(input.as_fd(), reload, status, descriptor, deadline)?,
                descriptor.map(|fd| fd.as_raw_fd()),
            )
        };
        if let Some(request) = request {
            if publish(SessionEvent::Reload(request))?.is_break() {
                return Ok(());
            }
            let current_feedback =
                feedback.and_then(|socket| socket.borrow().as_ref().map(AsRawFd::as_raw_fd));
            // A control request can replace the polled descriptor. Read feedback on the next wake.
            if current_feedback != polled_feedback {
                feedback_ready = PollFlags::empty();
            }
        }
        if ready.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Ok(());
        }
        if timers
            .stop_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Keep the controller still on a stable surface, then retry.",
            ));
        }
        if let (Some(deadline), Some(measurement)) =
            (benchmark_deadline, measurement.as_deref_mut())
            && Instant::now() >= deadline
        {
            measurement.timed_out = true;
            return Ok(());
        }
        if let Some(deadline) = timers.keyboard.and_then(std::cell::Cell::get) {
            let now = Instant::now();
            if now >= deadline && publish(SessionEvent::KeyboardDeadline(now))?.is_break() {
                return Ok(());
            }
        }
        if feedback_ready.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL)
        {
            return Err(io::Error::other("virtual force feedback descriptor lost"));
        }
        if feedback_ready.contains(PollFlags::POLLIN) && publish(SessionEvent::Feedback)?.is_break()
        {
            return Ok(());
        }
        if !ready.contains(PollFlags::POLLIN) {
            continue;
        }
        let mut decoded = 0;
        let mut rejected = 0;
        for _ in 0..REPORT_BATCH_LIMIT {
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
            match parse_report(identity.model, identity.transport, &report[..len], None) {
                Ok(report) => {
                    decoded += 1;
                    let (progress, complete) = if let Some(measurement) = measurement.as_deref_mut()
                    {
                        let started = Instant::now();
                        let progress = publish(SessionEvent::Report(report))?;
                        measurement.record(started, started.elapsed(), report.imu.device_ticks);
                        (progress, measurement.complete())
                    } else {
                        (publish(SessionEvent::Report(report))?, false)
                    };
                    if progress.is_break() || complete {
                        if let Some(status) = status {
                            status.observe_reports(decoded, rejected);
                        }
                        return Ok(());
                    }
                }
                Err(_) => {
                    rejected += 1;
                    if let Some(measurement) = measurement.as_deref_mut() {
                        measurement.rejected += 1;
                    }
                }
            }
        }
        if let Some(status) = status
            && (decoded > 0 || rejected > 0)
        {
            status.observe_reports(decoded, rejected);
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

    use super::Identity;
    use joyshockrust_core::Button;
    use joyshockrust_input::{DeviceModel, Transport};

    const USB_EDGE: Identity = Identity {
        model: DeviceModel::DualSenseEdge,
        transport: Transport::Usb,
    };

    #[test]
    fn readiness_loop_ends_when_input_disconnects() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(sender);
        let mut input = File::from(OwnedFd::from(receiver));
        super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers::default(),
            |_| panic!("disconnected input published controls"),
        )
        .unwrap();
    }

    #[test]
    fn readiness_loop_services_feedback_when_input_is_idle() {
        let (_sender, receiver) = UnixDatagram::pair().unwrap();
        let (game, feedback) = UnixDatagram::pair().unwrap();
        let feedback = std::cell::RefCell::new(Some(feedback.into()));
        game.send(b"feedback").unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let result = super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            Some(&feedback),
            super::ReportTimers::default(),
            |event| {
                assert!(matches!(event, super::SessionEvent::Feedback));
                Err(io::ErrorKind::BrokenPipe.into())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn steady_state_report_replay_does_not_allocate() {
        const FRAMES: usize = 64;
        let profile = super::profile::parse(
            r#"
            version = 1
            stick_curve = 1.5
            remap_buttons = ["cross=triangle"]
            layer_remaps = ["l1:cross=circle"]
            [touchpad]
            rows = 1
            columns = 1
            [[touchpad.regions]]
            row = 1
            column = 1
            touch = "square"
            click = "r3"
            "#,
        )
        .unwrap();
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[37] = 0x80;
        for index in 0..FRAMES {
            let active = index % 2 == 0;
            frame[1] = if active { 196 } else { 128 };
            frame[8] = if active { 0x28 } else { 8 };
            frame[9] = u8::from(active);
            frame[10] = if active { 2 } else { 0 };
            frame[33] = if active { 1 } else { 0x80 };
            sender.send(&frame).unwrap();
        }
        // A zero-length datagram exits the read loop like end of input.
        sender.send(&[]).unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let mut pad = super::gamepad::GamepadState::default();
        let mut layer = super::gamepad::RemapState::default();
        let mut click = super::touchpad::ClickState::default();
        let mut reports = 0;
        let mut batches = 0;
        let allocations = allocation_counter::measure(|| {
            super::forward_reports(
                &mut input,
                USB_EDGE,
                None,
                None,
                None,
                super::ReportTimers::default(),
                |event| {
                    let super::SessionEvent::Report(report) = event else {
                        unreachable!()
                    };
                    let controls =
                        super::map_gamepad_controls(&profile, &report, &mut layer, &mut click);
                    if pad.publish(controls, |events| {
                        std::hint::black_box(events);
                        Ok(())
                    })? {
                        batches += 1;
                    }
                    reports += 1;
                    Ok(super::ControlFlow::Continue(()))
                },
            )
            .unwrap();
        });
        assert_eq!(reports, FRAMES);
        assert_eq!(batches, FRAMES);
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn benchmark_stops_at_the_requested_number_of_decoded_reports() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut valid = [0; 64];
        valid[0] = 1;
        sender.send(&[1; 65]).unwrap();
        for _ in 0..3 {
            sender.send(&valid).unwrap();
        }
        let mut input = File::from(OwnedFd::from(receiver));
        let mut measurement =
            super::benchmark::Benchmark::new(2, super::benchmark::DEFAULT_TIMEOUT);
        let mut published = 0;
        super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers {
                measurement: Some(&mut measurement),
                ..Default::default()
            },
            |event| {
                assert!(matches!(event, super::SessionEvent::Report(_)));
                published += 1;
                Ok(super::ControlFlow::Continue(()))
            },
        )
        .unwrap();
        assert_eq!(published, 2);
        assert_eq!(measurement.rejected, 1);
        assert!(measurement.complete());
    }

    #[test]
    fn benchmark_times_out_with_an_open_but_silent_input() {
        use std::{sync::mpsc, thread, time::Duration};

        let (sender, receiver) = UnixStream::pair().unwrap();
        let (finish, guard) = mpsc::channel::<()>();
        let watchdog = thread::spawn(move || {
            // Close the stream if a regression leaves the test stuck in poll.
            let _ = guard.recv_timeout(Duration::from_secs(1));
            drop(sender);
        });
        let mut input = File::from(OwnedFd::from(receiver));
        let mut measurement = super::benchmark::Benchmark::new(2, Duration::from_millis(20));
        let resource_start = super::benchmark::ResourceSnapshot::capture();
        let result = super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers {
                measurement: Some(&mut measurement),
                ..Default::default()
            },
            |_| panic!("silent input produced an event"),
        );
        drop(finish);
        watchdog.join().unwrap();
        result.unwrap();
        assert!(measurement.timed_out);
        assert!(!measurement.complete());
        measurement.finish_resources(resource_start);
        let mut report = Vec::new();
        measurement.write(USB_EDGE, false, &mut report).unwrap();
        let report = String::from_utf8(report).unwrap();
        assert!(report.lines().any(|line| line == "benchmark_end=timeout"));
        assert!(report.lines().any(|line| line == "reports_measured=0"));
        assert!(
            report
                .lines()
                .any(|line| line.starts_with("benchmark_cpu_percent="))
        );
    }

    #[test]
    fn readiness_loop_services_keyboard_deadlines_while_input_is_idle() {
        use std::{
            cell::Cell,
            time::{Duration, Instant},
        };
        let (_sender, receiver) = UnixDatagram::pair().unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let mut benchmark = super::benchmark::Benchmark::new(2, Duration::from_millis(500));
        let deadline = Instant::now() + Duration::from_millis(20);
        let keyboard = Cell::new(Some(deadline));
        let mut fired = false;
        super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers {
                measurement: Some(&mut benchmark),
                keyboard: Some(&keyboard),
                ..Default::default()
            },
            |event| {
                let super::SessionEvent::KeyboardDeadline(now) = event else {
                    unreachable!()
                };
                assert!(now >= deadline);
                fired = true;
                Ok(super::ControlFlow::Break(()))
            },
        )
        .unwrap();
        assert!(fired);
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
        let result = super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers::default(),
            |event| {
                let super::SessionEvent::Report(report) = event else {
                    unreachable!()
                };
                presses.push(report.controls.buttons.contains(Button::South));
                if presses.len() == 2 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Ok(super::ControlFlow::Continue(()))
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(presses, [true, false]);
    }
}

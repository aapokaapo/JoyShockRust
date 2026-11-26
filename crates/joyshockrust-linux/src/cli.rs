use crate::{
    Emulation, SessionDevice, adaptive_trigger, benchmark, controller_config, diagnostics,
    discovery, game_calibration, gamepad, gyro_calibration, gyro_gate, gyro_mouse, haptics, hidraw,
    launch, profile, reload, serve_session, status,
};

pub(super) fn run() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    use std::{env, path::PathBuf, process::ExitCode, time::Duration};

    use gyro_gate::{Binding, TriggerGate};
    use joyshockrust_core::Button;

    const USAGE: &str = concat!(
        "usage: joyshockrust-linux [--profile PATH|--profile-if-present PATH] [/dev/hidrawN]\n",
        "       joyshockrust-linux --check-profile PATH\n",
        "       joyshockrust-linux --test-haptics [--pattern all|start|done|ready|detent|sweep] [--pcm ALSA_DEVICE] [/dev/hidrawN]\n",
        "       joyshockrust-linux --test-adaptive-triggers [left|right|both] [/dev/hidrawN]\n",
        "       joyshockrust-linux --install-profile SOURCE [DESTINATION]\n",
        "       joyshockrust-linux --diagnose [--profile PATH] [--show-controller-id] [CONTROLLER_OPTIONS] [/dev/hidrawN]\n",
        "       joyshockrust-linux --list-controllers [--show-controller-id] [CONTROLLER_OPTIONS]\n",
        "       joyshockrust-linux --status [SOCKET]\n",
        "       joyshockrust-linux --reload-profile [SOCKET]\n",
        "       joyshockrust-linux --load-profile PATH [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --unload-profile [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --set-gamepad on|off [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --set-gyro-mouse on|off [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --set-emulation on|off [RELOAD_SOCKET]\n",
        "       joyshockrust-linux --set-emulation-master on|off [RELOAD_SOCKET]\n",
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
    let mut set_emulation = None;
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
    if args.next_if(|arg| arg == "--test-haptics").is_some() {
        haptics::test_cli(args)?;
        return Ok(ExitCode::SUCCESS);
    }
    if args
        .next_if(|arg| arg == "--test-adaptive-triggers")
        .is_some()
    {
        adaptive_trigger::test_cli(args)?;
        return Ok(ExitCode::SUCCESS);
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
        } else if let Some(target) = arg.to_str().and_then(reload::OutputTarget::from_flag) {
            if set_emulation.is_some() {
                return Err("select one emulation command".into());
            }
            let enabled = match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
                Some("on") => true,
                Some("off") => false,
                _ => {
                    return Err(
                        format!("expected on or off after {}", arg.to_string_lossy()).into(),
                    );
                }
            };
            set_emulation = Some((target, enabled));
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
            const EXPECTED: &str = "expected generic, xbox360 or xbox-series after --gamepad";
            gamepad_kind = Some(
                value
                    .to_str()
                    .ok_or(EXPECTED)?
                    .parse()
                    .map_err(|_| EXPECTED)?,
            );
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
        + u8::from(set_emulation.is_some())
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
            } else if let Some((target, enabled)) = set_emulation {
                reload::set_emulation(target, enabled, path.as_deref())
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
            } else if let Some((target, enabled)) = set_emulation {
                let label = match target {
                    reload::OutputTarget::Gamepad => "Virtual gamepad",
                    reload::OutputTarget::GyroMouse => "Gyro mouse",
                    reload::OutputTarget::Both => "Gamepad and gyro mouse",
                    reload::OutputTarget::Master => "Emulation master switch",
                };
                println!("{label} {}", if enabled { "on" } else { "off" });
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
    let selection_path = profile_path.as_ref().and_then(|(path, mode)| {
        (*mode == ProfileMode::RunIfPresent)
            .then(|| profile::selection_path(path))
            .flatten()
    });
    let restored = selection_path
        .as_deref()
        .and_then(profile::restore_selection);
    let restoring = restored.is_some();
    let (mut runtime, startup_profile, profile_source, active_profile) =
        if let Some((profile_path, mode)) = profile_path {
            if mapping_options {
                return Err("use either a TOML profile or command-line mapping options".into());
            }
            if mode == ProfileMode::Install {
                if bridge_options || selection_options {
                    return Err(
                        "--install-profile takes a source and an optional destination".into(),
                    );
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
            if mode == ProfileMode::Check && (path.is_some() || bridge_options || selection_options)
            {
                return Err("--check-profile takes only a profile path".into());
            }
            if let Some((runtime, active)) = restored {
                let source = if active.is_some() {
                    ProfileSource::Loaded
                } else {
                    ProfileSource::Defaults
                };
                (runtime, Some(profile_path), source, active)
            } else {
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
                let active = matches!(source, ProfileSource::Startup).then(|| profile_path.clone());
                (profile, Some(profile_path), source, active)
            }
        } else {
            let source = if mapping_options {
                ProfileSource::CommandLine
            } else {
                ProfileSource::Defaults
            };
            (default_runtime(), None, source, None)
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
        let path = active_profile.as_deref();
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
    let startup_bridge = if restoring {
        startup_profile
            .as_deref()
            .and_then(|path| profile::load(path).ok())
            .and_then(|runtime| runtime.bridge)
    } else {
        runtime.bridge
    };
    let configured_bridge = startup_bridge.unwrap_or_default().startup();
    let bridge_at_startup = profile::BridgeOptions {
        gamepad: gamepad_kind.unwrap_or(configured_bridge.gamepad),
        rumble: rumble_enabled || configured_bridge.rumble,
        ..configured_bridge
    };
    let mut bridge = profile::BridgeOptions {
        gamepad: gamepad_kind
            .or_else(|| runtime.bridge.and_then(|settings| settings.gamepad))
            .unwrap_or(bridge_at_startup.gamepad),
        ..bridge_at_startup
    };
    let reload = if startup_profile.is_some() || reload_path.is_some() {
        Some(reload::Reload::new(
            startup_profile,
            reload_path.as_deref(),
            bridge_at_startup,
            path.is_none() && benchmark_target.is_none(),
            selection_path,
        )?)
    } else {
        None
    };
    let reload = reload.as_ref();
    let _inventory = status_path
        .as_deref()
        .filter(|_| benchmark_target.is_none())
        .and_then(|path| match crate::inventory::Worker::start(path) {
            Ok(worker) => Some(worker),
            Err(error) => {
                eprintln!("Controller inventory unavailable: {error}");
                None
            }
        });
    if let Some(status) = status {
        status.set_filter(filter);
        status.set_gamepad_enabled(bridge.virtual_gamepad);
        status.set_gamepad_kind(bridge.gamepad);
    }
    let mut emulation = Emulation::default();
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
                emulation: &mut emulation,
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
                emulation: &mut emulation,
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
                        emulation: &mut emulation,
                    },
                    &mut bridge,
                    None,
                ),
                discovery::Event::Reload(reload, current_filter) => {
                    let selection = match reload.resolve(current_filter) {
                        Some(reload::Update::Profile(next)) => {
                            bridge.gamepad = reload.profile_gamepad(&next);
                            runtime = *next;
                            if let Some(status) = status {
                                status.update(None, runtime.gyro.is_some());
                                status.set_gamepad_kind(bridge.gamepad);
                                status.set_touchpad_policy(
                                    grab_touchpad || runtime.grab_touchpad,
                                    false,
                                );
                            }
                            None
                        }
                        Some(reload::Update::Selection(selection)) => Some(selection),
                        Some(reload::Update::Emulation(target, enabled)) => {
                            let Some((next, gamepad)) =
                                emulation.changed(target, enabled, bridge.virtual_gamepad)
                            else {
                                reload.rejected("emulation is disabled by the master switch");
                                return Ok(None);
                            };
                            emulation = next;
                            bridge.virtual_gamepad = gamepad;
                            emulation.publish(status, gamepad);
                            None
                        }
                        Some(reload::Update::OutputsPaused(paused)) => {
                            outputs_paused.set(paused);
                            if let Some(status) = status {
                                status.set_outputs_paused(paused);
                            }
                            None
                        }
                        Some(
                            reload::Update::TriggerPreview(_) | reload::Update::TriggerTravel(_),
                        )
                        | None => return Ok(None),
                    };
                    reload.applied(status);
                    Ok(selection)
                }
            },
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

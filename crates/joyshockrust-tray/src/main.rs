use std::{
    env, fs,
    io::{self, Read},
    os::unix::{ffi::OsStringExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command},
    sync::mpsc,
    time::{Duration, Instant},
};

use ksni::{
    MenuItem,
    blocking::TrayMethods,
    menu::{CheckmarkItem, StandardItem, SubMenu},
};

const BRIDGE_SERVICE: &str = "joyshockrust.service";
const DESKTOP_ID: &str = "io.github.aapokaapo.JoyShockRust.desktop";
const DESKTOP_ENTRY: &str =
    include_str!("../../../dist/desktop/io.github.aapokaapo.JoyShockRust.desktop");
const REFRESH: Duration = Duration::from_secs(1);
const INVENTORY_REFRESH: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Controller {
    node: String,
    id: String,
    model: &'static str,
    transport: &'static str,
}

impl Controller {
    fn from_inventory(line: &str) -> Option<Self> {
        let (node, description) = line.split_once(": ")?;
        let (description, id) = description.split_once(", controller_id=")?;
        let suffix = node.strip_prefix("/dev/hidraw")?;
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if id == "unavailable" {
            return None;
        }
        let (controller, _) = description.split_once(", ")?;
        let (transport, model) = controller.split_once(' ')?;
        let (transport, model) = (
            match transport {
                "wired" => "usb",
                "Bluetooth" => "bluetooth",
                _ => return None,
            },
            match model {
                "DualSense" => "dualsense",
                "DualSense Edge" => "dualsense-edge",
                _ => return None,
            },
        );
        Some(Self {
            node: node.into(),
            id: id.into(),
            model,
            transport,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    controller: String,
    connection_icon: &'static str,
    rate: String,
    battery: Option<String>,
    active_node: Option<String>,
    profile: String,
    active_profile: Option<PathBuf>,
    outputs_paused: bool,
    gamepad_enabled: Option<bool>,
    gyro_mouse_enabled: Option<bool>,
}

impl Status {
    fn parse(reply: &str) -> Option<Self> {
        let raw_field = |key| {
            reply
                .lines()
                .filter_map(|line| line.split_once('='))
                .find_map(|(name, value)| (name == key).then_some(value))
        };
        let field = |key| raw_field(key).filter(|&value| value != "none");
        if field("version") != Some("1") {
            return None;
        }
        let connected = match field("controller")? {
            "connected" => true,
            "waiting" => false,
            _ => return None,
        };
        let model = match field("model") {
            Some("dualsense") => "DualSense",
            Some("dualsense_edge") => "DualSense Edge",
            _ => "Controller",
        };
        let transport = field("transport").filter(|_| connected);
        let connection_icon = match transport {
            Some("usb") => "drive-harddisk-usb-symbolic",
            Some("bluetooth") => "bluetooth-symbolic",
            _ => "",
        };
        let controller = if connected {
            model
        } else {
            "No active controller"
        }
        .into();
        let rate = field("report_rate_hz")
            .filter(|_| connected)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map_or_else(|| "—".into(), |rate| format!("{rate:.1} Hz"));
        let battery = match field("battery_state").filter(|_| transport == Some("bluetooth")) {
            Some("discharging") => Some(
                field("battery_percent")
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|&value| value <= 100)
                    .map_or_else(
                        || "🔋".into(),
                        |value| {
                            let icon = if value <= 10 { "🪫" } else { "🔋" };
                            format!("{icon} {value}%")
                        },
                    ),
            ),
            Some("charging") => Some("🔋⚡".into()),
            Some("full") => Some("🔋".into()),
            Some("not_charging") => Some("🔋⏸".into()),
            _ => None,
        };
        let fallback_profile = match field("profile_source") {
            Some("loaded") => "Loaded profile",
            Some("startup") => "Startup profile",
            Some("command_line") => "Command-line mappings",
            _ => "Default mappings",
        };
        let profile = raw_field("profile_name")
            .filter(|name| !name.is_empty())
            .unwrap_or(fallback_profile);
        let enabled = |key| field(key).and_then(|value| value.parse::<bool>().ok());
        Some(Self {
            controller,
            connection_icon,
            rate,
            battery,
            active_node: field("active_hidraw")
                .filter(|_| connected)
                .map(str::to_owned),
            profile: profile.into(),
            active_profile: field("profile_path_hex").and_then(decode_profile_path),
            outputs_paused: field("outputs_paused") == Some("true"),
            gamepad_enabled: enabled("virtual_gamepad_enabled"),
            gyro_mouse_enabled: enabled("gyro_mouse_enabled"),
        })
    }
}

fn decode_profile_path(hex: &str) -> Option<PathBuf> {
    let (chunks, remainder) = hex.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let bytes: Vec<u8> = chunks
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect::<Option<_>>()?;
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

#[derive(Clone, Debug)]
enum Action {
    Select(Controller),
    AutoSelect,
    Load(PathBuf),
    Edit,
    CalibrateGame,
    CalibrateGyro,
    Unload,
    ResumeOutputs,
    SetEmulation(&'static str, bool),
    Start,
    Autostart(bool),
    Quit,
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct TrayState {
    status: Option<Status>,
    inventory: Vec<Controller>,
    profiles: Vec<PathBuf>,
    autostart: bool,
    calibrating: bool,
    editor_open: bool,
    error: Option<String>,
}

struct Tray {
    actions: mpsc::Sender<Action>,
    state: TrayState,
}

fn item(label: impl Into<String>, action: Action) -> MenuItem<Tray> {
    StandardItem {
        label: label.into().replace('_', "__"),
        activate: Box::new(move |tray: &mut Tray| {
            let _ = tray.actions.send(action.clone());
        }),
        ..Default::default()
    }
    .into()
}

fn label(text: impl Into<String>) -> MenuItem<Tray> {
    StandardItem {
        label: text.into().replace('_', "__"),
        enabled: false,
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for Tray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "joyshockrust".into()
    }
    fn title(&self) -> String {
        "JoyShockRust".into()
    }
    fn icon_name(&self) -> String {
        "input-gaming".into()
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let state = &self.state;
        let mut menu = if let Some(status) = &state.status {
            vec![
                StandardItem {
                    label: status.battery.as_ref().map_or_else(
                        || status.rate.clone(),
                        |battery| format!("{} · {battery}", status.rate),
                    ),
                    icon_name: status.connection_icon.into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ]
        } else {
            vec![
                label("Bridge unavailable"),
                item("Start bridge", Action::Start),
            ]
        };
        if let Some(error) = &state.error {
            menu.push(label(error));
        }
        if state.calibrating {
            menu.push(label("Calibration in progress"));
            return menu;
        }
        if state
            .status
            .as_ref()
            .is_some_and(|status| status.outputs_paused)
        {
            menu.push(item("Resume virtual outputs", Action::ResumeOutputs));
        }
        let mut controllers = vec![item("Automatic", Action::AutoSelect)];
        for controller in &state.inventory {
            let active = state
                .status
                .as_ref()
                .and_then(|status| status.active_node.as_deref())
                == Some(controller.node.as_str());
            let prefix = if active { "✓ " } else { "" };
            let model = if controller.model == "dualsense-edge" {
                "DualSense Edge"
            } else {
                "DualSense"
            };
            let transport = if controller.transport == "usb" {
                "USB"
            } else {
                "Bluetooth"
            };
            controllers.push(item(
                format!("{prefix}{model} · {transport} ({})", controller.node),
                Action::Select(controller.clone()),
            ));
        }
        menu.push(label("Active controller"));
        menu.push(
            SubMenu {
                label: state
                    .status
                    .as_ref()
                    .map_or("No active controller", |status| &status.controller)
                    .into(),
                enabled: state.status.is_some(),
                submenu: controllers,
                ..Default::default()
            }
            .into(),
        );
        let status = state.status.as_ref();
        let active_profile = status.and_then(|status| status.active_profile.as_ref());
        let mut profiles = vec![item("Default mappings", Action::Unload)];
        for path in &state.profiles {
            if let Some(name) = path.file_stem() {
                let name = name.to_string_lossy();
                let prefix = if Some(path) == active_profile {
                    "✓ "
                } else {
                    ""
                };
                profiles.push(item(format!("{prefix}{name}"), Action::Load(path.clone())));
            }
        }
        menu.push(label("Active profile"));
        menu.push(
            SubMenu {
                label: status
                    .map_or("No active profile", |status| &status.profile)
                    .replace('_', "__"),
                enabled: state.status.is_some(),
                submenu: profiles,
                ..Default::default()
            }
            .into(),
        );
        if state
            .status
            .as_ref()
            .is_some_and(|status| status.active_node.is_some())
        {
            menu.push(item("Calibrate Controller Gyro", Action::CalibrateGyro));
            menu.push(item(
                "Calibrate game sensitivity (RWC)",
                Action::CalibrateGame,
            ));
        }

        let gamepad = status.and_then(|status| status.gamepad_enabled);
        let gyro = status.and_then(|status| status.gyro_mouse_enabled);
        let both = gamepad.zip(gyro).map(|(gamepad, gyro)| gamepad && gyro);
        for (text, flag, setting) in [
            ("Gamepad emulation", "--set-gamepad", gamepad),
            ("Gyro mouse", "--set-gyro-mouse", gyro),
            ("Gamepad and gyro mouse", "--set-emulation", both),
        ] {
            let checked = setting.unwrap_or(false);
            menu.push(
                CheckmarkItem {
                    label: text.into(),
                    checked,
                    enabled: setting.is_some()
                        && status.is_some_and(|status| !status.outputs_paused),
                    activate: Box::new(move |tray: &mut Tray| {
                        let _ = tray.actions.send(Action::SetEmulation(flag, !checked));
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        if state.editor_open {
            menu.push(label("Profile editor"));
        } else {
            menu.push(item("Profile editor", Action::Edit));
        }
        menu.push(MenuItem::Separator);
        menu.push(
            CheckmarkItem {
                label: "Start with system".into(),
                checked: state.autostart,
                activate: Box::new(|tray: &mut Tray| {
                    let _ = tray.actions.send(Action::Autostart(!tray.state.autostart));
                }),
                ..Default::default()
            }
            .into(),
        );
        if state.editor_open {
            menu.push(label("Close the profile editor before quitting"));
        } else {
            menu.push(item("Quit JoyShockRust", Action::Quit));
        }
        menu
    }
}

struct Desktop {
    bridge: PathBuf,
    config: PathBuf,
    status_socket: PathBuf,
}

impl Desktop {
    fn autostart_file(&self) -> PathBuf {
        self.config.join("autostart").join(DESKTOP_ID)
    }

    fn profile_paths(&self) -> Vec<PathBuf> {
        let root = self.config.join("joyshockrust");
        let mut paths: Vec<_> = fs::read_dir(root.join("profiles"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
            .collect();
        paths.sort();
        let startup = root.join("profile.toml");
        if startup.is_file() {
            paths.insert(0, startup);
        }
        paths
    }

    fn command(&self, args: &[&str]) -> io::Result<String> {
        output(Command::new(&self.bridge).args(args))
    }

    fn status(&self) -> io::Result<Option<Status>> {
        let stream = UnixStream::connect(&self.status_socket)?;
        stream.set_read_timeout(Some(REFRESH))?;
        let mut reply = String::new();
        stream.take(64 * 1024).read_to_string(&mut reply)?;
        Ok(Status::parse(&reply))
    }

    fn set_outputs_paused(&self, paused: bool) -> io::Result<()> {
        self.command(&["--set-outputs", if paused { "pause" } else { "resume" }])
            .map(|_| ())
    }

    fn calibrate(&self, game: bool, status: &Status) -> io::Result<Child> {
        let node = status
            .active_node
            .as_deref()
            .filter(|node| {
                node.strip_prefix("/dev/hidraw").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
            .ok_or_else(|| io::Error::other("connect a controller to calibrate"))?;
        let mut dialog = Command::new(self.bridge.with_file_name("joyshockrust-editor"));
        if game {
            dialog.args(["--calibrate-game-select", node]);
            if let Some(profile) = &status.active_profile {
                dialog.arg("--active-profile").arg(profile);
            }
            dialog.args(self.profile_paths());
        } else {
            dialog.args(["--calibrate-controller-gyro", node]);
        }
        self.set_outputs_paused(true)?;
        match dialog.spawn() {
            Ok(child) => Ok(child),
            Err(error) => {
                if let Err(resume) = self.set_outputs_paused(false) {
                    return Err(io::Error::other(format!(
                        "{error}; could not resume outputs: {resume}"
                    )));
                }
                Err(error)
            }
        }
    }

    fn run(&self, action: &Action) -> io::Result<()> {
        match action {
            Action::Select(controller) => {
                let transport = if controller.transport == "usb" {
                    "--usb"
                } else {
                    "--bluetooth"
                };
                self.command(&[
                    "--select-controller",
                    &controller.id,
                    "--controller-model",
                    controller.model,
                    transport,
                ])?;
            }
            Action::AutoSelect => {
                self.command(&[
                    "--select-controller",
                    "any",
                    "--controller-model",
                    "any",
                    "--auto-transport",
                ])?;
            }
            Action::Load(path) => {
                output(Command::new(&self.bridge).arg("--load-profile").arg(path))?;
            }
            Action::Edit => unreachable!("the editor is handled by the tray loop"),
            Action::Unload => {
                self.command(&["--unload-profile"])?;
            }
            Action::CalibrateGame | Action::CalibrateGyro => {
                unreachable!("calibration is handled by the tray loop")
            }
            Action::ResumeOutputs => self.set_outputs_paused(false)?,
            Action::SetEmulation(flag, enabled) => {
                self.command(&[flag, if *enabled { "on" } else { "off" }])?;
            }

            Action::Start => {
                bridge_service("start")?;
            }
            Action::Autostart(enabled) => {
                let directory = self.config.join("autostart");
                let path = self.autostart_file();
                if *enabled {
                    fs::create_dir_all(directory)?;
                    fs::write(path, DESKTOP_ENTRY)?;
                } else if let Err(error) = fs::remove_file(path)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    return Err(error);
                }
            }
            Action::Quit => {
                bridge_service("stop")?;
            }
        }
        Ok(())
    }
}

fn output(command: &mut Command) -> io::Result<String> {
    let output = command.output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        Err(io::Error::other(if message.is_empty() {
            format!(
                "{}: {}",
                command.get_program().to_string_lossy(),
                output.status
            )
        } else {
            message.into()
        }))
    }
}

fn bridge_service(action: &str) -> io::Result<()> {
    output(Command::new("systemctl").args(["--user", action, BRIDGE_SERVICE])).map(|_| ())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("HOME must be an absolute path")?;
    let desktop = Desktop {
        bridge: env::current_exe()?.with_file_name("joyshockrust-linux"),
        config: env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config")),
        status_socket: env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or("XDG_RUNTIME_DIR must be an absolute path")?
            .join("joyshockrust/status.sock"),
    };
    let (sender, receiver) = mpsc::channel();
    let tray = Tray {
        actions: sender,
        state: TrayState::default(),
    }
    .spawn()?;
    let mut state = TrayState {
        error: desktop
            .run(&Action::Start)
            .err()
            .map(|error| format!("Start failed: {error}")),
        ..TrayState::default()
    };
    let mut published = TrayState::default();
    let mut last_inventory = None;
    let mut calibration: Option<Child> = None;
    let mut editor: Option<Child> = None;
    while !tray.is_closed() || calibration.is_some() {
        match editor.as_mut().map(Child::try_wait) {
            Some(Ok(Some(status))) => {
                editor = None;
                if !status.success() {
                    state.error = Some(format!("Profile editor exited with {status}"));
                }
            }
            Some(Err(error)) => {
                editor = None;
                state.error = Some(format!("Could not wait for the profile editor: {error}"));
            }
            _ => {}
        }
        state.editor_open = editor.is_some();
        let finished = match calibration.as_mut().map(Child::try_wait) {
            Some(Ok(Some(result))) => Some(if result.success() {
                None
            } else {
                Some("Calibration cancelled or failed".to_owned())
            }),
            Some(Err(error)) => Some(Some(format!("Could not wait for calibration: {error}"))),
            _ => None,
        };
        if let Some(outcome) = finished {
            calibration = None;
            state.calibrating = false;
            state.error = match desktop.set_outputs_paused(false) {
                Ok(()) => outcome,
                Err(error) => Some(format!("Could not resume virtual outputs: {error}")),
            };
        }
        state.status = desktop.status().ok().flatten();
        if last_inventory.is_none_or(|last: Instant| last.elapsed() >= INVENTORY_REFRESH) {
            state.inventory = desktop
                .command(&[
                    "--list-controllers",
                    "--show-controller-id",
                    "--auto-transport",
                    "--controller-model",
                    "any",
                ])
                .map_or_else(
                    |_| Vec::new(),
                    |reply| {
                        reply
                            .lines()
                            .filter_map(Controller::from_inventory)
                            .collect()
                    },
                );
            state.profiles = desktop.profile_paths();
            last_inventory = Some(Instant::now());
        }
        state.autostart = desktop.autostart_file().is_file();
        if published != state {
            published.clone_from(&state);
            tray.update(|tray| tray.state.clone_from(&state));
        }
        match receiver.recv_timeout(REFRESH) {
            Ok(_) if calibration.is_some() => {}
            Ok(Action::Edit) if editor.is_none() => {
                let path = state
                    .status
                    .as_ref()
                    .and_then(|status| status.active_profile.as_ref())
                    .filter(|path| path.is_file());
                match Command::new(desktop.bridge.with_file_name("joyshockrust-editor"))
                    .args(path)
                    .spawn()
                {
                    Ok(child) => {
                        editor = Some(child);
                        state.error = None;
                    }
                    Err(error) => {
                        state.error = Some(format!("Could not open the profile editor: {error}"))
                    }
                }
            }
            Ok(Action::Edit | Action::Quit) if editor.is_some() => {}
            Ok(action @ (Action::CalibrateGame | Action::CalibrateGyro)) => {
                let result = state
                    .status
                    .as_ref()
                    .ok_or_else(|| io::Error::other("bridge unavailable"))
                    .and_then(|status| {
                        desktop.calibrate(matches!(action, Action::CalibrateGame), status)
                    });
                match result {
                    Ok(child) => {
                        calibration = Some(child);
                        state.calibrating = true;
                        state.error = None;
                    }
                    Err(error) => {
                        state.error = Some(format!("Could not start calibration: {error}"))
                    }
                }
            }
            Ok(action) => match desktop.run(&action) {
                Ok(()) if matches!(action, Action::Quit) => break,
                Ok(()) => state.error = None,
                Err(error) => state.error = Some(format!("Action failed: {error}")),
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if calibration.is_some() => {
                std::thread::sleep(REFRESH)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    tray.shutdown().wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use ksni::Tray as _;

    use super::*;

    #[test]
    fn status_formats_discharge_level_and_clears_disconnected_telemetry() {
        let connected = "version=1\ncontroller=connected\nmodel=dualsense_edge\ntransport=bluetooth\nreport_rate_hz=249.4\nbattery_state=discharging\nbattery_percent=95\nprofile_source=loaded\nactive_hidraw=/dev/hidraw6";
        let status = Status::parse(connected).unwrap();
        assert_eq!(status.controller, "DualSense Edge");
        assert_eq!(status.connection_icon, "bluetooth-symbolic");
        assert_eq!(status.rate, "249.4 Hz");
        assert_eq!(status.battery.as_deref(), Some("🔋 95%"));
        assert_eq!(status.active_node.as_deref(), Some("/dev/hidraw6"));
        assert_eq!(status.profile, "Loaded profile");
        assert_eq!(
            Status::parse(&format!("{connected}\nprofile_name=gyro_profile.toml"))
                .unwrap()
                .profile,
            "gyro_profile.toml"
        );
        assert_eq!(
            Status::parse(&connected.replace("discharging", "charging"))
                .unwrap()
                .battery
                .as_deref(),
            Some("🔋⚡")
        );
        assert_eq!(
            Status::parse(&format!(
                "{connected}\nprofile_path_hex=2f746d702f612e746f6d6c"
            ))
            .unwrap()
            .active_profile,
            Some(PathBuf::from("/tmp/a.toml"))
        );
        let waiting = Status::parse(&connected.replace("connected", "waiting")).unwrap();
        assert_eq!(waiting.controller, "No active controller");
        assert_eq!(waiting.connection_icon, "");
        assert_eq!(waiting.active_node, None);
        assert_eq!(waiting.rate, "—");
        assert_eq!(waiting.battery, None);
        assert!(Status::parse(&connected.replace("version=1", "version=2")).is_none());
    }

    #[test]
    fn emulation_menu_has_independent_and_combined_controls() {
        let (actions, receiver) = mpsc::channel();
        let mut tray = Tray {
            actions,
            state: TrayState {
                status: Status::parse(
                    "version=1\ncontroller=waiting\nvirtual_gamepad_enabled=true\ngyro_mouse_enabled=false",
                ),
                ..TrayState::default()
            },
        };
        let controls = |tray: &Tray| {
            tray.menu()
                .into_iter()
                .filter_map(|entry| match entry {
                    MenuItem::Checkmark(item) if item.label != "Start with system" => Some(item),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (item, (text, flag, checked)) in controls(&tray).into_iter().zip([
            ("Gamepad emulation", "--set-gamepad", true),
            ("Gyro mouse", "--set-gyro-mouse", false),
            ("Gamepad and gyro mouse", "--set-emulation", false),
        ]) {
            assert_eq!(item.label, text);
            assert_eq!(item.checked, checked);
            assert!(item.enabled);
            (item.activate)(&mut tray);
            assert!(
                matches!(receiver.try_recv().unwrap(), Action::SetEmulation(command, enabled)
                if command == flag && enabled != checked)
            );
        }
        let status = tray.state.status.as_mut().unwrap();
        status.gyro_mouse_enabled = Some(true);
        let items = controls(&tray);
        assert_eq!(items.len(), 3);
        assert!(items[2].checked);
        (items[2].activate)(&mut tray);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Action::SetEmulation("--set-emulation", false)
        ));
        tray.state.status.as_mut().unwrap().outputs_paused = true;
        assert!(controls(&tray).iter().all(|item| !item.enabled));
    }

    #[test]
    fn gyro_calibration_is_available_with_a_controller_and_without_profiles() {
        let (actions, receiver) = mpsc::channel();
        let mut tray = Tray {
            actions,
            state: TrayState {
                status: Status::parse(
                    "version=1\ncontroller=connected\nactive_hidraw=/dev/hidraw6",
                ),
                ..TrayState::default()
            },
        };
        let button = tray
            .menu()
            .into_iter()
            .find_map(|entry| match entry {
                MenuItem::Standard(item) if item.label == "Calibrate Controller Gyro" => Some(item),
                _ => None,
            })
            .unwrap();
        (button.activate)(&mut tray);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Action::CalibrateGyro
        ));
        let button = tray
            .menu()
            .into_iter()
            .find_map(|entry| match entry {
                MenuItem::Standard(item) if item.label == "Calibrate game sensitivity (RWC)" => {
                    Some(item)
                }
                _ => None,
            })
            .unwrap();
        (button.activate)(&mut tray);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Action::CalibrateGame
        ));
        tray.state.status = Status::parse("version=1\ncontroller=waiting");
        assert!(!tray.menu().into_iter().any(|entry| matches!(entry,
            MenuItem::Standard(item) if item.label == "Calibrate Controller Gyro")));
    }

    #[test]
    fn controller_and_profile_menu_actions_are_independent() {
        let controller = Controller::from_inventory(
            "/dev/hidraw6: wired DualSense Edge, readable, controller_id=01:02:03:04:05:06",
        )
        .unwrap();
        assert!(
            Controller::from_inventory(
                "/dev/hidraw8: Bluetooth DualSense, readable, controller_id=unavailable"
            )
            .is_none()
        );
        let profile = PathBuf::from("/tmp/gyro profile.toml");
        let (actions, receiver) = mpsc::channel();
        let mut tray = Tray {
            actions,
            state: TrayState {
                status: Status::parse(
                    "version=1\ncontroller=connected\nmodel=dualsense_edge\ntransport=usb\nreport_rate_hz=1000\nbattery_state=full\nprofile_name=gyro profile.toml\nprofile_path_hex=2f746d702f6779726f2070726f66696c652e746f6d6c\nactive_hidraw=/dev/hidraw6",
                ),
                inventory: vec![controller.clone()],
                profiles: vec![profile.clone()],
                ..TrayState::default()
            },
        };
        let menu = tray.menu();
        assert!(matches!(&menu[0], MenuItem::Standard(row)
            if row.label == "1000.0 Hz" && row.icon_name == "drive-harddisk-usb-symbolic"));
        for (heading, name) in [
            ("Active controller", "DualSense Edge"),
            ("Active profile", "gyro profile.toml"),
        ] {
            let option = menu.windows(2).find_map(|entries| match entries {
                [MenuItem::Standard(label), MenuItem::SubMenu(menu)]
                    if label.label == heading && !label.enabled && menu.label == name =>
                {
                    menu.submenu.last()
                }
                _ => None,
            });
            let Some(MenuItem::Standard(option)) = option else {
                panic!("missing submenu option for {name}");
            };
            assert!(option.label.starts_with("✓ "));
            (option.activate)(&mut tray);
        }
        assert!(
            matches!(receiver.try_recv().unwrap(), Action::Select(selected) if selected == controller)
        );

        assert!(
            matches!(receiver.try_recv().unwrap(), Action::Load(selected) if selected == profile)
        );
        let button = menu
            .iter()
            .find_map(|entry| match entry {
                MenuItem::Standard(item) if item.label == "Profile editor" => Some(item),
                _ => None,
            })
            .unwrap();
        (button.activate)(&mut tray);
        assert!(matches!(receiver.try_recv().unwrap(), Action::Edit));
    }
}

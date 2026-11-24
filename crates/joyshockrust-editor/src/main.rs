use joyshockrust_ipc::control::ResistancePreview;
use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use adw::{gtk, prelude::*};

mod bindings;
mod calibration;
mod calibration_navigation;
mod controller;
mod controller_art;
mod controller_input;
mod flick_preview;
mod gyro_activation;
mod imu_monitor;
mod key_capture;
mod response_plot;
mod sensitivity;
mod stick_keys;
mod touchpad;
mod trigger_travel;
mod triggers;
mod ui;

use joyshockrust_config::{
    new_profile_text,
    paths::{
        APP_DIRECTORY, DESKTOP_APP_ID, PROFILE_FILE, PROFILES_DIRECTORY, config_home_from_env,
    },
};

fn profile_filename(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err("Enter a profile name without path separators or control characters.".into());
    }
    Ok(if name.ends_with(".toml") {
        name.to_owned()
    } else {
        format!("{name}.toml")
    })
}

fn run_bridge(bridge: &Path, args: &[&Path]) -> Result<(), String> {
    let output = Command::new(bridge)
        .args(args)
        .output()
        .map_err(|error| format!("{}: {error}", bridge.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr);
    if message.trim().is_empty() {
        Err(format!("JoyShockRust exited with {}.", output.status))
    } else {
        Err(message.trim().into())
    }
}

fn stage(directory: Option<&Path>, text: &str) -> io::Result<tempfile::NamedTempFile> {
    let mut file = if let Some(directory) = directory {
        fs::create_dir_all(directory)?;
        tempfile::NamedTempFile::new_in(directory)?
    } else {
        tempfile::NamedTempFile::new()?
    };
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    Ok(file)
}

struct Editor {
    bridge: PathBuf,
    directory: PathBuf,
    path: Option<PathBuf>,
    original: String,
    text: String,
    name: String,
    selected: &'static str,
    calibration: sensitivity::CalibrationDraft,
    link_gyro_sensitivity: bool,
    invalid_gyro_sensitivity: [Option<String>; 2],
    trigger_preview_active: bool,
}

impl Editor {
    fn open(bridge: PathBuf, directory: PathBuf, path: Option<PathBuf>) -> io::Result<Self> {
        let original = path
            .as_ref()
            .map_or_else(|| Ok(new_profile_text()), fs::read_to_string)?;
        Ok(Self {
            bridge,
            directory,
            path,
            text: original.clone(),
            original,
            name: String::new(),
            selected: "cross",
            calibration: sensitivity::CalibrationDraft::default(),
            link_gyro_sensitivity: false,
            invalid_gyro_sensitivity: [None, None],
            trigger_preview_active: false,
        })
    }

    fn has_unsaved_changes(&self) -> bool {
        self.text != self.original
            || (self.path.is_none() && !self.name.trim().is_empty())
            || self.draft_error().is_some()
    }

    fn profile_paths(&self) -> Vec<PathBuf> {
        let startup = self.directory.parent().map(|root| root.join(PROFILE_FILE));
        let mut paths =
            joyshockrust_config::paths::profile_paths(&self.directory, startup.as_deref());
        if let Some(path) = &self.path
            && !paths.contains(path)
        {
            paths.insert(0, path.clone());
        }
        paths
    }

    fn switch_profile(&mut self, path: Option<PathBuf>) -> io::Result<()> {
        if self.path != path || path.is_none() {
            let next = Self::open(self.bridge.clone(), self.directory.clone(), path)?;
            self.clear_trigger_preview();
            *self = next;
        }
        Ok(())
    }

    fn preview_trigger(&mut self, preview: ResistancePreview) -> io::Result<()> {
        triggers::send_preview_message(&preview.encode())?;
        self.trigger_preview_active = true;
        Ok(())
    }

    fn clear_trigger_preview(&mut self) {
        if self.trigger_preview_active {
            if let Err(error) = triggers::send_preview_message(&ResistancePreview::Reset.encode()) {
                eprintln!("Could not end adaptive trigger preview: {error}");
            }
            self.trigger_preview_active = false;
        }
    }

    fn draft_error(&self) -> Option<&'static str> {
        self.calibration.error.or_else(|| {
            self.invalid_gyro_sensitivity
                .iter()
                .any(Option::is_some)
                .then_some(sensitivity::GYRO_SENSITIVITY_ERROR)
        })
    }

    fn validate(&self) -> Result<String, String> {
        if let Some(error) = self.draft_error() {
            return Err(error.into());
        }
        self.validate_profile_text(&self.text)?;
        Ok("Profile is valid.".into())
    }

    fn validate_profile_text(&self, text: &str) -> Result<(), String> {
        let staged = stage(None, text).map_err(|error| error.to_string())?;
        run_bridge(&self.bridge, &[Path::new("--check-profile"), staged.path()])
    }

    fn save(&mut self) -> Result<PathBuf, String> {
        if let Some(error) = self.draft_error() {
            return Err(error.into());
        }
        if let Some(path) = &self.path {
            let current =
                fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
            if current != self.original {
                return Err("The profile changed on disk. Reopen it before editing again.".into());
            }
            let staged = stage(None, &self.text).map_err(|error| error.to_string())?;
            run_bridge(
                &self.bridge,
                &[Path::new("--install-profile"), staged.path(), path],
            )?;
            self.original.clone_from(&self.text);
            return Ok(path.clone());
        }
        let path = self.directory.join(profile_filename(&self.name)?);
        let staged = stage(Some(&self.directory), &self.text).map_err(|error| error.to_string())?;
        run_bridge(&self.bridge, &[Path::new("--check-profile"), staged.path()])?;
        staged
            .persist_noclobber(&path)
            .map_err(|error| format!("{}: {}", path.display(), error.error))?;
        self.path = Some(path.clone());
        self.original.clone_from(&self.text);
        Ok(path)
    }

    fn save_and_apply(&mut self, apply: bool) -> Result<String, String> {
        let path = self.save()?;
        if apply {
            run_bridge(&self.bridge, &[Path::new("--load-profile"), &path])
                .map_err(|error| format!("Profile saved. Could not apply it: {error}"))?;
            self.trigger_preview_active = false;
            return Ok("Profile saved and applied.".into());
        }
        Ok("Profile saved.".into())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1).peekable();
    let bridge = env::current_exe()?.with_file_name("joyshockrust-linux");
    if args.next_if(|arg| arg == "--calibrate-game").is_some() {
        let [Some(profile), Some(node), None] = [args.next(), args.next(), args.next()] else {
            return Err("usage: joyshockrust-editor --calibrate-game PROFILE HIDRAW".into());
        };
        return calibration::run(
            bridge,
            Some(calibration::GameProfiles {
                active: Some(profile.into()),
                available: Vec::new(),
            }),
            node.into(),
        );
    }
    if args
        .next_if(|arg| arg == "--calibrate-game-select")
        .is_some()
    {
        let node = args.next().ok_or("expected a controller node")?;
        let active = if args.next_if(|arg| arg == "--active-profile").is_some() {
            Some(args.next().ok_or("expected an active profile path")?.into())
        } else {
            None
        };
        let available = args.map(PathBuf::from).collect();
        return calibration::run(
            bridge,
            Some(calibration::GameProfiles { active, available }),
            node.into(),
        );
    }
    if args
        .next_if(|arg| arg == "--calibrate-controller-gyro")
        .is_some()
    {
        let [Some(node), None] = [args.next(), args.next()] else {
            return Err("usage: joyshockrust-editor --calibrate-controller-gyro HIDRAW".into());
        };
        return calibration::run(bridge, None, node.into());
    }
    let path = args.next().map(std::path::absolute).transpose()?;
    if args.next().is_some() {
        return Err("usage: joyshockrust-editor [PROFILE]".into());
    }
    let directory = config_home_from_env()
        .ok_or("expected an absolute XDG_CONFIG_HOME or HOME")?
        .join(APP_DIRECTORY)
        .join(PROFILES_DIRECTORY);
    let editor = std::rc::Rc::new(std::cell::RefCell::new(Editor::open(
        bridge, directory, path,
    )?));
    // With NON_UNIQUE, GTK uses the program name as the Wayland app ID.
    adw::glib::set_program_name(Some(DESKTOP_APP_ID));
    gtk::gdk::set_allowed_backends("wayland");
    let app = adw::Application::builder()
        .application_id(DESKTOP_APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| ui::open_window(app, editor.clone()));
    if app.run_with_args(&["joyshockrust-editor"]) != adw::glib::ExitCode::SUCCESS {
        return Err("Could not start the profile editor in a Wayland session".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_new_profiles_in_the_profile_directory() {
        assert_eq!(profile_filename("  My Game  ").unwrap(), "My Game.toml");
        assert_eq!(profile_filename("Game.toml").unwrap(), "Game.toml");
        for name in ["", "../test", "/tmp/escape", "a\\b", ".hidden", "a\nb"] {
            assert!(profile_filename(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_profile_changed_outside_the_editor_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.toml");
        fs::write(&path, new_profile_text()).unwrap();
        let mut editor = Editor::open(
            directory.path().join("unused-bridge"),
            directory.path().into(),
            Some(path.clone()),
        )
        .unwrap();
        editor.text.push_str("# Editor change\n");
        let external = "version = 1\n# External change\n";
        fs::write(&path, external).unwrap();
        assert!(editor.save().unwrap_err().contains("changed on disk"));
        assert_eq!(fs::read_to_string(path).unwrap(), external);
    }
}

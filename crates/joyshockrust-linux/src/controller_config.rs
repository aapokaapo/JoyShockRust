use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    discovery::{ControllerFilter, ModelSelection, Selection},
    hidraw::ControllerId,
    profile,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    transport: Selection,
    model: ModelSelection,
    last_controller_id: Option<ControllerId>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            transport: Selection::Auto,
            model: ModelSelection::Any,
            last_controller_id: None,
        }
    }
}

#[derive(Default)]
pub struct ControllerConfig {
    path: Option<PathBuf>,
    settings: Settings,
    dirty: bool,
}

impl ControllerConfig {
    pub fn load() -> Self {
        let result = profile::default_path()
            .and_then(|path| Self::load_from(path.with_file_name("controller.toml")));
        result.unwrap_or_else(|err| {
            eprintln!("Controller settings unavailable: {err}");
            Self::default()
        })
    }

    fn load_from(path: PathBuf) -> io::Result<Self> {
        let settings = match fs::read_to_string(&path) {
            Ok(source) => toml::from_str(&source).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {err}", path.display()),
                )
            })?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!("{}: {err}", path.display()),
                ));
            }
        };
        Ok(Self {
            path: Some(path),
            settings,
            dirty: false,
        })
    }

    pub fn filter(&self) -> ControllerFilter {
        ControllerFilter {
            transport: self.settings.transport,
            model: self.settings.model,
            controller_id: None,
        }
    }

    pub fn preferred(&self) -> Option<ControllerId> {
        self.settings.last_controller_id
    }

    pub fn remember(&mut self, id: ControllerId) {
        let id = Some(id);
        self.dirty |= self.settings.last_controller_id != id;
        self.settings.last_controller_id = id;
        self.save();
    }

    pub fn change_filters(&mut self, previous: ControllerFilter, next: ControllerFilter) {
        if previous.transport != next.transport {
            self.dirty |= self.settings.transport != next.transport;
            self.settings.transport = next.transport;
        }
        if previous.model != next.model {
            self.dirty |= self.settings.model != next.model;
            self.settings.model = next.model;
        }
        self.save();
    }

    fn save(&mut self) {
        let Some(path) = self.path.as_ref().filter(|_| self.dirty) else {
            return;
        };
        let result = toml::to_string(&self.settings)
            .map_err(io::Error::other)
            .and_then(|contents| profile::replace_file(path, contents.as_bytes()));
        match result {
            Ok(()) => self.dirty = false,
            Err(err) => eprintln!(
                "Could not save controller settings to {}: {err}",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn last_controller_survives_restart_as_a_preference() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("jsr-controller-{}-{nonce}", std::process::id()));
        let path = directory.join("controller.toml");
        let first = ControllerId::parse("00:11:22:33:44:55").unwrap();
        let fallback = ControllerId::parse("00:11:22:33:44:66").unwrap();
        let mut config = ControllerConfig::load_from(path.clone()).unwrap();
        assert_eq!(config.filter().transport, Selection::Auto);
        assert!(config.preferred().is_none());
        assert!(!path.exists());
        config.remember(first);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let mut restored = ControllerConfig::load_from(path.clone()).unwrap();
        assert_eq!(restored.preferred(), Some(first));
        assert_eq!(restored.filter().controller_id, None);
        restored.remember(fallback);
        assert_eq!(
            ControllerConfig::load_from(path).unwrap().preferred(),
            Some(fallback)
        );
        fs::remove_dir_all(directory).unwrap();
    }
}

use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub const APP_DIRECTORY: &str = "joyshockrust";
pub const PROFILE_FILE: &str = "profile.toml";
pub const PROFILES_DIRECTORY: &str = "profiles";
pub const DESKTOP_APP_ID: &str = "io.github.aapokaapo.JoyShockRust";

pub fn absolute_env(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Callers such as the tray can supply their own already-checked HOME.
pub fn config_home(xdg: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    xdg.filter(|path| path.is_absolute()).or_else(|| {
        home.filter(|path| path.is_absolute())
            .map(|path| path.join(".config"))
    })
}

pub fn config_home_from_env() -> Option<PathBuf> {
    config_home(absolute_env("XDG_CONFIG_HOME"), absolute_env("HOME"))
}

/// Some clients have historically accepted a relative runtime directory.
pub fn runtime_socket(name: &str, absolute_required: bool) -> Option<PathBuf> {
    let path = env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)?;
    (!absolute_required || path.is_absolute()).then(|| path.join(APP_DIRECTORY).join(name))
}

pub fn profile_paths(directory: &Path, startup: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(directory)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml") && path.is_file())
        .collect();
    paths.sort();
    if let Some(startup) = startup.filter(|path| path.is_file()) {
        paths.insert(0, startup.to_owned());
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_xdg_can_replace_home_and_relative_xdg_falls_back() {
        assert_eq!(
            config_home(Some("/config".into()), None),
            Some("/config".into())
        );
        assert_eq!(
            config_home(Some("relative".into()), Some("/home/user".into())),
            Some("/home/user/.config".into())
        );
        assert_eq!(config_home(Some("".into()), Some("relative".into())), None);
    }
}

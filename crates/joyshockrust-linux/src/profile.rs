use joyshockrust_config::{
    linux_keys::LinuxKeys,
    paths::{APP_DIRECTORY, PROFILE_FILE, config_home_from_env},
};
pub use joyshockrust_config::{
    profile::{BridgeOptions, RuntimeOptions},
    profile_file::{replace_file, save_game_calibration},
};

use std::{
    error::Error,
    fs, io,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

pub fn load(path: &Path) -> Result<RuntimeOptions, Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    parse(&source)
}

pub fn selection_path(startup: &Path) -> Option<PathBuf> {
    let mut name = startup.file_name()?.to_os_string();
    name.push(".active");
    Some(startup.with_file_name(name))
}

pub fn restore_selection(selection: &Path) -> Option<(RuntimeOptions, Option<PathBuf>)> {
    let data = match fs::read(selection) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!(
                "Could not read active profile selection {}: {error}",
                selection.display()
            );
            return None;
        }
    };
    if data == b"defaults\n" {
        return Some((RuntimeOptions::default(), None));
    }
    if let Some(raw_path) = data.strip_prefix(b"profile\n") {
        let path = PathBuf::from(std::ffi::OsString::from_vec(raw_path.to_vec()));
        if path.is_absolute() {
            match load(&path) {
                Ok(runtime) => return Some((runtime, Some(path))),
                Err(error) => {
                    eprintln!(
                        "Could not restore active profile {}: {error}",
                        path.display()
                    );
                    return None;
                }
            }
        }
    }
    eprintln!(
        "Invalid active profile selection in {}",
        selection.display()
    );
    None
}

pub fn remember_selection(file: &Path, selected: Option<&Path>, defaults: bool) -> io::Result<()> {
    if let Some(path) = selected {
        let path = std::path::absolute(path)?;
        let mut data = b"profile\n".to_vec();
        data.extend_from_slice(path.as_os_str().as_bytes());
        replace_file(file, &data)
    } else if defaults {
        replace_file(file, b"defaults\n")
    } else {
        match fs::remove_file(file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

pub fn default_path() -> io::Result<PathBuf> {
    config_home_from_env()
        .map(|dir| dir.join(APP_DIRECTORY).join(PROFILE_FILE))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no absolute XDG_CONFIG_HOME or HOME",
            )
        })
}

pub fn install(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let content =
        fs::read_to_string(source).map_err(|err| format!("{}: {err}", source.display()))?;
    parse(&content).map_err(|err| format!("{}: {err}", source.display()))?;
    if destination.file_name().is_none() {
        return Err("expected a destination file".into());
    }
    replace_file(destination, content.as_bytes())
        .map_err(|err| format!("{}: {err}", destination.display()))?;
    Ok(())
}

pub(crate) fn parse(source: &str) -> Result<RuntimeOptions, Box<dyn Error>> {
    joyshockrust_config::profile::parse(source, &LinuxKeys)
}

#[cfg(test)]
mod tests;

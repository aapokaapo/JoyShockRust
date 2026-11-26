//! Linux profile persistence shared by the bridge and editor.

use std::{
    error::Error,
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

use crate::{linux_keys::LinuxKeys, profile};

pub enum WriteMode<'a> {
    New,
    Current(&'a str),
}

pub fn save(destination: &Path, content: &str, mode: WriteMode<'_>) -> Result<(), Box<dyn Error>> {
    profile::parse(content, &LinuxKeys)?;
    write(destination, content.as_bytes(), Some(mode))?;
    Ok(())
}

pub fn replace_file(destination: &Path, content: &[u8]) -> io::Result<()> {
    write(destination, content, None)
}

fn write(destination: &Path, content: &[u8], mode: Option<WriteMode<'_>>) -> io::Result<()> {
    if destination.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a destination file",
        ));
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = tempfile::Builder::new()
        .prefix(".joyshockrust-profile-")
        .tempfile_in(parent)?;
    file.write_all(content)?;
    file.as_file().sync_all()?;

    // Serialize the compare and publish steps for JoyShockRust writers in
    // this directory. External editors may write without taking this lock.
    let directory = File::open(parent)?;
    directory.lock()?;
    if matches!(mode, Some(WriteMode::New)) {
        file.persist_noclobber(destination)?;
        return Ok(());
    }
    match fs::symlink_metadata(destination) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination must be a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if mode.is_none() && error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(WriteMode::Current(original)) = mode
        && fs::read(destination)? != original.as_bytes()
    {
        return Err(io::Error::other(
            "The profile changed on disk. Reopen it before editing again.",
        ));
    }
    file.persist(destination)?;
    Ok(())
}

pub fn save_game_calibration(
    path: &Path,
    real_world_calibration: f64,
    in_game_sens: f64,
) -> Result<(), Box<dyn Error>> {
    let source = fs::read_to_string(path)?;
    let rendered = render_game_calibration(&source, real_world_calibration, in_game_sens)?;
    save(path, &rendered, WriteMode::Current(&source))
}

fn render_game_calibration(
    source: &str,
    real_world_calibration: f64,
    in_game_sens: f64,
) -> Result<String, Box<dyn Error>> {
    profile::parse(source, &LinuxKeys)?;
    let mut document = source.parse::<toml_edit::DocumentMut>()?;
    let gyro = document
        .entry("gyro")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
        .ok_or("expected a [gyro] table")?;
    for (key, number) in [
        ("real_world_calibration", real_world_calibration),
        ("in_game_sens", in_game_sens),
    ] {
        let mut value = toml_edit::Value::from(number);
        if let Some(existing) = gyro.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        } else {
            gyro.insert(key, toml_edit::Item::Value(value));
        }
    }
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_before_writing_and_never_overwrites_a_new_profile_collision() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.toml");
        let original = "version = 1\n# Original\n";
        assert!(save(&path, "version = 2\n", WriteMode::New).is_err());
        assert!(!path.exists());
        save(&path, original, WriteMode::New).unwrap();
        assert!(save(&path, "version = 1\n", WriteMode::New).is_err());
        let invalid_key =
            "version = 1\n[[keyboard.bindings]]\nbutton = 'cross'\ntap = 'KEY_UNDEFINED'\n";
        assert!(save(&path, invalid_key, WriteMode::Current(original)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        let link = directory.path().join("link.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(save(&link, "version = 1\n", WriteMode::New).is_err());
        assert!(save(&link, "version = 1\n", WriteMode::Current(original)).is_err());
        assert_eq!(fs::read_link(link).unwrap(), path);
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn concurrent_saves_from_the_same_original_publish_only_one_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.toml");
        let original = "version = 1\n";
        save(&path, original, WriteMode::New).unwrap();

        let start = std::sync::Barrier::new(2);
        let drafts = ["version = 1\n# First\n", "version = 1\n# Second\n"];
        let replies = std::thread::scope(|scope| {
            let (start, path) = (&start, &path);
            let jobs = drafts.map(|text| {
                scope.spawn(move || {
                    start.wait();
                    save(path, text, WriteMode::Current(original)).map_err(|err| err.to_string())
                })
            });
            jobs.map(|job| job.join().unwrap())
        });
        let winner = replies.iter().position(Result::is_ok).unwrap();
        assert!(matches!(&replies[1 - winner], Err(err) if err.contains("changed on disk")));
        assert_eq!(fs::read_to_string(path).unwrap(), drafts[winner]);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn recorded_game_calibration_preserves_profile_comments_and_other_values() {
        let source = concat!(
            "# My profile\nversion = 1\n",
            "[gyro]\nsensitivity_x = 1.5 # horizontal\n",
            "real_world_calibration = 42.0 # old measurement\n",
            "[gyro.binding]\nmode = \"hold\"\ninput = \"l2\"\n",
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.toml");
        fs::write(&path, source).unwrap();
        save_game_calibration(&path, 40.0, 2.0).unwrap();
        let rendered = fs::read_to_string(&path).unwrap();
        assert!(rendered.contains("# My profile\nversion = 1"));
        assert!(rendered.contains("sensitivity_x = 1.5 # horizontal"));
        assert!(rendered.contains("real_world_calibration = 40.0 # old measurement"));
        assert!(rendered.contains("in_game_sens = 2.0"));
        assert!(rendered.contains("[gyro.binding]\nmode = \"hold\"\ninput = \"l2\""));
        let inline = render_game_calibration(
            "version = 1\ngyro = { sensitivity_x = 2 } # inline settings\n",
            40.0,
            2.0,
        )
        .unwrap();
        assert!(inline.contains("} # inline settings"));
        let document = inline.parse::<toml_edit::DocumentMut>().unwrap();
        let gyro = document["gyro"].as_inline_table().unwrap();
        assert_eq!(gyro["sensitivity_x"].as_integer(), Some(2));
        assert_eq!(gyro["real_world_calibration"].as_float(), Some(40.0));
        assert!(render_game_calibration("version = 1", 40.0, 2.0).is_ok());
        assert!(save_game_calibration(&path, 0.0, 2.0).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), rendered);
    }
}

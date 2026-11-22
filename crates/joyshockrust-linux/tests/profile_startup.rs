#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn invalid_startup_profile_exits_with_config_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .args(["--profile", "/dev/null"])
        .output()
        .expect("run bridge with an empty profile");

    assert_eq!(output.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing field `version`"));
}

#[test]
fn installing_a_profile_preserves_source_bytes_and_rejects_invalid_replacements() {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        path::PathBuf,
        process,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct TestDirectory(PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = TestDirectory(
        std::env::temp_dir().join(format!("joyshockrust-install-{}-{stamp}", process::id())),
    );
    fs::create_dir(&directory.0).unwrap();
    let source = directory.0.join("preset.toml");
    let destination = directory.0.join("joyshockrust/profile.toml");
    let alternate = directory.0.join("other.toml");
    let install = |destination: Option<&PathBuf>| {
        Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .env("XDG_CONFIG_HOME", &directory.0)
            .arg("--install-profile")
            .arg(&source)
            .args(destination)
            .output()
            .unwrap()
    };

    let original = "# Preserve this comment\nversion = 1\n[gyro]\n";
    fs::write(&source, original).unwrap();
    assert!(install(None).status.success());
    assert_eq!(fs::read_to_string(&destination).unwrap(), original);
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o077,
        0
    );

    fs::write(&source, "version = 2\n").unwrap();
    assert!(!install(None).status.success());
    assert_eq!(fs::read_to_string(&destination).unwrap(), original);

    let replacement = "version = 1\nremap_buttons = [\"cross=circle\"]\n";
    fs::write(&source, replacement).unwrap();
    assert!(install(None).status.success());
    assert_eq!(fs::read_to_string(&destination).unwrap(), replacement);
    assert_eq!(
        fs::read_dir(destination.parent().unwrap()).unwrap().count(),
        1
    );

    symlink(&destination, &alternate).unwrap();
    assert!(!install(Some(&alternate)).status.success());
    assert_eq!(fs::read_link(&alternate).unwrap(), destination);
}

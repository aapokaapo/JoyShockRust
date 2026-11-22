#![cfg(target_os = "linux")]

use std::{
    fs,
    path::PathBuf,
    process::{self, Command},
    time::{SystemTime, UNIX_EPOCH},
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("jsr-{label}-{}-{stamp}", process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
fn diagnostics_loads_the_profile_and_allows_a_transport_override() {
    let profile =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .args(["--diagnose", "--bluetooth", "--profile"])
        .arg(profile)
        .arg("/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Controller filter: Bluetooth DualSense or DualSense Edge\n"));
    assert!(stdout.lines().any(|line| line.starts_with("/dev/uinput: ")));

    let invalid = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .args(["--diagnose", "--profile", "/dev/null"])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("missing field `version`"));
}

#[test]
fn installing_a_profile_preserves_source_bytes_and_rejects_invalid_replacements() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let directory = TestDirectory::new("install");
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

#[test]
fn status_reports_the_current_selection_after_live_reload_while_waiting() {
    use std::{os::unix::net::UnixDatagram, process::Child, time::Duration};

    struct Bridge(Child);
    impl Drop for Bridge {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let directory = TestDirectory::new("selection");
    let startup = directory.0.join("profile.toml");
    let preset = directory.0.join("preset.toml");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    // The broadcast address cannot be an individual controller's HID ID.
    let settings = |transport, model| {
        format!(
            "version = 1\n[bridge]\ntransport = '{transport}'\nmodel = '{model}'\ncontroller_id = 'ff:ff:ff:ff:ff:ff'\n"
        )
    };
    fs::write(&startup, settings("usb", "dualsense")).unwrap();
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let _bridge = Bridge(
        Command::new(binary)
            .env("NOTIFY_SOCKET", &notify)
            .arg("--profile")
            .arg(&startup)
            .arg("--status-socket")
            .arg(&status)
            .arg("--reload-socket")
            .arg(&reload)
            .spawn()
            .unwrap(),
    );
    let mut ready = [0; 64];
    let len = listener.recv(&mut ready).expect("bridge readiness");
    assert_eq!(&ready[..len], b"READY=1");

    let check = |transport, model| {
        let output = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        for line in [
            "controller=waiting".to_owned(),
            format!("selected_transport={transport}"),
            format!("selected_model={model}"),
            "controller_id_configured=true".to_owned(),
        ] {
            assert!(stdout.lines().any(|entry| entry == line), "{stdout}");
        }
        assert!(!stdout.contains("ff:ff:ff:ff:ff:ff"), "{stdout}");
    };
    check("usb", "dualsense");
    fs::write(&startup, settings("auto", "dualsense-edge")).unwrap();
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("auto", "dualsense-edge");

    fs::write(&preset, settings("bluetooth", "dualsense")).unwrap();
    let load = || {
        Command::new(binary)
            .arg("--load-profile")
            .arg(&preset)
            .arg(&reload)
            .output()
            .unwrap()
    };
    let result = load();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense");

    fs::write(&preset, "version = 1\n[gyro]\n").unwrap();
    let result = load();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense");

    let result = Command::new(binary)
        .args(["--select-controller", "ff:ff:ff:ff:ff:fe"])
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense");
    let output = Command::new(binary)
        .arg("--status")
        .arg(&status)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("gyro_configured=true\nlast_reload=applied\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("ff:ff:ff:ff:ff:fe"), "{stdout}");
    assert_eq!(
        fs::read_to_string(&startup).unwrap(),
        settings("auto", "dualsense-edge")
    );
}

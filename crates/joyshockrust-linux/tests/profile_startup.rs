#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    process::{self, Child, Command},
    time::{Duration, SystemTime, UNIX_EPOCH},
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

struct Bridge(Child);

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn invalid_startup_profile_exits_with_config_status() {
    for flag in ["--profile", "--profile-if-present"] {
        let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .args([flag, "/dev/null"])
            .output()
            .expect("run bridge with an empty profile");
        assert_eq!(output.status.code(), Some(78));
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing field `version`"));
    }
}

#[test]
fn diagnostics_loads_the_profile_and_allows_a_transport_override() {
    let profile =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/gyro-except-l2.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
        .args([
            "--diagnose",
            "--controller-model",
            "any",
            "--bluetooth",
            "--profile",
        ])
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
fn live_profiles_and_controller_selection_change_independently_while_waiting() {
    let directory = TestDirectory::new("selection");
    let controller_file = directory.0.join("joyshockrust/controller.toml");
    fs::create_dir_all(controller_file.parent().unwrap()).unwrap();
    fs::write(
        &controller_file,
        "transport = 'auto'\nmodel = 'dualsense-edge'\n",
    )
    .unwrap();
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
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let _bridge = Bridge(
        Command::new(binary)
            .env("NOTIFY_SOCKET", &notify)
            .env("XDG_CONFIG_HOME", &directory.0)
            .arg("--profile-if-present")
            .arg(&startup)
            .args(["--controller-id", "ff:ff:ff:ff:ff:ff"])
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

    let check = |transport, model, gyro| {
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
            format!("gyro_configured={gyro}"),
            "controller_id_configured=true".to_owned(),
        ] {
            assert!(stdout.lines().any(|entry| entry == line), "{stdout}");
        }
        assert!(!stdout.contains("ff:ff:ff:ff:ff:ff"), "{stdout}");
    };
    check("auto", "dualsense-edge", false);
    let startup_contents = format!("{}[gyro]\n", settings("usb", "dualsense"));
    fs::write(&startup, &startup_contents).unwrap();
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("auto", "dualsense-edge", true);

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
    check("auto", "dualsense-edge", false);

    let result = Command::new(binary)
        .args([
            "--select-controller",
            "ff:ff:ff:ff:ff:fe",
            "--controller-model",
            "dualsense",
            "--bluetooth",
        ])
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense", false);

    // Reload a file with conflicting startup defaults after the live selection.
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense", true);
    let saved = fs::read_to_string(controller_file).unwrap();
    assert!(saved.contains("transport = \"bluetooth\""), "{saved}");
    assert!(saved.contains("model = \"dualsense\""), "{saved}");
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
    for _ in 0..2 {
        let result = Command::new(binary)
            .arg("--unload-profile")
            .arg(&reload)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        check("bluetooth", "dualsense", false);
    }
    assert_eq!(fs::read_to_string(&startup).unwrap(), startup_contents);
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check("bluetooth", "dualsense", true);
}

#[test]
fn controller_can_be_selected_and_a_preset_loaded_without_a_startup_profile() {
    let directory = TestDirectory::new("no-profile");
    let preset = directory.0.join("preset");
    let status = directory.0.join("status");
    let reload = directory.0.join("reload");
    let notify = directory.0.join("notify");
    let listener = UnixDatagram::bind(&notify).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let _bridge = Bridge(
        Command::new(binary)
            .env("NOTIFY_SOCKET", &notify)
            .env("XDG_CONFIG_HOME", &directory.0)
            .args(["--controller-id", "ff:ff:ff:ff:ff:ff"])
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

    let result = Command::new(binary)
        .args([
            "--select-controller",
            "ff:ff:ff:ff:ff:fe",
            "--controller-model",
            "dualsense",
            "--bluetooth",
        ])
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    let check = |gyro, outcome| {
        let output = Command::new(binary)
            .arg("--status")
            .arg(&status)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        for expected in [
            "selected_model=dualsense".to_owned(),
            "selected_transport=bluetooth".to_owned(),
            format!("gyro_configured={gyro}"),
            format!("last_reload={outcome}"),
        ] {
            assert!(stdout.lines().any(|line| line == expected), "{stdout}");
        }
    };
    check(false, "unavailable");
    fs::write(&preset, "version = 1\n[gyro]\n").unwrap();
    let result = Command::new(binary)
        .arg("--load-profile")
        .arg(&preset)
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check(true, "applied");
    let result = Command::new(binary)
        .arg("--reload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(!result.status.success(), "{result:?}");
    assert!(String::from_utf8_lossy(&result.stderr).contains("no startup profile configured"));
    check(true, "applied");
    let result = Command::new(binary)
        .arg("--unload-profile")
        .arg(&reload)
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    check(false, "applied");
    assert_eq!(fs::read_to_string(preset).unwrap(), "version = 1\n[gyro]\n");
}

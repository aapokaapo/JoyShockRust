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

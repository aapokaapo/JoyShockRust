#![cfg(target_os = "linux")]

use std::process::Command;

const IGNORE: &str = "SDL_GAMECONTROLLER_IGNORE_DEVICES";
const ALLOW: &str = "SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT";

fn launcher() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"));
    command
        .arg("--launch-sdl")
        .env_remove(IGNORE)
        .env_remove(ALLOW);
    command
}

#[test]
fn launches_with_dualsense_filter_and_preserves_arguments_and_exit_status() {
    let output = launcher()
        .env(IGNORE, "0x1234/0xabcd")
        .args([
            "--",
            "/bin/sh",
            "-c",
            "printf '%s\n' \"$SDL_GAMECONTROLLER_IGNORE_DEVICES\" \"$1\" \"$2\"; exit 42",
            "check",
            "two words",
            "$(literal)",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(
        output.stdout,
        b"0x1234/0xabcd,0x054c/0x0ce6,0x054c/0x0df2\ntwo words\n$(literal)\n"
    );
}

#[test]
fn refuses_an_allow_list_and_requires_an_explicit_command_separator() {
    let output = launcher()
        .env(ALLOW, "0x054c/0x0df2")
        .args(["--", "/bin/echo", "unexpected"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("takes precedence"));

    let output = launcher().arg("/bin/true").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--launch-sdl -- COMMAND"));
}

#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{self, Command},
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn cli_routes_detent_audio_and_surfaces_player_errors() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("jsr-haptics-{}-{nonce}", process::id()));
    fs::create_dir(&directory).unwrap();
    let player = directory.join("aplay");
    let capture = directory.join("capture.pcm");
    fs::write(&player, r#"#!/bin/sh
[ "$1 $2 $3 $4 $5 $6 $7 $8 $9 ${10} ${11} ${12}" = '-q -N -D test-device -t raw -c 4 -r 48000 -f S16_LE' ] || exit 9
if [ "$FAIL_PLAYBACK" = yes ]; then
    printf 'simulated ALSA device busy\n' >&2
    exit 3
fi
/bin/cat > "$CAPTURE"
"#).unwrap();
    fs::set_permissions(&player, fs::Permissions::from_mode(0o700)).unwrap();
    let run = |fail| {
        Command::new(env!("CARGO_BIN_EXE_joyshockrust-linux"))
            .args([
                "--test-haptics",
                "--pattern",
                "detent",
                "--pcm",
                "test-device",
            ])
            .env("PATH", &directory)
            .env("CAPTURE", &capture)
            .env("FAIL_PLAYBACK", fail)
            .output()
            .unwrap()
    };
    let success = run("no");
    let pcm = fs::read(&capture).unwrap();
    let failed = run("yes");
    fs::remove_dir_all(&directory).unwrap();

    assert!(success.status.success(), "{success:?}");
    assert!(String::from_utf8_lossy(&success.stdout).contains("PCM playback completed"));
    let (frames, remainder) = pcm.as_chunks::<8>();
    assert!(remainder.is_empty());
    assert!(frames.iter().all(|frame| frame[..4] == [0; 4]));
    assert!(frames.iter().any(|frame| frame[4..] != [0; 4]));
    assert!(!failed.status.success(), "{failed:?}");
    let error = String::from_utf8_lossy(&failed.stderr);
    assert!(error.contains("simulated ALSA device busy"), "{error}");
    assert!(error.contains("aplay on test-device"), "{error}");
}

#[test]
fn cli_rejects_unknown_patterns_and_reports_missing_controller_nodes() {
    let binary = env!("CARGO_BIN_EXE_joyshockrust-linux");
    let invalid = Command::new(binary)
        .args(["--test-haptics", "--pattern", "unknown", "--pcm", "null"])
        .output()
        .unwrap();
    assert!(!invalid.status.success(), "{invalid:?}");
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("usage:"));

    let missing = Command::new(binary)
        .args(["--test-haptics", "/dev/hidraw-not-a-device"])
        .output()
        .unwrap();
    assert!(!missing.status.success(), "{missing:?}");
    assert!(String::from_utf8_lossy(&missing.stderr).contains("/dev/hidraw-not-a-device"));
}

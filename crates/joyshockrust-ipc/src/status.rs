use std::{
    ffi::OsString,
    fmt::{Display, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

pub const STATUS_VERSION: &str = "1";

macro_rules! status_fields {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Field { $($variant,)+ }

        impl Field {
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name,)+ }
            }
        }
    };
}

status_fields! {
    Version => "version",
    Controller => "controller",
    GyroConfigured => "gyro_configured",
    LastReload => "last_reload",
    Transport => "transport",
    Model => "model",
    BatteryState => "battery_state",
    BatteryPercent => "battery_percent",
    ReportsDecoded => "reports_decoded",
    ReportsRejected => "reports_rejected",
    ReportRateHz => "report_rate_hz",
    ReportFullBatches => "report_full_batches",
    UinputGamepadBatches => "uinput_gamepad_batches",
    UinputMouseBatches => "uinput_mouse_batches",
    ControllerSessionsStarted => "controller_sessions_started",
    ControllerSessionsEnded => "controller_sessions_ended",
    SelectedTransport => "selected_transport",
    SelectedModel => "selected_model",
    ControllerIdConfigured => "controller_id_configured",
    UinputKeyboardBatches => "uinput_keyboard_batches",
    UinputMouseButtonBatches => "uinput_mouse_button_batches",
    ProfileSource => "profile_source",
    ActiveHidraw => "active_hidraw",
    VirtualGamepadEnabled => "virtual_gamepad_enabled",
    VirtualGamepadKind => "virtual_gamepad_kind",
    VirtualGamepadSysfs => "virtual_gamepad_sysfs",
    NativeTouchpadGrabbed => "native_touchpad_grabbed",
    NativeTouchpadGrabRequested => "native_touchpad_grab_requested",
    OutputsPaused => "outputs_paused",
    ProfileName => "profile_name",
    GyroMouseEnabled => "gyro_mouse_enabled",
    EmulationMasterEnabled => "emulation_master_enabled",
    VirtualGamepadSelected => "virtual_gamepad_selected",
    GyroMouseSelected => "gyro_mouse_selected",
    ProfilePathHex => "profile_path_hex",
    Imu => "imu",
    GyroSpeedDps => "gyro_speed_dps",
    StickTravel => "stick_travel",
    TriggerTravel => "trigger_travel",
    TouchContacts => "touch_contacts",
}

impl Field {
    /// Look up the first occurrence, as the desktop clients have historically done.
    pub fn line_value(self, line: &str) -> Option<&str> {
        let (key, value) = line.split_once('=')?;
        (key == self.name()).then_some(value)
    }

    pub fn value(self, snapshot: &str) -> Option<&str> {
        snapshot.lines().find_map(|line| self.line_value(line))
    }
}

pub fn write_field(output: &mut String, field: Field, value: impl Display) {
    let _ = writeln!(output, "{}={value}", field.name());
}

pub fn encode_profile_path(path: &Path) -> String {
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub fn decode_profile_path(hex: &str) -> Option<PathBuf> {
    let (chunks, remainder) = hex.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let bytes: Vec<u8> = chunks
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect::<Option<_>>()?;
    let path = PathBuf::from(OsString::from_vec(bytes));
    path.is_absolute().then_some(path)
}

pub fn hidraw_device_name(value: &str) -> bool {
    value.strip_prefix("/dev/hidraw").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_preserve_first_match_and_equals_in_values() {
        let snapshot = "version=1\nunknown=future\nprofile_name=a=b\nprofile_name=second\n";
        assert_eq!(Field::Version.value(snapshot), Some(STATUS_VERSION));
        assert_eq!(Field::ProfileName.value(snapshot), Some("a=b"));
        assert_eq!(Field::BatteryState.value(snapshot), None);
    }

    #[test]
    fn profile_path_preserves_non_utf8_bytes_and_rejects_relative_paths() {
        let original = PathBuf::from(OsString::from_vec(b"/tmp/a\xff".to_vec()));
        let encoded = encode_profile_path(&original);
        assert_eq!(decode_profile_path(&encoded), Some(original));
        assert_eq!(decode_profile_path("f"), None);
        assert_eq!(decode_profile_path("61"), None);
    }
}

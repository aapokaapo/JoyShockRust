use std::fmt;

use joyshockrust_input::Transport;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Selection {
    #[default]
    Usb,
    Bluetooth,
    Auto,
}

impl Selection {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "usb" => Ok(Self::Usb),
            "bluetooth" => Ok(Self::Bluetooth),
            "auto" => Ok(Self::Auto),
            _ => Err("expected usb, bluetooth or auto"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Bluetooth => "bluetooth",
            Self::Auto => "auto",
        }
    }

    pub fn accepts(self, transport: Transport) -> bool {
        matches!(
            (self, transport),
            (Self::Usb | Self::Auto, Transport::Usb)
                | (Self::Bluetooth | Self::Auto, Transport::Bluetooth)
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelSelection {
    #[default]
    Any,
    #[serde(rename = "dualsense")]
    DualSense,
    #[serde(rename = "dualsense-edge")]
    DualSenseEdge,
}

impl ModelSelection {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "any" => Ok(Self::Any),
            "dualsense" => Ok(Self::DualSense),
            "dualsense-edge" => Ok(Self::DualSenseEdge),
            _ => Err("expected any, dualsense or dualsense-edge"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::DualSense => "dualsense",
            Self::DualSenseEdge => "dualsense-edge",
        }
    }
}

/// Controller address exposed by the Linux DualSense driver as HID unique ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerId([u8; 6]);

impl ControllerId {
    pub fn parse(value: &str) -> Option<Self> {
        let mut address = [0; 6];
        let mut parts = value.split(':');
        for byte in &mut address {
            let part = parts.next()?;
            if part.len() != 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            *byte = u8::from_str_radix(part, 16).ok()?;
        }
        parts.next().is_none().then_some(Self(address))
    }
}

impl<'de> serde::Deserialize<'de> for ControllerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| {
            serde::de::Error::custom("expected a controller ID like 00:11:22:33:44:55")
        })
    }
}

impl serde::Serialize for ControllerId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl fmt::Display for ControllerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, f] = self.0;
        write!(formatter, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
    }
}

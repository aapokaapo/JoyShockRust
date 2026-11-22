use std::{fmt, fs::File, io, os::fd::AsRawFd};

use joyshockrust_input::{
    DUALSENSE_USB_CALIBRATION_REPORT_ID, DUALSENSE_USB_CALIBRATION_REPORT_LEN, DeviceModel,
    FactoryCalibration, Transport,
};

#[repr(C)]
#[derive(Default)]
struct HidrawDevInfo {
    bus_type: u32,
    vendor: i16,
    product: i16,
}

nix::ioctl_read!(hid_get_raw_info, b'H', 0x03, HidrawDevInfo);
nix::ioctl_readwrite_buf!(hid_get_feature, b'H', 0x07, u8);
nix::ioctl_read_buf!(hid_get_raw_unique, b'H', 0x08, u8);

/// Controller address exposed by the Linux DualSense driver as HID unique ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerId([u8; 6]);

impl ControllerId {
    fn parse(value: &str) -> Option<Self> {
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

impl fmt::Display for ControllerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, f] = self.0;
        write!(formatter, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{f:02x}")
    }
}

pub fn controller_id(device: &File) -> io::Result<Option<ControllerId>> {
    let mut unique = [0; 128];
    // SAFETY: The borrowed file and writable buffer outlive the ioctl.
    // HIDIOCGRAWUNIQ encodes the buffer length and copies at most that size.
    let length = unsafe { hid_get_raw_unique(device.as_raw_fd(), &mut unique) }? as usize;
    let bytes = unique
        .get(..length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "oversized HID unique ID"))?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    Ok(std::str::from_utf8(bytes)
        .ok()
        .and_then(ControllerId::parse))
}

pub fn read_factory_calibration(
    device: &File,
    transport: Transport,
) -> io::Result<FactoryCalibration> {
    let mut report = [0; DUALSENSE_USB_CALIBRATION_REPORT_LEN];
    report[0] = DUALSENSE_USB_CALIBRATION_REPORT_ID;
    // SAFETY: The open descriptor stays alive and HIDIOCGFEATURE encodes the
    // slice length in the request. The buffer is writable for that full length.
    let length = unsafe { hid_get_feature(device.as_raw_fd(), &mut report) }? as usize;
    let bytes = report.get(..length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized HID calibration reply",
        )
    })?;
    let calibration = match transport {
        Transport::Usb => FactoryCalibration::from_report(bytes),
        Transport::Bluetooth => FactoryCalibration::from_bluetooth_report(bytes),
        Transport::Other => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported HID transport",
            ));
        }
    };
    calibration.map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub model: DeviceModel,
    pub transport: Transport,
}

impl Identity {
    pub fn from_hid_id(id: &str) -> Option<Self> {
        let mut fields = id.split(':');
        let bus = u16::from_str_radix(fields.next()?, 16).ok()?;
        let vendor = u16::from_str_radix(fields.next()?, 16).ok()?;
        let product = u16::from_str_radix(fields.next()?, 16).ok()?;
        if fields.next().is_some() {
            return None;
        }
        Self::from_ids(u32::from(bus), vendor, product)
    }

    fn from_ids(bus: u32, vendor: u16, product: u16) -> Option<Self> {
        let transport = match bus {
            bus if bus == u32::from(evdev::BusType::BUS_USB.0) => Transport::Usb,
            bus if bus == u32::from(evdev::BusType::BUS_BLUETOOTH.0) => Transport::Bluetooth,
            _ => return None,
        };
        Self::from_vid_pid(vendor, product, transport)
    }

    pub fn from_vid_pid(vendor: u16, product: u16, transport: Transport) -> Option<Self> {
        let model = DeviceModel::from_vid_pid(vendor, product);
        (!matches!(model, DeviceModel::Other { .. })).then_some(Self { model, transport })
    }

    pub fn model_name(self) -> &'static str {
        match self.model {
            DeviceModel::DualSense => "DualSense",
            DeviceModel::DualSenseEdge => "DualSense Edge",
            DeviceModel::Other { .. } => "unknown model",
        }
    }
}

pub fn identity(device: &File) -> io::Result<Option<Identity>> {
    let mut info = HidrawDevInfo::default();
    // SAFETY: The borrowed file keeps the descriptor alive. HIDIOCGRAWINFO
    // writes the Linux hidraw_devinfo struct to this writable, C-layout buffer.
    unsafe { hid_get_raw_info(device.as_raw_fd(), &mut info) }?;
    Ok(Identity::from_ids(
        info.bus_type,
        info.vendor as u16,
        info.product as u16,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_id_requires_six_hex_octets_and_normalizes_case() {
        assert_eq!(
            ControllerId::parse("01:23:45:67:89:AB"),
            ControllerId::parse("01:23:45:67:89:ab")
        );
        assert_eq!(
            ControllerId::parse("01:23:45:67:89:AB")
                .unwrap()
                .to_string(),
            "01:23:45:67:89:ab"
        );
        for invalid in [
            "",
            "01:23:45:67:89",
            "01:23:45:67:89:az",
            "1:23:45:67:89:ab",
            "01:23:45:67:89:ab:00",
        ] {
            assert!(ControllerId::parse(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn sysfs_identity_accepts_only_supported_models_and_buses() {
        for (id, model, transport) in [
            (
                "0003:0000054C:00000CE6",
                DeviceModel::DualSense,
                Transport::Usb,
            ),
            (
                "0005:0000054C:00000CE6",
                DeviceModel::DualSense,
                Transport::Bluetooth,
            ),
            (
                "0003:0000054C:00000DF2",
                DeviceModel::DualSenseEdge,
                Transport::Usb,
            ),
            (
                "0005:0000054C:00000DF2",
                DeviceModel::DualSenseEdge,
                Transport::Bluetooth,
            ),
        ] {
            assert_eq!(
                Identity::from_hid_id(id),
                Some(Identity { model, transport })
            );
        }
        for id in [
            "0006:0000054C:00000CE6",
            "0003:0000045E:00000CE6",
            "0005:0000054C:0000FFFF",
            "0003:0000054C",
            "0003:0000054C:00000CE6:01",
        ] {
            assert!(Identity::from_hid_id(id).is_none(), "{id}");
        }
    }
}

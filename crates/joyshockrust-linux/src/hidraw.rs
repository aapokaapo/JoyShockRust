use std::{fs::File, io, os::fd::AsRawFd};

use joyshockrust_input::{
    DUALSENSE_USB_CALIBRATION_REPORT_ID, DUALSENSE_USB_CALIBRATION_REPORT_LEN,
    UsbFactoryCalibration,
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

pub fn read_factory_calibration(device: &File) -> io::Result<UsbFactoryCalibration> {
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
    UsbFactoryCalibration::from_report(bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub fn is_wired_dualsense_edge(device: &File) -> io::Result<bool> {
    let mut info = HidrawDevInfo::default();
    // SAFETY: The borrowed file keeps the descriptor alive. HIDIOCGRAWINFO
    // writes the Linux hidraw_devinfo struct to this writable, C-layout buffer.
    unsafe { hid_get_raw_info(device.as_raw_fd(), &mut info) }?;
    Ok(info.bus_type == u32::from(evdev::BusType::BUS_USB.0)
        && joyshockrust_input::DeviceModel::from_vid_pid(info.vendor as u16, info.product as u16)
            == joyshockrust_input::DeviceModel::DualSenseEdge)
}

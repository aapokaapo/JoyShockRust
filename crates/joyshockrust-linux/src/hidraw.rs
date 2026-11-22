use std::{fs::File, io, os::fd::AsRawFd};

#[repr(C)]
#[derive(Default)]
struct HidrawDevInfo {
    bus_type: u32,
    vendor: i16,
    product: i16,
}

nix::ioctl_read!(hid_get_raw_info, b'H', 0x03, HidrawDevInfo);

pub fn is_wired_dualsense_edge(device: &File) -> io::Result<bool> {
    let mut info = HidrawDevInfo::default();
    // SAFETY: The borrowed file keeps the descriptor alive. HIDIOCGRAWINFO
    // writes the Linux hidraw_devinfo struct to this writable, C-layout buffer.
    unsafe { hid_get_raw_info(device.as_raw_fd(), &mut info) }?;
    Ok(info.bus_type == u32::from(evdev::BusType::BUS_USB.0)
        && joyshockrust_input::DeviceModel::from_vid_pid(info.vendor as u16, info.product as u16)
            == joyshockrust_input::DeviceModel::DualSenseEdge)
}

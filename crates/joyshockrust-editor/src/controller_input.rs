use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use adw::{gio, glib, prelude::*};
use joyshockrust_core::Controls;
use joyshockrust_input::{
    DUALSENSE_EDGE_BLUETOOTH_REPORT_LEN, DUALSENSE_EDGE_USB_REPORT_LEN, DeviceModel, Transport,
    parse_report,
};

pub fn status_address() -> Option<gio::UnixSocketAddress> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let runtime = Path::new(&runtime);
    runtime
        .is_absolute()
        .then(|| gio::UnixSocketAddress::new(&runtime.join("joyshockrust/status.sock")))
}

pub async fn status_value(
    client: &gio::SocketClient,
    address: &gio::UnixSocketAddress,
    key: &str,
) -> Result<Option<String>, glib::Error> {
    let connection = client.connect_future(address).await?;
    let stream = gio::DataInputStream::new(&connection.input_stream());
    for _ in 0..64 {
        let Some(line) = stream
            .read_line_utf8_future(glib::Priority::DEFAULT)
            .await?
        else {
            break;
        };
        if let Some(value) = line.strip_prefix(key) {
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(None)
}

pub fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

pub fn read_reports(
    mut input: impl Read,
    bytes: &mut [u8],
    limit: usize,
    mut publish: impl FnMut(Controls) -> bool,
) -> io::Result<()> {
    for _ in 0..limit {
        let len = match input.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Controller disconnected",
                ));
            }
            Ok(len) => len,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let transport = match len {
            DUALSENSE_EDGE_USB_REPORT_LEN => Transport::Usb,
            DUALSENSE_EDGE_BLUETOOTH_REPORT_LEN => Transport::Bluetooth,
            _ => continue,
        };
        // The controls used for navigation have the same layout on both supported models.
        if let Ok(report) = parse_report(DeviceModel::DualSense, transport, &bytes[..len], None)
            && !publish(report.controls)
        {
            break;
        }
    }
    Ok(())
}

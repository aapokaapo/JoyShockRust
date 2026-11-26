use joyshockrust_ipc::status::Field;
use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    rc::Rc,
    time::Duration,
};

use adw::{gio, glib, gtk, prelude::*};
use joyshockrust_core::Controls;
use joyshockrust_input::{
    DUALSENSE_EDGE_BLUETOOTH_REPORT_LEN, DUALSENSE_EDGE_USB_REPORT_LEN, DeviceModel, Transport,
    parse_report,
};

pub fn status_address() -> Option<gio::UnixSocketAddress> {
    joyshockrust_config::paths::runtime_socket(joyshockrust_ipc::STATUS_SOCKET, true)
        .map(|path| gio::UnixSocketAddress::new(&path))
}

pub async fn status_value(
    client: &gio::SocketClient,
    address: &gio::UnixSocketAddress,
    key: Field,
) -> Result<Option<String>, glib::Error> {
    let [value] = status_values(client, address, [key]).await?;
    Ok(value)
}

async fn status_values<const N: usize>(
    client: &gio::SocketClient,
    address: &gio::UnixSocketAddress,
    keys: [Field; N],
) -> Result<[Option<String>; N], glib::Error> {
    let connection = client.connect_future(address).await?;
    let stream = gio::DataInputStream::new(&connection.input_stream());
    let mut values = std::array::from_fn(|_| None);
    for _ in 0..64 {
        let Some(line) = stream
            .read_line_utf8_future(glib::Priority::DEFAULT)
            .await?
        else {
            break;
        };
        for (slot, field) in values.iter_mut().zip(keys) {
            if slot.is_none()
                && let Some(value) = field.line_value(&line)
            {
                *slot = Some(value.to_owned());
            }
        }
        if values.iter().all(Option::is_some) {
            break;
        }
    }
    Ok(values)
}

pub fn watch_status(
    widget: &impl IsA<gtk::Widget>,
    key: Field,
    interval: Duration,
    update: impl Fn(Result<Option<String>, ()>) + 'static,
) {
    watch_status_fields(widget, [key], interval, move |result| {
        update(result.map(|[value]| value));
    });
}

pub fn watch_status_fields<const N: usize>(
    widget: &impl IsA<gtk::Widget>,
    fields: [Field; N],
    interval: Duration,
    update: impl Fn(Result<[Option<String>; N], ()>) + 'static,
) {
    let update = Rc::new(update);
    let polling = Rc::new(RefCell::new(None::<glib::JoinHandle<()>>));
    let on_map = polling.clone();
    let start = move || {
        let update = update.clone();
        let task = glib::MainContext::default().spawn_local(async move {
            update(Ok(std::array::from_fn(|_| None)));
            let Some(address) = status_address() else {
                update(Err(()));
                return;
            };
            let client = gio::SocketClient::new();
            client.set_timeout(1);
            loop {
                update(
                    status_values(&client, &address, fields)
                        .await
                        .map_err(|_| ()),
                );
                glib::timeout_future(interval).await;
            }
        });
        on_map.replace(Some(task));
    };
    let on_map = start.clone();
    widget.connect_map(move |_| on_map());
    widget.connect_unmap(move |_| {
        if let Some(task) = polling.borrow_mut().take() {
            task.abort();
        }
    });
    // The inspector can attach a chart to an already visible preferences group.
    if widget.as_ref().is_mapped() {
        start();
    }
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

#[cfg(test)]
mod tests {
    use std::{io::Write, os::unix::net::UnixListener};

    use super::*;

    #[test]
    fn status_reads_related_gyro_fields_from_one_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .write_all(b"version=1\nimu=1,2,3,0,0,1\nimu=ignored\ngyro_speed_dps=2.2\n")
                    .unwrap();
            });
            glib::MainContext::new().block_on(async {
                let client = gio::SocketClient::new();
                client.set_timeout(1);
                let address = gio::UnixSocketAddress::new(&path);
                let [imu, speed, absent] = status_values(
                    &client,
                    &address,
                    [Field::Imu, Field::GyroSpeedDps, Field::TouchContacts],
                )
                .await
                .unwrap();
                assert_eq!(imu.as_deref(), Some("1,2,3,0,0,1"));
                assert_eq!(speed.as_deref(), Some("2.2"));
                assert_eq!(absent, None);
            });
        });
    }
}

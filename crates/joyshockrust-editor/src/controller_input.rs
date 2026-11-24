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

pub fn watch_status(
    widget: &impl IsA<gtk::Widget>,
    key: &'static str,
    interval: Duration,
    update: impl Fn(Result<Option<String>, ()>) + 'static,
) {
    let update = Rc::new(update);
    let polling = Rc::new(RefCell::new(None::<glib::JoinHandle<()>>));
    let on_map = polling.clone();
    let start = move || {
        let update = update.clone();
        let task = glib::MainContext::default().spawn_local(async move {
            update(Ok(None));
            let Some(address) = status_address() else {
                update(Err(()));
                return;
            };
            let client = gio::SocketClient::new();
            client.set_timeout(1);
            loop {
                update(status_value(&client, &address, key).await.map_err(|_| ()));
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

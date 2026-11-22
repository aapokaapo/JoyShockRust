use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use joyshockrust_input::Transport;
use nix::poll::PollFlags;

use super::{
    reload::{self, Reload},
    status::StatusSocket,
    systemd,
};

pub fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Usb => "wired",
        Transport::Bluetooth => "Bluetooth",
        Transport::Other => "unknown transport",
    }
}

pub fn open_edge(path: &Path, transport: Transport) -> io::Result<File> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    if super::hidraw::edge_transport(&input)? != Some(transport) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("expected a {} DualSense Edge", transport_name(transport)),
        ));
    }
    Ok(input)
}

pub enum Event<'a> {
    Connected(&'a mut File),
    Reload(reload::Request<'a>),
}

pub fn run(
    transport: Transport,
    reload: Option<&Reload>,
    status: Option<&StatusSocket>,
    mut handle: impl FnMut(Event<'_>) -> io::Result<()>,
) -> io::Result<()> {
    // Subscribe before enumerating so a connection between the scan and the
    // wait is still visible to the process.
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("hidraw")?
        .listen()?;
    // The startup profile, status listener and udev monitor are ready.
    systemd::notify_ready()?;
    let mut disconnected = None;
    let mut waiting = false;
    loop {
        if let Some((path, mut input)) = find_edge(transport, disconnected.as_deref())? {
            waiting = false;
            eprintln!("Connected: {}", path.display());
            handle(Event::Connected(&mut input))?;
            eprintln!("Disconnected: {}", path.display());
            // Give udev a chance to settle before reopening the same path.
            // A different already-connected controller can take over immediately.
            disconnected = Some(path);
        } else {
            if !waiting {
                eprintln!("Waiting for a {} DualSense Edge", transport_name(transport));
                waiting = true;
            }
            loop {
                let (ready, request) = reload::wait(monitor.as_fd(), reload, status)?;
                if ready.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
                    return Err(io::Error::other("udev monitor lost"));
                }
                let requested = request.is_some();
                if let Some(request) = request {
                    handle(Event::Reload(request))?;
                }
                if ready.contains(PollFlags::POLLIN) {
                    for _ in monitor.iter().take(64) {}
                    disconnected = None;
                    break;
                }
                if requested {
                    break;
                }
            }
        }
    }
}

fn find_edge(transport: Transport, skip: Option<&Path>) -> io::Result<Option<(PathBuf, File)>> {
    for path in edge_nodes(transport)? {
        if Some(path.as_path()) == skip {
            continue;
        }
        match open_edge(&path, transport) {
            Ok(file) => return Ok(Some((path, file))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", path.display()),
        }
    }
    Ok(None)
}

pub fn edge_nodes(transport: Transport) -> io::Result<Vec<PathBuf>> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    let mut nodes = Vec::new();
    for device in enumerator.scan_devices()? {
        let Some(path) = device.devnode() else {
            continue;
        };
        let matches = match transport {
            Transport::Usb => device
                .parent_with_subsystem_devtype("usb", "usb_device")?
                .is_some_and(|usb| {
                    usb.attribute_value("idVendor") == Some(OsStr::new("054c"))
                        && usb.attribute_value("idProduct") == Some(OsStr::new("0df2"))
                }),
            Transport::Bluetooth => device.parent_with_subsystem("hid")?.is_some_and(|hid| {
                hid.property_value("HID_ID") == Some(OsStr::new("0005:0000054C:00000DF2"))
            }),
            Transport::Other => false,
        };
        if matches {
            nodes.push(path.to_path_buf());
        }
    }
    Ok(nodes)
}

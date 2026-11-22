use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use nix::poll::PollFlags;

use super::{
    reload::{self, Reload},
    status::StatusSocket,
};

pub fn open_wired_edge(path: &Path) -> io::Result<File> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    if !super::hidraw::is_wired_dualsense_edge(&input)? {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "expected a wired DualSense Edge",
        ));
    }
    Ok(input)
}

pub enum Event<'a> {
    Connected(&'a mut File),
    Reload(&'a Reload),
}

pub fn run(
    reload: Option<&Reload>,
    status: Option<&StatusSocket>,
    mut handle: impl FnMut(Event<'_>) -> io::Result<()>,
) -> io::Result<()> {
    // Subscribe before enumerating so a connection between the scan and the
    // wait is still visible to the process.
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("hidraw")?
        .listen()?;
    let mut disconnected = None;
    let mut waiting = false;
    loop {
        if let Some((path, mut input)) = find_wired_edge(disconnected.as_deref())? {
            waiting = false;
            eprintln!("Connected: {}", path.display());
            handle(Event::Connected(&mut input))?;
            eprintln!("Disconnected: {}", path.display());
            // Give udev a chance to settle before reopening the same path.
            // A different already-connected controller can take over immediately.
            disconnected = Some(path);
        } else {
            if !waiting {
                eprintln!("Waiting for a wired DualSense Edge");
                waiting = true;
            }
            loop {
                let (ready, requested) = reload::wait(monitor.as_fd(), reload, status)?;
                if ready.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
                    return Err(io::Error::other("udev monitor lost"));
                }
                if let Some(reload) = reload.filter(|_| requested) {
                    handle(Event::Reload(reload))?;
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

fn find_wired_edge(skip: Option<&Path>) -> io::Result<Option<(PathBuf, File)>> {
    for path in wired_edge_nodes()? {
        if Some(path.as_path()) == skip {
            continue;
        }
        match open_wired_edge(&path) {
            Ok(file) => return Ok(Some((path, file))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", path.display()),
        }
    }
    Ok(None)
}

pub fn wired_edge_nodes() -> io::Result<Vec<PathBuf>> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    let mut nodes = Vec::new();
    for device in enumerator.scan_devices()? {
        let Some(path) = device.devnode() else {
            continue;
        };
        let Some(usb) = device.parent_with_subsystem_devtype("usb", "usb_device")? else {
            continue;
        };
        if usb.attribute_value("idVendor") != Some(OsStr::new("054c"))
            || usb.attribute_value("idProduct") != Some(OsStr::new("0df2"))
        {
            continue;
        }
        nodes.push(path.to_path_buf());
    }
    Ok(nodes)
}

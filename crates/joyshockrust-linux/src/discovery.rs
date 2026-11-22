use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
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

pub fn run(mut serve: impl FnMut(&mut File) -> io::Result<()>) -> io::Result<()> {
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
            serve(&mut input)?;
            eprintln!("Disconnected: {}", path.display());
            // Give udev a chance to settle before reopening the same path.
            // A different already-connected controller can take over immediately.
            disconnected = Some(path);
        } else {
            if !waiting {
                eprintln!("Waiting for a wired DualSense Edge");
                waiting = true;
            }
            wait_for_change(&monitor)?;
            disconnected = None;
        }
    }
}

fn find_wired_edge(skip: Option<&Path>) -> io::Result<Option<(PathBuf, File)>> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    for device in enumerator.scan_devices()? {
        let Some(path) = device.devnode() else {
            continue;
        };
        if Some(path) == skip {
            continue;
        }
        let Some(usb) = device.parent_with_subsystem_devtype("usb", "usb_device")? else {
            continue;
        };
        if usb.attribute_value("idVendor") != Some(OsStr::new("054c"))
            || usb.attribute_value("idProduct") != Some(OsStr::new("0df2"))
        {
            continue;
        }
        match open_wired_edge(path) {
            Ok(file) => return Ok(Some((path.to_path_buf(), file))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", path.display()),
        }
    }
    Ok(None)
}

fn wait_for_change(monitor: &udev::MonitorSocket) -> io::Result<()> {
    loop {
        let mut fds = [PollFd::new(monitor.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
        let flags = fds[0].revents().unwrap_or_else(PollFlags::empty);
        if flags.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
            return Err(io::Error::other("udev monitor lost"));
        }
        if flags.contains(PollFlags::POLLIN) {
            for _ in monitor.iter().take(64) {}
            return Ok(());
        }
    }
}

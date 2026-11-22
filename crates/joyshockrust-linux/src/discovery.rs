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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Usb,
    Bluetooth,
    Auto,
}

impl Selection {
    fn accepts(self, transport: Transport) -> bool {
        matches!(
            (self, transport),
            (Self::Usb | Self::Auto, Transport::Usb)
                | (Self::Bluetooth | Self::Auto, Transport::Bluetooth)
        )
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Usb => "wired DualSense Edge",
            Self::Bluetooth => "Bluetooth DualSense Edge",
            Self::Auto => "DualSense Edge over USB or Bluetooth",
        }
    }
}

pub fn open_edge(path: &Path, selection: Selection) -> io::Result<(File, Transport)> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    let transport = super::hidraw::edge_transport(&input)?
        .filter(|&transport| selection.accepts(transport))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("expected a {}", selection.description()),
            )
        })?;
    Ok((input, transport))
}

pub enum Event<'a> {
    Connected(&'a mut File, Transport),
    Reload(reload::Request<'a>),
}

pub fn run(
    selection: Selection,
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
        if let Some(((path, transport), mut input)) = find_edge(selection, disconnected.as_ref())? {
            waiting = false;
            eprintln!(
                "Connected: {} ({})",
                path.display(),
                transport_name(transport)
            );
            handle(Event::Connected(&mut input, transport))?;
            eprintln!(
                "Disconnected: {} ({})",
                path.display(),
                transport_name(transport)
            );
            // Give udev a chance to settle before reopening the same path.
            // A different already-connected controller can take over immediately.
            disconnected = Some((path, transport));
        } else {
            if !waiting {
                eprintln!("Waiting for a {}", selection.description());
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

type Candidate = (PathBuf, Transport);

fn find_edge(
    selection: Selection,
    skip: Option<&Candidate>,
) -> io::Result<Option<(Candidate, File)>> {
    Ok(first_available(
        edge_nodes(selection)?,
        skip,
        |path, transport| {
            let selection = match transport {
                Transport::Usb => Selection::Usb,
                Transport::Bluetooth => Selection::Bluetooth,
                Transport::Other => return Err(io::ErrorKind::Unsupported.into()),
            };
            open_edge(path, selection).map(|(file, _)| file)
        },
    ))
}

fn first_available<T>(
    mut nodes: Vec<Candidate>,
    skip: Option<&Candidate>,
    mut open: impl FnMut(&Path, Transport) -> io::Result<T>,
) -> Option<(Candidate, T)> {
    nodes.sort_by_key(|(_, transport)| *transport == Transport::Bluetooth);
    for candidate in nodes {
        if Some(&candidate) == skip {
            continue;
        }
        let (path, transport) = &candidate;
        match open(path, *transport) {
            Ok(input) => return Some((candidate, input)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", path.display()),
        }
    }
    None
}

pub fn edge_nodes(selection: Selection) -> io::Result<Vec<Candidate>> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    let mut nodes = Vec::new();
    for device in enumerator.scan_devices()? {
        let Some(path) = device.devnode() else {
            continue;
        };
        if selection.accepts(Transport::Usb)
            && device
                .parent_with_subsystem_devtype("usb", "usb_device")?
                .is_some_and(|usb| {
                    usb.attribute_value("idVendor") == Some(OsStr::new("054c"))
                        && usb.attribute_value("idProduct") == Some(OsStr::new("0df2"))
                })
        {
            nodes.push((path.to_path_buf(), Transport::Usb));
        } else if selection.accepts(Transport::Bluetooth)
            && device.parent_with_subsystem("hid")?.is_some_and(|hid| {
                hid.property_value("HID_ID") == Some(OsStr::new("0005:0000054C:00000DF2"))
            })
        {
            nodes.push((path.to_path_buf(), Transport::Bluetooth));
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_usb_and_falls_back_to_bluetooth() {
        let bluetooth = (PathBuf::from("/dev/hidraw0"), Transport::Bluetooth);
        let usb = (PathBuf::from("/dev/hidraw1"), Transport::Usb);
        let candidates = [bluetooth.clone(), usb.clone()];

        let selected = first_available(candidates.to_vec(), None, |_, _| Ok(()));
        assert_eq!(selected, Some((usb.clone(), ())));
        let fallback = first_available(candidates.to_vec(), None, |_, transport| {
            if transport == Transport::Usb {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        });
        assert_eq!(fallback, Some((bluetooth, ())));
    }

    #[test]
    fn a_new_transport_can_reuse_the_path_of_the_disconnected_device() {
        let path = PathBuf::from("/dev/hidraw0");
        let disconnected = (path.clone(), Transport::Bluetooth);
        let new_device = (path, Transport::Usb);
        let selected =
            first_available(vec![new_device.clone()], Some(&disconnected), |_, _| Ok(()));
        assert_eq!(selected, Some((new_device, ())));
        let selected = first_available(vec![disconnected.clone()], Some(&disconnected), |_, _| {
            Ok(())
        });
        assert!(selected.is_none());
    }
}

use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use joyshockrust_input::{DeviceModel, Transport};
use nix::poll::PollFlags;

use super::{
    hidraw::{self, Identity},
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Selection {
    #[default]
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
            Self::Usb => "wired DualSense or DualSense Edge",
            Self::Bluetooth => "Bluetooth DualSense or DualSense Edge",
            Self::Auto => "DualSense or DualSense Edge over USB or Bluetooth",
        }
    }
}

pub fn open_controller(path: &Path, selection: Selection) -> io::Result<(File, Identity)> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    let identity = hidraw::identity(&input)?
        .filter(|identity| selection.accepts(identity.transport))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("expected a {}", selection.description()),
            )
        })?;
    Ok((input, identity))
}

pub fn open_first_controller(selection: Selection) -> io::Result<(File, Identity)> {
    find_controller(selection, None)?
        .map(|((_, identity), input)| (input, identity))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("connect a {}", selection.description()),
            )
        })
}

pub enum Event<'a> {
    Connected(&'a mut File, Identity),
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
        if let Some(((path, identity), mut input)) =
            find_controller(selection, disconnected.as_ref())?
        {
            waiting = false;
            eprintln!(
                "Connected: {} ({}) {}",
                path.display(),
                transport_name(identity.transport),
                identity.model_name()
            );
            handle(Event::Connected(&mut input, identity))?;
            eprintln!(
                "Disconnected: {} ({}) {}",
                path.display(),
                transport_name(identity.transport),
                identity.model_name()
            );
            // Give udev a chance to settle before reopening the same path.
            // A different already-connected controller can take over immediately.
            disconnected = Some((path, identity));
        } else {
            if !waiting {
                eprintln!("Waiting for a {}", selection.description());
                waiting = true;
            }
            loop {
                let (ready, request, _) = reload::wait(monitor.as_fd(), reload, status, None)?;
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

type Candidate = (PathBuf, Identity);

fn find_controller(
    selection: Selection,
    skip: Option<&Candidate>,
) -> io::Result<Option<(Candidate, File)>> {
    Ok(first_available(
        controller_nodes(selection)?,
        skip,
        |path, expected| {
            let (file, actual) = open_controller(path, selection)?;
            if actual != expected {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "HID node identity changed since discovery",
                ));
            }
            Ok(file)
        },
    ))
}

fn first_available<T>(
    mut nodes: Vec<Candidate>,
    skip: Option<&Candidate>,
    mut open: impl FnMut(&Path, Identity) -> io::Result<T>,
) -> Option<(Candidate, T)> {
    nodes.sort_by_key(|(_, identity)| {
        (
            identity.transport == Transport::Bluetooth,
            identity.model != DeviceModel::DualSenseEdge,
        )
    });
    for candidate in nodes {
        if Some(&candidate) == skip {
            continue;
        }
        let (path, identity) = &candidate;
        match open(path, *identity) {
            Ok(input) => return Some((candidate, input)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", path.display()),
        }
    }
    None
}

pub fn controller_nodes(selection: Selection) -> io::Result<Vec<Candidate>> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    let mut nodes = Vec::new();
    for device in enumerator.scan_devices()? {
        let Some(path) = device.devnode() else {
            continue;
        };
        let usb = if selection.accepts(Transport::Usb) {
            device
                .parent_with_subsystem_devtype("usb", "usb_device")?
                .and_then(|usb| {
                    let vendor = usb.attribute_value("idVendor")?.to_str()?;
                    let product = usb.attribute_value("idProduct")?.to_str()?;
                    Identity::from_vid_pid(
                        u16::from_str_radix(vendor, 16).ok()?,
                        u16::from_str_radix(product, 16).ok()?,
                        Transport::Usb,
                    )
                })
        } else {
            None
        };
        if let Some(identity) = usb {
            nodes.push((path.to_path_buf(), identity));
        } else if selection.accepts(Transport::Bluetooth)
            && let Some(hid) = device.parent_with_subsystem("hid")?
            && let Some(identity) = hid
                .property_value("HID_ID")
                .and_then(OsStr::to_str)
                .and_then(Identity::from_hid_id)
                .filter(|identity| identity.transport == Transport::Bluetooth)
        {
            nodes.push((path.to_path_buf(), identity));
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str, model: DeviceModel, transport: Transport) -> Candidate {
        (PathBuf::from(path), Identity { model, transport })
    }

    #[test]
    fn auto_prefers_usb_then_edge_and_falls_back_to_bluetooth() {
        let bluetooth = candidate(
            "/dev/hidraw0",
            DeviceModel::DualSenseEdge,
            Transport::Bluetooth,
        );
        let usb = candidate("/dev/hidraw1", DeviceModel::DualSense, Transport::Usb);
        let edge_usb = candidate("/dev/hidraw2", DeviceModel::DualSenseEdge, Transport::Usb);
        let candidates = [bluetooth.clone(), usb.clone(), edge_usb.clone()];

        let selected = first_available(candidates.to_vec(), None, |_, _| Ok(()));
        assert_eq!(selected, Some((edge_usb.clone(), ())));
        let next = first_available(candidates.to_vec(), Some(&edge_usb), |_, _| Ok(()));
        assert_eq!(next, Some((usb, ())));
        let fallback = first_available(candidates.to_vec(), None, |_, identity| {
            if identity.transport == Transport::Usb {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        });
        assert_eq!(fallback, Some((bluetooth, ())));
    }

    #[test]
    fn a_new_transport_or_model_can_reuse_the_path_of_the_disconnected_device() {
        let path = "/dev/hidraw0";
        let disconnected = candidate(path, DeviceModel::DualSenseEdge, Transport::Bluetooth);
        for new_device in [
            candidate(path, DeviceModel::DualSenseEdge, Transport::Usb),
            candidate(path, DeviceModel::DualSense, Transport::Bluetooth),
        ] {
            let selected =
                first_available(vec![new_device.clone()], Some(&disconnected), |_, _| Ok(()));
            assert_eq!(selected, Some((new_device, ())));
        }
        let selected = first_available(vec![disconnected.clone()], Some(&disconnected), |_, _| {
            Ok(())
        });
        assert!(selected.is_none());
    }
}

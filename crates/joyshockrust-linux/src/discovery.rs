use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
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
    find_controller(selection, &mut HashSet::new())?
        .map(|(candidate, input)| (input, candidate.identity))
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
    let mut disconnected = HashSet::new();
    let mut waiting = false;
    loop {
        if let Some((candidate, mut input)) = find_controller(selection, &mut disconnected)? {
            let Candidate {
                path,
                syspath,
                identity,
            } = candidate;
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
            // A stale enumeration entry may outlive the descriptor. A fresh
            // HID instance can take over even if it reuses the same node name.
            disconnected.insert(syspath);
        } else {
            if !waiting {
                eprintln!("Waiting for a {}", selection.description());
                waiting = true;
            }
            loop {
                let (ready, request, _) =
                    reload::wait(monitor.as_fd(), reload, status, None, None)?;
                if ready.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
                    return Err(io::Error::other("udev monitor lost"));
                }
                let requested = request.is_some();
                if let Some(request) = request {
                    handle(Event::Reload(request))?;
                }
                if ready.contains(PollFlags::POLLIN) {
                    for _ in monitor.iter().take(64) {}
                    break;
                }
                if requested {
                    break;
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    syspath: PathBuf,
    identity: Identity,
}

fn find_controller(
    selection: Selection,
    disconnected: &mut HashSet<PathBuf>,
) -> io::Result<Option<(Candidate, File)>> {
    Ok(first_available(
        controller_nodes(selection)?,
        disconnected,
        |candidate| {
            let (file, actual) = open_controller(&candidate.path, selection)?;
            let node =
                udev::Device::from_devnum(udev::DeviceType::Character, file.metadata()?.rdev())?;
            if actual != candidate.identity || node.syspath() != candidate.syspath {
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
    disconnected: &mut HashSet<PathBuf>,
    mut open: impl FnMut(&Candidate) -> io::Result<T>,
) -> Option<(Candidate, T)> {
    disconnected.retain(|syspath| nodes.iter().any(|node| &node.syspath == syspath));
    nodes.sort_by_key(|candidate| {
        let identity = candidate.identity;
        (
            identity.transport == Transport::Bluetooth,
            identity.model != DeviceModel::DualSenseEdge,
        )
    });
    for candidate in nodes {
        if disconnected.contains(&candidate.syspath) {
            continue;
        }
        match open(&candidate) {
            Ok(input) => return Some((candidate, input)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", candidate.path.display()),
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
        let identity = if usb.is_some() {
            usb
        } else if selection.accepts(Transport::Bluetooth)
            && let Some(hid) = device.parent_with_subsystem("hid")?
        {
            hid.property_value("HID_ID")
                .and_then(OsStr::to_str)
                .and_then(Identity::from_hid_id)
                .filter(|identity| identity.transport == Transport::Bluetooth)
        } else {
            None
        };
        if let Some(identity) = identity {
            nodes.push(Candidate {
                path: path.to_path_buf(),
                syspath: device.syspath().to_path_buf(),
                identity,
            });
        }
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        path: &str,
        instance: &str,
        model: DeviceModel,
        transport: Transport,
    ) -> Candidate {
        Candidate {
            path: PathBuf::from(path),
            syspath: Path::new("/sys/devices").join(instance),
            identity: Identity { model, transport },
        }
    }

    #[test]
    fn auto_prefers_usb_then_edge_and_falls_back_to_bluetooth() {
        let bluetooth = candidate(
            "/dev/hidraw0",
            "bt",
            DeviceModel::DualSenseEdge,
            Transport::Bluetooth,
        );
        let usb = candidate(
            "/dev/hidraw1",
            "usb",
            DeviceModel::DualSense,
            Transport::Usb,
        );
        let edge_usb = candidate(
            "/dev/hidraw2",
            "edge-usb",
            DeviceModel::DualSenseEdge,
            Transport::Usb,
        );
        let candidates = [bluetooth.clone(), usb.clone(), edge_usb.clone()];
        let mut disconnected = HashSet::new();

        let selected = first_available(candidates.to_vec(), &mut disconnected, |_| Ok(()));
        assert_eq!(selected, Some((edge_usb.clone(), ())));
        disconnected.insert(edge_usb.syspath);
        let next = first_available(candidates.to_vec(), &mut disconnected, |_| Ok(()));
        assert_eq!(next, Some((usb, ())));
        disconnected.clear();
        let fallback = first_available(candidates.to_vec(), &mut disconnected, |candidate| {
            if candidate.identity.transport == Transport::Usb {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        });
        assert_eq!(fallback, Some((bluetooth, ())));
    }

    #[test]
    fn reused_node_name_accepts_a_fresh_instance_of_the_same_model() {
        let path = "/dev/hidraw0";
        let old = candidate(
            path,
            "old",
            DeviceModel::DualSenseEdge,
            Transport::Bluetooth,
        );
        let mut disconnected = HashSet::from([old.syspath.clone()]);
        let selected = first_available(vec![old.clone()], &mut disconnected, |_| {
            panic!("stale node reopened")
        });
        assert_eq!(selected, None::<(Candidate, ())>);

        for (model, transport) in [
            (DeviceModel::DualSenseEdge, Transport::Bluetooth),
            (DeviceModel::DualSenseEdge, Transport::Usb),
            (DeviceModel::DualSense, Transport::Bluetooth),
        ] {
            let new = candidate(path, "new", model, transport);
            let selected = first_available(
                vec![old.clone(), new.clone()],
                &mut disconnected,
                |candidate| {
                    assert_eq!(candidate, &new);
                    Ok(())
                },
            );
            assert_eq!(selected, Some((new, ())));
        }
    }

    #[test]
    fn stale_instances_stay_skipped_across_rescans_until_they_disappear() {
        let first = candidate(
            "/dev/hidraw0",
            "first",
            DeviceModel::DualSenseEdge,
            Transport::Usb,
        );
        let second = candidate(
            "/dev/hidraw1",
            "second",
            DeviceModel::DualSense,
            Transport::Usb,
        );
        let mut disconnected = HashSet::from([first.syspath.clone(), second.syspath.clone()]);

        for nodes in [vec![first.clone(), second.clone()], vec![second.clone()]] {
            let selected =
                first_available(nodes, &mut disconnected, |_| panic!("stale node reopened"));
            assert_eq!(selected, None::<(Candidate, ())>);
        }
        assert_eq!(disconnected, HashSet::from([second.syspath.clone()]));
        let selected = first_available(
            vec![first.clone(), second],
            &mut disconnected,
            |candidate| {
                assert_eq!(candidate, &first);
                Ok(())
            },
        );
        assert_eq!(selected, Some((first, ())));
    }
}

use std::{
    collections::HashSet,
    ffi::OsStr,
    fmt,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

use joyshockrust_input::{DeviceModel, Transport};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};

use super::{
    controller_config::ControllerConfig,
    hidraw::{self, ControllerId, Identity},
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Selection {
    #[default]
    Usb,
    Bluetooth,
    Auto,
}

impl Selection {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "usb" => Ok(Self::Usb),
            "bluetooth" => Ok(Self::Bluetooth),
            "auto" => Ok(Self::Auto),
            _ => Err("expected usb, bluetooth or auto"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Bluetooth => "bluetooth",
            Self::Auto => "auto",
        }
    }

    fn accepts(self, transport: Transport) -> bool {
        matches!(
            (self, transport),
            (Self::Usb | Self::Auto, Transport::Usb)
                | (Self::Bluetooth | Self::Auto, Transport::Bluetooth)
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelSelection {
    #[default]
    Any,
    #[serde(rename = "dualsense")]
    DualSense,
    #[serde(rename = "dualsense-edge")]
    DualSenseEdge,
}

impl ModelSelection {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "any" => Ok(Self::Any),
            "dualsense" => Ok(Self::DualSense),
            "dualsense-edge" => Ok(Self::DualSenseEdge),
            _ => Err("expected any, dualsense or dualsense-edge"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::DualSense => "dualsense",
            Self::DualSenseEdge => "dualsense-edge",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerFilter {
    pub transport: Selection,
    pub model: ModelSelection,
    pub controller_id: Option<ControllerId>,
}

impl ControllerFilter {
    pub fn accepts(self, identity: Identity) -> bool {
        self.transport.accepts(identity.transport)
            && match self.model {
                ModelSelection::Any => true,
                ModelSelection::DualSense => identity.model == DeviceModel::DualSense,
                ModelSelection::DualSenseEdge => identity.model == DeviceModel::DualSenseEdge,
            }
    }

    pub fn accepts_id(self, observed: Option<ControllerId>) -> bool {
        self.controller_id
            .is_none_or(|expected| Some(expected) == observed)
    }
}

impl fmt::Display for ControllerFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let model = match self.model {
            ModelSelection::Any => "DualSense or DualSense Edge",
            ModelSelection::DualSense => "DualSense",
            ModelSelection::DualSenseEdge => "DualSense Edge",
        };
        match self.transport {
            Selection::Usb => write!(f, "wired {model}"),
            Selection::Bluetooth => write!(f, "Bluetooth {model}"),
            Selection::Auto => write!(f, "{model} over USB or Bluetooth"),
        }?;
        if self.controller_id.is_some() {
            write!(f, " with the configured HID ID")?;
        }
        Ok(())
    }
}

pub fn open_controller(path: &Path, filter: ControllerFilter) -> io::Result<(File, Identity)> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    let identity = hidraw::identity(&input)?
        .filter(|&identity| filter.accepts(identity))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, format!("expected a {filter}"))
        })?;
    if filter.controller_id.is_some() && !filter.accepts_id(hidraw::controller_id(&input)?) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "controller HID ID is absent or differs from the selection",
        ));
    }
    Ok((input, identity))
}

pub fn open_first_controller(
    filter: ControllerFilter,
    preferred: Option<ControllerId>,
) -> io::Result<(File, Candidate)> {
    find_controller(filter, preferred, &mut HashSet::new())?
        .map(|(candidate, opened)| (opened.file, candidate))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("connect a {filter}")))
}

pub enum Event<'a> {
    Connected(&'a mut File, Identity, ControllerFilter, &'a Path),
    Reload(reload::Request<'a>, ControllerFilter),
}

pub fn run(
    mut filter: ControllerFilter,
    mut config: ControllerConfig,
    reload: Option<&Reload>,
    status: Option<&StatusSocket>,
    mut handle: impl FnMut(Event<'_>) -> io::Result<Option<ControllerFilter>>,
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
        if let Some(status) = status {
            status.set_filter(filter);
        }
        if let Some((candidate, opened)) =
            find_controller(filter, config.preferred(), &mut disconnected)?
        {
            let OpenController { mut file, id } = opened;
            let Candidate {
                path,
                syspath,
                identity,
            } = candidate;
            waiting = false;
            if let Some(id) = id {
                config.remember(id);
            }
            eprintln!(
                "Connected: {} ({}) {}",
                path.display(),
                transport_name(identity.transport),
                identity.model_name()
            );
            match handle(Event::Connected(&mut file, identity, filter, &path)) {
                Ok(Some(next)) => {
                    config.change_filters(filter, next);
                    filter = next;
                    continue;
                }
                result => finish_session(file.as_fd(), result.map(|_| ()))?,
            }
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
                eprintln!("Waiting for a {filter}");
                waiting = true;
            }
            loop {
                let (ready, request, _) =
                    reload::wait(monitor.as_fd(), reload, status, None, None)?;
                if ready.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
                    return Err(io::Error::other("udev monitor lost"));
                }
                let requested = request.is_some();
                if let Some(request) = request
                    && let Some(next) = handle(Event::Reload(request, filter))?
                    && next != filter
                {
                    config.change_filters(filter, next);
                    filter = next;
                    waiting = false;
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

fn finish_session(input: BorrowedFd<'_>, result: io::Result<()>) -> io::Result<()> {
    let Err(error) = result else {
        return Ok(());
    };
    // The setup path can fail before the report loop sees the detach.
    // Check the physical descriptor without reading pending input.
    let mut descriptor = [PollFd::new(input, PollFlags::empty())];
    loop {
        match poll(&mut descriptor, PollTimeout::ZERO) {
            Err(Errno::EINTR) => continue,
            Ok(_)
                if descriptor[0].revents().is_some_and(|flags| {
                    flags.intersects(PollFlags::POLLHUP | PollFlags::POLLERR)
                }) =>
            {
                eprintln!("Controller session interrupted by disconnect: {error}");
                return Ok(());
            }
            _ => return Err(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    syspath: PathBuf,
    pub identity: Identity,
}

struct OpenController {
    file: File,
    id: Option<ControllerId>,
}

fn find_controller(
    filter: ControllerFilter,
    preferred: Option<ControllerId>,
    disconnected: &mut HashSet<PathBuf>,
) -> io::Result<Option<(Candidate, OpenController)>> {
    Ok(first_available(
        controller_nodes(filter)?,
        disconnected,
        preferred.filter(|_| filter.controller_id.is_none()),
        |candidate| {
            let (file, actual) = open_controller(&candidate.path, filter)?;
            let node =
                udev::Device::from_devnum(udev::DeviceType::Character, file.metadata()?.rdev())?;
            if actual != candidate.identity || node.syspath() != candidate.syspath {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "HID node identity changed since discovery",
                ));
            }
            let id = filter
                .controller_id
                .or_else(|| hidraw::controller_id(&file).ok().flatten());
            Ok(OpenController { file, id })
        },
        |opened| opened.id,
    ))
}

fn first_available<T>(
    mut nodes: Vec<Candidate>,
    disconnected: &mut HashSet<PathBuf>,
    preferred: Option<ControllerId>,
    mut open: impl FnMut(&Candidate) -> io::Result<T>,
    identity: impl Fn(&T) -> Option<ControllerId>,
) -> Option<(Candidate, T)> {
    disconnected.retain(|syspath| nodes.iter().any(|node| &node.syspath == syspath));
    nodes.sort_by_key(|candidate| {
        let identity = candidate.identity;
        (
            identity.transport == Transport::Bluetooth,
            identity.model != DeviceModel::DualSenseEdge,
        )
    });
    let mut fallback = None;
    for candidate in nodes {
        if disconnected.contains(&candidate.syspath) {
            continue;
        }
        match open(&candidate) {
            Ok(input) if preferred.is_none() || identity(&input) == preferred => {
                return Some((candidate, input));
            }
            Ok(input) => {
                if fallback.is_none() {
                    fallback = Some((candidate, input));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => eprintln!("Could not use {}: {err}", candidate.path.display()),
        }
    }
    fallback
}

pub fn controller_nodes(filter: ControllerFilter) -> io::Result<Vec<Candidate>> {
    let mut enumerator = udev::Enumerator::new()?;
    enumerator.match_subsystem("hidraw")?;
    let mut nodes = Vec::new();
    for device in enumerator.scan_devices()? {
        let Some(path) = device.devnode() else {
            continue;
        };
        let usb = if filter.transport.accepts(Transport::Usb) {
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
        } else if filter.transport.accepts(Transport::Bluetooth)
            && let Some(hid) = device.parent_with_subsystem("hid")?
        {
            hid.property_value("HID_ID")
                .and_then(OsStr::to_str)
                .and_then(Identity::from_hid_id)
                .filter(|identity| identity.transport == Transport::Bluetooth)
        } else {
            None
        };
        if let Some(identity) = identity.filter(|&identity| filter.accepts(identity)) {
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
    use std::{io::Write, os::unix::net::UnixStream};

    use super::*;

    fn first_without_preference<T>(
        nodes: Vec<Candidate>,
        disconnected: &mut HashSet<PathBuf>,
        open: impl FnMut(&Candidate) -> io::Result<T>,
    ) -> Option<(Candidate, T)> {
        first_available(nodes, disconnected, None, open, |_| None)
    }

    #[test]
    fn failed_session_recovers_only_after_a_physical_disconnect() {
        let (mut sender, input) = UnixStream::pair().unwrap();
        sender.write_all(&[1]).unwrap();
        let startup_error = || io::Error::new(io::ErrorKind::PermissionDenied, "test setup error");

        let error = finish_session(input.as_fd(), Err(startup_error())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "test setup error");

        drop(sender);
        assert!(finish_session(input.as_fd(), Err(startup_error())).is_ok());
    }

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
    fn controller_filter_combines_model_and_transport() {
        let edge_usb = Identity {
            model: DeviceModel::DualSenseEdge,
            transport: Transport::Usb,
        };
        let edge_bt = Identity {
            transport: Transport::Bluetooth,
            ..edge_usb
        };
        let standard_usb = Identity {
            model: DeviceModel::DualSense,
            ..edge_usb
        };
        let standard_bt = Identity {
            transport: Transport::Bluetooth,
            ..standard_usb
        };
        let identities = [edge_usb, edge_bt, standard_usb, standard_bt];
        for (filter, expected) in [
            (ControllerFilter::default(), [true, false, true, false]),
            (
                ControllerFilter {
                    transport: Selection::Auto,
                    model: ModelSelection::DualSenseEdge,
                    controller_id: None,
                },
                [true, true, false, false],
            ),
            (
                ControllerFilter {
                    transport: Selection::Bluetooth,
                    model: ModelSelection::DualSense,
                    controller_id: None,
                },
                [false, false, false, true],
            ),
        ] {
            assert_eq!(
                identities.map(|identity| filter.accepts(identity)),
                expected
            );
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

        let selected = first_without_preference(candidates.to_vec(), &mut disconnected, |_| Ok(()));
        assert_eq!(selected, Some((edge_usb.clone(), ())));
        disconnected.insert(edge_usb.syspath);
        let next = first_without_preference(candidates.to_vec(), &mut disconnected, |_| Ok(()));
        assert_eq!(next, Some((usb, ())));
        disconnected.clear();
        let fallback =
            first_without_preference(candidates.to_vec(), &mut disconnected, |candidate| {
                if candidate.identity.transport == Transport::Usb {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                Ok(())
            });
        assert_eq!(fallback, Some((bluetooth, ())));
    }

    #[test]
    fn remembered_controller_wins_then_falls_back_or_waits() {
        let wired = candidate(
            "/dev/hidraw0",
            "wired",
            DeviceModel::DualSenseEdge,
            Transport::Usb,
        );
        let wireless = candidate(
            "/dev/hidraw1",
            "wireless",
            DeviceModel::DualSense,
            Transport::Bluetooth,
        );
        let remembered = ControllerId::parse("00:11:22:33:44:55");
        let mut disconnected = HashSet::new();
        for (nodes, expected) in [
            (
                vec![wired.clone(), wireless.clone()],
                Some(wireless.clone()),
            ),
            (vec![wired.clone()], Some(wired.clone())),
            (Vec::new(), None),
            (vec![wireless.clone()], Some(wireless.clone())),
        ] {
            let chosen = first_available(
                nodes,
                &mut disconnected,
                remembered,
                |node| Ok(if node == &wireless { remembered } else { None }),
                |id| *id,
            );
            assert_eq!(chosen.map(|(node, _)| node), expected);
        }
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
        let selected = first_without_preference(vec![old.clone()], &mut disconnected, |_| {
            panic!("stale node reopened")
        });
        assert_eq!(selected, None::<(Candidate, ())>);

        for (model, transport) in [
            (DeviceModel::DualSenseEdge, Transport::Bluetooth),
            (DeviceModel::DualSenseEdge, Transport::Usb),
            (DeviceModel::DualSense, Transport::Bluetooth),
        ] {
            let new = candidate(path, "new", model, transport);
            let selected = first_without_preference(
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
            let selected = first_without_preference(nodes, &mut disconnected, |_| {
                panic!("stale node reopened")
            });
            assert_eq!(selected, None::<(Candidate, ())>);
        }
        assert_eq!(disconnected, HashSet::from([second.syspath.clone()]));
        let selected = first_without_preference(
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

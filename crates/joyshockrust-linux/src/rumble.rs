use std::{
    env,
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    time::{Duration, Instant},
};

use evdev::{
    Device, EventSummary, FFEffect, FFEffectCode, FFEffectKind, UInputCode, uinput::VirtualDevice,
};
use nix::{
    libc,
    poll::{PollFd, PollFlags, poll},
};

pub const EFFECT_SLOTS: usize = 16;
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_INTERVAL: Duration = Duration::from_millis(25);

pub struct Rumble {
    physical: Device,
    effects: [Option<FFEffect>; EFFECT_SLOTS],
    trace: bool,
}

impl Rumble {
    pub fn open(hidraw: &File) -> io::Result<Self> {
        let node =
            udev::Device::from_devnum(udev::DeviceType::Character, hidraw.metadata()?.rdev())?;
        let hid = node.parent_with_subsystem("hid")?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not find the controller's HID parent",
            )
        })?;
        // hidraw can be ready before the evdev node and its active-seat ACL.
        // Resolve that race before creating the virtual device, whose force
        // feedback capabilities are fixed for the rest of this session.
        let deadline = Instant::now() + PROBE_TIMEOUT;
        open_when_ready(
            || Self::probe(&hid),
            || {
                let delay = deadline
                    .saturating_duration_since(Instant::now())
                    .min(PROBE_INTERVAL);
                if delay.is_zero() {
                    return Ok(false);
                }

                // Observe hangups without consuming queued HID reports.
                let mut fds = [PollFd::new(hidraw.as_fd(), PollFlags::empty())];
                poll(&mut fds, delay.as_millis().max(1) as u16)?;
                let flags = fds[0].revents().unwrap_or_else(PollFlags::empty);
                if flags.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL) {
                    return Err(io::Error::new(
                        io::ErrorKind::NotConnected,
                        "controller detached before force feedback was ready",
                    ));
                }
                Ok(true)
            },
        )
    }

    fn probe(hid: &udev::Device) -> io::Result<Self> {
        let mut devices = udev::Enumerator::new()?;
        devices.match_parent(hid)?;
        devices.match_subsystem("input")?;
        let mut open_error = None;
        let mut has_candidate = false;
        for device in devices.scan_devices()? {
            if !device.sysname().as_encoded_bytes().starts_with(b"event") {
                continue;
            }
            let Some(path) = device.devnode() else {
                continue;
            };
            // Inspect the input parent before opening any sibling event nodes.
            let Some(input) = device.parent_with_subsystem("input")? else {
                continue;
            };
            if !has_force_feedback(input.attribute_value("capabilities/ff")) {
                continue;
            }
            has_candidate = true;
            let physical = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
                .and_then(|file| Device::from_fd(file.into()));
            match physical {
                Ok(physical)
                    if physical
                        .supported_ff()
                        .is_some_and(|effects| effects.contains(FFEffectCode::FF_RUMBLE)) =>
                {
                    eprintln!("Rumble forwarding ready: {}", path.display());
                    return Ok(Self {
                        physical,
                        effects: Default::default(),
                        trace: env::var_os("JOYSHOCKRUST_RUMBLE_TRACE")
                            .is_some_and(|value| value == "1"),
                    });
                }
                Ok(_) => {}
                Err(err) => {
                    let kind = if err.raw_os_error() == Some(libc::ENODEV) {
                        io::ErrorKind::NotFound
                    } else {
                        err.kind()
                    };
                    open_error = Some(io::Error::new(kind, format!("{}: {err}", path.display())));
                }
            }
        }
        Err(open_error.unwrap_or_else(|| {
            if has_candidate {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "no physical evdev node advertises FF_RUMBLE",
                )
            } else {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no physical force feedback evdev node found",
                )
            }
        }))
    }

    pub fn service(&mut self, virtual_device: &mut VirtualDevice) -> io::Result<()> {
        // A single ready read services at most one kernel event batch.
        let events: Vec<_> = match virtual_device.fetch_events() {
            Ok(events) => events.collect(),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        for event in events {
            match event.destructure() {
                EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, _) => {
                    let mut request = match virtual_device.process_ff_upload(event) {
                        Ok(request) => request,
                        // A client can time out before the bridge sees its request.
                        Err(err) if err.raw_os_error() == Some(libc::EINVAL) => continue,
                        Err(err) => return Err(err),
                    };
                    let id = request.effect_id();
                    let data = request.effect();
                    let result = match self.effects.get_mut(id as usize) {
                        Some(slot) if matches!(data.kind, FFEffectKind::Rumble { .. }) => {
                            match slot {
                                Some(effect) => effect.update(data).map(|()| effect.id()),
                                None => self.physical.upload_ff_effect(data).map(|effect| {
                                    let physical_id = effect.id();
                                    *slot = Some(effect);
                                    physical_id
                                }),
                            }
                        }
                        _ => Err(io::Error::from_raw_os_error(libc::EINVAL)),
                    };
                    match result {
                        Ok(physical_id) if self.trace => eprintln!(
                            "Rumble upload accepted: virtual={id} physical={physical_id} length={}ms {:?}",
                            data.replay.length, data.kind,
                        ),
                        Err(err) => {
                            if self.trace {
                                eprintln!("Rumble upload failed: virtual={id}: {err}");
                            }
                            request.set_retval(-err.raw_os_error().unwrap_or(libc::EIO));
                        }
                        Ok(_) => {}
                    }
                }
                EventSummary::UInput(event, UInputCode::UI_FF_ERASE, _) => {
                    let mut request = match virtual_device.process_ff_erase(event) {
                        Ok(request) => request,
                        Err(err) if err.raw_os_error() == Some(libc::EINVAL) => continue,
                        Err(err) => return Err(err),
                    };
                    if let Some(slot) = self.effects.get_mut(request.effect_id() as usize) {
                        // FFEffect's drop stops and removes the owned effect in the kernel.
                        *slot = None;
                    } else {
                        request.set_retval(-libc::EINVAL);
                    }
                }
                EventSummary::ForceFeedback(_, id, count) if count >= 0 => {
                    if let Some(effect) =
                        self.effects.get_mut(id.0 as usize).and_then(Option::as_mut)
                    {
                        match effect.play(count) {
                            Ok(()) if self.trace => eprintln!(
                                "Rumble playback written: virtual={} physical={} count={count}",
                                id.0,
                                effect.id(),
                            ),
                            Err(err) => eprintln!("Could not forward rumble playback: {err}"),
                            Ok(()) => {}
                        }
                    } else if self.trace {
                        eprintln!(
                            "Rumble playback has no uploaded effect: virtual={} count={count}",
                            id.0
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn open_when_ready<T>(
    mut probe: impl FnMut() -> io::Result<T>,
    mut wait: impl FnMut() -> io::Result<bool>,
) -> io::Result<T> {
    loop {
        match probe() {
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) && wait()? => {}
            result => return result,
        }
    }
}

fn has_force_feedback(capabilities: Option<&OsStr>) -> bool {
    // Sysfs prints a whitespace-separated set of hexadecimal bitmap words.
    // Test for a nonzero bitmap independent of the kernel's word size.
    capabilities.is_some_and(|value| {
        value
            .as_encoded_bytes()
            .iter()
            .any(|byte| matches!(byte, b'1'..=b'9' | b'a'..=b'f' | b'A'..=b'F'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rumble_probe_skips_event_nodes_without_force_feedback() {
        assert!(!has_force_feedback(None));
        for bitmap in ["", "0", "0000 0", "0 0\n"] {
            assert!(!has_force_feedback(Some(OsStr::new(bitmap))));
        }
        for bitmap in ["10000 0", "0 0000000a", "A0000 0 0"] {
            assert!(has_force_feedback(Some(OsStr::new(bitmap))));
        }
    }
    #[test]
    fn waits_for_the_rumble_node_and_its_permissions() {
        let mut probes = [
            Err(io::ErrorKind::NotFound.into()),
            Err(io::ErrorKind::PermissionDenied.into()),
            Ok("ready"),
        ]
        .into_iter();
        let mut waits = 0;
        let opened = open_when_ready(
            || probes.next().expect("only three probe attempts"),
            || {
                waits += 1;
                Ok(true)
            },
        );
        assert_eq!(opened.unwrap(), "ready");
        assert_eq!(waits, 2);
    }

    #[test]
    fn permanent_failure_and_expired_deadline_preserve_the_probe_error() {
        let unsupported = open_when_ready::<()>(
            || Err(io::ErrorKind::Unsupported.into()),
            || panic!("unsupported devices should fall back immediately"),
        );
        assert_eq!(unsupported.unwrap_err().kind(), io::ErrorKind::Unsupported);

        let denied =
            open_when_ready::<()>(|| Err(io::ErrorKind::PermissionDenied.into()), || Ok(false));
        assert_eq!(denied.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}

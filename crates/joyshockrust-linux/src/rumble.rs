use std::{
    env,
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

use evdev::{
    Device, EventSummary, FFEffect, FFEffectCode, FFEffectKind, UInputCode, uinput::VirtualDevice,
};
use nix::libc;

use crate::hidraw;

pub const EFFECT_SLOTS: usize = 16;

pub struct Rumble {
    physical: Device,
    effects: [Option<FFEffect>; EFFECT_SLOTS],
    trace: bool,
}

impl Rumble {
    pub fn open(hidraw: &File) -> io::Result<Self> {
        let (physical, path) = Self::open_physical(hidraw)?;
        eprintln!("Rumble forwarding ready: {}", path.display());
        Ok(Self {
            physical,
            effects: Default::default(),
            trace: env::var_os("JOYSHOCKRUST_RUMBLE_TRACE").is_some_and(|value| value == "1"),
        })
    }

    pub(crate) fn open_physical(hidraw: &File) -> io::Result<(Device, PathBuf)> {
        let hid = hidraw::hid_parent(hidraw)?;
        // hidraw can be ready before the evdev node and its active-seat ACL.
        hidraw::wait_for_evdev(hidraw, || Self::probe(&hid))
    }

    fn probe(hid: &udev::Device) -> io::Result<(Device, PathBuf)> {
        let mut devices = hidraw::input_enumerator(hid)?;
        let mut open_error = None;
        let mut has_candidate = false;
        for device in devices.scan_devices()? {
            // Inspect the input parent before opening any sibling event nodes.
            let Some((path, input)) = hidraw::input_event_candidate(&device)? else {
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
                    return Ok((physical, path.to_path_buf()));
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
    use crate::hidraw::open_when_ready;

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

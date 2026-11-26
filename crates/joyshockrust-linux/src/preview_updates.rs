//! Capture bounded editor telemetry at bridge report boundaries.

use std::{
    cell::RefCell,
    collections::VecDeque,
    io,
    os::fd::BorrowedFd,
    path::Path,
    time::{Duration, Instant},
};

use joyshockrust_config::controller::{ModelSelection, Selection};
use joyshockrust_core::Controls;
use joyshockrust_input::{DeviceModel, TouchContact, Transport};
use joyshockrust_ipc::{
    telemetry::{
        GyroSpeed, ImuReading, TouchPoints,
        preview::{Device, HISTORY_LIMIT, History, Sample},
    },
    unix::subscriptions::publisher::PreviewPublisher,
};

use crate::hidraw::Identity;

const INTERVAL: Duration = Duration::from_millis(16);

pub(crate) struct Publisher {
    transport: PreviewPublisher,
    state: RefCell<State>,
}

struct State {
    generation: Option<u64>,
    session: u64,
    device: Option<Device>,
    samples: VecDeque<Sample>,
    next_sample: u64,
    pending: bool,
    publish_at: Option<Instant>,
}

impl Publisher {
    pub(crate) fn start(path: &Path) -> io::Result<Self> {
        Ok(Self {
            transport: PreviewPublisher::start("preview", path)?,
            state: RefCell::new(State {
                generation: None,
                session: 0,
                device: None,
                samples: VecDeque::with_capacity(HISTORY_LIMIT),
                next_sample: 0,
                pending: false,
                publish_at: None,
            }),
        })
    }

    pub(crate) fn wake_fd(&self) -> BorrowedFd<'_> {
        self.transport.wake_fd()
    }

    pub(crate) fn set_session(&self, identity: Option<Identity>) {
        let mut state = self.state.borrow_mut();
        state.device = identity.and_then(preview_device);
        state.reset();
    }

    pub(crate) fn observe(&self, controls: Controls, contacts: [Option<TouchContact>; 2]) {
        let mut state = self.state.borrow_mut();
        if state.generation.is_none() || state.device.is_none() {
            return;
        }
        if state.samples.len() == HISTORY_LIMIT {
            state.samples.pop_front();
        }
        state.samples.push_back(Sample {
            buttons: controls.buttons.union(controls.dpad.buttons()),
            sticks: [controls.left_stick, controls.right_stick],
            triggers: [controls.left_trigger, controls.right_trigger],
            touch: TouchPoints(contacts.map(|point| point.map(|point| (point.x, point.y)))),
        });
        state.next_sample += 1;
        state.pending = true;
    }

    pub(crate) fn publish(
        &self,
        wake: bool,
        imu: impl FnOnce() -> Option<(ImuReading, GyroSpeed)>,
    ) -> Option<Instant> {
        if wake {
            self.transport.drain_wake();
        }
        let mut state = self.state.borrow_mut();
        let generation = self.transport.generation();
        if state.generation != generation {
            state.generation = generation;
            state.reset();
        }
        let generation = generation?;
        if !state.pending {
            return None;
        }
        let now = Instant::now();
        if let Some(next) = state.publish_at.filter(|&next| next > now) {
            return Some(next);
        }

        let result = self.transport.send_for(generation, || History {
            session: state.session,
            device: state.device,
            first: state.next_sample - state.samples.len() as u64,
            samples: state.samples.iter().copied().collect(),
            imu: (!state.samples.is_empty()).then(imu).flatten(),
        });
        let next = now + INTERVAL;
        state.publish_at = Some(next);
        state.pending = result.is_err_and(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            )
        });
        state.pending.then_some(next)
    }
}

impl State {
    fn reset(&mut self) {
        self.session += 1;
        self.samples.clear();
        self.next_sample = 0;
        self.publish_at = None;
        self.pending = true;
    }
}

fn preview_device(identity: Identity) -> Option<Device> {
    let model = match identity.model {
        DeviceModel::DualSense => ModelSelection::DualSense,
        DeviceModel::DualSenseEdge => ModelSelection::DualSenseEdge,
        DeviceModel::Other { .. } => return None,
    };
    let transport = match identity.transport {
        Transport::Usb => Selection::Usb,
        Transport::Bluetooth => Selection::Bluetooth,
        Transport::Other => return None,
    };
    Some(Device { model, transport })
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        os::{fd::AsFd, unix::net::UnixStream},
        thread,
    };

    use joyshockrust_core::Button;
    use joyshockrust_input::{bluetooth_crc32, parse_report};
    use joyshockrust_ipc::{stream::Topic, telemetry::preview::Preview};

    use crate::{reload, status::StatusSocket};

    use super::*;

    #[test]
    fn bridge_preview_keeps_button_edges_and_clears_on_disconnect_across_models_and_transports() {
        let dir = std::env::temp_dir().join(format!("jsr-preview-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("status");
        {
            let mut status = StatusSocket::bind(&path).unwrap();
            status.enable_preview(&path).unwrap();
            let mut client = UnixStream::connect(Topic::Preview.socket_path(&path)).unwrap();
            let timeout = Duration::from_secs(2);
            client.set_read_timeout(Some(timeout)).unwrap();
            client.write_all(&Topic::Preview.subscribe(7)).unwrap();
            let (_writer, idle) = UnixStream::pair().unwrap();
            reload::wait(
                idle.as_fd(),
                None,
                Some(&status),
                None,
                Some(Instant::now() + timeout),
            )
            .unwrap();
            let read = |stream: &mut UnixStream| Preview::read(stream, 7).unwrap().1.history;
            let mut last = read(&mut client);
            assert!(last.device.is_none());
            for model in [DeviceModel::DualSense, DeviceModel::DualSenseEdge] {
                for transport in [Transport::Usb, Transport::Bluetooth] {
                    let identity = Identity { model, transport };
                    status.update(Some(identity), true);
                    for pressed in [false, true, false] {
                        let mut usb = [0; 64];
                        usb[0] = 1;
                        usb[1..5].fill(128);
                        usb[8] = if pressed { 0x28 } else { 8 };
                        usb[33] = 0x80;
                        usb[37] = 0x80;
                        let mut bluetooth = [0; 78];
                        bluetooth[0] = 0x31;
                        bluetooth[2..65].copy_from_slice(&usb[1..]);
                        let crc = bluetooth_crc32(0xa1, &bluetooth[..74]);
                        bluetooth[74..].copy_from_slice(&crc.to_le_bytes());
                        let frame = if transport == Transport::Usb {
                            &usb[..]
                        } else {
                            &bluetooth[..]
                        };
                        let report = parse_report(model, transport, frame, None).unwrap();
                        status.observe_preview(report.controls, report.touch_contacts);
                    }
                    assert!(status.publish_preview(false).is_none());
                    let active = read(&mut client);
                    assert!(active.session > last.session);
                    assert_eq!(active.device, preview_device(identity));
                    assert_eq!(
                        active
                            .samples
                            .iter()
                            .map(|sample| sample.buttons.contains(Button::South))
                            .collect::<Vec<_>>(),
                        [false, true, false]
                    );

                    let capture = allocation_counter::measure(|| {
                        for _ in 0..HISTORY_LIMIT + 2 {
                            status.observe_preview(Controls::default(), [None, None]);
                        }
                    });
                    assert_eq!(capture.count_total, 0);
                    if let Some(until) = status.publish_preview(false) {
                        thread::sleep(until.saturating_duration_since(Instant::now()));
                        status.publish_preview(false);
                    }
                    let capped = read(&mut client);
                    assert_eq!(capped.samples.len(), HISTORY_LIMIT);
                    assert_eq!(capped.first, 5);
                    status.update(None, false);
                    assert!(status.publish_preview(false).is_none());
                    last = read(&mut client);
                    assert!(last.session > capped.session);
                    assert!(last.samples.is_empty());
                    assert!(last.device.is_none());
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

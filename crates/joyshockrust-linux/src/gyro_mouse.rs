use std::{
    fs::File,
    io,
    time::{Duration, Instant},
};

use evdev::{
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode as Axis,
    uinput::VirtualDevice,
};
use joyshockrust_input::{FactoryCalibration, InputReport, Transport};
use joyshockrust_motion::{Calibration, StationaryCalibration, pointer::PointerMotion};

use super::{
    gyro_calibration::{self, BiasWriter},
    gyro_gate::GyroGate,
    haptics::{Cue, Haptics},
    hidraw::ControllerId,
};

pub struct GyroMouse {
    pointer: PointerMotion,
    output: VirtualDevice,
    gate: GyroGate,
    hold: Option<HoldDetector>,
    shortcut: Option<ShortcutCalibration>,
    haptics: Option<Haptics>,
    bias_writer: Option<BiasWriter>,
    controller: Option<ControllerId>,
}

struct HoldDetector {
    binding: HoldBinding,
    pressed_since: Option<Instant>,
    fired: bool,
}

impl HoldDetector {
    fn new(binding: HoldBinding) -> Self {
        Self {
            binding,
            pressed_since: None,
            fired: true,
        }
    }

    fn observe(&mut self, pressed: bool, now: Instant) -> bool {
        if !pressed {
            self.pressed_since = None;
            self.fired = false;
            return false;
        }
        if self.fired {
            return false;
        }
        if now.duration_since(*self.pressed_since.get_or_insert(now)) >= self.binding.duration {
            self.fired = true;
            return true;
        }
        false
    }
}

struct ShortcutCalibration {
    settle_at: Instant,
    deadline: Instant,
    measuring: bool,
}

use joyshockrust_config::profile::gyro::HoldBinding;
pub use joyshockrust_config::profile::gyro::{Options, validate_flick_scale};
#[cfg(test)]
pub use joyshockrust_motion::sensitivity::GyroSensitivity as Sensitivity;

pub fn open_output(name: &str) -> io::Result<VirtualDevice> {
    open_output_with_buttons(name, AttributeSet::new(), false)
}

pub(crate) fn open_output_with_buttons(
    name: &str,
    mut keys: AttributeSet<KeyCode>,
    scroll: bool,
) -> io::Result<VirtualDevice> {
    let mut axes = [Axis::REL_X, Axis::REL_Y]
        .into_iter()
        .collect::<AttributeSet<_>>();
    if scroll {
        axes.insert(Axis::REL_WHEEL);
        axes.insert(Axis::REL_HWHEEL);
    }
    // Advertise the left button so consumers classify this as a mouse.
    keys.insert(KeyCode::BTN_LEFT);
    VirtualDevice::builder()?
        .name(name)
        .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
        .with_keys(&keys)?
        .with_relative_axes(&axes)?
        .build()
}

impl GyroMouse {
    pub fn open(input: &File, transport: Transport, options: Options) -> io::Result<Self> {
        let factory = super::hidraw::read_factory_calibration(input, transport).map_err(|err| {
            io::Error::other(format!("could not read factory calibration: {err}"))
        })?;
        let stored = gyro_calibration::load(input);
        let output = open_output("JoyShockRust Gyro Mouse")?;
        let bias_writer = if options.calibration_hold.is_some() {
            BiasWriter::open(input).unwrap_or_else(|error| {
                eprintln!("Could not prepare controller gyro calibration storage: {error}");
                None
            })
        } else {
            None
        };
        if stored.is_some() {
            eprintln!("Applied controller gyro calibration");
        }
        Ok(Self {
            pointer: PointerMotion::new(
                factory,
                options.calibrate,
                options.pointer_options(),
                stored,
            ),
            output,
            hold: options.calibration_hold.map(HoldDetector::new),
            shortcut: None,
            haptics: Haptics::open_controller(input, transport),
            bias_writer,
            controller: super::hidraw::controller_id(input).ok().flatten(),
            gate: GyroGate::new(options.binding),
        })
    }

    pub fn is_calibrating(&self) -> bool {
        self.pointer.is_calibrating()
    }

    pub fn signal_outputs_ready(&self) {
        // A startup calibration announces readiness with its completion cue.
        if !self.pointer.is_calibrating()
            && let Some(haptics) = &self.haptics
        {
            haptics.send(Cue::OutputsReady);
        }
    }

    pub fn reconfigure(&mut self, options: Options) {
        self.pointer.reconfigure(options.pointer_options());
    }

    pub fn factory_calibration(&self) -> FactoryCalibration {
        self.pointer.factory_calibration()
    }

    pub fn stationary_calibration(&self) -> Calibration {
        self.pointer.calibration()
    }

    pub fn projected_interval(&self) -> Option<([f32; 2], Duration)> {
        self.pointer.projected_interval()
    }

    pub fn publish(&mut self, report: InputReport) -> io::Result<bool> {
        let mut suppress_flick = false;
        if let Some(hold) = &mut self.hold {
            let pressed = report.controls.buttons.contains(hold.binding.button);
            let now = Instant::now();
            if hold.observe(pressed, now) && self.shortcut.is_none() {
                self.shortcut = Some(ShortcutCalibration {
                    settle_at: now + Duration::from_millis(500),
                    deadline: now + Duration::from_secs(15),
                    measuring: false,
                });
                self.pointer.prepare_calibration();
                if let Some(haptics) = &self.haptics {
                    haptics.send(Cue::CalibrationStart);
                }
                eprintln!(
                    "Gyro calibration requested. Release the button and keep the controller still for {} seconds.",
                    StationaryCalibration::SAMPLE_DURATION.as_secs(),
                );
            }
            if let Some(shortcut) = &mut self.shortcut {
                suppress_flick = true;
                if now >= shortcut.deadline {
                    self.shortcut = None;
                    self.pointer.cancel_calibration();
                    eprintln!("Gyro calibration timed out. Previous calibration retained.");
                    return Ok(false);
                }
                if !shortcut.measuring {
                    if pressed || now < shortcut.settle_at {
                        self.pointer.clear_projection();
                        return Ok(false);
                    }
                    shortcut.measuring = true;
                    self.pointer.start_calibration();
                }
            }
        }
        let enabled = self
            .gate
            .enabled(&report.controls, &report.touch_contacts, Instant::now);
        let movement = self
            .pointer
            .advance(report.imu, &report.controls, enabled, !suppress_flick);
        if let Some(bias) = self.pointer.take_completed_bias() {
            if let Some(id) = self.controller {
                gyro_calibration::remember(id, bias);
            }
            if self.shortcut.take().is_some()
                && let Some(writer) = &self.bias_writer
            {
                writer.send(bias);
            }
            if let Some(haptics) = &self.haptics {
                haptics.send(Cue::GyroCalibrationDone);
            }
            eprintln!("Gyro rest calibration complete");
        }
        emit_movement(movement, |events| self.output.emit(events))
    }
}

fn emit_movement(
    movement: [i32; 2],
    emit: impl FnOnce(&[InputEvent]) -> io::Result<()>,
) -> io::Result<bool> {
    let mut events = [InputEvent::new(0, 0, 0); 2];
    let mut count = 0;
    for (axis, delta) in [Axis::REL_X, Axis::REL_Y].into_iter().zip(movement) {
        if delta != 0 {
            events[count] = InputEvent::new(EventType::RELATIVE.0, axis.0, delta);
            count += 1;
        }
    }
    if count > 0 {
        emit(&events[..count])?;
    }
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use joyshockrust_core::Button;
    use joyshockrust_motion::flick_stick::FlickStick;
    use joyshockrust_motion::game_calibration::GameCalibration;

    #[test]
    fn mouse_batch_requires_movement_and_successful_emission() {
        assert!(!emit_movement([0, 0], |_| panic!("empty batch emitted")).unwrap());
        assert!(
            emit_movement([-4, 2], |events| {
                assert_eq!(events.len(), 2);
                assert_eq!(events[0].code(), Axis::REL_X.0);
                assert_eq!(events[1].code(), Axis::REL_Y.0);
                assert_eq!(events[0].value(), -4);
                assert_eq!(events[1].value(), 2);
                Ok(())
            })
            .unwrap()
        );
        let err = emit_movement([1, 0], |_| Err(io::ErrorKind::BrokenPipe.into())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn game_calibration_scales_both_gyro_axes_and_flick_stick() {
        let profile = crate::profile::parse(
            "version = 1\n[gyro]\nsensitivity_x = 1.0\nsensitivity_y = 0.5\nflick_stick = 1.0\nreal_world_calibration = 40.0\nin_game_sens = 2.0",
        ).unwrap();
        let options = profile.gyro.unwrap();
        assert_eq!(options.pointer_options().sensitivity, [20.0, 10.0]);
        assert_eq!(options.pointer_options().flick.unwrap().scale, 20.0);
        let mut flick = FlickStick::new(options.pointer_options().flick.unwrap().scale);
        let center = joyshockrust_core::Stick::default();
        let right = joyshockrust_core::Stick { x: 1.0, y: 0.0 };
        assert_eq!(flick.advance(center, 0), 0);
        assert_eq!(flick.advance(right, 30_000), 0);
        assert_eq!(flick.advance(right, 180_000), 900);
        assert_eq!(flick.advance(center, 330_000), 900);

        let updated = Options {
            game_calibration: GameCalibration::new(40.0, 1.0).unwrap(),
            ..options
        };
        flick.set_counts_per_degree(updated.pointer_options().flick.unwrap().scale);
        assert_eq!(flick.advance(right, 360_000), 0);
        assert_eq!(flick.advance(right, 510_000), 1800);
        assert_eq!(flick.advance(center, 660_000), 1800);
    }

    #[test]
    fn calibration_hold_rearms_only_after_release() {
        let mut detector = HoldDetector::new(HoldBinding {
            button: Button::Home,
            duration: Duration::from_millis(1500),
        });
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        assert!(!detector.observe(true, at(0)));
        assert!(!detector.observe(true, at(2000)));
        assert!(!detector.observe(false, at(2100)));
        assert!(!detector.observe(true, at(2200)));
        assert!(!detector.observe(false, at(2300)));
        assert!(!detector.observe(true, at(2400)));
        assert!(!detector.observe(true, at(3899)));
        assert!(detector.observe(true, at(3900)));
        assert!(!detector.observe(true, at(6000)));
        assert!(!detector.observe(false, at(6100)));
        assert!(!detector.observe(true, at(6200)));
        assert!(detector.observe(true, at(7700)));
    }
}

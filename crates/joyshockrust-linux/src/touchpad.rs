use joyshockrust_core::touch_region::{GridSize, parse_cell};
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
};

use evdev::{AbsoluteAxisCode, AttributeSetRef, PropType, raw_stream::RawDevice};
use joyshockrust_core::{Button, Buttons};
use joyshockrust_input::{DUALSENSE_TOUCHPAD_HEIGHT, DUALSENSE_TOUCHPAD_WIDTH, TouchContact};
use serde::Deserialize;

use crate::hidraw;

pub fn try_grab_native(hidraw: &File) -> Option<RawDevice> {
    let result = hidraw::hid_parent(hidraw)
        .and_then(|hid| hidraw::wait_for_evdev(hidraw, || probe_native(&hid)));
    match result {
        Ok(device) => Some(device),
        Err(err) => {
            eprintln!("Native touchpad grab unavailable: {err}");
            None
        }
    }
}

fn probe_native(hid: &udev::Device) -> io::Result<RawDevice> {
    let mut devices = udev::Enumerator::new()?;
    devices.match_parent(hid)?;
    devices.match_subsystem("input")?;
    let mut last_error = None;
    for node in devices.scan_devices()? {
        if !node.sysname().as_encoded_bytes().starts_with(b"event") {
            continue;
        }
        let Some(path) = node.devnode() else {
            continue;
        };
        let Some(parent) = node.parent_with_subsystem("input")? else {
            continue;
        };
        if !parent
            .attribute_value("name")
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.ends_with(" Touchpad"))
        {
            continue;
        }
        match open_native(path, hid) {
            Ok(Some(device)) => return Ok(device),
            Ok(None) => {}
            Err(err) => {
                last_error = Some(io::Error::new(
                    err.kind(),
                    format!("{}: {err}", path.display()),
                ))
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no matching native touchpad evdev node",
        )
    }))
}

fn open_native(path: &std::path::Path, hid: &udev::Device) -> io::Result<Option<RawDevice>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)?;
    // Recheck parent and capabilities on the opened descriptor before grabbing.
    if hidraw::hid_parent(&file)?.syspath() != hid.syspath() {
        return Ok(None);
    }
    let mut device = RawDevice::from_fd(file.into())?;
    if !matches_touchpad(
        device.name(),
        device.properties(),
        device.supported_absolute_axes(),
    ) {
        return Ok(None);
    }
    device.grab()?;
    Ok(Some(device))
}

fn matches_touchpad(
    name: Option<&str>,
    properties: &AttributeSetRef<PropType>,
    axes: Option<&AttributeSetRef<AbsoluteAxisCode>>,
) -> bool {
    name.is_some_and(|name| name.ends_with(" Touchpad"))
        && properties.contains(PropType::BUTTONPAD)
        && axes.is_some_and(|axes| {
            axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_X)
                && axes.contains(AbsoluteAxisCode::ABS_MT_POSITION_Y)
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    columns: usize,
    rows: usize,
    #[serde(default)]
    regions: Vec<RegionConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionConfig {
    column: usize,
    row: usize,
    touch: Option<String>,
    click: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Region {
    touch: Option<Button>,
    click: Option<Button>,
}

#[derive(PartialEq, Eq)]
pub struct Grid {
    size: GridSize,
    regions: Vec<Option<Region>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TouchRegion {
    size: GridSize,
    index: usize,
}

impl TouchRegion {
    pub fn pressed(self, contacts: &[Option<TouchContact>; 2]) -> bool {
        contacts
            .iter()
            .flatten()
            .any(|&contact| self.contains(contact))
    }

    pub fn contains(self, contact: TouchContact) -> bool {
        contact_index(self.size, contact) == self.index
    }
}

fn contact_index(size: GridSize, contact: TouchContact) -> usize {
    size.contact_index(
        usize::from(contact.x),
        usize::from(contact.y),
        usize::from(DUALSENSE_TOUCHPAD_WIDTH),
        usize::from(DUALSENSE_TOUCHPAD_HEIGHT),
    )
}

#[derive(Default)]
pub enum ClickState {
    #[default]
    Released,
    Unmapped,
    Mapped(Button),
}

impl Config {
    pub fn resolve(self) -> Result<Grid, &'static str> {
        let size = GridSize::new(self.rows, self.columns)
            .ok_or("touchpad grid must have between 1 and 25 cells")?;
        let mut regions = vec![None; size.count()];
        for region in self.regions {
            let index = size
                .cell_index(region.row, region.column)
                .ok_or("touchpad region row or column is outside the grid")?;
            if regions[index].is_some() {
                return Err("duplicate touchpad region");
            }
            let touch = region
                .touch
                .as_deref()
                .map(super::gamepad::parse_target)
                .transpose()?;
            let click = region
                .click
                .as_deref()
                .map(super::gamepad::parse_target)
                .transpose()?;
            if touch.is_none() && click.is_none() {
                return Err("touchpad region needs a touch or click binding");
            }
            regions[index] = Some(Region { touch, click });
        }
        Ok(Grid { size, regions })
    }
}

impl Grid {
    pub fn parse_region(&self, spec: &str) -> Result<TouchRegion, &'static str> {
        let (row, column) = parse_cell(spec)?;
        let index = self
            .size
            .cell_index(row, column)
            .ok_or("touchpad region row or column is outside the grid")?;
        Ok(TouchRegion {
            size: self.size,
            index,
        })
    }

    pub fn map(
        &self,
        physical: &mut Buttons,
        contacts: [Option<TouchContact>; 2],
        click: &mut ClickState,
    ) -> Buttons {
        let mut regions = contacts
            .into_iter()
            .flatten()
            .map(|contact| self.regions[contact_index(self.size, contact)]);
        let primary = regions.next().flatten();
        let mut outputs = Buttons::default();
        for region in primary.into_iter().chain(regions.flatten()) {
            if let Some(button) = region.touch {
                outputs.insert(button);
            }
        }

        if !physical.contains(Button::TouchpadClick) {
            *click = ClickState::Released;
        } else if matches!(click, ClickState::Released) {
            *click = primary
                .and_then(|region| region.click)
                .map_or(ClickState::Unmapped, ClickState::Mapped);
        }
        if let ClickState::Mapped(button) = *click {
            physical.remove(Button::TouchpadClick);
            outputs.insert(button);
        }
        outputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{gamepad::GamepadState, profile};
    use evdev::KeyCode;
    use joyshockrust_input::{DeviceModel, Transport, parse_report};

    #[test]
    fn native_grab_requires_the_touchpad_name_and_capabilities() {
        use evdev::AttributeSet;

        let properties: AttributeSet<_> = [PropType::BUTTONPAD].into_iter().collect();
        let axes: AttributeSet<_> = [
            AbsoluteAxisCode::ABS_MT_POSITION_X,
            AbsoluteAxisCode::ABS_MT_POSITION_Y,
        ]
        .into_iter()
        .collect();
        let sensor_axes: AttributeSet<_> = [AbsoluteAxisCode::ABS_X, AbsoluteAxisCode::ABS_Y]
            .into_iter()
            .collect();
        let name = Some("DualSense Edge Wireless Controller Touchpad");
        assert!(matches_touchpad(name, &properties, Some(&axes)));
        assert!(!matches_touchpad(
            Some("DualSense Edge Wireless Controller Motion Sensors"),
            &properties,
            Some(&axes)
        ));
        assert!(!matches_touchpad(name, &AttributeSet::new(), Some(&axes)));
        assert!(!matches_touchpad(name, &properties, Some(&sensor_axes)));
        assert!(!matches_touchpad(name, &properties, None));
    }

    const PROFILE: &str = r#"
        version = 1
        [touchpad]
        columns = 2
        rows = 2
        [[touchpad.regions]]
        column = 1
        row = 1
        touch = "cross"
        click = "square"
        [[touchpad.regions]]
        column = 2
        row = 2
        touch = "circle"
        click = "triangle"
    "#;

    #[test]
    fn touch_grid_follows_both_fingers_and_latches_the_click_region() {
        let runtime = profile::parse(PROFILE).unwrap();
        let grid = runtime.touchpad.unwrap();
        let mut click = ClickState::default();
        let mut pad = GamepadState::default();
        let mut frame = [0_u8; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[8] = 8;
        let upper_left = [0x11, 0xbf, 0xb3, 0x21]; // (959, 539)
        let lower_right = [0x22, 0xc0, 0xc3, 0x21]; // (960, 540)
        let inactive = [0x80, 0, 0, 0];

        for (first, second, pressed, physical_cross, expected) in [
            (
                upper_left,
                inactive,
                false,
                false,
                &[(KeyCode::BTN_SOUTH, 1)][..],
            ),
            (
                upper_left,
                lower_right,
                true,
                true,
                &[(KeyCode::BTN_EAST, 1), (KeyCode::BTN_NORTH, 1)],
            ),
            (lower_right, inactive, true, true, &[]),
            (
                lower_right,
                inactive,
                false,
                true,
                &[(KeyCode::BTN_NORTH, 0)],
            ),
            (
                inactive,
                lower_right,
                true,
                false,
                &[(KeyCode::BTN_SOUTH, 0), (KeyCode::BTN_WEST, 1)],
            ),
            (inactive, inactive, true, false, &[(KeyCode::BTN_EAST, 0)]),
            (inactive, inactive, false, false, &[(KeyCode::BTN_WEST, 0)]),
        ] {
            frame[33..37].copy_from_slice(&first);
            frame[37..41].copy_from_slice(&second);
            frame[10] = if pressed { 2 } else { 0 };
            frame[8] = if physical_cross { 0x28 } else { 8 };
            let report =
                parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None).unwrap();
            let mut controls = report.controls;
            let outputs = grid.map(&mut controls.buttons, report.touch_contacts, &mut click);
            assert!(!controls.buttons.contains(Button::TouchpadClick));
            controls.buttons = controls.buttons.union(outputs);
            let mut changes = Vec::new();
            pad.publish(controls, |events| {
                changes.extend(
                    events
                        .iter()
                        .map(|event| (KeyCode(event.code()), event.value())),
                );
                Ok(())
            })
            .unwrap();
            assert_eq!(changes, expected);
        }

        let mut buttons = Buttons::default();
        buttons.insert(Button::TouchpadClick);
        assert_eq!(
            grid.map(&mut buttons, [None, None], &mut click),
            Buttons::default()
        );
        assert!(buttons.contains(Button::TouchpadClick));
        // A click that started outside the bound regions keeps its original behavior.
        let contact = TouchContact {
            id: 42,
            x: 1919,
            y: 1079,
        };
        let outputs = grid.map(&mut buttons, [Some(contact), None], &mut click);
        assert!(buttons.contains(Button::TouchpadClick));
        assert!(outputs.contains(Button::East));
        assert!(!outputs.contains(Button::North));
    }

    #[test]
    fn validates_grid_shape_cells_and_targets_in_profiles() {
        for (source, expected) in [
            (
                PROFILE.replace("rows = 2", "rows = 0"),
                "between 1 and 25 cells",
            ),
            (
                PROFILE.replace("rows = 2", "rows = 13"),
                "between 1 and 25 cells",
            ),
            (
                PROFILE.replace("column = 2", "column = 3"),
                "outside the grid",
            ),
            (
                PROFILE.replace("column = 2\n        row = 2", "column = 1\n        row = 1"),
                "duplicate touchpad region",
            ),
            (
                PROFILE.replace("\"cross\"", "\"invalid\""),
                "unknown target button",
            ),
            (
                PROFILE.replace("\"cross\"", "\"mute\""),
                "unavailable on the virtual gamepad",
            ),
            (
                PROFILE
                    .replace("touch = \"cross\"", "")
                    .replace("click = \"square\"", ""),
                "needs a touch or click binding",
            ),
        ] {
            let error = profile::parse(&source)
                .err()
                .expect("invalid profile was accepted");
            assert!(error.to_string().contains(expected), "{error}");
        }
        assert!(profile::parse("version = 1").unwrap().touchpad.is_none());
    }
}

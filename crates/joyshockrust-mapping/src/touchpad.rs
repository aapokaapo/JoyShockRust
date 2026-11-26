//! Touch region lookup and latching for controller mappings.

use joyshockrust_core::{
    Button, Buttons,
    touch_region::{GridSize, parse_cell},
};
use joyshockrust_input::{DUALSENSE_TOUCHPAD_HEIGHT, DUALSENSE_TOUCHPAD_WIDTH, TouchContact};

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

pub fn resolve<'a>(
    size: GridSize,
    settings: impl IntoIterator<Item = (usize, usize, Option<&'a str>, Option<&'a str>)>,
) -> Result<Grid, &'static str> {
    let mut regions = vec![None; size.count()];
    for (row, column, touch, click) in settings {
        let index = size
            .cell_index(row, column)
            .ok_or("touchpad region row or column is outside the grid")?;
        if regions[index].is_some() {
            return Err("duplicate touchpad region");
        }
        let touch = touch.map(super::gamepad::parse_target).transpose()?;
        let click = click.map(super::gamepad::parse_target).transpose()?;
        if touch.is_none() && click.is_none() {
            return Err("touchpad region needs a touch or click binding");
        }
        regions[index] = Some(Region { touch, click });
    }
    Ok(Grid { size, regions })
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

use std::{
    cell::RefCell,
    io::{self, Read},
    os::fd::AsRawFd,
    path::Path,
    rc::Rc,
};

use adw::{glib, gtk, prelude::*};
use joyshockrust_core::{Button, Controls, DPad};

use crate::{calibration::positive_decimal, controller_input};

pub fn watch(
    window: &adw::ApplicationWindow,
    path: &Path,
    mut on_command: impl FnMut(&str) + 'static,
    on_error: impl Fn(&io::Error) + 'static,
) -> io::Result<()> {
    let mut input = controller_input::open(path)?;
    let source = Rc::new(RefCell::new(None::<glib::SourceId>));
    let active = source.clone();
    let mut previous = None;
    let mut bytes = [0; 16_384];
    let id = glib::source::unix_fd_add_local(
        input.as_raw_fd(),
        glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
        move |_, condition| {
            let result = if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR) {
                Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "Controller disconnected",
                ))
            } else {
                read_commands(&mut input, &mut bytes, &mut previous, |command| {
                    on_command(command);
                    active.borrow().is_some()
                })
            };
            if active.borrow().is_none() {
                return glib::ControlFlow::Break;
            }
            if let Err(error) = result {
                active.borrow_mut().take();
                on_error(&error);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        },
    );
    source.replace(Some(id));
    window.connect_hide(move |_| {
        if let Some(id) = source.borrow_mut().take() {
            id.remove();
        }
    });
    Ok(())
}

fn read_commands(
    input: impl Read,
    bytes: &mut [u8],
    previous: &mut Option<Controls>,
    mut dispatch: impl FnMut(&str) -> bool,
) -> io::Result<()> {
    controller_input::read_reports(input, bytes, 32, |controls| {
        command(previous, controls).is_none_or(&mut dispatch)
    })
}

#[derive(Clone)]
pub enum Control {
    Button(glib::WeakRef<gtk::Button>),
    Sensitivity(glib::WeakRef<adw::EntryRow>),
    Turns(glib::WeakRef<adw::EntryRow>),
    Profile(glib::WeakRef<adw::ComboRow>),
}

impl Control {
    fn widget(&self) -> Option<gtk::Widget> {
        match self {
            Self::Button(button) => button.upgrade().map(Cast::upcast),
            Self::Sensitivity(entry) | Self::Turns(entry) => entry.upgrade().map(Cast::upcast),
            Self::Profile(row) => row.upgrade().map(Cast::upcast),
        }
    }

    pub fn focus(&self) {
        if let Some(widget) = self.widget()
            && !widget.grab_focus()
        {
            widget.child_focus(gtk::DirectionType::TabForward);
        }
    }

    fn adjust(&self, direction: i32, fine: bool) {
        match self {
            Self::Sensitivity(entry) => {
                if let Some(entry) = entry.upgrade() {
                    let value =
                        positive_decimal(&entry.text().trim().replace(',', ".")).unwrap_or(1.0);
                    let step = if fine { 0.01 } else { 0.1 };
                    let next = (value + f64::from(direction) * step).max(0.01);
                    if next != value {
                        let text = format!("{next:.6}");
                        entry.set_text(text.trim_end_matches('0').trim_end_matches('.'));
                    }
                }
            }
            Self::Turns(entry) if !fine => {
                if let Some(entry) = entry.upgrade() {
                    let value = entry.text().trim().parse::<u32>().unwrap_or(1);
                    let next = value.saturating_add_signed(direction).max(1);
                    entry.set_text(&next.to_string());
                }
            }
            Self::Profile(row) if !fine => {
                if let Some(row) = row.upgrade()
                    && let Some(model) = row.model()
                    && model.n_items() > 1
                {
                    let selected = row
                        .selected()
                        .saturating_add_signed(direction)
                        .clamp(1, model.n_items() - 1);
                    row.set_selected(selected);
                }
            }
            _ => {}
        }
    }
}

pub fn navigate(window: &adw::ApplicationWindow, controls: &[Control], command: &str) {
    if command == "back" {
        window.close();
        return;
    }
    let controls: Vec<_> = controls
        .iter()
        .filter_map(|control| {
            let widget = control.widget()?;
            (widget.is_visible() && widget.is_sensitive()).then_some((control, widget))
        })
        .collect();
    if controls.is_empty() {
        return;
    }
    window.set_focus_visible(true);
    let focused = gtk::prelude::GtkWindowExt::focus(window).and_then(|focus| {
        controls
            .iter()
            .position(|(_, widget)| focus == *widget || focus.is_ancestor(widget))
    });
    let index = focused.unwrap_or(0);
    let next = (index + 1) % controls.len();
    let (control, _) = &controls[index];
    match command {
        "up" => controls[focused.map_or(controls.len() - 1, |index| {
            (index + controls.len() - 1) % controls.len()
        })]
        .0
        .focus(),
        "down" => controls[focused.map_or(0, |_| next)].0.focus(),
        "accept" => {
            if let Control::Button(button) = control {
                if let Some(button) = button.upgrade() {
                    button.emit_clicked();
                }
            } else {
                controls[next].0.focus();
            }
        }
        "left" | "right" | "fine-left" | "fine-right" => {
            if focused.is_none() {
                control.focus();
            }
            control.adjust(
                if command.ends_with("left") { -1 } else { 1 },
                command.starts_with("fine-"),
            );
        }
        _ => {}
    }
}

fn direction(dpad: DPad) -> Option<&'static str> {
    match dpad {
        DPad::Up | DPad::UpLeft | DPad::UpRight => Some("up"),
        DPad::Down | DPad::DownLeft | DPad::DownRight => Some("down"),
        DPad::Left => Some("left"),
        DPad::Right => Some("right"),
        DPad::Neutral => None,
    }
}

fn command(previous: &mut Option<Controls>, current: Controls) -> Option<&'static str> {
    let previous = previous.replace(current)?;
    let pressed = |button| current.buttons.contains(button) && !previous.buttons.contains(button);
    if pressed(Button::East) {
        return Some("back");
    }
    let current_direction = direction(current.dpad);
    if current_direction.is_some() && current_direction != direction(previous.dpad) {
        return current_direction;
    }
    [
        (Button::South, "accept"),
        (Button::LeftShoulder, "fine-left"),
        (Button::RightShoulder, "fine-right"),
    ]
    .into_iter()
    .find_map(|(button, command)| pressed(button).then_some(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_reads_complete_usb_reports_and_preserves_press_edges() {
        use joyshockrust_input::DUALSENSE_EDGE_USB_REPORT_LEN;
        use std::os::unix::net::UnixDatagram;

        struct Reports(UnixDatagram);
        impl Read for Reports {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.0.recv(bytes)
            }
        }

        let (tx, rx) = UnixDatagram::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let mut report = [0; DUALSENSE_EDGE_USB_REPORT_LEN];
        report[0] = 0x01;
        for face_and_dpad in [0x08, 0x28, 0x28, 0x08, 0x02, 0x42] {
            report[8] = face_and_dpad;
            tx.send(&report).unwrap();
        }
        let mut commands = Vec::new();
        read_commands(Reports(rx), &mut [0; 128], &mut None, |command| {
            commands.push(command.to_owned());
            true
        })
        .unwrap();
        assert_eq!(commands, ["accept", "right", "back"]);
    }

    #[test]
    fn navigation_emits_fresh_presses_and_leaves_options_for_measurement() {
        let mut previous = None;
        let mut controls = Controls::default();
        controls.buttons.insert(Button::South);
        assert_eq!(command(&mut previous, controls), None);
        assert_eq!(command(&mut previous, controls), None);
        controls.buttons.remove(Button::South);
        controls.buttons.insert(Button::Start);
        assert_eq!(command(&mut previous, controls), None);
        controls.buttons.insert(Button::South);
        assert_eq!(command(&mut previous, controls), Some("accept"));
        controls.dpad = DPad::Down;
        assert_eq!(command(&mut previous, controls), Some("down"));
        controls.dpad = DPad::DownRight;
        assert_eq!(command(&mut previous, controls), None);
        controls.dpad = DPad::Right;
        assert_eq!(command(&mut previous, controls), Some("right"));
        controls.buttons.insert(Button::East);
        assert_eq!(command(&mut previous, controls), Some("back"));
    }
}

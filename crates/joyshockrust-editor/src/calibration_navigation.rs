use adw::{glib, gtk, prelude::*};

use crate::calibration::positive_decimal;

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

    fn focus(&self) {
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

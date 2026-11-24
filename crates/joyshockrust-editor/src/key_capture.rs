use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::{glib, gtk, prelude::*};
use evdev::KeyCode;

const BIND_LABEL: &str = "Bind key…";
const CANCEL_LABEL: &str = "Hit ESC or Click anywhere to cancel";
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn row(
    title: &str,
    current: Option<&str>,
    mouse_buttons: &'static [(&str, &str)],
    changed: impl Fn(Option<&str>) + 'static,
) -> adw::ActionRow {
    let label = move |key: Option<&str>| {
        key.map_or_else(
            || "No action".to_owned(),
            |key| {
                mouse_buttons
                    .iter()
                    .find_map(|&(code, label)| (code == key).then_some(label))
                    .map(str::to_owned)
                    .unwrap_or_else(|| key.strip_prefix("KEY_").unwrap_or(key).replace('_', " "))
            },
        )
    };
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(label(current))
        .build();
    let bind = gtk::Button::builder()
        .label(BIND_LABEL)
        .valign(gtk::Align::Center)
        .build();
    bind.update_property(&[gtk::accessible::Property::Label(&format!(
        "Bind key for {title}"
    ))]);
    row.add_suffix(&bind);
    let clear = gtk::Button::builder()
        .label("Unbind")
        .tooltip_text("Remove this binding")
        .sensitive(current.is_some())
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&clear);

    let mouse_index = move |key: Option<&str>| {
        mouse_buttons
            .iter()
            .position(|&(candidate, _)| Some(candidate) == key)
            .map_or(0, |index| index as u32 + 1)
    };
    let mouse = (!mouse_buttons.is_empty()).then(|| {
        let options: Vec<_> = std::iter::once("Mouse buttons…")
            .chain(mouse_buttons.iter().map(|&(_, label)| label))
            .collect();
        let dropdown = gtk::DropDown::from_strings(&options);
        dropdown.set_valign(gtk::Align::Center);
        dropdown.set_selected(mouse_index(current));
        dropdown.update_property(&[gtk::accessible::Property::Label("Mouse button")]);
        row.add_suffix(&dropdown);
        dropdown
    });
    let refreshing = Rc::new(Cell::new(false));
    let update = {
        let row = row.downgrade();
        let clear = clear.downgrade();
        let mouse = mouse.as_ref().map(gtk::DropDown::downgrade);
        let refreshing = refreshing.clone();
        Rc::new(move |key: Option<&str>| {
            changed(key);
            if let Some(clear) = clear.upgrade() {
                clear.set_sensitive(key.is_some());
            }
            if let Some(row) = row.upgrade() {
                row.set_subtitle(&label(key));
            }
            if let Some(mouse) = mouse.as_ref().and_then(glib::WeakRef::upgrade) {
                refreshing.set(true);
                mouse.set_selected(mouse_index(key));
                refreshing.set(false);
            }
        })
    };
    let capture = Rc::new(RefCell::new(None));
    bind.connect_clicked({
        let update = update.clone();
        let capture = capture.clone();
        move |button| {
            let update = update.clone();
            listen(button, &capture, move |key| update(Some(key)));
        }
    });
    bind.connect_unmap(move |button| {
        stop(button, &capture);
    });
    clear.connect_clicked({
        let update = update.clone();
        move |_| update(None)
    });
    if let Some(mouse) = mouse {
        mouse.connect_selected_notify(move |dropdown| {
            if !refreshing.get() {
                update(
                    dropdown
                        .selected()
                        .checked_sub(1)
                        .and_then(|index| mouse_buttons.get(index as usize))
                        .map(|&(key, _)| key),
                );
            }
        });
    }
    row
}

struct Capture {
    keys: gtk::EventControllerKey,
    clicks: gtk::GestureClick,
    timeout: Option<glib::SourceId>,
}

impl Capture {
    fn cancel_timeout(&mut self) {
        if let Some(timeout) = self.timeout.take() {
            timeout.remove();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.cancel_timeout();
        if let Some(window) = self.keys.widget() {
            window.remove_controller(&self.keys);
            window.remove_controller(&self.clicks);
        }
    }
}

fn stop(button: &gtk::Button, active: &RefCell<Option<Capture>>) -> Option<Capture> {
    let mut capture = active.borrow_mut().take()?;
    capture.cancel_timeout();
    button.set_label(BIND_LABEL);
    button.reset_property(gtk::AccessibleProperty::Description);
    Some(capture)
}

fn listen(
    button: &gtk::Button,
    active: &Rc<RefCell<Option<Capture>>>,
    bind: impl Fn(&str) + 'static,
) {
    let Some(window) = button.root() else { return };
    if active.borrow().is_some() {
        return;
    }

    let keys = gtk::EventControllerKey::new();
    keys.set_name(Some("binding-key-capture"));
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let clicks = gtk::GestureClick::new();
    clicks.set_name(Some("binding-click-cancel"));
    clicks.set_button(0);
    clicks.set_propagation_phase(gtk::PropagationPhase::Capture);

    keys.connect_key_pressed({
        let button = button.downgrade();
        let active = Rc::downgrade(active);
        move |keys, keyval, keycode, _| {
            let (Some(button), Some(active)) = (button.upgrade(), active.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            if !active
                .borrow()
                .as_ref()
                .is_some_and(|capture| capture.keys == *keys)
            {
                return glib::Propagation::Proceed;
            }
            if keyval == gtk::gdk::Key::Escape || linux_keycode(keycode) == Some(KeyCode::KEY_ESC) {
                stop(&button, &active);
            } else if let Some(key) = binding_name(keycode) {
                stop(&button, &active);
                bind(&key);
            }
            glib::Propagation::Stop
        }
    });
    clicks.connect_pressed({
        let button = button.downgrade();
        let active = Rc::downgrade(active);
        move |click, _, _, _| {
            let (Some(button), Some(active)) = (button.upgrade(), active.upgrade()) else {
                return;
            };
            if active
                .borrow()
                .as_ref()
                .is_some_and(|capture| capture.clicks == *click)
            {
                // Consume the cancel click before removing the controller.
                click.set_state(gtk::EventSequenceState::Claimed);
                let capture = stop(&button, &active);
                glib::idle_add_local_once(move || drop(capture));
            }
        }
    });

    let prompt = gtk::Label::builder()
        .label(CANCEL_LABEL)
        .wrap(true)
        .max_width_chars(24)
        .justify(gtk::Justification::Center)
        .build();
    button.set_child(Some(&prompt));
    button.update_property(&[gtk::accessible::Property::Description(CANCEL_LABEL)]);
    button.grab_focus();
    window.add_controller(keys.clone());
    window.add_controller(clicks.clone());
    let timeout = glib::timeout_add_local_once(CAPTURE_TIMEOUT, {
        let button = button.downgrade();
        let active = Rc::downgrade(active);
        move || {
            if let (Some(button), Some(active)) = (button.upgrade(), active.upgrade()) {
                if let Some(capture) = active.borrow_mut().as_mut() {
                    capture.timeout.take();
                }
                stop(&button, &active);
            }
        }
    });
    active.replace(Some(Capture {
        keys,
        clicks,
        timeout: Some(timeout),
    }));
}

fn linux_keycode(keycode: u32) -> Option<KeyCode> {
    // GDK's Wayland backend delivers XKB keycodes, which are evdev codes plus 8.
    let evdev = u16::try_from(keycode.checked_sub(8)?).ok()?;
    Some(KeyCode::new(evdev))
}

fn binding_name(keycode: u32) -> Option<String> {
    let key = linux_keycode(keycode)?;
    let name = format!("{key:?}");
    (key != KeyCode::KEY_ESC && key != KeyCode::KEY_RESERVED && name.starts_with("KEY_"))
        .then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_physical_keys_but_excludes_escape_mouse_and_unknown_codes() {
        for (code, expected) in [
            (KeyCode::KEY_W, "KEY_W"),
            (KeyCode::KEY_LEFTSHIFT, "KEY_LEFTSHIFT"),
            (KeyCode::KEY_KPENTER, "KEY_KPENTER"),
            (KeyCode::KEY_F20, "KEY_F20"),
        ] {
            assert_eq!(
                binding_name(u32::from(code.code()) + 8).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(binding_name(0), None);
        for code in [KeyCode::KEY_ESC, KeyCode::BTN_LEFT, KeyCode::KEY_RESERVED] {
            assert_eq!(binding_name(u32::from(code.code()) + 8), None);
        }
        assert_eq!(binding_name(u32::MAX), None);
    }
}

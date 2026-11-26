use evdev::KeyCode;
use joyshockrust_mapping::keyboard::{Key, KeyKind, KeyResolver};

pub struct LinuxKeys;

impl KeyResolver for LinuxKeys {
    fn keyboard(&self, name: &str) -> Result<Key, String> {
        keyboard_key(name).map(resolved_key)
    }

    fn mouse_button(&self, name: &str) -> Result<Key, String> {
        name.parse::<KeyCode>()
            .ok()
            .filter(|&key| is_mouse_button(key))
            .map(resolved_key)
            .ok_or_else(|| format!("expected a mouse button, got {name}"))
    }

    fn gesture(&self, name: &str) -> Result<Key, String> {
        gesture_key(name).map(resolved_key)
    }

    fn default_tap_buttons(&self) -> [Key; 2] {
        [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT].map(resolved_key)
    }

    fn describe(&self, key: Key) -> String {
        format!("{:?}", KeyCode(key.id))
    }
}

fn resolved_key(key: KeyCode) -> Key {
    Key {
        id: key.0,
        kind: if is_mouse_button(key) {
            KeyKind::MouseButton
        } else if is_modifier(key) {
            KeyKind::Modifier
        } else {
            KeyKind::Key
        },
    }
}

fn keyboard_key(name: &str) -> Result<KeyCode, String> {
    if !name.starts_with("KEY_") || name == "KEY_RESERVED" {
        return Err(format!("expected a Linux keyboard key, got {name}"));
    }
    name.parse()
        .map_err(|_| format!("unknown keyboard key {name}"))
}

pub fn is_mouse_button(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::BTN_LEFT
            | KeyCode::BTN_RIGHT
            | KeyCode::BTN_MIDDLE
            | KeyCode::BTN_SIDE
            | KeyCode::BTN_EXTRA
    )
}

fn gesture_key(name: &str) -> Result<KeyCode, String> {
    match name.parse::<KeyCode>() {
        Ok(button) if is_mouse_button(button) => Ok(button),
        _ => keyboard_key(name),
    }
}

fn is_modifier(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
    )
}

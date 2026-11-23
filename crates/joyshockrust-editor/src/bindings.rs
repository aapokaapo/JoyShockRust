use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value, value};

#[derive(Clone, Copy)]
pub enum Gesture {
    Tap,
    Hold,
    DoubleTap,
}

impl Gesture {
    pub const ALL: [Self; 3] = [Self::Tap, Self::Hold, Self::DoubleTap];

    pub fn label(self) -> &'static str {
        match self {
            Self::Tap => "Tap",
            Self::Hold => "Hold",
            Self::DoubleTap => "Double tap",
        }
    }

    fn field(self) -> &'static str {
        match self {
            Self::Tap => "tap",
            Self::Hold => "hold",
            Self::DoubleTap => "double_tap",
        }
    }
}

fn keyboard_bindings(doc: &DocumentMut) -> Result<Option<&ArrayOfTables>, &'static str> {
    let Some(keyboard) = doc.get("keyboard") else {
        return Ok(None);
    };
    let keyboard = keyboard
        .as_table()
        .ok_or("Edit this keyboard table in Advanced TOML.")?;
    keyboard
        .get("bindings")
        .map(|item| {
            item.as_array_of_tables()
                .ok_or("Edit these bindings in Advanced TOML.")
        })
        .transpose()
}

fn binding_index(bindings: &ArrayOfTables, button: &str) -> Result<Option<usize>, &'static str> {
    let mut matching = bindings
        .iter()
        .enumerate()
        .filter(|(_, table)| table.get("button").and_then(Item::as_str) == Some(button));
    let Some((index, binding)) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() || binding.contains_key("trigger") || binding.contains_key("chord")
    {
        return Err("Edit this input in Advanced TOML.");
    }
    Ok(Some(index))
}

pub fn keyboard_binding<'a>(
    doc: &'a DocumentMut,
    button: &str,
) -> Result<Option<&'a Table>, &'static str> {
    let Some(bindings) = keyboard_bindings(doc)? else {
        return Ok(None);
    };
    Ok(binding_index(bindings, button)?.and_then(|index| bindings.get(index)))
}

pub fn gesture_output(binding: &Table, gesture: Gesture) -> Option<&Item> {
    binding.get(gesture.field())
}

pub fn set_key(
    doc: &mut DocumentMut,
    button: &str,
    gesture: Gesture,
    output: Option<&str>,
) -> Result<(), &'static str> {
    let existing = keyboard_bindings(doc)?
        .map(|bindings| binding_index(bindings, button))
        .transpose()?
        .flatten();
    if output.is_none() && existing.is_none() {
        return Ok(());
    }
    let keyboard = doc
        .entry("keyboard")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .unwrap();
    let bindings = keyboard
        .entry("bindings")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .unwrap();
    if let Some(index) = existing {
        let binding = bindings.get_mut(index).unwrap();
        if binding
            .get(gesture.field())
            .is_some_and(|item| item.as_str().is_none())
        {
            return Err("Edit this custom action in Advanced TOML.");
        }
        if let Some(output) = output {
            if let Some(old) = binding
                .get_mut(gesture.field())
                .and_then(Item::as_value_mut)
            {
                replace_value(old, Value::from(output));
            } else {
                binding.insert(gesture.field(), value(output));
            }
        } else {
            binding.remove(gesture.field());
            if matches!(gesture, Gesture::Hold) {
                binding.remove("scroll_repeat_ms");
            }
            if Gesture::ALL
                .iter()
                .all(|gesture| !binding.contains_key(gesture.field()))
            {
                bindings.remove(index);
            }
        }
    } else if let Some(output) = output {
        let mut binding = Table::new();
        binding.insert("button", value(button));
        binding.insert(gesture.field(), value(output));
        bindings.push(binding);
    }
    Ok(())
}

fn remap_index(array: &Array, button: &str) -> Result<Option<usize>, &'static str> {
    let mut matching = array.iter().enumerate().filter(|(_, item)| {
        item.as_str()
            .and_then(|rule| rule.split_once('='))
            .is_some_and(|(source, _)| source == button)
    });
    let first = matching.next().map(|(index, _)| index);
    if matching.next().is_some() {
        return Err("Edit duplicate remaps in Advanced TOML.");
    }
    Ok(first)
}

pub fn gamepad_target<'a>(
    doc: &'a DocumentMut,
    button: &str,
) -> Result<Option<&'a str>, &'static str> {
    let Some(item) = doc.get("remap_buttons") else {
        return Ok(None);
    };
    let array = item
        .as_array()
        .ok_or("Edit these remaps in Advanced TOML.")?;
    Ok(remap_index(array, button)?
        .and_then(|index| array.get(index))
        .and_then(Value::as_str)
        .and_then(|rule| rule.split_once('=').map(|(_, target)| target)))
}

pub fn set_gamepad(
    doc: &mut DocumentMut,
    button: &str,
    target: Option<&str>,
) -> Result<(), &'static str> {
    if target.is_none() && doc.get("remap_buttons").is_none() {
        return Ok(());
    }
    let array = doc
        .entry("remap_buttons")
        .or_insert_with(|| value(Array::new()))
        .as_array_mut()
        .ok_or("Edit these remaps in Advanced TOML.")?;
    let index = remap_index(array, button)?;
    match (index, target) {
        (Some(index), Some(target)) => {
            replace_value(
                array.get_mut(index).unwrap(),
                Value::from(format!("{button}={target}")),
            );
        }
        (None, Some(target)) => array.push(format!("{button}={target}")),
        (Some(index), None) => {
            array.remove(index);
        }
        (None, None) => {}
    }
    Ok(())
}

fn replace_value(old: &mut Value, mut new: Value) {
    *new.decor_mut() = old.decor().clone();
    *old = new;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_single_keys_preserves_custom_actions_and_other_profile_sections() {
        let mut doc = r#"
version = 1
[[keyboard.bindings]]
button = "cross"
tap = "KEY_SPACE" # jump
hold = ["KEY_LEFTSHIFT", "KEY_W"]
[gyro]
sensitivity_x = 1.5
"#
        .parse::<DocumentMut>()
        .unwrap();
        assert!(set_key(&mut doc, "cross", Gesture::Hold, None).is_err());
        set_key(&mut doc, "cross", Gesture::Tap, Some("KEY_E")).unwrap();
        set_key(&mut doc, "cross", Gesture::DoubleTap, Some("BTN_RIGHT")).unwrap();
        let result = doc.to_string();
        assert!(result.contains("tap = \"KEY_E\" # jump"));
        assert!(result.contains("hold = [\"KEY_LEFTSHIFT\", \"KEY_W\"]"));
        assert!(result.contains("double_tap = \"BTN_RIGHT\""));
        assert!(result.contains("sensitivity_x = 1.5"));
        set_key(&mut doc, "circle", Gesture::Tap, Some("KEY_F")).unwrap();
        set_key(&mut doc, "circle", Gesture::Tap, None).unwrap();
        assert!(keyboard_binding(&doc, "circle").unwrap().is_none());
        assert!(keyboard_binding(&doc, "cross").unwrap().is_some());
    }

    #[test]
    fn editing_base_remaps_keeps_other_inputs_and_layers() {
        let mut doc = r#"
version = 1
remap_buttons = ["cross=triangle", "l1=r1"]
layer_remaps = ["l2:cross=circle"]
"#
        .parse::<DocumentMut>()
        .unwrap();
        assert_eq!(gamepad_target(&doc, "cross").unwrap(), Some("triangle"));
        set_gamepad(&mut doc, "cross", Some("square")).unwrap();
        assert_eq!(gamepad_target(&doc, "cross").unwrap(), Some("square"));
        set_gamepad(&mut doc, "cross", None).unwrap();
        assert_eq!(gamepad_target(&doc, "cross").unwrap(), None);
        assert_eq!(gamepad_target(&doc, "l1").unwrap(), Some("r1"));
        assert!(doc.to_string().contains("l2:cross=circle"));
    }
}

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

pub(crate) fn region_input(input: &str) -> Option<(&str, i64, i64)> {
    let (kind, location) = input.split_once(':')?;
    if !matches!(kind, "touch" | "click") {
        return None;
    }
    let (row, column) = location.split_once(':')?;
    Some((kind, row.parse().ok()?, column.parse().ok()?))
}

#[derive(Clone, Copy)]
pub enum BindingInput<'a> {
    Button(&'a str),
    Trigger(&'a str),
}

impl BindingInput<'_> {
    fn field(self) -> &'static str {
        match self {
            Self::Button(_) => "button",
            Self::Trigger(_) => "trigger",
        }
    }

    fn matches(self, source: &str) -> bool {
        match self {
            Self::Button(button) => {
                source == button
                    || region_input(button).is_some_and(|cell| region_input(source) == Some(cell))
            }
            Self::Trigger(spec) => matching_triggers(spec, source),
        }
    }
}

fn matching_triggers(first: &str, second: &str) -> bool {
    let mut left = first.split(':');
    let mut right = second.split(':');
    if left.next() != right.next() {
        return false;
    }
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b))
                if a.parse::<f32>()
                    .ok()
                    .is_some_and(|value| b.parse::<f32>() == Ok(value)) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn binding_index(
    bindings: &ArrayOfTables,
    input: BindingInput<'_>,
) -> Result<Option<usize>, &'static str> {
    let mut matching = bindings.iter().enumerate().filter(|(_, table)| {
        table
            .get(input.field())
            .and_then(Item::as_str)
            .is_some_and(|source| input.matches(source))
    });
    let Some((index, binding)) = matching.next() else {
        return Ok(None);
    };
    let other = if input.field() == "button" {
        "trigger"
    } else {
        "button"
    };
    if matching.next().is_some() || binding.contains_key(other) || binding.contains_key("chord") {
        return Err("Edit this input in Advanced TOML.");
    }
    Ok(Some(index))
}

pub fn keyboard_binding<'a>(
    doc: &'a DocumentMut,
    button: &str,
) -> Result<Option<&'a Table>, &'static str> {
    binding(doc, BindingInput::Button(button))
}

pub fn binding<'a>(
    doc: &'a DocumentMut,
    input: BindingInput<'_>,
) -> Result<Option<&'a Table>, &'static str> {
    let Some(bindings) = keyboard_bindings(doc)? else {
        return Ok(None);
    };
    Ok(binding_index(bindings, input)?.and_then(|index| bindings.get(index)))
}
pub fn gesture_output(binding: &Table, gesture: Gesture) -> Option<&Item> {
    binding.get(gesture.field())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Gamepad,
    Keyboard,
    Mouse,
    Legacy,
}

impl OutputMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::Gamepad => "gamepad",
            Self::Keyboard => "keyboard",
            Self::Mouse => "mouse",
            Self::Legacy => "legacy",
        }
    }

    pub fn matches_key(self, key: &str) -> bool {
        match self {
            Self::Keyboard => key.starts_with("KEY_"),
            Self::Mouse => key.starts_with("BTN_"),
            _ => true,
        }
    }

    fn matches_action(self, action: &Item) -> bool {
        self != Self::Gamepad
            && (action.as_str().is_some_and(|key| self.matches_key(key))
                || action.as_array().is_some_and(|keys| {
                    keys.iter()
                        .all(|key| key.as_str().is_some_and(|key| self.matches_key(key)))
                })
                || (self == Self::Mouse && action.get("scroll").is_some()))
    }
}

fn belongs_to_button(table: &Table, id: &str) -> bool {
    table.get("button").and_then(Item::as_str) == Some(id)
        || (matches!(id, "l2" | "r2")
            && table
                .get("trigger")
                .and_then(Item::as_str)
                .is_some_and(|trigger| trigger.split(':').next() == Some(id)))
}

pub fn output_mode(doc: &DocumentMut, id: &str) -> Result<OutputMode, &'static str> {
    let explicit = doc
        .get("keyboard")
        .and_then(|keyboard| keyboard.get("button_output"))
        .map(|item| {
            item.as_table_like()
                .ok_or("Edit button_output in Advanced TOML.")
        })
        .transpose()?
        .and_then(|table| table.get(id));
    match explicit {
        Some(item) => match item.as_str() {
            Some("keyboard") => Ok(OutputMode::Keyboard),
            Some("mouse") => Ok(OutputMode::Mouse),
            _ => Err("Edit this button output in Advanced TOML."),
        },
        None if keyboard_bindings(doc)?.is_some_and(|bindings| {
            bindings
                .iter()
                .any(|binding| belongs_to_button(binding, id))
        }) =>
        {
            Ok(OutputMode::Legacy)
        }
        None => Ok(OutputMode::Gamepad),
    }
}

pub fn set_output_mode(
    doc: &mut DocumentMut,
    id: &str,
    mode: OutputMode,
) -> Result<(), &'static str> {
    output_mode(doc, id)?;
    if mode == OutputMode::Legacy {
        return Ok(());
    }
    if mode != OutputMode::Gamepad {
        set_gamepad(doc, id, None)?;
    }
    let keyboard = doc
        .entry("keyboard")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("Edit this keyboard table in Advanced TOML.")?;
    let outputs = keyboard
        .entry("button_output")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("Edit button_output in Advanced TOML.")?;
    if mode == OutputMode::Gamepad {
        outputs.remove(id);
    } else {
        outputs.insert(id, value(mode.key()));
    }
    if outputs.is_empty() {
        keyboard.remove("button_output");
    }
    if let Some(bindings) = keyboard
        .get_mut("bindings")
        .and_then(Item::as_array_of_tables_mut)
    {
        for index in (0..bindings.len()).rev() {
            let binding = bindings.get_mut(index).unwrap();
            if !belongs_to_button(binding, id) {
                continue;
            }
            for gesture in Gesture::ALL {
                if binding
                    .get(gesture.field())
                    .is_some_and(|action| !mode.matches_action(action))
                {
                    binding.remove(gesture.field());
                    if matches!(gesture, Gesture::Hold) {
                        binding.remove("scroll_repeat_ms");
                    }
                }
            }
            if !Gesture::ALL
                .iter()
                .any(|gesture| binding.contains_key(gesture.field()))
            {
                bindings.remove(index);
            }
        }
    }
    Ok(())
}

pub fn set_key(
    doc: &mut DocumentMut,
    button: &str,
    gesture: Gesture,
    output: Option<&str>,
) -> Result<(), &'static str> {
    set_action(doc, BindingInput::Button(button), gesture, output)
}

pub fn set_action(
    doc: &mut DocumentMut,
    input: BindingInput<'_>,
    gesture: Gesture,
    output: Option<&str>,
) -> Result<(), &'static str> {
    let existing = keyboard_bindings(doc)?
        .map(|bindings| binding_index(bindings, input))
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
        let source = match input {
            BindingInput::Button(button) | BindingInput::Trigger(button) => button,
        };
        binding.insert(input.field(), value(source));
        binding.insert(gesture.field(), value(output));
        bindings.push(binding);
    }
    Ok(())
}

const KEEP_PARTIAL_ERROR: &str = "Edit this trigger setting in Advanced TOML.";

pub fn keep_partial(doc: &DocumentMut, trigger: &str) -> Result<bool, &'static str> {
    let Some(keyboard) = doc.get("keyboard") else {
        return Ok(false);
    };
    let keyboard = keyboard.as_table_like().ok_or(KEEP_PARTIAL_ERROR)?;
    let Some(modes) = keyboard.get("keep_partial") else {
        return Ok(false);
    };
    let modes = modes.as_table_like().ok_or(KEEP_PARTIAL_ERROR)?;
    modes
        .get(trigger)
        .map_or(Ok(false), |item| item.as_bool().ok_or(KEEP_PARTIAL_ERROR))
}

pub fn set_keep_partial(
    doc: &mut DocumentMut,
    trigger: &str,
    enabled: bool,
) -> Result<(), &'static str> {
    let keyboard = doc
        .entry("keyboard")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or(KEEP_PARTIAL_ERROR)?;
    let modes = keyboard
        .entry("keep_partial")
        .or_insert_with(|| value(toml_edit::InlineTable::new()))
        .as_table_like_mut()
        .ok_or(KEEP_PARTIAL_ERROR)?;
    if let Some(previous) = modes.get_mut(trigger).and_then(Item::as_value_mut) {
        replace_value(previous, Value::from(enabled));
    } else {
        modes.insert(trigger, value(enabled));
    }
    Ok(())
}

pub fn set_output_kind(doc: &mut DocumentMut, kind: Option<&str>) -> Result<(), &'static str> {
    if kind.is_none() && !doc.contains_key("bridge") {
        return Ok(());
    }
    let bridge = doc
        .entry("bridge")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("Edit the bridge table in Advanced TOML.")?;
    if let Some(kind) = kind {
        if let Some(old) = bridge.get_mut("gamepad").and_then(Item::as_value_mut) {
            replace_value(old, Value::from(kind));
        } else {
            bridge.insert("gamepad", value(kind));
        }
    } else {
        bridge.remove("gamepad");
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

pub(crate) fn replace_value(old: &mut Value, mut new: Value) {
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

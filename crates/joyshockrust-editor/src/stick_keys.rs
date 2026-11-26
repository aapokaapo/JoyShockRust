use std::{cell::RefCell, rc::Rc};

use adw::{gtk, prelude::*};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::{bindings::replace_value, controller, ui::Context};

const DIRECTIONS: [(&str, &str); 4] = [
    ("up", "Up"),
    ("left", "Left"),
    ("down", "Down"),
    ("right", "Right"),
];
const ADVANCED: &str = "Edit this keyboard stick mapping in Advanced TOML.";

struct Zone {
    key: String,
    mode: String,
    threshold: f64,
    release: Option<f64>,
}

fn stick_setting(doc: &DocumentMut) -> Result<Option<&Item>, &'static str> {
    let Some(keyboard) = doc.get("keyboard") else {
        return Ok(None);
    };
    Ok(keyboard.as_table_like().ok_or(ADVANCED)?.get("sticks"))
}

fn stick_map<'a>(
    doc: &'a DocumentMut,
    side: &str,
) -> Result<Option<(usize, &'a dyn TableLike)>, &'static str> {
    let Some(item) = stick_setting(doc)? else {
        return Ok(None);
    };
    let mut entries = Vec::<&dyn TableLike>::new();
    if let Some(tables) = item.as_array_of_tables() {
        entries.extend(tables.iter().map(|table| table as &dyn TableLike));
    } else if let Some(array) = item.as_array() {
        for entry in array {
            entries.push(entry.as_inline_table().ok_or(ADVANCED)?);
        }
    } else {
        return Err(ADVANCED);
    }
    let mut matches = entries
        .into_iter()
        .enumerate()
        .filter(|(_, table)| table.get("side").and_then(Item::as_str) == Some(side));
    let found = matches.next();
    if matches.next().is_some() {
        return Err(ADVANCED);
    }
    Ok(found)
}

fn apply_mapping(
    doc: &mut DocumentMut,
    side: &str,
    directions: &[Option<String>; 4],
    zones: &[Option<Zone>; 2],
) -> Result<(), &'static str> {
    let existing = stick_map(doc, side)?.map(|(index, _)| index);
    let has_keys = directions.iter().any(Option::is_some) || zones.iter().any(Option::is_some);
    if existing.is_none() && !has_keys {
        return Ok(());
    }
    let keyboard = doc
        .entry("keyboard")
        .or_insert_with(|| Item::Table(Table::new()));
    if keyboard.get("sticks").is_none() {
        let empty = if keyboard.is_inline_table() {
            Item::Value(Value::Array(Array::new()))
        } else {
            Item::ArrayOfTables(ArrayOfTables::new())
        };
        keyboard
            .as_table_like_mut()
            .ok_or(ADVANCED)?
            .insert("sticks", empty);
    }
    let sticks = keyboard.get_mut("sticks").ok_or(ADVANCED)?;
    let mapping: &mut dyn TableLike = if let Some(array) = sticks.as_array_mut() {
        match (existing, has_keys) {
            (Some(index), false) => {
                array.remove(index);
                return Ok(());
            }
            (Some(index), true) => array
                .get_mut(index)
                .and_then(Value::as_inline_table_mut)
                .ok_or(ADVANCED)?,
            (None, _) => {
                array.push(InlineTable::new());
                let index = array.len() - 1;
                array
                    .get_mut(index)
                    .and_then(Value::as_inline_table_mut)
                    .ok_or(ADVANCED)?
            }
        }
    } else {
        let tables = sticks.as_array_of_tables_mut().ok_or(ADVANCED)?;
        match (existing, has_keys) {
            (Some(index), false) => {
                tables.remove(index);
                return Ok(());
            }
            (Some(index), true) => tables.get_mut(index).ok_or(ADVANCED)?,
            (None, _) => {
                tables.push(Table::new());
                tables.get_mut(tables.len() - 1).ok_or(ADVANCED)?
            }
        }
    };
    set_field(mapping, "side", Some(Value::from(side)));
    for ((field, _), key) in DIRECTIONS.into_iter().zip(directions) {
        set_field(mapping, field, key.as_deref().map(Value::from));
    }
    for (field, zone) in ["inner", "outer"].into_iter().zip(zones) {
        let Some(zone) = zone else {
            mapping.remove(field);
            continue;
        };
        let entry = mapping
            .entry(field)
            .or_insert_with(|| Item::Value(Value::InlineTable(InlineTable::new())));
        let table = entry.as_table_like_mut().ok_or(ADVANCED)?;
        set_field(table, "key", Some(Value::from(zone.key.as_str())));
        if zone.mode != "hold" || table.contains_key("mode") {
            set_field(table, "mode", Some(Value::from(zone.mode.as_str())));
        }
        set_field(table, "threshold", Some(Value::from(zone.threshold)));
        set_field(table, "release", zone.release.map(Value::from));
    }
    Ok(())
}

fn set_field(table: &mut dyn TableLike, field: &str, new: Option<Value>) {
    if let Some(new) = new {
        if let Some(old) = table.get_mut(field).and_then(Item::as_value_mut) {
            let same_text = new
                .as_str()
                .is_some_and(|value| old.as_str() == Some(value));
            let same_number = new.as_float().is_some_and(|value| {
                old.as_float()
                    .or_else(|| old.as_integer().map(|number| number as f64))
                    == Some(value)
            });
            if !same_text && !same_number {
                replace_value(old, new);
            }
        } else {
            table.insert(field, Item::Value(new));
        }
    } else {
        table.remove(field);
    }
}

pub fn editor_row(ctx: &Context, side: &'static str, title: &'static str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("Keyboard mapping")
        .subtitle("Direction keys and inner or outer zones")
        .build();
    let edit = gtk::Button::with_label("Edit…");
    edit.set_valign(gtk::Align::Center);
    row.add_suffix(&edit);
    row.set_activatable_widget(Some(&edit));
    let ctx = ctx.clone();
    edit.connect_clicked(move |button| {
        let result = ctx.document().and_then(|doc| {
            let mapping = stick_map(&doc, side)?.map(|(_, table)| table);
            let dialog = mapping_dialog(&ctx, side, title, mapping)?;
            dialog.present(Some(button));
            Ok(())
        });
        if let Err(error) = result {
            ctx.report(Err(error));
        }
    });
    row
}

pub(super) fn mapping_dialog(
    ctx: &Context,
    side: &'static str,
    title: &str,
    mapping: Option<&dyn TableLike>,
) -> Result<adw::Dialog, &'static str> {
    let dialog = adw::Dialog::builder()
        .title(format!("{title} keyboard mapping"))
        .content_width(520)
        .content_height(670)
        .build();
    let page = adw::PreferencesPage::new();
    let direction_group = adw::PreferencesGroup::builder()
        .title("Directions")
        .description("Moving diagonally can hold two direction keys.")
        .build();
    let directions = DIRECTIONS.map(|(field, label)| {
        let current = mapping
            .and_then(|table| table.get(field))
            .and_then(Item::as_str);
        let (row, key) = key_picker(label, current);
        direction_group.add(&row);
        key
    });
    page.add(&direction_group);

    let zone_table = |field| {
        mapping
            .and_then(|table| table.get(field))
            .map(|item| item.as_table_like().ok_or(ADVANCED))
            .transpose()
    };
    let inner = zone_editor(
        &page,
        "Inner zone",
        "Activate near the center outside the deadzone.",
        zone_table("inner")?,
        "0.5",
    );
    let outer = zone_editor(
        &page,
        "Outer zone",
        "Activate when the stick moves toward the edge.",
        zone_table("outer")?,
        "0.8",
    );

    let header = adw::HeaderBar::new();
    let apply = gtk::Button::with_label("Update Mapping");
    apply.add_css_class("suggested-action");
    header.pack_end(&apply);
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(8)
        .visible(false)
        .build();
    error.add_css_class("error");
    content.add_top_bar(&error);
    content.set_content(Some(&page));
    dialog.set_child(Some(&content));
    let dialog_ref = dialog.downgrade();
    let error_ref = error.downgrade();
    let ctx = ctx.clone();
    apply.connect_clicked(move |_| {
        let result: Result<(), String> = (|| {
            let mut doc = ctx.document()?;
            let keys = directions.each_ref().map(|key| key.borrow().clone());
            let zones = [inner.read()?, outer.read()?];
            apply_mapping(&mut doc, side, &keys, &zones)?;
            let text = doc.to_string();
            let mut editor = ctx.editor.borrow_mut();
            editor.validate_profile_text(&text)?;
            editor.draft = crate::Draft::from_document(doc);
            Ok(())
        })();
        match result {
            Ok(()) => {
                if let Some(dialog) = dialog_ref.upgrade() {
                    dialog.close();
                }
            }
            Err(message) => {
                if let Some(error) = error_ref.upgrade() {
                    error.set_text(&message);
                    error.set_visible(true);
                }
            }
        }
    });
    Ok(dialog)
}

fn key_picker(title: &str, current: Option<&str>) -> (adw::ActionRow, Rc<RefCell<Option<String>>>) {
    let key = Rc::new(RefCell::new(current.map(str::to_owned)));
    let selected = key.clone();
    let row = crate::key_capture::row(title, current, &[], move |value| {
        *selected.borrow_mut() = value.map(str::to_owned)
    });
    (row, key)
}

struct ZoneEditor {
    key: Rc<RefCell<Option<String>>>,
    mode: Rc<RefCell<Option<String>>>,
    threshold: adw::EntryRow,
    release: adw::EntryRow,
}

impl ZoneEditor {
    fn read(&self) -> Result<Option<Zone>, &'static str> {
        let Some(key) = self.key.borrow().clone() else {
            return Ok(None);
        };
        let threshold = radius(&self.threshold.text())?;
        let release = self.release.text();
        let release = if release.trim().is_empty() {
            None
        } else {
            Some(radius(&release)?)
        };
        let mode = self.mode.borrow().clone().unwrap_or_else(|| "hold".into());
        Ok(Some(Zone {
            key,
            mode,
            threshold,
            release,
        }))
    }
}

fn radius(text: &str) -> Result<f64, &'static str> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|number| (0.0..=1.0).contains(number))
        .ok_or("Enter a radius between 0 and 1 for enabled zones.")
}

fn zone_editor(
    page: &adw::PreferencesPage,
    title: &str,
    description: &str,
    existing: Option<&dyn TableLike>,
    default_threshold: &str,
) -> ZoneEditor {
    let group = adw::PreferencesGroup::builder()
        .title(title)
        .description(description)
        .build();
    let item = |field| existing.and_then(|table| table.get(field));
    let (key_row, key) = key_picker("Key", item("key").and_then(Item::as_str));
    group.add(&key_row);
    let current_mode = item("mode")
        .and_then(Item::as_str)
        .filter(|mode| *mode != "hold");
    let mode = Rc::new(RefCell::new(current_mode.map(str::to_owned)));
    let selected = mode.clone();
    let mode_row = controller::dropdown(
        "Mode",
        current_mode,
        "Hold",
        [("toggle", "Toggle"), ("keep_sprint", "Keep sprint")]
            .into_iter()
            .map(|(key, label)| (key.into(), label.into())),
        move |value| *selected.borrow_mut() = value.map(str::to_owned),
    );
    group.add(&mode_row);
    let entry = |title, field, default| {
        let row = adw::EntryRow::builder()
            .title(title)
            .input_purpose(gtk::InputPurpose::Number)
            .build();
        let number = item(field).and_then(|item| {
            item.as_float()
                .or_else(|| item.as_integer().map(|n| n as f64))
        });
        row.set_text(&number.map_or_else(|| String::from(default), |number| number.to_string()));
        group.add(&row);
        row
    };
    let threshold = entry("Engage radius", "threshold", default_threshold);
    let release = entry("Release radius (empty for automatic)", "release", "");
    for widget in [
        mode_row.upcast_ref::<gtk::Widget>(),
        threshold.upcast_ref(),
        release.upcast_ref(),
    ] {
        widget.set_sensitive(key.borrow().is_some());
        let widget = widget.downgrade();
        let key = key.clone();
        key_row.connect_subtitle_notify(move |_| {
            if let Some(widget) = widget.upgrade() {
                widget.set_sensitive(key.borrow().is_some());
            }
        });
    }
    page.add(&group);
    ZoneEditor {
        key,
        mode,
        threshold,
        release,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_both_zones_without_rewriting_other_stick_settings() {
        let mut doc: DocumentMut = r#"
version = 1
[[keyboard.sticks]]
side = "left"
deadzone = 0.25
up = "KEY_W" # forward
inner = { key = "KEY_LEFTSHIFT", threshold = 0.55 }
[[keyboard.sticks]]
side = "right"
left = "KEY_LEFT"
"#
        .parse()
        .unwrap();
        apply_mapping(
            &mut doc,
            "left",
            &[Some("KEY_W".into()), Some("KEY_A".into()), None, None],
            &[
                Some(Zone {
                    key: "KEY_LEFTSHIFT".into(),
                    mode: "hold".into(),
                    threshold: 0.55,
                    release: None,
                }),
                Some(Zone {
                    key: "KEY_R".into(),
                    mode: "keep_sprint".into(),
                    threshold: 0.9,
                    release: Some(0.8),
                }),
            ],
        )
        .unwrap();
        let text = doc.to_string();
        assert!(text.contains("up = \"KEY_W\" # forward"));
        assert!(text.contains("deadzone = 0.25"));
        assert!(text.contains("inner = { key = \"KEY_LEFTSHIFT\", threshold = 0.55 }"));
        let doc: DocumentMut = text.parse().unwrap();
        let (_, left) = stick_map(&doc, "left").unwrap().unwrap();
        let outer = left.get("outer").unwrap().as_table_like().unwrap();
        assert_eq!(
            outer.get("mode").and_then(Item::as_str),
            Some("keep_sprint")
        );
        assert_eq!(outer.get("release").and_then(Item::as_float), Some(0.8));
        let (_, right) = stick_map(&doc, "right").unwrap().unwrap();
        assert_eq!(right.get("left").and_then(Item::as_str), Some("KEY_LEFT"));
    }

    #[test]
    fn creates_and_removes_a_mapping_in_an_inline_profile() {
        let mut doc: DocumentMut = "version = 1\nkeyboard = { sticks = [] }\n".parse().unwrap();
        apply_mapping(
            &mut doc,
            "right",
            &[None, None, None, Some("KEY_D".into())],
            &[None, None],
        )
        .unwrap();
        let mut doc: DocumentMut = doc.to_string().parse().unwrap();
        let (_, right) = stick_map(&doc, "right").unwrap().unwrap();
        assert_eq!(right.get("right").and_then(Item::as_str), Some("KEY_D"));
        apply_mapping(&mut doc, "right", &[None, None, None, None], &[None, None]).unwrap();
        assert!(stick_map(&doc, "right").unwrap().is_none());
    }
}

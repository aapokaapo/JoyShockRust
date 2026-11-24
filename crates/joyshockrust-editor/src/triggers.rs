use std::{
    io,
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
};

use adw::{gtk, prelude::*};
use joyshockrust_core::{DEFAULT_TRIGGER_THRESHOLDS, validate_trigger_thresholds};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value, value};

use crate::{
    bindings::{self, BindingInput, Gesture, OutputMode},
    controller, trigger_travel,
    ui::Context,
};

const ADVANCED: &str = "Edit this trigger configuration in Advanced TOML.";
const STAGES: [(&str, &str, &str, &str); 2] = [
    (
        "Partial pull",
        "partial",
        "partial_button",
        "0.45:0.35:0.95:0.85",
    ),
    ("Full pull", "full", "full_button", "0.95:0.85"),
];

pub fn send_preview(payload: &[u8]) -> io::Result<()> {
    send_preview_message(b"\0preview-trigger\n", payload)
}

pub(crate) fn send_preview_message(prefix: &[u8], payload: &[u8]) -> io::Result<()> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|path| !path.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unset"))?;
    send_preview_to(
        &PathBuf::from(runtime).join("joyshockrust/reload.sock.load"),
        prefix,
        payload,
    )
}

fn send_preview_to(path: &Path, prefix: &[u8], payload: &[u8]) -> io::Result<()> {
    let socket = UnixDatagram::unbound()?;
    socket.set_nonblocking(true)?;
    let mut message = prefix.to_vec();
    message.extend_from_slice(payload);
    socket.send_to(&message, path)?;
    Ok(())
}

pub fn settings<'a>(
    doc: &'a DocumentMut,
    side: &str,
) -> Result<Option<&'a dyn TableLike>, &'static str> {
    doc.get("triggers")
        .map(|item| item.as_table_like().ok_or(ADVANCED))
        .transpose()?
        .and_then(|table| table.get(side))
        .map(|item| item.as_table_like().ok_or(ADVANCED))
        .transpose()
}

fn settings_mut<'a>(
    doc: &'a mut DocumentMut,
    side: &str,
) -> Result<&'a mut dyn TableLike, &'static str> {
    doc.entry("triggers")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or(ADVANCED)?
        .entry(side)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or(ADVANCED)
}

fn thresholds(settings: Option<&dyn TableLike>) -> Result<[u8; 4], &'static str> {
    let Some(item) = settings.and_then(|table| table.get("thresholds")) else {
        return Ok(DEFAULT_TRIGGER_THRESHOLDS);
    };
    let values = item.as_array().ok_or(ADVANCED)?;
    if values.len() != 4 {
        return Err(ADVANCED);
    }
    let mut result = [0; 4];
    for (stage, value) in result.iter_mut().zip(values.iter()) {
        *stage = value
            .as_integer()
            .and_then(|number| u8::try_from(number).ok())
            .ok_or(ADVANCED)?;
    }
    validate_trigger_thresholds(result)?;
    Ok(result)
}

fn legacy_overlap(doc: &DocumentMut, side: &str) -> bool {
    doc.get("keyboard")
        .and_then(|item| item.get("keep_partial"))
        .and_then(|item| item.get(side))
        .and_then(Item::as_bool)
        .unwrap_or(false)
}

fn migrate_stages(doc: &mut DocumentMut, side: &str) -> Result<(), &'static str> {
    for (_, stage, _, old_spec) in STAGES {
        bindings::rename_trigger(
            doc,
            &format!("{side}:{old_spec}"),
            &format!("{side}:{stage}"),
        )?;
    }
    let overlap = legacy_overlap(doc, side);
    let settings = settings_mut(doc, side)?;
    if !settings.contains_key("keep_partial") && overlap {
        settings.insert("keep_partial", value(true));
    }
    Ok(())
}

pub fn append(
    inspector: &gtk::Box,
    ctx: &Context,
    doc: &DocumentMut,
    side: &'static str,
    mode: OutputMode,
) {
    let values = settings(doc, side);
    let group = adw::PreferencesGroup::builder()
        .title("Trigger travel")
        .description("Set where each stage engages and releases, as a percentage of travel.")
        .build();
    inspector.append(&group);
    let (values, initial) = match values.and_then(|table| Ok((table, thresholds(table)?))) {
        Ok(values) => values,
        Err(error) => {
            group.add(&controller::message_row("Trigger settings", error));
            return;
        }
    };
    let overlap = values
        .and_then(|table| table.get("keep_partial"))
        .and_then(Item::as_bool)
        .unwrap_or_else(|| legacy_overlap(doc, side));
    let travel = trigger_travel::add(&group, side, initial, overlap);

    let rows = [
        "Partial pull (%)",
        "Partial release (%)",
        "Full pull (%)",
        "Full release (%)",
    ]
    .map(|title| spin(title, 0, 100, 0));
    for (row, initial) in rows.iter().zip(initial) {
        row.set_value(f64::from(initial));
        group.add(row);
    }
    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    group.add(&error);
    let weak = rows.each_ref().map(|row| row.downgrade());
    let edit = ctx.clone();
    let thresholds_chart = travel.clone();
    let update = move |_: &adw::SpinRow| {
        let [Some(a), Some(b), Some(c), Some(d)] = weak.each_ref().map(|row| row.upgrade()) else {
            return;
        };
        let proposed = [a, b, c, d].map(|row| row.value() as u8);
        let result = edit.try_edit(|doc| {
            validate_trigger_thresholds(proposed)?;
            migrate_stages(doc, side)?;
            let thresholds: Array = proposed.into_iter().map(i64::from).collect();
            settings_mut(doc, side)?.insert("thresholds", value(thresholds));
            Ok(())
        });
        error.set_visible(result.is_err());
        if let Err(message) = result {
            error.set_text(&message);
        } else {
            thresholds_chart.set_thresholds(proposed);
        }
    };
    for row in rows {
        row.connect_value_notify(update.clone());
    }

    let keep = adw::SwitchRow::builder()
        .title("Keep partial action at full pull")
        .active(overlap)
        .build();
    let edit = ctx.clone();
    keep.connect_active_notify(move |row| {
        if edit.edit(|doc| {
            migrate_stages(doc, side)?;
            settings_mut(doc, side)?.insert("keep_partial", value(row.is_active()));
            Ok(())
        }) {
            travel.set_keep_partial(row.is_active());
        }
    });
    group.add(&keep);
    for (title, stage, field, legacy) in STAGES {
        let stage_group = adw::PreferencesGroup::builder().title(title).build();
        inspector.append(&stage_group);
        if mode == OutputMode::Gamepad {
            let current = values
                .and_then(|table| table.get(field))
                .and_then(Item::as_str);
            let edit = ctx.clone();
            stage_group.add(&controller::dropdown(
                "Output button",
                current,
                "No action",
                controller::GAMEPAD_TARGETS
                    .into_iter()
                    .map(|(key, label)| (key.into(), label.into())),
                move |action| {
                    edit.edit(|doc| {
                        let stage = settings_mut(doc, side)?;
                        if let Some(action) = action {
                            stage.insert(field, value(action));
                        } else {
                            stage.remove(field);
                        }
                        Ok(())
                    });
                },
            ));
        } else {
            let spec = format!("{side}:{stage}");
            let action = bindings::binding(doc, BindingInput::Trigger(&spec)).and_then(|binding| {
                if binding.is_some() {
                    Ok(binding)
                } else {
                    bindings::binding(doc, BindingInput::Trigger(&format!("{side}:{legacy}")))
                }
            });
            let current = match action {
                Ok(binding)
                    if !binding.is_some_and(|table| {
                        table.contains_key("tap")
                            || table.contains_key("double_tap")
                            || table
                                .get("hold")
                                .is_some_and(|value| value.as_str().is_none())
                    }) =>
                {
                    binding
                        .and_then(|table| table.get("hold"))
                        .and_then(Item::as_str)
                }
                _ => {
                    stage_group.add(&controller::message_row(
                        "Existing trigger action",
                        ADVANCED,
                    ));
                    continue;
                }
            };
            let edit = ctx.clone();
            let changed = move |action: Option<&str>| {
                edit.edit(|doc| {
                    migrate_stages(doc, side)?;
                    bindings::set_action(doc, BindingInput::Trigger(&spec), Gesture::Hold, action)
                });
            };
            if mode == OutputMode::Mouse {
                stage_group.add(&controller::mouse_action_row("Action", current, changed));
            } else {
                let mouse = if mode == OutputMode::Legacy {
                    controller::MOUSE_BUTTONS
                } else {
                    &[]
                };
                stage_group.add(&crate::key_capture::row("Action", current, mouse, changed));
            }
        }
    }
    if bindings::keyboard_binding(doc, side)
        .ok()
        .flatten()
        .is_some()
        || bindings::gamepad_target(doc, side).ok().flatten().is_some()
    {
        group.add(&controller::message_row(
            "Existing digital trigger binding",
            ADVANCED,
        ));
    }
    add_adaptive(inspector, ctx, side, values);
}

fn spin(title: &str, min: u8, max: u8, initial: u8) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(f64::from(min), f64::from(max), 1.0);
    row.set_title(title);
    row.set_snap_to_ticks(true);
    row.set_value(f64::from(initial));
    row
}

fn add_adaptive(
    inspector: &gtk::Box,
    ctx: &Context,
    side: &'static str,
    settings: Option<&dyn TableLike>,
) {
    let group = adw::PreferencesGroup::builder()
        .title("Adaptive trigger")
        .description("Changes preview immediately on the connected controller. Save &amp; Apply to keep them in the active profile.")
        .build();
    inspector.append(&group);
    let effect = settings.and_then(|table| table.get("adaptive"));
    let current = effect
        .and_then(|item| item.get("mode"))
        .and_then(Item::as_str)
        .unwrap_or("off");
    let numeric = |key, fallback| {
        effect
            .and_then(|item| item.get(key))
            .and_then(Item::as_integer)
            .and_then(|number| u8::try_from(number).ok())
            .unwrap_or(fallback)
    };
    let start = spin("Start zone (0–9)", 0, 9, numeric("start", 3));
    let strength = spin("Resistance strength", 1, 8, numeric("strength", 3));
    let feedback = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    let mode = controller::required_dropdown(
        "Effect",
        current,
        [
            ("off".into(), "Off".into()),
            ("resistance".into(), "Resistance".into()),
        ]
        .into_iter(),
        {
            let edit = ctx.clone();
            let start = start.downgrade();
            let strength = strength.downgrade();
            let feedback = feedback.clone();
            move |mode| {
                if let (Some(start), Some(strength)) = (start.upgrade(), strength.upgrade()) {
                    save_effect(
                        &edit,
                        &feedback,
                        side,
                        mode == "resistance",
                        start.value() as u8,
                        strength.value() as u8,
                    );
                }
            }
        },
    );
    group.add(&mode);
    for row in [&start, &strength] {
        mode.bind_property("selected", row, "sensitive")
            .transform_to(|_, index: u32| Some(index == 1))
            .sync_create()
            .build();
        let edit = ctx.clone();
        let mode = mode.downgrade();
        let start = start.downgrade();
        let strength = strength.downgrade();
        let feedback = feedback.clone();
        row.connect_value_notify(move |_| {
            if let (Some(mode), Some(start), Some(strength)) =
                (mode.upgrade(), start.upgrade(), strength.upgrade())
                && mode.selected() == 1
            {
                save_effect(
                    &edit,
                    &feedback,
                    side,
                    true,
                    start.value() as u8,
                    strength.value() as u8,
                );
            }
        });
        group.add(row);
    }
    group.add(&feedback);
}

fn save_effect(
    ctx: &Context,
    feedback: &gtk::Label,
    side: &str,
    enabled: bool,
    start: u8,
    strength: u8,
) {
    if !ctx.edit(|doc| {
        let table = settings_mut(doc, side)?;
        if enabled {
            let mut effect = InlineTable::new();
            effect.insert("mode", Value::from("resistance"));
            effect.insert("start", Value::from(i64::from(start)));
            effect.insert("strength", Value::from(i64::from(strength)));
            table.insert("adaptive", value(effect));
        } else {
            table.remove("adaptive");
        }
        Ok(())
    }) {
        return;
    }
    let side = u8::from(side == "r2");
    let [start, strength] = if enabled { [start, strength] } else { [0, 0] };
    let result = ctx
        .editor
        .borrow_mut()
        .preview_trigger([side, start, strength]);
    feedback.set_visible(result.is_err());
    if let Err(error) = result {
        feedback.set_text(&format!(
            "Could not send the live preview to JoyShockRust: {error}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_preview_sends_effect_and_reset_as_separate_datagrams() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reload.sock.load");
        let listener = UnixDatagram::bind(&path).unwrap();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();

        for (payload, expected) in [
            (&[1, 3, 7][..], &b"\0preview-trigger\n\x01\x03\x07"[..]),
            (&[0, 0, 0][..], &b"\0preview-trigger\n\x00\x00\x00"[..]),
            (&[][..], &b"\0preview-trigger\n"[..]),
        ] {
            send_preview_to(&path, b"\0preview-trigger\n", payload).unwrap();
            let mut buffer = [0; 32];
            let (len, sender) = listener.recv_from(&mut buffer).unwrap();
            assert!(sender.is_unnamed());
            assert_eq!(&buffer[..len], expected);
        }
    }

    #[test]
    fn editing_default_legacy_stages_keeps_actions_and_overlap() {
        let mut doc = r#"
version = 1
[keyboard.keep_partial]
l2 = true
[[keyboard.bindings]]
trigger = "l2:0.450:0.350:0.950:0.850"
hold = "KEY_Q"
[[keyboard.bindings]]
trigger = "l2:0.95:0.85"
hold = "KEY_E"
[[keyboard.bindings]]
trigger = "r2:0.60:0.50"
hold = "KEY_W"
"#
        .parse()
        .unwrap();
        migrate_stages(&mut doc, "l2").unwrap();
        migrate_stages(&mut doc, "l2").unwrap();
        assert_eq!(doc["triggers"]["l2"]["keep_partial"].as_bool(), Some(true));
        for (input, key) in [
            ("l2:partial", "KEY_Q"),
            ("l2:full", "KEY_E"),
            ("r2:0.60:0.50", "KEY_W"),
        ] {
            assert_eq!(
                bindings::binding(&doc, BindingInput::Trigger(input))
                    .unwrap()
                    .unwrap()["hold"]
                    .as_str(),
                Some(key)
            );
        }
    }
}

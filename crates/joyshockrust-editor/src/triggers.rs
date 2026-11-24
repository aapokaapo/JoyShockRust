use std::{cell::Cell, io, os::unix::net::UnixDatagram, path::Path, rc::Rc};

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
    let path = joyshockrust_config::paths::runtime_socket("reload.sock.load", false)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unset"))?;
    send_preview_to(&path, prefix, payload)
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
    refresh: impl Fn() + Clone + 'static,
) {
    let values = settings(doc, side);
    let group = adw::PreferencesGroup::builder()
        .title("Trigger travel")
        .build();
    inspector.append(&group);
    let (values, initial) = match values.and_then(|table| Ok((table, thresholds(table)?))) {
        Ok(values) => values,
        Err(error) => {
            group.add(&controller::message_row("Trigger settings", error));
            return;
        }
    };
    let exclusive = values
        .and_then(|table| table.get("full_pull_exclusive"))
        .and_then(Item::as_bool)
        .unwrap_or(false);
    let overlap = values
        .and_then(|table| table.get("keep_partial"))
        .and_then(Item::as_bool)
        .unwrap_or_else(|| legacy_overlap(doc, side));
    let range = travel_range(values);
    let travel = trigger_travel::add(&group, side, initial, overlap, range);
    add_travel_limit(&group, ctx, side, mode, range.0, travel.clone());
    let stages = Rc::new(Cell::new([false; 2]));

    let rows = [
        "Partial pull (%)",
        "Partial release (%)",
        "Full pull (%)",
        "Full release (%)",
    ]
    .map(|title| spin(title, 0, 100, 0));
    for (row, initial) in rows.iter().zip(initial) {
        row.set_value(f64::from(initial));
        row.set_visible(false);
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
    let threshold_error = error.clone();
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
        threshold_error.set_visible(result.is_err());
        if let Err(message) = result {
            threshold_error.set_text(&message);
        } else {
            thresholds_chart.set_thresholds(proposed);
        }
    };
    for row in &rows {
        row.connect_value_notify(update.clone());
    }

    let keep = adw::SwitchRow::builder()
        .title("Keep partial action at full pull")
        .active(overlap)
        .visible(false)
        .build();
    let edit = ctx.clone();
    let overlap_chart = travel.clone();
    keep.connect_active_notify(move |row| {
        if edit.edit(|doc| {
            migrate_stages(doc, side)?;
            settings_mut(doc, side)?.insert("keep_partial", value(row.is_active()));
            Ok(())
        }) {
            overlap_chart.set_keep_partial(row.is_active());
        }
    });
    group.add(&keep);
    let assigned_stages = stages.clone();
    let stage_chart = travel.clone();
    let update_stage = move |index: usize, assigned: bool| {
        let mut shown = stages.get();
        shown[index] = assigned;
        stages.set(shown);
        for row in &rows[index * 2..index * 2 + 2] {
            row.set_visible(assigned);
        }
        keep.set_visible(shown == [true; 2]);
        if !shown.contains(&true) {
            error.set_visible(false);
        }
        stage_chart.set_stages(shown);
    };
    let [partial_group, full_group] =
        STAGES.map(|(title, ..)| adw::PreferencesGroup::builder().title(title).build());
    partial_group.set_visible(false);
    inspector.append(&full_group);
    inspector.append(&partial_group);
    for index in [1, 0] {
        let (_, stage, field, legacy) = STAGES[index];
        let stage_group = if index == 0 {
            &partial_group
        } else {
            &full_group
        };
        if mode == OutputMode::Gamepad {
            let current = values
                .and_then(|table| table.get(field))
                .and_then(Item::as_str);
            let edit = ctx.clone();
            let analog = index == 1 && is_analog_target(current);
            if index == 1 && !analog {
                partial_group.set_visible(true);
            }
            update_stage(
                index,
                current.is_some() && !analog && (index == 1 || partial_group.is_visible()),
            );
            let update_stage = update_stage.clone();
            let partial_group = partial_group.downgrade();
            let keep_own_entry = !exclusive && current == Some(side);
            let choices = controller::GAMEPAD_TARGETS
                .into_iter()
                .filter(move |(key, _)| index == 0 || *key != side || keep_own_entry)
                .map(move |(key, label)| {
                    let label = match (index, key) {
                        (1, "l2") => "Left trigger",
                        (1, "r2") => "Right trigger",
                        _ => label,
                    };
                    (key.into(), label.into())
                });
            let travel = travel.clone();
            let assigned_stages = assigned_stages.clone();
            let refresh = refresh.clone();
            stage_group.add(&controller::dropdown(
                "Output button",
                current.filter(|target| index == 0 || !exclusive || *target != side),
                if index == 1 {
                    "Same as controller"
                } else {
                    "No action"
                },
                choices,
                move |action| {
                    let analog = index == 1 && is_analog_target(action);
                    if edit.edit(|doc| {
                        let stage = settings_mut(doc, side)?;
                        if let Some(action) = action.filter(|action| index == 0 || *action != side)
                        {
                            stage.insert(field, value(action));
                        } else {
                            stage.remove(field);
                        }
                        if index == 1 {
                            stage.insert("full_pull_exclusive", value(true));
                            if analog {
                                stage.remove("partial_button");
                            }
                        }
                        Ok(())
                    }) {
                        if index == 1 {
                            if analog && assigned_stages.get()[0] {
                                refresh();
                                return;
                            }
                            if let Some(partial_group) = partial_group.upgrade() {
                                partial_group.set_visible(!analog);
                            }
                            if let Ok(doc) = edit.document()
                                && let Ok(values) = settings(&doc, side)
                            {
                                travel.set_range(travel_range(values));
                            }
                        }
                        update_stage(index, action.is_some() && !analog);
                    }
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
            let assigned = action.as_ref().is_ok_and(|binding| binding.is_some());
            update_stage(index, assigned);
            if assigned {
                partial_group.set_visible(true);
            }
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
            let update_stage = update_stage.clone();
            let partial_group = partial_group.downgrade();
            let assigned_stages = assigned_stages.clone();
            let refresh = refresh.clone();
            let changed = move |action: Option<&str>| {
                if edit.edit(|doc| {
                    migrate_stages(doc, side)?;
                    bindings::set_action(doc, BindingInput::Trigger(&spec), Gesture::Hold, action)?;
                    if index == 1 && action.is_none() {
                        bindings::set_action(
                            doc,
                            BindingInput::Trigger(&format!("{side}:partial")),
                            Gesture::Hold,
                            None,
                        )?;
                    }
                    Ok(())
                }) {
                    if index == 1 {
                        if action.is_none() && assigned_stages.get()[0] {
                            refresh();
                            return;
                        }
                        if let Some(partial_group) = partial_group.upgrade() {
                            partial_group.set_visible(action.is_some());
                        }
                    }
                    if index == 0
                        && action.is_none()
                        && !assigned_stages.get()[1]
                        && let Some(partial_group) = partial_group.upgrade()
                    {
                        partial_group.set_visible(false);
                    }
                    update_stage(index, action.is_some());
                }
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
        inspector.append(&controller::message_row(
            "Existing digital trigger binding",
            ADVANCED,
        ));
    }
    add_adaptive(inspector, ctx, side, values);
}

fn is_analog_target(action: Option<&str>) -> bool {
    matches!(action, None | Some("l2" | "r2"))
}

fn travel_range(settings: Option<&dyn TableLike>) -> (u8, bool) {
    let limit = settings.and_then(|table| table.get("travel_limit_percent"));
    let exclusive = settings
        .and_then(|table| table.get("full_pull_exclusive"))
        .and_then(Item::as_bool)
        .unwrap_or_default();
    let relative =
        joyshockrust_config::profile::relative_trigger_stages(exclusive, limit.is_some());
    let percent = limit
        .or_else(|| settings.and_then(|table| table.get("full_travel_percent")))
        .and_then(Item::as_integer)
        .and_then(|value| u8::try_from(value).ok())
        .and_then(|percent| {
            joyshockrust_config::profile::trigger_travel_limit(Some(percent))
                .ok()
                .flatten()
        })
        .unwrap_or(100);
    (percent, relative)
}

fn add_travel_limit(
    group: &adw::PreferencesGroup,
    ctx: &Context,
    side: &'static str,
    mode: OutputMode,
    full: u8,
    chart: trigger_travel::Chart,
) {
    let row = spin("Trigger travel limit (%)", 1, 100, full);
    row.set_subtitle(if mode == OutputMode::Gamepad {
        "Physical travel used for maximum analog output and the stage percentages."
    } else {
        "Physical travel used for the stage percentages."
    });
    let edit = ctx.clone();
    row.connect_value_notify(move |row| {
        let limit = row.value() as u8;
        if edit.edit(|doc| {
            let settings = settings_mut(doc, side)?;
            settings.remove("full_travel_percent");
            if limit == 100 {
                settings.remove("travel_limit_percent");
            } else {
                settings.insert("travel_limit_percent", value(i64::from(limit)));
            }
            Ok(())
        }) {
            chart.set_range((limit, true));
        }
    });
    group.add(&row);
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

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::{gtk, prelude::*};
use toml_edit::{DocumentMut, Item, Table, Value, value};

use crate::{
    bindings::replace_value, controller, sensitivity::gyro_table_mut, touchpad, ui::Context,
};

const MODES: [(&str, &str); 7] = [
    ("hold", "Enable while held"),
    ("disable-while-held", "Disable while held"),
    ("toggle", "Toggle on press"),
    ("tap-hold", "Toggle on tap, enable while held"),
    ("double-tap", "Toggle on double tap"),
    ("chord", "Enable while both inputs are held"),
    ("trigger", "Enable while analog trigger is pulled"),
];
const ADVANCED: &str = "Edit this gyro binding in Advanced TOML.";
type InputRows = RefCell<Vec<gtk::Widget>>;

pub fn binding(doc: &DocumentMut) -> Result<Option<(&str, &str)>, &'static str> {
    let Some(gyro) = doc.get("gyro") else {
        return Ok(None);
    };
    let gyro = gyro.as_table_like().ok_or(ADVANCED)?;
    let Some(binding) = gyro.get("binding") else {
        return Ok(None);
    };
    let binding = binding.as_table_like().ok_or(ADVANCED)?;
    let mode = binding.get("mode").and_then(Item::as_str).ok_or(ADVANCED)?;
    let input = binding
        .get("input")
        .and_then(Item::as_str)
        .ok_or(ADVANCED)?;
    if binding.len() != 2 || !MODES.iter().any(|&(key, _)| key == mode) {
        return Err(ADVANCED);
    }
    Ok(Some((mode, input)))
}

pub fn set_binding(doc: &mut DocumentMut, mode: &str, input: &str) -> Result<(), &'static str> {
    binding(doc)?;
    for source in input
        .split(',')
        .filter(|source| source.starts_with("touch:"))
    {
        touchpad::prepare_gyro_input(doc, source)?;
    }
    let binding = gyro_table_mut(doc)?
        .entry("binding")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or(ADVANCED)?;
    for (key, text) in [("mode", mode), ("input", input)] {
        if let Some(previous) = binding.get_mut(key).and_then(Item::as_value_mut) {
            if previous.as_str() != Some(text) {
                replace_value(previous, Value::from(text));
            }
        } else {
            binding.insert(key, value(text));
        }
    }
    Ok(())
}

pub fn clear_binding(doc: &mut DocumentMut) -> Result<(), &'static str> {
    binding(doc)?;
    gyro_table_mut(doc)?.remove("binding");
    Ok(())
}

pub fn group(ctx: &Context, doc: &DocumentMut) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Gyro activation")
        .description("Choose when gyro aiming is active.")
        .build();
    let current = match binding(doc) {
        Ok(value) => value,
        Err(error) => {
            group.add(&controller::message_row("Gyro binding", error));
            return group;
        }
    };
    let rows = Rc::new(InputRows::default());
    let group_ref = group.downgrade();
    let editor = ctx.clone();
    let unconfigured = !doc.contains_key("gyro");
    let choices = unconfigured
        .then_some(("always", "Always active"))
        .into_iter()
        .chain(MODES);
    group.add(&controller::dropdown(
        "Activation mode",
        current.map(|(mode, _)| mode),
        if unconfigured {
            "Choose activation"
        } else {
            "Always active"
        },
        choices.map(|(key, label)| (key.into(), label.into())),
        {
            let rows = rows.clone();
            move |mode| {
                if editor.edit(|doc| select_mode(doc, mode))
                    && let Some(group) = group_ref.upgrade()
                {
                    refresh_inputs(&group, &editor, &rows);
                }
            }
        },
    ));
    refresh_inputs(&group, ctx, &rows);
    group
}

fn select_mode(doc: &mut DocumentMut, mode: Option<&str>) -> Result<(), &'static str> {
    let Some(mode) = mode.filter(|&mode| mode != "always") else {
        if mode.is_some() || doc.contains_key("gyro") {
            clear_binding(doc)?;
        }
        return Ok(());
    };
    let current = binding(doc)?;
    let input = match current {
        Some((old, _)) if old == mode => return Ok(()),
        Some(("chord", sources)) => sources
            .split_once(',')
            .map(|(first, _)| first)
            .unwrap_or("l1"),
        Some(("trigger", spec)) => spec.split(':').next().unwrap_or("l2"),
        Some((_, input)) => input,
        None => "l1",
    };
    let input = match mode {
        "chord" => format!("{input},{}", if input == "r1" { "l1" } else { "r1" }),
        "trigger" if !matches!(input, "l2" | "r2") => "l2".to_owned(),
        _ => input.to_owned(),
    };
    set_binding(doc, mode, &input)
}

fn refresh_inputs(group: &adw::PreferencesGroup, ctx: &Context, rows: &Rc<InputRows>) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let Ok(doc) = ctx.document() else { return };
    let Ok(Some((mode, input))) = binding(&doc) else {
        return;
    };
    let mut widgets = rows.borrow_mut();
    if mode == "trigger" {
        widgets.extend(trigger_rows(ctx, input));
    } else {
        let choices: Vec<_> = controller::physical_buttons()
            .chain(touchpad::gyro_inputs(&doc))
            .collect();
        if mode == "chord" {
            let Some((first, second)) = input.split_once(',') else {
                widgets.push(controller::message_row("Gyro binding", ADVANCED).upcast());
                for widget in widgets.iter() {
                    group.add(widget);
                }
                return;
            };
            for (index, (selected, other)) in
                [(first, second), (second, first)].into_iter().enumerate()
            {
                let editor = ctx.clone();
                let group = group.downgrade();
                let rows = Rc::downgrade(rows);
                let other = other.to_owned();
                widgets.push(
                    controller::required_dropdown(
                        if index == 0 {
                            "First input"
                        } else {
                            "Second input"
                        },
                        selected,
                        choices.iter().filter(|(key, _)| *key != other).cloned(),
                        {
                            let other = other.clone();
                            move |selected| {
                                let input = if index == 0 {
                                    format!("{selected},{other}")
                                } else {
                                    format!("{other},{selected}")
                                };
                                if editor.edit(|doc| set_binding(doc, "chord", &input))
                                    && let (Some(group), Some(rows)) =
                                        (group.upgrade(), rows.upgrade())
                                {
                                    refresh_inputs(&group, &editor, &rows);
                                }
                            }
                        },
                    )
                    .upcast(),
                );
            }
        } else {
            let editor = ctx.clone();
            let mode = mode.to_owned();
            widgets.push(
                controller::required_dropdown("Input", input, choices.into_iter(), move |input| {
                    editor.edit(|doc| set_binding(doc, &mode, input));
                })
                .upcast(),
            );
        }
    }
    for widget in widgets.iter() {
        group.add(widget);
    }
}

fn trigger_parts(spec: &str) -> Option<(&str, f64, f64)> {
    let (side, press, release) = joyshockrust_core::trigger::parse_analog_trigger(spec).ok()?;
    Some((side.name(), press, release))
}

fn trigger_rows(ctx: &Context, input: &str) -> Vec<gtk::Widget> {
    let Some((side, press, release)) = trigger_parts(input) else {
        return vec![controller::message_row("Analog trigger", ADVANCED).upcast()];
    };
    let editor = ctx.clone();
    let trigger = controller::required_dropdown(
        "Trigger",
        side,
        [("l2", "L2 · Left trigger"), ("r2", "R2 · Right trigger")]
            .into_iter()
            .map(|(key, text)| (key.into(), text.into())),
        move |selected| {
            editor.edit(|doc| {
                let (_, input) = binding(doc)?.ok_or(ADVANCED)?;
                let suffix = input
                    .split_once(':')
                    .map_or("", |(side, _)| &input[side.len()..]);
                let spec = format!("{selected}{suffix}");
                set_binding(doc, "trigger", &spec)
            });
        },
    );
    let mut rows = vec![trigger.upcast()];
    for (index, (title, value)) in [("Press threshold", press), ("Release threshold", release)]
        .into_iter()
        .enumerate()
    {
        let row = adw::SpinRow::builder()
            .title(title)
            .digits(3)
            .adjustment(&gtk::Adjustment::new(value, 0.0, 1.0, 0.01, 0.1, 0.0))
            .build();
        let ctx = ctx.clone();
        let reverting = Cell::new(false);
        row.connect_value_notify(move |row| {
            if reverting.get() {
                return;
            }
            let Ok(doc) = ctx.document() else { return };
            let Ok(Some((_, input))) = binding(&doc) else {
                return;
            };
            let Some((side, press, release)) = trigger_parts(input) else {
                return;
            };
            let previous = [press, release];
            let mut values = previous;
            values[index] = row.value();
            if values == previous {
                return;
            }
            let [press, release] = values;
            let spec = format!("{side}:{press}:{release}");
            let edited = ctx.edit(|doc| {
                if trigger_parts(&spec).is_none() {
                    return Err("Press threshold must be greater than release threshold.");
                }
                set_binding(doc, "trigger", &spec)
            });
            if !edited {
                reverting.set(true);
                row.set_value(previous[index]);
                reverting.set(false);
            }
        });
        rows.push(row.upcast());
    }
    rows
}

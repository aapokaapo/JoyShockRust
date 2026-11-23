use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::{gtk, prelude::*};
use joyshockrust_motion::aim_response::{
    DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, DEFAULT_FLICK_STEERING_SENSITIVITY, GyroResponse,
    MAX_FLICK_AXIAL_THRESHOLD_DEG, MAX_GYRO_ACCEL_MULTIPLIER, MAX_GYRO_RATE_DPS,
};

use crate::{
    bindings::replace_value,
    controller,
    ui::{self, Context},
};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

const GYRO_FIELDS: [&str; 2] = ["sensitivity_x", "sensitivity_y"];
const GYRO_SLIDER_MAX: f64 = 20.0;
const GYRO_TEXT_MAX: f64 = 100.0;
pub const GYRO_SENSITIVITY_ERROR: &str = "Enter a gyro sensitivity between 0 and 100.";
const RESPONSE_FIELDS: [&str; 4] = [
    "threshold_dps",
    "accel_start_dps",
    "accel_end_dps",
    "accel_multiplier",
];
const FLICK_THRESHOLD: &str = "flick_axial_threshold_deg";
const FLICK_STEERING: &str = "flick_steering_sensitivity";
const CALIBRATION_FIELDS: [&str; 2] = ["real_world_calibration", "in_game_sens"];
const STICK_FIELDS: [&str; 2] = ["stick_curve_left", "stick_curve_right"];

#[derive(Default)]
pub struct CalibrationDraft {
    values: Option<[String; 2]>,
    pub error: Option<&'static str>,
}

impl CalibrationDraft {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn number(table: &dyn TableLike, key: &str, default: f64) -> f64 {
    table
        .get(key)
        .and_then(|item| {
            item.as_float()
                .or_else(|| item.as_integer().map(|n| n as f64))
        })
        .filter(|n| n.is_finite())
        .unwrap_or(default)
}

fn set_number(table: &mut dyn TableLike, key: &str, number: f64) {
    let mut value = Value::from(number);
    if let Some(old) = table.get(key).and_then(Item::as_value) {
        *value.decor_mut() = old.decor().clone();
    }
    table.insert(key, Item::Value(value));
}

fn calibration_values(drafts: &[String; 2]) -> Result<[f64; 2], &'static str> {
    let [rwc, sensitivity] = drafts.each_ref().map(|text| {
        text.trim()
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
    });
    let rwc = rwc.ok_or("Real-world calibration must be a positive number.")?;
    let sensitivity = sensitivity.ok_or("In-game sensitivity must be a positive number.")?;
    let ratio = rwc / sensitivity;
    if !(ratio > 0.0 && ratio <= 10_000.0) {
        return Err(
            "RWC divided by in-game sensitivity must be greater than 0 and at most 10,000.",
        );
    }
    Ok([rwc, sensitivity])
}

pub fn gyro_page(ctx: &Context) -> gtk::Widget {
    let Ok(doc) = ctx.document() else {
        return ui::advanced_required("Fix the profile syntax in Advanced TOML to edit gyro aim.");
    };
    let Some(gyro) = doc
        .get("gyro")
        .map_or(Some(None), |item| item.as_table_like().map(Some))
    else {
        return ui::advanced_required("Edit this gyro configuration in Advanced TOML.");
    };
    let page = adw::PreferencesPage::new();
    page.add(&gyro_sensitivity_group(ctx, gyro));
    page.add(&gyro_response_group(ctx, gyro));
    page.add(&flick_threshold_group(ctx, gyro));
    page.add(&calibration_shortcut(ctx, gyro));

    let calibration = adw::PreferencesGroup::builder()
        .title("Game calibration")
        .build();
    let saved = CALIBRATION_FIELDS.map(|key| {
        gyro.map_or(1.0, |table| number(table, key, 1.0))
            .to_string()
    });
    let draft = ctx.editor.borrow();
    let values = draft
        .calibration
        .values
        .clone()
        .unwrap_or_else(|| saved.clone());
    let error = draft.calibration.error;
    drop(draft);

    let entries = ["Real-world calibration (RWC)", "In-game sensitivity"].map(|title| {
        let entry = adw::EntryRow::builder()
            .title(title)
            .input_purpose(gtk::InputPurpose::Number)
            .build();
        calibration.add(&entry);
        entry
    });
    for (entry, value) in entries.iter().zip(&values) {
        entry.set_text(value);
    }
    let warning = gtk::Label::builder().wrap(true).xalign(0.0).build();
    warning.add_css_class("error");
    warning.set_margin_top(6);
    let reset = gtk::Button::with_label("Reset Fields");
    reset.set_halign(gtk::Align::Start);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    footer.append(&warning);
    footer.append(&reset);
    calibration.add(&footer);
    display_error(error, &warning, &reset);

    let resetting = Rc::new(Cell::new(false));
    let draft_values = Rc::new(std::cell::RefCell::new(values));
    for (index, entry) in entries.iter().enumerate() {
        let ctx = ctx.clone();
        let warning = warning.downgrade();
        let reset = reset.downgrade();
        let resetting = resetting.clone();
        let draft_values = draft_values.clone();
        entry.connect_changed(move |entry| {
            if resetting.get() {
                return;
            }
            let mut drafts = draft_values.borrow_mut();
            drafts[index] = entry.text().into();
            let result = calibration_values(&drafts);
            ctx.editor.borrow_mut().calibration = CalibrationDraft {
                values: Some(drafts.clone()),
                error: result.err(),
            };
            if let Ok(values) = result {
                ctx.edit(|doc| set_gyro_numbers(doc, CALIBRATION_FIELDS.into_iter().zip(values)));
            }
            if let (Some(warning), Some(reset)) = (warning.upgrade(), reset.upgrade()) {
                display_error(result.err(), &warning, &reset);
            }
        });
    }
    let ctx = ctx.clone();
    let entries = entries.each_ref().map(|entry| entry.downgrade());
    let warning = warning.downgrade();
    reset.connect_clicked(move |reset| {
        let Ok(doc) = ctx.document() else { return };
        let gyro = doc.get("gyro").and_then(Item::as_table_like);
        let values = CALIBRATION_FIELDS.map(|key| {
            gyro.map_or(1.0, |table| number(table, key, 1.0))
                .to_string()
        });
        resetting.set(true);
        for (entry, value) in entries.iter().zip(&values) {
            if let Some(entry) = entry.upgrade() {
                entry.set_text(value);
            }
        }
        resetting.set(false);
        *draft_values.borrow_mut() = values;
        ctx.editor.borrow_mut().calibration.reset();
        if let Some(warning) = warning.upgrade() {
            display_error(None, &warning, reset);
        }
    });
    page.add(&calibration);
    page.upcast()
}

fn gyro_response_values(gyro: Option<&dyn TableLike>) -> [f64; 4] {
    let defaults = GyroResponse::default().values();
    std::array::from_fn(|i| {
        gyro.map_or(defaults[i], |table| {
            number(table, RESPONSE_FIELDS[i], defaults[i])
        })
    })
}

fn setting_row(title: &str, subtitle: &str, value: f64, lower: f64, upper: f64) -> adw::SpinRow {
    adw::SpinRow::builder()
        .title(title)
        .subtitle(subtitle)
        .adjustment(&gtk::Adjustment::new(value, lower, upper, 0.1, 1.0, 0.0))
        .digits(2)
        .build()
}

fn gyro_response_group(ctx: &Context, gyro: Option<&dyn TableLike>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Gyro response")
        .build();
    let values = gyro_response_values(gyro);
    for (axis, (title, subtitle, lower, upper)) in [
        (
            "Minimum speed",
            "Suppresses aim below this speed in °/s.",
            0.0,
            MAX_GYRO_RATE_DPS,
        ),
        (
            "Acceleration start",
            "Speed in °/s where the boost begins.",
            0.0,
            MAX_GYRO_RATE_DPS,
        ),
        (
            "Acceleration end",
            "Speed in °/s where the full boost applies.",
            0.0,
            MAX_GYRO_RATE_DPS,
        ),
        (
            "Acceleration multiplier",
            "Maximum boost to both axes. 1 disables acceleration.",
            1.0,
            MAX_GYRO_ACCEL_MULTIPLIER,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let row = setting_row(title, subtitle, values[axis], lower, upper);
        group.add(&row);
        let ctx = ctx.clone();
        let reverting = Cell::new(false);
        row.connect_value_notify(move |row| {
            if reverting.get() {
                return;
            }
            let Ok(doc) = ctx.document() else { return };
            let mut values = gyro_response_values(doc.get("gyro").and_then(Item::as_table_like));
            let old = values[axis];
            values[axis] = row.value();
            let [minimum, start, end, multiplier] = values;
            if let Err(error) = GyroResponse::new(minimum, start, end, multiplier) {
                reverting.set(true);
                row.set_value(old);
                reverting.set(false);
                ctx.report(Err(error.into()));
                return;
            }
            ctx.edit(|doc| set_gyro_numbers(doc, [(RESPONSE_FIELDS[axis], values[axis])]));
        });
    }
    group
}

fn flick_threshold_group(ctx: &Context, gyro: Option<&dyn TableLike>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Flick Stick")
        .description("Shaded sectors show where the initial flick snaps to a cardinal direction.")
        .build();
    let value = gyro.map_or(DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, |table| {
        number(table, FLICK_THRESHOLD, DEFAULT_FLICK_AXIAL_THRESHOLD_DEG)
    });
    let row = setting_row(
        "Axial threshold",
        "Snap the initial flick toward a cardinal direction. 0 disables snapping.",
        value,
        0.0,
        MAX_FLICK_AXIAL_THRESHOLD_DEG,
    );
    let steering_value = gyro.map_or(DEFAULT_FLICK_STEERING_SENSITIVITY, |table| {
        number(table, FLICK_STEERING, DEFAULT_FLICK_STEERING_SENSITIVITY)
    });
    let steering = setting_row(
        "Steering sensitivity",
        "Multiplier for turns while the stick is held. 0 disables steering. 1 uses full sensitivity.",
        steering_value,
        0.0,
        1.0,
    );
    steering.adjustment().set_step_increment(0.05);
    for (key, control) in [(FLICK_THRESHOLD, &row), (FLICK_STEERING, &steering)] {
        let ctx = ctx.clone();
        control.connect_value_notify(move |row| {
            ctx.edit(|doc| set_gyro_numbers(doc, [(key, row.value())]));
        });
        group.add(control);
    }
    crate::flick_preview::add(&group, &row.adjustment(), &steering.adjustment());
    group
}

fn gyro_values(gyro: Option<&dyn TableLike>) -> [f64; 2] {
    GYRO_FIELDS.map(|key| gyro.map_or(4.0, |table| number(table, key, 4.0)))
}

fn gyro_sensitivity_group(ctx: &Context, gyro: Option<&dyn TableLike>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Gyro sensitivity")
        .description("Counts per degree. Slide from 0 to 20 or enter a value from 0 to 100.")
        .build();
    let [horizontal, vertical] = gyro_values(gyro);
    let mut editor = ctx.editor.borrow_mut();
    editor.link_gyro_sensitivity &= horizontal == vertical;
    let toggle = adw::SwitchRow::builder()
        .title("Link horizontal and vertical")
        .subtitle("Uses the horizontal value for both axes.")
        .active(editor.link_gyro_sensitivity)
        .sensitive(editor.invalid_gyro_sensitivity.iter().all(Option::is_none))
        .build();
    drop(editor);
    group.add(&toggle);

    let rows = Rc::new(RefCell::new(Vec::new()));
    update_gyro_rows(
        ctx,
        &group,
        &toggle,
        &mut rows.borrow_mut(),
        [horizontal, vertical],
    );
    let group_ref = group.downgrade();
    let ctx = ctx.clone();
    toggle.connect_active_notify(move |toggle| {
        let Some(group) = group_ref.upgrade() else {
            return;
        };
        let Ok(doc) = ctx.document() else { return };
        let mut values = gyro_values(doc.get("gyro").and_then(Item::as_table_like));
        if toggle.is_active() && values[0] != values[1] {
            values[1] = values[0];
            if !ctx.edit(|doc| set_gyro_numbers(doc, [(GYRO_FIELDS[1], values[0])])) {
                toggle.set_active(false);
                return;
            }
        }
        ctx.editor.borrow_mut().link_gyro_sensitivity = toggle.is_active();
        update_gyro_rows(&ctx, &group, toggle, &mut rows.borrow_mut(), values);
    });
    group
}

fn update_gyro_rows(
    ctx: &Context,
    group: &adw::PreferencesGroup,
    toggle: &adw::SwitchRow,
    rows: &mut Vec<adw::ActionRow>,
    [horizontal, vertical]: [f64; 2],
) {
    for row in rows.drain(..) {
        group.remove(&row);
    }
    let fields: &[(&[usize], &str, f64)] = if toggle.is_active() {
        &[(&[0, 1], "Both axes", horizontal)]
    } else {
        &[
            (&[0], "Horizontal", horizontal),
            (&[1], "Vertical", vertical),
        ]
    };
    for &(axes, label, value) in fields {
        let row = gyro_sensitivity_row(ctx, toggle, axes, label, value);
        group.add(&row);
        rows.push(row);
    }
}

fn parse_gyro_sensitivity(text: &str) -> Option<f64> {
    text.trim()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|value| (0.0..=GYRO_TEXT_MAX).contains(value))
}

fn gyro_sensitivity_row(
    ctx: &Context,
    toggle: &adw::SwitchRow,
    axes: &'static [usize],
    label: &str,
    initial: f64,
) -> adw::ActionRow {
    let scale = gtk::Scale::builder()
        .adjustment(&gtk::Adjustment::new(
            initial.clamp(0.0, GYRO_SLIDER_MAX),
            0.0,
            GYRO_SLIDER_MAX,
            0.1,
            1.0,
            0.0,
        ))
        .draw_value(false)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    scale.set_size_request(132, -1);
    let axis_name = format!("{label} sensitivity");
    scale.update_property(&[gtk::accessible::Property::Label(&axis_name)]);

    let entry = gtk::Entry::builder()
        .width_chars(7)
        .max_width_chars(7)
        .input_purpose(gtk::InputPurpose::Number)
        .valign(gtk::Align::Center)
        .build();
    entry.update_property(&[
        gtk::accessible::Property::Label(&axis_name),
        gtk::accessible::Property::Description("Accepts values from 0 to 100."),
    ]);
    let invalid = ctx.editor.borrow().invalid_gyro_sensitivity[axes[0]].clone();
    if let Some(text) = &invalid {
        entry.set_text(text);
    } else {
        entry.set_text(&initial.to_string());
    }

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    // The suffix stays at its natural width regardless of the row title length.
    controls.set_hexpand(false);
    controls.append(&scale);
    controls.append(&entry);
    let row = adw::ActionRow::builder().title(label).build();
    row.add_suffix(&controls);
    set_gyro_sensitivity_error(&row, &entry, invalid.is_some());

    let syncing = Rc::new(Cell::new(false));
    let updating_entry = entry.downgrade();
    let updating_scale = syncing.clone();
    scale.connect_value_changed(move |scale| {
        if updating_scale.get() {
            return;
        }
        let snapped = (scale.value() * 10.0).round() / 10.0;
        updating_scale.set(true);
        scale.set_value(snapped);
        updating_scale.set(false);
        if let Some(entry) = updating_entry.upgrade() {
            entry.set_text(&format!("{snapped:.1}"));
        }
    });

    let ctx = ctx.clone();
    let updating_row = row.downgrade();
    let scale = scale.downgrade();
    let toggle = toggle.downgrade();
    entry.connect_changed(move |entry| {
        let text = entry.text();
        let value = parse_gyro_sensitivity(&text);
        let mut editor = ctx.editor.borrow_mut();
        for &axis in axes {
            editor.invalid_gyro_sensitivity[axis] = value.is_none().then(|| text.to_string());
        }
        let valid = editor.invalid_gyro_sensitivity.iter().all(Option::is_none);
        drop(editor);
        if let Some(toggle) = toggle.upgrade() {
            toggle.set_sensitive(valid);
        }
        if let Some(row) = updating_row.upgrade() {
            set_gyro_sensitivity_error(&row, entry, value.is_none());
        }
        if let Some(value) = value {
            ctx.edit(|doc| {
                set_gyro_numbers(doc, axes.iter().map(|&axis| (GYRO_FIELDS[axis], value)))
            });
            if let Some(scale) = scale.upgrade() {
                syncing.set(true);
                scale.set_value(value.min(GYRO_SLIDER_MAX));
                syncing.set(false);
            }
        }
    });
    row
}

fn set_gyro_sensitivity_error(row: &adw::ActionRow, entry: &gtk::Entry, invalid: bool) {
    let state = if invalid {
        gtk::AccessibleInvalidState::True
    } else {
        gtk::AccessibleInvalidState::False
    };
    entry.update_state(&[gtk::accessible::State::Invalid(state)]);
    if invalid {
        row.set_subtitle(GYRO_SENSITIVITY_ERROR);
        entry.add_css_class("error");
    } else {
        row.set_subtitle("");
        entry.remove_css_class("error");
    }
}

fn calibration_shortcut(ctx: &Context, gyro: Option<&dyn TableLike>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Controller gyro calibration")
        .description("Hold the selected button, then release it and leave the controller still.")
        .build();
    let shortcut = gyro.and_then(|table| table.get("calibration_hold"));
    let current = match shortcut.map(Item::as_table_like) {
        Some(Some(table)) => table.get("button").and_then(Item::as_str),
        None => None,
        Some(None) => {
            group.add(&controller::message_row(
                "Calibration shortcut",
                "Edit this shortcut in Advanced TOML.",
            ));
            return group;
        }
    };
    let ctx = ctx.clone();
    group.add(&controller::dropdown(
        "Hold button",
        current,
        "Disabled",
        controller::physical_buttons(),
        move |button| {
            ctx.edit(|doc| {
                let gyro = doc
                    .entry("gyro")
                    .or_insert_with(|| Item::Table(Table::new()))
                    .as_table_like_mut()
                    .ok_or("Edit this gyro configuration in Advanced TOML.")?;
                if let Some(button) = button {
                    let hold = gyro
                        .entry("calibration_hold")
                        .or_insert_with(|| {
                            Item::Value(Value::InlineTable(toml_edit::InlineTable::new()))
                        })
                        .as_table_like_mut()
                        .ok_or("Edit this shortcut in Advanced TOML.")?;
                    if let Some(value) = hold.get_mut("button").and_then(Item::as_value_mut) {
                        replace_value(value, Value::from(button));
                    } else {
                        hold.insert("button", toml_edit::value(button));
                    }
                } else {
                    gyro.remove("calibration_hold");
                }
                Ok(())
            });
        },
    ));
    group
}

fn display_error(error: Option<&str>, warning: &gtk::Label, reset: &gtk::Button) {
    warning.set_text(error.unwrap_or_default());
    warning.set_visible(error.is_some());
    reset.set_visible(error.is_some());
}

fn set_gyro_numbers(
    doc: &mut DocumentMut,
    changes: impl IntoIterator<Item = (&'static str, f64)>,
) -> Result<(), &'static str> {
    let gyro = doc
        .entry("gyro")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("Edit this gyro configuration in Advanced TOML.")?;
    for (key, value) in changes {
        set_number(gyro, key, value);
    }
    Ok(())
}

pub fn sticks_page(ctx: &Context) -> gtk::Widget {
    let Ok(doc) = ctx.document() else {
        return ui::advanced_required(
            "Fix the profile syntax in Advanced TOML to edit stick response.",
        );
    };
    let page = adw::PreferencesPage::new();
    let fallback = number(doc.as_table(), "stick_curve", 1.0);
    for ((key, title), side) in STICK_FIELDS
        .into_iter()
        .zip(["Left stick", "Right stick"])
        .zip(["left", "right"])
    {
        let group = adw::PreferencesGroup::builder()
            .title(title)
            .description("Lower exponents give more response near the center. 1.00 is linear.")
            .build();
        let initial = number(doc.as_table(), key, fallback);
        let plot = gtk::DrawingArea::builder()
            .content_height(122)
            .hexpand(true)
            .build();
        plot.set_margin_top(12);
        plot.set_margin_bottom(12);
        plot.set_margin_start(12);
        plot.set_margin_end(12);
        let exponent = Rc::new(Cell::new(initial));
        let plot_state = exponent.clone();
        plot.set_draw_func(move |area, cr, width, height| {
            draw_curve(area, cr, width, height, plot_state.get())
        });
        let plot_ref = plot.downgrade();
        let curve_ctx = ctx.clone();
        let row = scale_row("Response exponent", initial, 0.25, 4.0, 20, move |value| {
            exponent.set(value);
            if let Some(plot) = plot_ref.upgrade() {
                plot.queue_draw();
            }
            curve_ctx.edit(|doc| {
                set_number(doc.as_table_mut(), key, value);
                Ok(())
            });
        });
        group.add(&plot);
        group.add(&row);
        group.add(&crate::stick_keys::editor_row(ctx, side, title));
        page.add(&group);
    }
    page.upcast()
}

fn scale_row(
    label: &str,
    initial: f64,
    min: f64,
    max: f64,
    steps_per_unit: i32,
    changed: impl Fn(f64) + 'static,
) -> adw::ActionRow {
    let units = f64::from(steps_per_unit);
    let step = units.recip();
    let digits = if steps_per_unit == 10 { 1 } else { 2 };
    let adjustment = gtk::Adjustment::new(initial, min, max, step, 10.0 * step, 0.0);
    let scale = gtk::Scale::builder()
        .adjustment(&adjustment)
        .digits(digits)
        .draw_value(true)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    scale.set_size_request(150, -1);
    scale.update_property(&[gtk::accessible::Property::Label(label)]);
    scale.connect_value_changed(move |scale| {
        let value = scale.value();
        let snapped = ((value * units).round() / units).clamp(min, max);
        if (value - snapped).abs() > 1e-9 {
            scale.set_value(snapped);
        } else {
            changed(snapped);
        }
    });
    let row = adw::ActionRow::builder().title(label).build();
    row.add_suffix(&scale);
    row
}

fn draw_curve(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    exponent: f64,
) {
    let width = f64::from(width) - 2.0;
    let height = f64::from(height) - 2.0;
    let at = |x: f64, y: f64| (1.0 + x * width, 1.0 + (1.0 - y) * height);
    let line = |from, to| {
        let (x, y) = from;
        cr.move_to(x, y);
        let (x, y) = to;
        cr.line_to(x, y);
    };
    let foreground = area.style_context().color();
    let rgb = (
        foreground.red().into(),
        foreground.green().into(),
        foreground.blue().into(),
    );
    cr.set_source_rgba(rgb.0, rgb.1, rgb.2, 0.16);
    cr.set_line_width(1.0);
    for tick in 0..=4 {
        let fraction = f64::from(tick) / 4.0;
        line(at(fraction, 0.0), at(fraction, 1.0));
        line(at(0.0, fraction), at(1.0, fraction));
    }
    let _ = cr.stroke();
    cr.set_source_rgba(rgb.0, rgb.1, rgb.2, 0.45);
    line(at(0.0, 0.0), at(1.0, 1.0));
    let _ = cr.stroke();
    let accent = area
        .style_context()
        .lookup_color("accent_color")
        .unwrap_or(foreground);
    cr.set_source_rgb(
        accent.red().into(),
        accent.green().into(),
        accent.blue().into(),
    );
    cr.set_line_width(2.5);
    let (x, y) = at(0.0, 0.0);
    cr.move_to(x, y);
    for index in 1..=64 {
        let x = f64::from(index) / 64.0;
        let (x, y) = at(x, x.powf(exponent.clamp(0.25, 4.0)));
        cr.line_to(x, y);
    }
    let _ = cr.stroke();
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_uses_positive_finite_values_and_accepts_decimal_comma() {
        assert_eq!(
            calibration_values(&["44,34375".into(), "2.5".into()]),
            Ok([44.34375, 2.5])
        );
        for values in [
            ["", "2"],
            ["1", "0"],
            ["NaN", "1"],
            ["10001", "1"],
            ["1e-300", "1e300"],
        ] {
            assert!(calibration_values(&values.map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn changing_numbers_preserves_comments_and_shared_fallback() {
        let mut doc = "version = 1\nstick_curve = 1.5\n[gyro]\nsensitivity_x = 4 # horizontal\n"
            .parse::<DocumentMut>()
            .unwrap();
        set_number(doc.as_table_mut(), "stick_curve_left", 0.8);
        set_number(doc["gyro"].as_table_mut().unwrap(), "sensitivity_x", 4.5);
        assert_eq!(number(doc.as_table(), "stick_curve", 1.0), 1.5);
        assert!(doc.to_string().contains("sensitivity_x = 4.5 # horizontal"));
        assert!(!doc.to_string().contains("stick_curve_right"));
    }
}

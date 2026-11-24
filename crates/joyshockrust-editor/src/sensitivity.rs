use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use adw::{gtk, prelude::*};
use joyshockrust_core::StickSide;
use joyshockrust_motion::AimMode;
use joyshockrust_motion::aim_response::{
    DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, DEFAULT_FLICK_DURATION_MS, DEFAULT_FLICK_ENGAGE_PERCENT,
    DEFAULT_FLICK_STEERING_SENSITIVITY, GyroResponse, MAX_FLICK_AXIAL_THRESHOLD_DEG,
    MAX_FLICK_DURATION_MS, MAX_GYRO_ACCEL_MULTIPLIER, MAX_GYRO_RATE_DPS, MIN_FLICK_DURATION_MS,
    MIN_FLICK_ENGAGE_PERCENT, snap_flick_angle,
};

use crate::{
    bindings::replace_value,
    controller, controller_input,
    response_plot::{self, Curve},
    ui::{self, Context},
};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

const GYRO_FIELDS: [&str; 2] = ["sensitivity_x", "sensitivity_y"];
const GYRO_SLIDER_MAX: f64 = 20.0;
const GYRO_TEXT_MAX: f64 = 100.0;
pub const GYRO_SENSITIVITY_ERROR: &str = "Enter a gyro sensitivity between 0 and 100.";
const ACCEL_ENABLED: &str = "accel_enabled";
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
    page.add(&aim_mode_group(ctx, gyro));
    page.add(&crate::imu_monitor::group());
    page.add(&crate::gyro_activation::group(ctx, &doc));
    page.add(&gyro_sensitivity_group(ctx, gyro));
    page.add(&gyro_response_group(ctx, gyro));
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

fn aim_mode_group(ctx: &Context, gyro: Option<&dyn TableLike>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Aim input")
        .description(
            "Gyro aim uses the controller axes. Sensor fusion aim uses gravity as its reference.",
        )
        .build();
    let current = gyro
        .and_then(|table| table.get("aim_mode"))
        .and_then(Item::as_str)
        .unwrap_or(AimMode::default().as_str());
    let ctx = ctx.clone();
    let row = controller::required_dropdown(
        "Aim mode",
        current,
        [
            (AimMode::Gyro, "Gyro aim"),
            (AimMode::SensorFusion, "Sensor fusion aim"),
        ]
        .into_iter()
        .map(|(mode, name)| (mode.as_str().into(), name.into())),
        move |mode| {
            ctx.edit(|doc| {
                let gyro = gyro_table_mut(doc)?;
                let value = Value::from(mode);
                if let Some(previous) = gyro.get_mut("aim_mode").and_then(Item::as_value_mut) {
                    replace_value(previous, value);
                } else {
                    gyro.insert("aim_mode", Item::Value(value));
                }
                Ok(())
            });
        },
    );
    group.add(&row);
    group
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
    let acceleration = adw::SwitchRow::builder()
        .title("Gyro acceleration")
        .active(
            gyro.and_then(|table| table.get(ACCEL_ENABLED))
                .and_then(Item::as_bool)
                .unwrap_or(values[3] > 1.0),
        )
        .build();
    let toggle_ctx = ctx.clone();
    acceleration.connect_active_notify(move |row| {
        toggle_ctx.edit(|doc| {
            let table = gyro_table_mut(doc)?;
            let value = Value::from(row.is_active());
            if let Some(previous) = table.get_mut(ACCEL_ENABLED).and_then(Item::as_value_mut) {
                replace_value(previous, value);
            } else {
                table.insert(ACCEL_ENABLED, Item::Value(value));
            }
            Ok(())
        });
    });
    let mut adjustments = Vec::new();
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
            "Maximum boost to both axes.",
            1.0,
            MAX_GYRO_ACCEL_MULTIPLIER,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let row = setting_row(title, subtitle, values[axis], lower, upper);
        if axis == 1 {
            group.add(&acceleration);
        }
        if axis != 0 {
            acceleration
                .bind_property("active", &row, "sensitive")
                .sync_create()
                .build();
        }
        group.add(&row);
        adjustments.push(row.adjustment());
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
    let live_speed = Rc::new(Cell::new(None));
    let plot = response_plot::add(
        &group,
        "Gyro speed response",
        "Equivalent output speed (°/s)",
        "Projected aim speed (°/s)",
        true,
        {
            let live_speed = live_speed.clone();
            let [minimum, start, end, multiplier]: [gtk::Adjustment; 4] = adjustments
                .clone()
                .try_into()
                .expect("four response controls");
            let acceleration = acceleration.downgrade();
            move || {
                let response = GyroResponse::new(
                    minimum.value(),
                    start.value(),
                    end.value(),
                    multiplier.value(),
                )
                .unwrap_or_default()
                .with_acceleration_enabled(
                    acceleration.upgrade().is_some_and(|row| row.is_active()),
                );
                gyro_curve(response, live_speed.get())
            }
        },
    );
    let plot_ref = plot.downgrade();
    controller_input::watch_status(
        &group,
        "gyro_speed_dps=",
        Duration::from_millis(33),
        move |result| {
            let speed = result.ok().flatten().as_deref().and_then(response_input);
            if live_speed.replace(speed) != speed
                && let Some(plot) = plot_ref.upgrade()
            {
                plot.queue_draw();
            }
        },
    );
    watch_plot(&plot, &adjustments);
    let plot = plot.downgrade();
    acceleration.connect_active_notify(move |_| {
        if let Some(plot) = plot.upgrade() {
            plot.queue_draw();
        }
    });
    group
}

fn watch_plot(plot: &gtk::DrawingArea, settings: &[gtk::Adjustment]) {
    for setting in settings {
        let plot = plot.downgrade();
        setting.connect_value_changed(move |_| {
            if let Some(plot) = plot.upgrade() {
                plot.queue_draw();
            }
        });
    }
}

fn response_input(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn gyro_curve(response: GyroResponse, live_speed: Option<f64>) -> Curve {
    let [minimum, start, end, _] = response.values();
    let input_max = ((minimum.max(end) * 1.2)
        .max(live_speed.unwrap_or_default())
        .max(40.0)
        / 40.0)
        .ceil()
        * 40.0;
    let mut speeds: Vec<_> = (0..=100)
        .map(|i| f64::from(i) / 100.0 * input_max)
        .collect();
    speeds.extend([minimum.next_down().max(0.0), minimum, start, end]);
    speeds.sort_unstable_by(f64::total_cmp);
    let output_max = ((input_max * response.gain_at_speed(input_max) / 40.0).ceil()) * 40.0;
    Curve {
        input_max,
        output_max,
        sample: live_speed.map(|speed| (speed, speed * response.gain_at_speed(speed))),
        points: speeds
            .into_iter()
            .map(|speed| (speed, speed * response.gain_at_speed(speed)))
            .collect(),
    }
}

fn flick_group(ctx: &Context, gyro: Option<&dyn TableLike>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Flick Stick")
        .build();
    let side = gyro
        .and_then(|table| table.get("flick_stick_side"))
        .and_then(Item::as_str);
    let enabled = gyro.is_some_and(|table| table.contains_key("flick_stick"));
    let selection = if enabled {
        side.unwrap_or("right")
    } else {
        "off"
    };
    let side = Rc::new(Cell::new(if selection == "left" {
        StickSide::Left
    } else {
        StickSide::Right
    }));
    let scale = setting_row(
        "Turn sensitivity",
        "Base multiplier before game calibration. 1 gives the calibrated turn.",
        gyro.map_or(1.0, |table| number(table, "flick_stick", 1.0)),
        0.01,
        100.0,
    );
    scale.adjustment().set_step_increment(0.05);
    let threshold = setting_row(
        "Axial threshold",
        "Snap the initial turn toward a cardinal direction. 0 disables snapping.",
        gyro.map_or(DEFAULT_FLICK_AXIAL_THRESHOLD_DEG, |table| {
            number(table, FLICK_THRESHOLD, DEFAULT_FLICK_AXIAL_THRESHOLD_DEG)
        }),
        0.0,
        MAX_FLICK_AXIAL_THRESHOLD_DEG,
    );
    let steering = setting_row(
        "Steering sensitivity",
        "Multiplier for turns while the stick is held. 0 disables steering.",
        gyro.map_or(DEFAULT_FLICK_STEERING_SENSITIVITY, |table| {
            number(table, FLICK_STEERING, DEFAULT_FLICK_STEERING_SENSITIVITY)
        }),
        0.0,
        1.0,
    );
    steering.adjustment().set_step_increment(0.05);
    let engagement = setting_row(
        "Engagement (%)",
        "Stick travel needed to start a turn. Return near the center to start another.",
        gyro.map_or(DEFAULT_FLICK_ENGAGE_PERCENT, |table| {
            number(table, "flick_engage_percent", DEFAULT_FLICK_ENGAGE_PERCENT)
        }),
        MIN_FLICK_ENGAGE_PERCENT,
        100.0,
    );
    engagement.adjustment().set_step_increment(1.0);
    let duration = setting_row(
        "Flick duration (ms)",
        "Time it takes to complete the first turn.",
        gyro.map_or(DEFAULT_FLICK_DURATION_MS, |table| {
            number(table, "flick_duration_ms", DEFAULT_FLICK_DURATION_MS)
        }),
        MIN_FLICK_DURATION_MS,
        MAX_FLICK_DURATION_MS,
    );
    duration.adjustment().set_step_increment(5.0);
    let rows = [
        ("flick_stick", &scale),
        ("flick_engage_percent", &engagement),
        ("flick_duration_ms", &duration),
        (FLICK_THRESHOLD, &threshold),
        (FLICK_STEERING, &steering),
    ];
    let row = controller::required_dropdown(
        "Flick Stick input",
        selection,
        [
            ("off", "Off"),
            ("left", "Left stick"),
            ("right", "Right stick"),
        ]
        .into_iter()
        .map(|(key, label)| (key.into(), label.into())),
        {
            let ctx = ctx.clone();
            let side = side.clone();
            let scale = scale.adjustment();
            let controls = rows.map(|(_, row)| row.downgrade());
            move |selection| {
                let enabled = selection != "off";
                if ctx.edit(|doc| {
                    let gyro = gyro_table_mut(doc)?;
                    if enabled {
                        set_number(gyro, "flick_stick", scale.value());
                        let value = Value::from(selection);
                        if let Some(previous) = gyro
                            .get_mut("flick_stick_side")
                            .and_then(Item::as_value_mut)
                        {
                            replace_value(previous, value);
                        } else {
                            gyro.insert("flick_stick_side", Item::Value(value));
                        }
                    } else {
                        gyro.remove("flick_stick");
                    }
                    Ok(())
                }) {
                    if enabled {
                        side.set(if selection == "left" {
                            StickSide::Left
                        } else {
                            StickSide::Right
                        });
                    }
                    for row in controls.iter().filter_map(adw::glib::WeakRef::upgrade) {
                        row.set_sensitive(enabled);
                        row.set_visible(enabled);
                    }
                }
            }
        },
    );
    row.set_subtitle("The selected stick turns the camera and its virtual stick stays centered.");
    group.add(&row);
    for (key, row) in rows {
        row.set_sensitive(enabled);
        row.set_visible(enabled);
        let ctx = ctx.clone();
        row.connect_value_notify(move |row| {
            if row.is_sensitive() {
                ctx.edit(|doc| set_gyro_numbers(doc, [(key, row.value())]));
            }
        });
        group.add(row);
    }
    let preview = crate::flick_preview::add(
        &group,
        &threshold.adjustment(),
        &steering.adjustment(),
        &engagement.adjustment(),
        side,
    );
    row.bind_property("selected", &preview, "visible")
        .transform_to(|_, selected: u32| Some(matches!(selected, 1 | 2)))
        .sync_create()
        .build();
    for (title, setting, is_initial) in [
        (
            "Initial turn near a cardinal direction",
            threshold.adjustment(),
            true,
        ),
        ("Steering while held", steering.adjustment(), false),
    ] {
        let base = scale.adjustment();
        let plot = response_plot::add(
            &group,
            title,
            "Turn before game calibration (°)",
            "Stick angle change (°)",
            false,
            {
                let setting = setting.clone();
                let base = base.clone();
                move || {
                    let input_max = if is_initial { 45.0 } else { 90.0 };
                    let output_max = input_max * base.value().max(1.0);
                    Curve {
                        input_max,
                        output_max,
                        sample: None,
                        points: (0..=90)
                            .map(|i| {
                                let angle = f64::from(i) / 90.0 * input_max;
                                let turn = if is_initial {
                                    snap_flick_angle(
                                        angle.to_radians(),
                                        setting.value().to_radians(),
                                    )
                                    .to_degrees()
                                } else {
                                    angle * setting.value()
                                };
                                (angle, turn * base.value())
                            })
                            .collect(),
                    }
                }
            },
        );
        watch_plot(&plot, &[setting, base]);
        if let Some(panel) = plot.parent() {
            row.bind_property("selected", &panel, "visible")
                .transform_to(|_, selected: u32| Some(matches!(selected, 1 | 2)))
                .sync_create()
                .build();
        }
    }
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

pub(crate) fn gyro_table_mut(doc: &mut DocumentMut) -> Result<&mut dyn TableLike, &'static str> {
    doc.entry("gyro")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("Edit this gyro configuration in Advanced TOML.")
}

fn set_gyro_numbers(
    doc: &mut DocumentMut,
    changes: impl IntoIterator<Item = (&'static str, f64)>,
) -> Result<(), &'static str> {
    let gyro = gyro_table_mut(doc)?;
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
    if let Some(gyro) = doc
        .get("gyro")
        .map_or(Some(None), |item| item.as_table_like().map(Some))
    {
        page.add(&flick_group(ctx, gyro));
    }
    let fallback = number(doc.as_table(), "stick_curve", 1.0);
    let live_travel = Rc::new(Cell::new(None::<[f64; 2]>));
    let mut plots = Vec::new();
    for (index, ((key, title), side)) in STICK_FIELDS
        .into_iter()
        .zip(["Left stick", "Right stick"])
        .zip(["left", "right"])
        .enumerate()
    {
        let group = adw::PreferencesGroup::builder()
            .title(title)
            .description("Lower exponents give more response near the center. 1.00 is linear.")
            .build();
        let initial = number(doc.as_table(), key, fallback);
        let exponent = Rc::new(Cell::new(initial));
        let plot = response_plot::add(
            &group,
            "Stick response",
            "Output travel (%)",
            "Physical travel (%)",
            true,
            {
                let exponent = exponent.clone();
                let live_travel = live_travel.clone();
                move || {
                    stick_curve(
                        exponent.get(),
                        live_travel.get().map(|travel| travel[index]),
                    )
                }
            },
        );
        let plot_ref = plot.downgrade();
        plots.push(plot_ref.clone());
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
        group.add(&row);
        group.add(&crate::stick_keys::editor_row(ctx, side, title));
        page.add(&group);
    }
    controller_input::watch_status(
        &page,
        "stick_travel=",
        Duration::from_millis(33),
        move |result| {
            let travel = result.ok().flatten().as_deref().and_then(stick_travel);
            if live_travel.replace(travel) != travel {
                for plot in plots.iter().filter_map(adw::glib::WeakRef::upgrade) {
                    plot.queue_draw();
                }
            }
        },
    );
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

fn stick_travel(value: &str) -> Option<[f64; 2]> {
    let (left, right) = value.split_once(',')?;
    let travel = [response_input(left)?, response_input(right)?];
    travel.iter().all(|&value| value <= 100.0).then_some(travel)
}

fn stick_curve(exponent: f64, travel: Option<f64>) -> Curve {
    let exponent = exponent.clamp(0.25, 4.0);
    let output = |input: f64| 100.0 * (input / 100.0).powf(exponent);
    Curve {
        input_max: 100.0,
        output_max: 100.0,
        points: (0..=100)
            .map(|input| (f64::from(input), output(f64::from(input))))
            .collect(),
        sample: travel.map(|input| (input, output(input))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_points_use_the_configured_curve_at_the_measured_input() {
        assert_eq!(stick_travel("50,75"), Some([50.0, 75.0]));
        assert_eq!(stick_travel("101,50"), None);
        assert_eq!(stick_curve(2.0, Some(50.0)).sample, Some((50.0, 25.0)));

        let response = GyroResponse::new(10.0, 20.0, 100.0, 2.0).unwrap();
        assert_eq!(gyro_curve(response, Some(5.0)).sample, Some((5.0, 0.0)));
        let high = gyro_curve(response, Some(300.0));
        assert_eq!(high.sample, Some((300.0, 600.0)));
        assert!(high.input_max >= 300.0 && high.output_max >= 600.0);
        assert_eq!(gyro_curve(response, None).sample, None);
    }

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

use eframe::egui::{self, Color32, Stroke, vec2};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

const GYRO_FIELDS: [&str; 2] = ["sensitivity_x", "sensitivity_y"];
const CALIBRATION_FIELDS: [&str; 2] = ["real_world_calibration", "in_game_sens"];
const STICK_FIELDS: [&str; 2] = ["stick_curve_left", "stick_curve_right"];
const ACCENT: Color32 = Color32::from_rgb(102, 77, 196);

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

pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(225, 222, 236)))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(16))
        .show(ui, add)
        .inner
}

pub fn gyro(ui: &mut egui::Ui, doc: &mut DocumentMut, draft: &mut CalibrationDraft) -> bool {
    ui.heading("Gyro aim");
    ui.label("Set horizontal and vertical response independently.");
    ui.add_space(8.0);
    let Some(gyro_table) = doc
        .get("gyro")
        .map_or(Some(None), |item| item.as_table_like().map(Some))
    else {
        ui.label("Edit this gyro configuration in Advanced TOML.");
        return false;
    };
    let mut changes = Vec::new();
    card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.heading("Sensitivity");
        ui.spacing_mut().slider_width = ui.available_width().clamp(140.0, 420.0);
        for (key, label) in GYRO_FIELDS.into_iter().zip(["Horizontal", "Vertical"]) {
            let mut value = gyro_table.map_or(4.0, |table| number(table, key, 4.0));
            ui.label(label);
            if ui
                .add(
                    egui::Slider::new(&mut value, 0.0..=100.0)
                        .step_by(0.1)
                        .clamping(egui::SliderClamping::Edits)
                        .suffix(" counts/°"),
                )
                .changed()
            {
                changes.push((key, value));
            }
        }
    });
    ui.add_space(8.0);
    let reset = card(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.heading("Game calibration");
        let values = draft.values.get_or_insert_with(|| {
            CALIBRATION_FIELDS.map(|key| {
                gyro_table
                    .map_or(1.0, |table| number(table, key, 1.0))
                    .to_string()
            })
        });
        let mut edited = false;
        egui::Grid::new("calibration-values")
            .num_columns(2)
            .show(ui, |ui| {
                for (text, label) in values
                    .iter_mut()
                    .zip(["Real-world calibration (RWC)", "In-game sensitivity"])
                {
                    ui.label(label);
                    edited |= ui
                        .add(
                            egui::TextEdit::singleline(text)
                                .desired_width(155.0)
                                .background_color(Color32::from_rgb(242, 238, 251)),
                        )
                        .changed();
                    ui.end_row();
                }
            });
        if edited {
            match calibration_values(values) {
                Ok(result) => {
                    draft.error = None;
                    changes.extend(CALIBRATION_FIELDS.into_iter().zip(result));
                }
                Err(error) => draft.error = Some(error),
            }
        }
        if let Some(error) = draft.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            return ui.button("Reset fields").clicked();
        }
        false
    });
    if reset {
        draft.reset();
    }
    if changes.is_empty() {
        return false;
    }
    let gyro_table = doc
        .entry("gyro")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .expect("validated gyro table");
    for (key, value) in changes {
        set_number(gyro_table, key, value);
    }
    true
}

pub fn sticks(ui: &mut egui::Ui, doc: &mut DocumentMut) -> bool {
    ui.heading("Stick response");
    ui.label("Lower exponents give more response near the center. 1.00 is linear.");
    ui.add_space(8.0);
    let fallback = number(doc.as_table(), "stick_curve", 1.0);
    let mut changed = false;
    ui.columns(2, |columns| {
        for ((ui, name), key) in columns
            .iter_mut()
            .zip(["Left stick", "Right stick"])
            .zip(STICK_FIELDS)
        {
            card(ui, |ui| {
                ui.heading(name);
                let mut exponent = number(doc.as_table(), key, fallback);
                curve_preview(ui, exponent);
                ui.label("Response exponent");
                ui.spacing_mut().slider_width = ui.available_width().clamp(100.0, 235.0);
                if ui
                    .add(
                        egui::Slider::new(&mut exponent, 0.25..=4.0)
                            .step_by(0.05)
                            .clamping(egui::SliderClamping::Edits)
                            .fixed_decimals(2),
                    )
                    .changed()
                {
                    set_number(doc.as_table_mut(), key, exponent);
                    changed = true;
                }
            });
        }
    });
    changed
}

fn curve_preview(ui: &mut egui::Ui, exponent: f64) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 174.0), egui::Sense::hover());
    let plot = rect.shrink(12.0);
    let at = |x: f32, y: f32| plot.left_bottom() + vec2(x * plot.width(), -y * plot.height());
    let painter = ui.painter();
    painter.rect_filled(rect, 8, Color32::from_rgb(246, 244, 252));
    for tick in 0..=4 {
        let step = tick as f32 / 4.0;
        let grid = Stroke::new(1.0_f32, Color32::from_rgb(225, 221, 239));
        painter.line_segment([at(step, 0.0), at(step, 1.0)], grid);
        painter.line_segment([at(0.0, step), at(1.0, step)], grid);
    }
    painter.line_segment(
        [at(0.0, 0.0), at(1.0, 1.0)],
        Stroke::new(1.0_f32, Color32::from_gray(158)),
    );
    let points = (0..=64)
        .map(|i| {
            let x = i as f32 / 64.0;
            at(x, x.powf(exponent.clamp(0.25, 4.0) as f32))
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.5_f32, ACCENT)));
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

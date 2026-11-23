use eframe::egui::{self, Color32, Rect, Stroke, vec2};
use toml_edit::DocumentMut;

use crate::{
    adwaita,
    bindings::{self, Gesture},
};

struct Control {
    id: &'static str,
    name: &'static str,
    glyph: &'static str,
    center: [f32; 2],
    size: [f32; 2],
}

const FRONT: [Control; 19] = [
    control("l2", "L2 · Left trigger", "L2", 126., 24., 74., 28.),
    control("r2", "R2 · Right trigger", "R2", 394., 24., 74., 28.),
    control("l1", "L1 · Left bumper", "L1", 126., 61., 74., 28.),
    control("r1", "R1 · Right bumper", "R1", 394., 61., 74., 28.),
    control("create", "Create", "Create", 158., 99., 57., 24.),
    control("options", "Options", "Options", 372., 99., 70., 24.),
    control(
        "touchpad",
        "Touchpad press",
        "Touchpad",
        260.,
        111.,
        124.,
        55.,
    ),
    control("dpad-up", "D-pad up", "↑", 121., 132., 32., 32.),
    control("dpad-left", "D-pad left", "←", 88., 166., 32., 32.),
    control("dpad-right", "D-pad right", "→", 154., 166., 32., 32.),
    control("dpad-down", "D-pad down", "↓", 121., 200., 32., 32.),
    control("triangle", "Triangle", "△", 399., 132., 32., 32.),
    control("square", "Square", "□", 366., 166., 32., 32.),
    control("circle", "Circle", "○", 432., 166., 32., 32.),
    control("cross", "Cross", "×", 399., 200., 32., 32.),
    control("l3", "L3 · Left stick press", "L3", 191., 218., 50., 50.),
    control("r3", "R3 · Right stick press", "R3", 329., 218., 50., 50.),
    control("ps", "PS button", "PS", 260., 205., 36., 31.),
    control("mute", "Microphone mute", "Mute", 260., 245., 60., 24.),
];

const EDGE: [(&str, &str); 4] = [
    ("fn-left", "Left Fn"),
    ("fn-right", "Right Fn"),
    ("paddle-left", "Left paddle"),
    ("paddle-right", "Right paddle"),
];

const GAMEPAD_TARGETS: [(&str, &str); 13] = [
    ("cross", "South · A / Cross"),
    ("circle", "East · B / Circle"),
    ("square", "West · X / Square"),
    ("triangle", "North · Y / Triangle"),
    ("l1", "Left bumper"),
    ("r1", "Right bumper"),
    ("l3", "Left stick press"),
    ("r3", "Right stick press"),
    ("create", "Back / Create"),
    ("options", "Start / Options"),
    ("ps", "Guide / PS"),
    ("l2", "Left digital trigger (Generic)"),
    ("r2", "Right digital trigger (Generic)"),
];

const KEYS: &[(&str, &str)] = &[
    ("KEY_SPACE", "Space"),
    ("KEY_ENTER", "Enter"),
    ("KEY_ESC", "Escape"),
    ("KEY_TAB", "Tab"),
    ("KEY_BACKSPACE", "Backspace"),
    ("KEY_LEFTSHIFT", "Left Shift"),
    ("KEY_LEFTCTRL", "Left Ctrl"),
    ("KEY_LEFTALT", "Left Alt"),
    ("KEY_UP", "Up arrow"),
    ("KEY_DOWN", "Down arrow"),
    ("KEY_LEFT", "Left arrow"),
    ("KEY_RIGHT", "Right arrow"),
    ("BTN_LEFT", "Left mouse"),
    ("BTN_RIGHT", "Right mouse"),
    ("BTN_MIDDLE", "Middle mouse"),
    ("BTN_SIDE", "Mouse back"),
    ("BTN_EXTRA", "Mouse forward"),
];

const fn control(
    id: &'static str,
    name: &'static str,
    glyph: &'static str,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Control {
    Control {
        id,
        name,
        glyph,
        center: [x, y],
        size: [w, h],
    }
}

fn name(id: &str) -> &str {
    FRONT
        .iter()
        .find(|control| control.id == id)
        .map(|control| control.name)
        .or_else(|| {
            EDGE.iter()
                .find(|(key, _)| *key == id)
                .map(|(_, name)| *name)
        })
        .unwrap_or(id)
}

fn mapped(doc: &DocumentMut, id: &str) -> bool {
    matches!(bindings::keyboard_binding(doc, id), Ok(Some(_)))
        || matches!(bindings::gamepad_target(doc, id), Ok(Some(_)))
}

pub fn layout(ui: &mut egui::Ui, doc: &DocumentMut, selected: &mut &'static str) {
    ui.heading("DualSense controller");
    ui.label("Select a control to see or change its actions.");
    let width = ui.available_width();
    let scale = (width / 520.0).min(1.15);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 318.0 * scale), egui::Sense::hover());
    let origin = rect.min + vec2((width - 520.0 * scale) * 0.5, 0.0);
    let at = |x, y| origin + vec2(x, y) * scale;
    let colors = adwaita::palette(ui);
    let painter = ui.painter();
    for (left, right, grip) in [(65., 178., 102.), (342., 455., 418.)] {
        painter.add(egui::Shape::convex_polygon(
            vec![
                at(left, 128.),
                at(right, 128.),
                at(grip + 49., 257.),
                at(grip - 49., 257.),
            ],
            colors.shell,
            Stroke::NONE,
        ));
        painter.circle_filled(at(grip, 251.), 49.0 * scale, colors.shell);
    }
    painter.rect_filled(
        Rect::from_min_max(at(55., 46.), at(465., 251.)),
        65.0 * scale,
        colors.shell,
    );
    painter.rect_filled(
        Rect::from_min_max(at(173., 79.), at(347., 180.)),
        22.0 * scale,
        colors.touchpad,
    );
    painter.line_segment(
        [at(224., 150.), at(296., 150.)],
        Stroke::new(2.0_f32, colors.accent),
    );

    let mut controls_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    for control in &FRONT {
        let [x, y] = control.center;
        let [w, h] = control.size;
        let active = *selected == control.id;
        let bound = mapped(doc, control.id);
        let icon = matches!(control.glyph, "↑" | "↓" | "←" | "→" | "△" | "×" | "○" | "□");
        let label = if icon { "" } else { control.glyph };
        let bounds = Rect::from_center_size(at(x, y), vec2(w, h) * scale);
        let button = egui::Button::new(egui::RichText::new(label).size((13.5 * scale).max(10.0)))
            .min_size(bounds.size())
            .wrap_mode(egui::TextWrapMode::Extend)
            .fill(if active { colors.selected } else { colors.card })
            .stroke(if active || bound {
                Stroke::new(if active { 2.0_f32 } else { 1.0 }, colors.accent)
            } else {
                Stroke::NONE
            })
            .corner_radius(
                if matches!(
                    control.id,
                    "triangle" | "cross" | "square" | "circle" | "l3" | "r3"
                ) {
                    h * scale * 0.5
                } else {
                    7.0
                },
            );
        let response = controls_ui.put(bounds, button).on_hover_text(control.name);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, control.name)
        });
        if icon {
            draw_icon(
                controls_ui.painter(),
                response.rect.center(),
                6.0 * scale,
                control.glyph,
                colors.foreground,
            );
        }
        if response.clicked() {
            *selected = control.id;
            ui.ctx().request_repaint();
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("DualSense Edge");
        for (id, label) in EDGE {
            let button = egui::Button::selectable(*selected == id, label);
            let button = if mapped(doc, id) {
                button.stroke(Stroke::new(1.0_f32, colors.accent))
            } else {
                button
            };
            if ui.add(button).clicked() {
                *selected = id;
                ui.ctx().request_repaint();
            }
        }
    });
    ui.label(egui::RichText::new("Thin outlines mark assigned controls.").weak());
}

fn draw_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    glyph: &str,
    foreground: Color32,
) {
    let at = |x, y| center + vec2(x, y) * radius;
    let stroke = Stroke::new(1.65_f32, foreground);
    match glyph {
        "○" => {
            painter.circle_stroke(center, radius, stroke);
        }
        "□" => {
            painter.rect_stroke(
                Rect::from_min_max(at(-0.86, -0.86), at(0.86, 0.86)),
                0,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        "△" => {
            painter.add(egui::Shape::closed_line(
                vec![at(0., -1.), at(1., 0.86), at(-1., 0.86)],
                stroke,
            ));
        }
        "×" => {
            painter.line_segment([at(-0.8, -0.8), at(0.8, 0.8)], stroke);
            painter.line_segment([at(-0.8, 0.8), at(0.8, -0.8)], stroke);
        }
        arrow => {
            let direction = match arrow {
                "↑" => vec2(0., -1.),
                "↓" => vec2(0., 1.),
                "←" => vec2(-1., 0.),
                "→" => vec2(1., 0.),
                _ => return,
            };
            let side = vec2(-direction.y, direction.x);
            let tip = center + direction * radius;
            painter.line_segment([center - direction * radius, tip], stroke);
            painter.add(egui::Shape::line(
                vec![
                    center + side * (radius * 0.75),
                    tip,
                    center - side * (radius * 0.75),
                ],
                stroke,
            ));
        }
    }
}

fn dropdown(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    text: &str,
    current: Option<&str>,
    default: &str,
    options: &[(&str, &str)],
    keyboard: bool,
) -> Option<Option<String>> {
    let mut result = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(ui.available_width().min(238.0))
        .height(310.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(current.is_none(), default).clicked() {
                result = Some(None);
            }
            for &(code, label) in options {
                if ui.selectable_label(current == Some(code), label).clicked() {
                    result = Some(Some(code.to_owned()));
                }
            }
            if keyboard {
                ui.separator();
                for letter in ('A'..='Z').chain('0'..='9') {
                    let code = format!("KEY_{letter}");
                    if ui
                        .selectable_label(current == Some(code.as_str()), letter.to_string())
                        .clicked()
                    {
                        result = Some(Some(code));
                    }
                }
                ui.separator();
                for number in 1..=12 {
                    let label = format!("F{number}");
                    let code = format!("KEY_{label}");
                    if ui
                        .selectable_label(current == Some(code.as_str()), label)
                        .clicked()
                    {
                        result = Some(Some(code));
                    }
                }
            }
        });
    result
}

fn output_label<'a>(value: &'a str, options: &'a [(&str, &str)]) -> &'a str {
    options.iter().find(|(code, _)| *code == value).map_or_else(
        || value.strip_prefix("KEY_").unwrap_or(value),
        |(_, label)| *label,
    )
}

pub fn inspector(ui: &mut egui::Ui, doc: &mut DocumentMut, selected: &str) -> bool {
    ui.heading(name(selected));
    ui.add_space(12.0);
    let mut changed = false;
    ui.label(egui::RichText::new("Keyboard and mouse").strong());
    if let Err(error) = keyboard_actions(ui, doc, selected, &mut changed) {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    if !selected.starts_with("dpad-") {
        ui.add_space(12.0);
        ui.separator();
        ui.label(egui::RichText::new("Virtual gamepad").strong());
        match gamepad_action(ui, doc, selected) {
            Ok(edited) => changed |= edited,
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }
    }
    changed
}

fn keyboard_actions(
    ui: &mut egui::Ui,
    doc: &mut DocumentMut,
    selected: &str,
    changed: &mut bool,
) -> Result<(), &'static str> {
    for gesture in Gesture::ALL {
        ui.label(gesture.label());
        let value = bindings::keyboard_binding(doc, selected)?
            .and_then(|binding| bindings::gesture_output(binding, gesture));
        if let Some(value) = value
            && value.as_str().is_none()
        {
            ui.label("Custom action · Advanced TOML");
            continue;
        }
        let current = value.and_then(|value| value.as_str());
        let preview = current.map_or("No action", |value| output_label(value, KEYS));
        if let Some(next) = dropdown(
            ui,
            gesture.label(),
            preview,
            current,
            "No action",
            KEYS,
            true,
        ) && next.as_deref() != current
        {
            bindings::set_key(doc, selected, gesture, next.as_deref())?;
            *changed = true;
        }
    }
    Ok(())
}

fn gamepad_action(
    ui: &mut egui::Ui,
    doc: &mut DocumentMut,
    selected: &str,
) -> Result<bool, &'static str> {
    let current = bindings::gamepad_target(doc, selected)?;
    let preview = current.map_or("Same as controller", |value| {
        output_label(value, &GAMEPAD_TARGETS)
    });
    if let Some(next) = dropdown(
        ui,
        "gamepad-target",
        preview,
        current,
        "Same as controller",
        &GAMEPAD_TARGETS,
        false,
    ) && next.as_deref() != current
    {
        bindings::set_gamepad(doc, selected, next.as_deref())?;
        return Ok(true);
    }
    Ok(false)
}

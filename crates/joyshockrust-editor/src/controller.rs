use adw::{glib, gtk, prelude::*};
use toml_edit::DocumentMut;

use crate::{
    bindings::{self, Gesture},
    ui::{self, Context},
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

pub fn page(ctx: &Context) -> gtk::Widget {
    let Ok(doc) = ctx.document() else {
        return ui::advanced_required("Fix the profile syntax in Advanced TOML to edit buttons.");
    };

    let layout = gtk::Box::new(gtk::Orientation::Vertical, 18);
    layout.set_margin_top(18);
    layout.set_margin_bottom(24);
    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .child(&layout)
        .build();
    let diagram = gtk::Fixed::new();
    diagram.set_halign(gtk::Align::Center);
    let diagram_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&diagram)
        .build();
    layout.append(&diagram_scroll);
    let outline = gtk::DrawingArea::builder()
        .content_width(520)
        .content_height(308)
        .build();
    outline.set_can_target(false);
    outline.set_draw_func(|area, cr, _, _| {
        let color = area.style_context().color();
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            0.09,
        );
        cr.move_to(115.0, 45.0);
        cr.curve_to(58.0, 45.0, 49.0, 98.0, 46.0, 156.0);
        cr.curve_to(37.0, 236.0, 50.0, 302.0, 99.0, 302.0);
        cr.curve_to(155.0, 302.0, 158.0, 250.0, 207.0, 250.0);
        cr.line_to(313.0, 250.0);
        cr.curve_to(362.0, 250.0, 365.0, 302.0, 421.0, 302.0);
        cr.curve_to(470.0, 302.0, 483.0, 236.0, 474.0, 156.0);
        cr.curve_to(471.0, 98.0, 462.0, 45.0, 405.0, 45.0);
        cr.close_path();
        let _ = cr.fill();
    });
    diagram.put(&outline, 0.0, 0.0);

    let inspector = gtk::Box::new(gtk::Orientation::Vertical, 18);
    inspector.set_margin_start(24);
    inspector.set_margin_end(24);
    let selected = ctx.editor.borrow().selected;

    let edge = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    edge.set_halign(gtk::Align::Center);
    layout.append(&edge);
    let mut first = None::<gtk::ToggleButton>;
    for (id, label, glyph, control) in FRONT
        .iter()
        .map(|control| (control.id, control.name, control.glyph, Some(control)))
        .chain(EDGE.into_iter().map(|(id, label)| (id, label, label, None)))
    {
        let button = gtk::ToggleButton::builder()
            .label(glyph)
            .tooltip_text(label)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        if let Some(first) = &first {
            button.set_group(Some(first));
        } else {
            first = Some(button.clone());
        }
        mark_assigned(&button, &doc, id);
        if id == selected {
            button.set_active(true);
            show_bindings(&inspector, ctx, id, &button);
        }
        let change_ctx = ctx.clone();
        let inspector_ref = inspector.downgrade();
        button.connect_toggled(move |button| {
            if button.is_active()
                && let Some(inspector) = inspector_ref.upgrade()
            {
                change_ctx.editor.borrow_mut().selected = id;
                show_bindings(&inspector, &change_ctx, id, button);
            }
        });
        if let Some(control) = control {
            let [x, y] = control.center.map(f64::from);
            let [width, height] = control.size.map(f64::from);
            button.set_size_request(width as i32, height as i32);
            if matches!(
                id,
                "triangle"
                    | "cross"
                    | "square"
                    | "circle"
                    | "l3"
                    | "r3"
                    | "dpad-up"
                    | "dpad-down"
                    | "dpad-left"
                    | "dpad-right"
            ) {
                button.add_css_class("circular");
            }
            diagram.put(&button, x - width / 2.0, y - height / 2.0);
        } else {
            edge.append(&button);
        }
    }
    layout.append(&inspector);

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build()
        .upcast()
}

fn show_bindings(
    inspector: &gtk::Box,
    ctx: &Context,
    selected: &'static str,
    button: &gtk::ToggleButton,
) {
    while let Some(child) = inspector.first_child() {
        inspector.remove(&child);
    }
    let Ok(doc) = ctx.document() else { return };
    let button = button.downgrade();
    let keyboard = adw::PreferencesGroup::builder()
        .title(name(selected))
        .description("Keyboard and mouse actions")
        .build();
    inspector.append(&keyboard);
    match bindings::keyboard_binding(&doc, selected) {
        Ok(binding) => {
            for gesture in Gesture::ALL {
                let value = binding.and_then(|binding| bindings::gesture_output(binding, gesture));
                if value.is_some_and(|value| value.as_str().is_none()) {
                    unavailable(
                        &keyboard,
                        gesture.label(),
                        "Edit this custom action in Advanced TOML.",
                    );
                    continue;
                }
                let ctx = ctx.clone();
                let button = button.clone();
                keyboard.add(&dropdown(
                    gesture.label(),
                    value.and_then(|value| value.as_str()),
                    "No action",
                    key_choices(),
                    move |next| {
                        edit_binding(&ctx, &button, selected, |doc| {
                            bindings::set_key(doc, selected, gesture, next)
                        })
                    },
                ));
            }
        }
        Err(error) => unavailable(&keyboard, "Keyboard bindings", error),
    }
    if !selected.starts_with("dpad-") {
        let gamepad = adw::PreferencesGroup::builder()
            .title("Virtual gamepad")
            .build();
        inspector.append(&gamepad);
        match bindings::gamepad_target(&doc, selected) {
            Ok(current) => {
                let ctx = ctx.clone();
                gamepad.add(&dropdown(
                    "Output button",
                    current,
                    "Same as controller",
                    GAMEPAD_TARGETS
                        .into_iter()
                        .map(|(key, label)| (key.into(), label.into())),
                    move |next| {
                        edit_binding(&ctx, &button, selected, |doc| {
                            bindings::set_gamepad(doc, selected, next)
                        })
                    },
                ));
            }
            Err(error) => unavailable(&gamepad, "Output button", error),
        }
    }
}

fn edit_binding(
    ctx: &Context,
    button: &glib::WeakRef<gtk::ToggleButton>,
    id: &str,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), &'static str>,
) {
    ctx.edit(|doc| {
        edit(doc)?;
        if let Some(button) = button.upgrade() {
            mark_assigned(&button, doc, id);
        }
        Ok(())
    });
}

fn mark_assigned(button: &gtk::ToggleButton, doc: &DocumentMut, id: &str) {
    if mapped(doc, id) {
        button.add_css_class("accent");
    } else {
        button.remove_css_class("accent");
    }
}

fn unavailable(group: &adw::PreferencesGroup, title: &str, detail: &str) {
    group.add(
        &adw::ActionRow::builder()
            .title(title)
            .subtitle(detail)
            .build(),
    );
}

fn key_choices() -> impl Iterator<Item = (String, String)> {
    KEYS.iter()
        .map(|&(key, label)| (key.into(), label.into()))
        .chain(
            ('A'..='Z')
                .chain('0'..='9')
                .map(|key| (format!("KEY_{key}"), key.to_string())),
        )
        .chain((1..=12).map(|number| (format!("KEY_F{number}"), format!("F{number}"))))
}

fn dropdown(
    title: &str,
    current: Option<&str>,
    default: &str,
    choices: impl Iterator<Item = (String, String)>,
    changed: impl Fn(Option<&str>) + 'static,
) -> adw::ComboRow {
    let mut choices: Vec<_> = std::iter::once((None, default.to_owned()))
        .chain(choices.map(|(key, label)| (Some(key), label)))
        .collect();
    let selected = if let Some(index) = choices
        .iter()
        .position(|(key, _)| key.as_deref() == current)
    {
        index
    } else {
        choices.push((
            current.map(str::to_owned),
            current.unwrap_or_default().to_owned(),
        ));
        choices.len() - 1
    };
    let labels: Vec<_> = choices.iter().map(|(_, label)| label.as_str()).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .selected(selected as u32)
        .build();
    row.connect_selected_notify(move |row| {
        if let Some((key, _)) = choices.get(row.selected() as usize) {
            changed(key.as_deref());
        }
    });
    row
}

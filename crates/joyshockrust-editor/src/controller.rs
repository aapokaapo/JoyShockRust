use adw::{glib, gtk, prelude::*};
use toml_edit::DocumentMut;

use crate::{
    bindings::{self, Gesture, OutputMode},
    controller_art,
    ui::{self, Context},
};

struct Control {
    id: &'static str,
    name: &'static str,
    glyph: &'static str,
    center: [f32; 2],
    size: [f32; 2],
}

const ART_SCALE: f64 = 2.0;
const ART_TOP: f64 = 42.0;
const ART_BOTTOM: f64 = 216.0;

const FRONT: [Control; 22] = [
    control("l2", "L2 · Left trigger", "L2", 74., 54., 46., 22.),
    control("r2", "R2 · Right trigger", "R2", 182., 54., 46., 22.),
    control("l1", "L1 · Left bumper", "L1", 74., 69., 50., 22.),
    control("r1", "R1 · Right bumper", "R1", 182., 69., 50., 22.),
    control("create", "Create", "Create", 82., 82., 42., 21.),
    control("options", "Options", "Options", 174., 82., 46., 21.),
    control("dpad-up", "D-pad up", "↑", 64., 94., 22., 22.),
    control("dpad-left", "D-pad left", "←", 52., 106., 22., 22.),
    control("dpad-right", "D-pad right", "→", 76., 106., 22., 22.),
    control("dpad-down", "D-pad down", "↓", 64., 118., 22., 22.),
    control("triangle", "Triangle", "△", 192., 94., 22., 22.),
    control("square", "Square", "□", 180., 106., 22., 22.),
    control("circle", "Circle", "○", 204., 106., 22., 22.),
    control("cross", "Cross", "×", 192., 118., 22., 22.),
    control("l3", "L3 · Left stick press", "L3", 102., 134., 42., 42.),
    control("r3", "R3 · Right stick press", "R3", 154., 134., 42., 42.),
    control("fn-left", "Left Fn", "Left Fn", 102., 155., 52., 20.),
    control("fn-right", "Right Fn", "Right Fn", 154., 155., 52., 20.),
    control(
        "paddle-left",
        "Left paddle",
        "Left paddle",
        102.,
        169.,
        84.,
        21.,
    ),
    control(
        "paddle-right",
        "Right paddle",
        "Right paddle",
        154.,
        169.,
        84.,
        21.,
    ),
    control("ps", "PS button", "PS", 128., 133., 29., 22.),
    control("mute", "Microphone mute", "Mute", 128., 147., 38., 20.),
];

pub(crate) fn physical_buttons() -> impl Iterator<Item = (String, String)> {
    FRONT
        .iter()
        .filter(|control| !control.id.starts_with("dpad-"))
        .map(|control| (control.id, control.name))
        .chain([("touchpad", "Touchpad press")])
        .map(|(id, name)| (id.into(), name.into()))
}

pub(crate) const GAMEPAD_TARGETS: [(&str, &str); 17] = [
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
    ("dpad-up", "D-pad up"),
    ("dpad-right", "D-pad right"),
    ("dpad-down", "D-pad down"),
    ("dpad-left", "D-pad left"),
    ("l2", "Left digital trigger (Generic)"),
    ("r2", "Right digital trigger (Generic)"),
];

pub(crate) const MOUSE_BUTTONS: &[(&str, &str)] = &[
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
        .unwrap_or(id)
}

fn mapped(doc: &DocumentMut, id: &str) -> bool {
    matches!(
        bindings::output_mode(doc, id),
        Ok(OutputMode::Keyboard | OutputMode::Mouse | OutputMode::Legacy)
    ) || matches!(bindings::gamepad_target(doc, id), Ok(Some(_)))
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
    let output = gamepad_type_row(ctx, &doc);
    output.set_margin_start(24);
    output.set_margin_end(24);
    layout.append(&output);
    let diagram = gtk::Fixed::new();
    diagram.set_halign(gtk::Align::Center);
    let diagram_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(&diagram)
        .build();
    layout.append(&diagram_scroll);
    let outline = gtk::DrawingArea::builder()
        .content_width((controller_art::SIZE * ART_SCALE) as i32)
        .content_height(((ART_BOTTOM - ART_TOP) * ART_SCALE) as i32)
        .build();
    outline.set_can_target(false);
    match controller_art::load() {
        Ok(svg) => outline.set_draw_func(move |area, cr, _, _| {
            let _ = cr.save();
            cr.scale(ART_SCALE, ART_SCALE);
            cr.translate(0.0, -ART_TOP);
            controller_art::draw(cr, &svg, area.style_context().color(), 0.60);
            let _ = cr.restore();
        }),
        Err(error) => eprintln!("DualSense art unavailable: {error}"),
    }
    let control_style = gtk::CssProvider::new();
    control_style.load_from_data(
        "button.diagram-control {
            min-width: 0;
            min-height: 0;
            padding: 0;
            font-size: 12px;
            background-color: mix(@window_bg_color, @window_fg_color, 0.12);
        }
        button.diagram-control:hover { background-color: mix(@window_bg_color, @window_fg_color, 0.18); }
        button:checked, button.diagram-control:checked {
            background-color: @accent_bg_color;
            color: @accent_fg_color;
            box-shadow: inset 0 0 0 2px @accent_fg_color;
        }",
    );
    diagram.put(&outline, 0.0, 0.0);

    let inspector = gtk::Box::new(gtk::Orientation::Vertical, 18);
    inspector.set_margin_start(24);
    inspector.set_margin_end(24);
    let selected = ctx.editor.borrow().selected;

    let mut first = None::<gtk::ToggleButton>;
    for control in &FRONT {
        let Control {
            id,
            name: label,
            glyph,
            ..
        } = *control;
        let button = gtk::ToggleButton::builder()
            .label(glyph)
            .tooltip_text(label)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        button
            .style_context()
            .add_provider(&control_style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
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
        let [x, y] = control.center.map(f64::from);
        let [width, height] = control.size.map(f64::from);
        button.add_css_class("diagram-control");
        button.set_size_request(width as i32, height as i32);
        if matches!(
            id,
            "triangle" | "cross" | "square" | "circle" | "l3" | "r3" | "ps"
        ) {
            button.add_css_class("circular");
        }
        diagram.put(
            &button,
            x * ART_SCALE - width / 2.0,
            (y - ART_TOP) * ART_SCALE - height / 2.0,
        );
    }
    layout.append(&inspector);

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build()
        .upcast()
}

fn gamepad_type_row(ctx: &Context, doc: &DocumentMut) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let existing = doc.get("bridge");
    if existing.is_some_and(|item| item.as_table_like().is_none()) {
        group.add(&message_row(
            "Virtual gamepad type",
            "Edit the bridge table in Advanced TOML.",
        ));
        return group;
    }
    let current = existing
        .and_then(|item| item.get("gamepad"))
        .and_then(toml_edit::Item::as_str);
    let ctx = ctx.clone();
    group.add(&dropdown(
        "Virtual gamepad type",
        current,
        "Use bridge default",
        [
            ("generic", "Generic"),
            ("xbox-series", "Xbox Series X|S"),
            ("xbox360", "Xbox 360"),
        ]
        .into_iter()
        .map(|(key, label)| (key.into(), label.into())),
        move |kind| {
            ctx.edit(|doc| bindings::set_output_kind(doc, kind));
        },
    ));
    group
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
    let group = adw::PreferencesGroup::builder()
        .title(name(selected))
        .build();
    inspector.append(&group);
    let mode = match bindings::output_mode(&doc, selected) {
        Ok(mode) => mode,
        Err(error) => {
            unavailable(&group, "Output type", error);
            return;
        }
    };
    let weak_button = button.downgrade();
    let choices = [
        (OutputMode::Gamepad, "Virtual gamepad"),
        (OutputMode::Keyboard, "Keyboard"),
        (OutputMode::Mouse, "Mouse"),
        (OutputMode::Legacy, "Existing bindings"),
    ];
    let mode_row = required_dropdown(
        "Output type",
        mode.key(),
        choices
            .into_iter()
            .filter(|&(mode_option, _)| {
                mode_option != OutputMode::Legacy || mode == OutputMode::Legacy
            })
            .map(|(mode, name)| (mode.key().into(), name.into())),
        {
            let ctx = ctx.clone();
            let inspector = inspector.downgrade();
            let button = weak_button.clone();
            move |key| {
                let mode = choices
                    .iter()
                    .find(|(mode, _)| mode.key() == key)
                    .unwrap()
                    .0;
                if mode == OutputMode::Legacy {
                    return;
                }
                if ctx.edit(|doc| bindings::set_output_mode(doc, selected, mode))
                    && let (Some(inspector), Some(button)) = (inspector.upgrade(), button.upgrade())
                {
                    if let Ok(doc) = ctx.document() {
                        mark_assigned(&button, &doc, selected);
                    }
                    show_bindings(&inspector, &ctx, selected, &button);
                }
            }
        },
    );
    mode_row.set_subtitle("Changing the type removes actions assigned to other output types.");
    group.add(&mode_row);
    if matches!(selected, "l2" | "r2") {
        let host = inspector.downgrade();
        let editor = ctx.clone();
        crate::triggers::append(inspector, ctx, &doc, selected, mode, move || {
            if let (Some(host), Some(button)) = (host.upgrade(), weak_button.upgrade()) {
                show_bindings(&host, &editor, selected, &button);
            }
        });
        return;
    }
    if mode == OutputMode::Legacy {
        group.add(&message_row(
            "Existing profile binding",
            "Choose an output type to edit this input.",
        ));
        return;
    }
    if mode == OutputMode::Gamepad {
        match bindings::gamepad_target(&doc, selected) {
            Ok(current) => {
                let ctx = ctx.clone();
                group.add(&dropdown(
                    "Output button",
                    current,
                    "Same as controller",
                    GAMEPAD_TARGETS
                        .into_iter()
                        .map(|(key, label)| (key.into(), label.into())),
                    move |next| {
                        edit_binding(&ctx, &weak_button, selected, |doc| {
                            bindings::set_gamepad(doc, selected, next)
                        })
                    },
                ));
            }
            Err(error) => unavailable(&group, "Output button", error),
        }
        return;
    }
    let button = weak_button;
    let key_ctx = ctx.clone();
    let key_button = button.clone();
    for row in gesture_rows(
        bindings::keyboard_binding(&doc, selected),
        mode,
        move |gesture, output| {
            edit_binding(&key_ctx, &key_button, selected, |doc| {
                bindings::set_key(doc, selected, gesture, output)
            });
        },
    ) {
        group.add(&row);
    }
}

pub(crate) fn keyboard_rows(
    doc: &DocumentMut,
    input: &str,
    changed: impl Fn(Gesture, Option<&str>) + Clone + 'static,
) -> Vec<adw::PreferencesRow> {
    gesture_rows(
        bindings::keyboard_binding(doc, input),
        OutputMode::Legacy,
        changed,
    )
}

fn gesture_rows(
    binding: Result<Option<&toml_edit::Table>, &'static str>,
    mode: OutputMode,
    changed: impl Fn(Gesture, Option<&str>) + Clone + 'static,
) -> Vec<adw::PreferencesRow> {
    let binding = match binding {
        Ok(binding) => binding,
        Err(error) => return vec![message_row("Keyboard bindings", error).upcast()],
    };
    Gesture::ALL
        .into_iter()
        .map(|gesture| {
            let value = binding.and_then(|binding| bindings::gesture_output(binding, gesture));
            if value.is_some_and(|value| value.as_str().is_none()) {
                return message_row(gesture.label(), "Edit this custom action in Advanced TOML.")
                    .upcast();
            }
            let changed = changed.clone();
            let value = value.and_then(|item| item.as_str());
            let edit = move |output: Option<&str>| changed(gesture, output);
            if mode == OutputMode::Mouse {
                mouse_action_row(gesture.label(), value, edit).upcast()
            } else {
                let mouse = if mode == OutputMode::Legacy {
                    MOUSE_BUTTONS
                } else {
                    &[]
                };
                crate::key_capture::row(gesture.label(), value, mouse, edit).upcast()
            }
        })
        .collect()
}

pub(crate) fn mouse_action_row(
    title: &str,
    current: Option<&str>,
    changed: impl Fn(Option<&str>) + 'static,
) -> adw::ComboRow {
    let row = dropdown(
        title,
        current,
        "No action",
        MOUSE_BUTTONS
            .iter()
            .map(|&(key, label)| (key.into(), label.into())),
        changed,
    );
    let unbind = gtk::Button::builder()
        .label("Unbind")
        .valign(gtk::Align::Center)
        .build();
    row.bind_property("selected", &unbind, "sensitive")
        .transform_to(|_, selected: u32| {
            Some(selected != 0 && selected != gtk::INVALID_LIST_POSITION)
        })
        .sync_create()
        .build();
    let row_ref = row.downgrade();
    unbind.connect_clicked(move |_| {
        if let Some(row) = row_ref.upgrade() {
            row.set_selected(0);
        }
    });
    row.add_suffix(&unbind);
    row
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
    group.add(&message_row(title, detail));
}

pub(crate) fn message_row(title: &str, detail: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(detail)
        .build()
}

pub(crate) fn dropdown(
    title: &str,
    current: Option<&str>,
    default: &str,
    choices: impl Iterator<Item = (String, String)>,
    changed: impl Fn(Option<&str>) + 'static,
) -> adw::ComboRow {
    choice_row(title, current, Some(default), choices, changed)
}

pub(crate) fn required_dropdown(
    title: &str,
    current: &str,
    choices: impl Iterator<Item = (String, String)>,
    changed: impl Fn(&str) + 'static,
) -> adw::ComboRow {
    choice_row(title, Some(current), None, choices, move |value| {
        if let Some(value) = value {
            changed(value);
        }
    })
}

fn choice_row(
    title: &str,
    current: Option<&str>,
    default: Option<&str>,
    choices: impl Iterator<Item = (String, String)>,
    changed: impl Fn(Option<&str>) + 'static,
) -> adw::ComboRow {
    let mut choices: Vec<_> = default
        .into_iter()
        .map(|text| (None, text.to_owned()))
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

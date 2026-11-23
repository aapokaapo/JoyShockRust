use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use adw::{gio, glib, gtk, prelude::*};
use toml_edit::DocumentMut;

use crate::{Editor, controller, sensitivity, touchpad};

#[derive(Clone)]
pub struct Context {
    pub editor: Rc<RefCell<Editor>>,
    window: glib::WeakRef<adw::ApplicationWindow>,
    overlay: glib::WeakRef<adw::ToastOverlay>,
}

impl Context {
    pub fn document(&self) -> Result<DocumentMut, String> {
        self.editor
            .borrow()
            .text
            .parse::<DocumentMut>()
            .map_err(|error| error.to_string())
    }

    pub fn edit(&self, change: impl FnOnce(&mut DocumentMut) -> Result<(), &'static str>) -> bool {
        let result = self.document().and_then(|mut document| {
            change(&mut document)?;
            self.editor.borrow_mut().text = document.to_string();
            Ok(())
        });
        if let Err(error) = result {
            self.report(Err(error));
            return false;
        }
        true
    }

    pub(crate) fn report(&self, result: Result<String, String>) {
        match result {
            Ok(message) => {
                if let Some(overlay) = self.overlay.upgrade() {
                    overlay.add_toast(adw::Toast::new(&message));
                }
            }
            Err(error) => {
                if let Some(window) = self.window.upgrade() {
                    let dialog =
                        adw::AlertDialog::new(Some("Profile needs attention"), Some(&error));
                    dialog.add_response("close", "Close");
                    dialog.set_close_response("close");
                    dialog.present(Some(&window));
                }
            }
        }
    }
}

pub fn open_window(app: &adw::Application, editor: Rc<RefCell<Editor>>) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("JoyShockRust Profiles")
        .default_width(1040)
        .default_height(760)
        .build();
    window.set_size_request(365, 370);
    let overlay = adw::ToastOverlay::new();
    let ctx = Context {
        editor,
        window: window.downgrade(),
        overlay: overlay.downgrade(),
    };
    let toolbar = adw::ToolbarView::new();
    window.set_content(Some(&overlay));
    overlay.set_child(Some(&toolbar));

    let stack = adw::ViewStack::new();
    let pages = [
        ("buttons", "Buttons", "input-gaming-symbolic"),
        ("sticks", "Sticks", "view-grid-symbolic"),
        ("touchpad", "Touchpad", "input-touchpad-symbolic"),
        ("gyro", "Gyro", "preferences-system-symbolic"),
        ("advanced", "Advanced", "text-x-generic-symbolic"),
    ];
    for (name, title, icon) in pages {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.add_titled_with_icon(&content, Some(name), title, icon);
    }
    let page_ctx = ctx.clone();
    stack.connect_visible_child_name_notify(move |stack| refresh_page(stack, &page_ctx));
    refresh_page(&stack, &ctx);
    toolbar.set_content(Some(&stack));

    let header = adw::HeaderBar::new();
    let switcher = adw::ViewSwitcher::builder()
        .policy(adw::ViewSwitcherPolicy::Wide)
        .stack(&stack)
        .build();
    header.set_title_widget(Some(&switcher));
    toolbar.add_top_bar(&header);

    let profile_name = gtk::Entry::builder()
        .placeholder_text("Profile name")
        .width_chars(12)
        .tooltip_text("Name of the profile")
        .build();
    profile_name.update_property(&[gtk::accessible::Property::Label("Profile name")]);
    set_profile_name(&profile_name, &ctx);
    let name_ctx = ctx.clone();
    profile_name
        .connect_changed(move |entry| name_ctx.editor.borrow_mut().name = entry.text().into());
    header.pack_start(&profile_name);

    for (action_name, apply, shortcut) in [
        ("save", false, "<Primary>s"),
        ("save-apply", true, "<Primary><Shift>s"),
    ] {
        let action = gio::SimpleAction::new(action_name, None);
        let save_ctx = ctx.clone();
        let entry = profile_name.downgrade();
        action.connect_activate(move |_, _| {
            let result = save_ctx.editor.borrow_mut().save_and_apply(apply);
            if let Some(entry) = entry.upgrade() {
                set_profile_name(&entry, &save_ctx);
            }
            save_ctx.report(result);
        });
        window.add_action(&action);
        app.set_accels_for_action(&format!("win.{action_name}"), &[shortcut]);
    }
    let validate = gio::SimpleAction::new("validate", None);
    let validate_ctx = ctx.clone();
    validate.connect_activate(move |_, _| {
        let result = validate_ctx.editor.borrow().validate();
        validate_ctx.report(result);
    });
    window.add_action(&validate);

    let menu = gio::Menu::new();
    menu.append(Some("Save"), Some("win.save"));
    menu.append(Some("Validate Profile"), Some("win.validate"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Profile actions")
        .menu_model(&menu)
        .build();
    let apply = gtk::Button::builder()
        .label("Save & Apply")
        .action_name("win.save-apply")
        .build();
    apply.add_css_class("suggested-action");
    header.pack_end(&menu_button);
    header.pack_end(&apply);

    let bottom = adw::ViewSwitcherBar::builder().stack(&stack).build();
    toolbar.add_bottom_bar(&bottom);
    if let Ok(condition) = adw::BreakpointCondition::parse("max-width: 900sp") {
        let narrow = adw::Breakpoint::new(condition);
        narrow.add_setter(&switcher, "visible", Some(&false.to_value()));
        narrow.add_setter(&bottom, "reveal", Some(&true.to_value()));
        window.add_breakpoint(narrow);
    }

    let close_ctx = ctx.clone();
    let confirmed = Rc::new(Cell::new(false));
    window.connect_close_request(move |window| {
        let editor = close_ctx.editor.borrow();
        if confirmed.get() || (editor.text == editor.original && editor.calibration.error.is_none())
        {
            return glib::Propagation::Proceed;
        }
        let dialog = adw::AlertDialog::new(
            Some("Discard unsaved changes?"),
            Some("Your edits to this profile will be lost."),
        );
        dialog.add_responses(&[("cancel", "Keep Editing"), ("discard", "Discard")]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let window_ref = window.downgrade();
        let confirmed = confirmed.clone();
        dialog.connect_response(Some("discard"), move |_, _| {
            confirmed.set(true);
            if let Some(window) = window_ref.upgrade() {
                window.close();
            }
        });
        dialog.present(Some(window));
        glib::Propagation::Stop
    });
    window.present();
}

fn set_profile_name(entry: &gtk::Entry, ctx: &Context) {
    let name = ctx
        .editor
        .borrow()
        .path
        .as_ref()
        .and_then(|path| path.file_stem())
        .map(|name| name.to_string_lossy().into_owned());
    if let Some(name) = name {
        entry.set_text(&name);
        entry.set_editable(false);
    }
}

fn refresh_page(stack: &adw::ViewStack, ctx: &Context) {
    let Some(name) = stack.visible_child_name() else {
        return;
    };
    let Some(host) = stack.visible_child().and_downcast::<gtk::Box>() else {
        return;
    };
    while let Some(child) = host.first_child() {
        host.remove(&child);
    }
    let page: gtk::Widget = match name.as_str() {
        "advanced" => advanced_page(ctx).upcast(),
        "buttons" => controller::page(ctx),
        "touchpad" => touchpad::page(ctx),
        "sticks" => sensitivity::sticks_page(ctx),
        "gyro" => sensitivity::gyro_page(ctx),
        _ => return,
    };
    page.set_vexpand(true);
    host.append(&page);
}

fn advanced_page(ctx: &Context) -> gtk::ScrolledWindow {
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&ctx.editor.borrow().text);
    let state = ctx.editor.clone();
    buffer.connect_changed(move |buffer| {
        let mut editor = state.borrow_mut();
        editor.text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .into();
        editor.calibration.reset();
    });
    let text = gtk::TextView::builder()
        .buffer(&buffer)
        .monospace(true)
        .left_margin(18)
        .top_margin(16)
        .build();
    gtk::ScrolledWindow::builder().child(&text).build()
}

pub fn advanced_required(description: &str) -> gtk::Widget {
    adw::StatusPage::builder()
        .icon_name("document-edit-symbolic")
        .title("Edit in Advanced TOML")
        .description(description)
        .build()
        .upcast()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn descendants<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Vec<T> {
        let mut matches = Vec::new();
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            matches.extend(descendants::<T>(&current));
            if let Ok(current) = current.downcast::<T>() {
                matches.push(current);
            }
        }
        matches
    }

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn touchpad_widgets_edit_the_selected_cell() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let editor = Editor::open(PathBuf::new(), PathBuf::new(), None).unwrap();
        let ctx = Context {
            editor: Rc::new(RefCell::new(editor)),
            window: glib::WeakRef::new(),
            overlay: glib::WeakRef::new(),
        };
        let page = touchpad::page(&ctx);
        let columns = descendants::<adw::SpinRow>(&page)
            .into_iter()
            .find(|row| row.title() == "Columns")
            .unwrap();
        columns.set_value(3.0);
        let resize = descendants::<gtk::Button>(&page)
            .into_iter()
            .find(|button| button.label().as_deref() == Some("Set Grid"))
            .unwrap();
        resize.emit_clicked();
        let cell = descendants::<gtk::ToggleButton>(&page)
            .into_iter()
            .find(|button| button.label().as_deref() == Some("1:3"))
            .unwrap();
        cell.set_active(true);
        let rows = descendants::<adw::ComboRow>(&page);
        rows.iter()
            .find(|row| row.title() == "Touch")
            .unwrap()
            .set_selected(1);
        rows.iter()
            .find(|row| row.title() == "Click")
            .unwrap()
            .set_selected(3);
        let groups = descendants::<adw::PreferencesGroup>(&page);
        let keyboard = groups
            .iter()
            .find(|group| group.title() == "Keyboard and mouse · Touch")
            .unwrap();
        descendants::<adw::ComboRow>(keyboard.upcast_ref())
            .into_iter()
            .find(|row| row.title() == "Tap")
            .unwrap()
            .set_selected(1);
        rows.iter()
            .find(|row| row.title() == "Touch activation")
            .unwrap()
            .set_selected(1);
        let doc = ctx.document().unwrap();
        assert_eq!(doc["touchpad"]["columns"].as_integer(), Some(3));
        let regions = doc["touchpad"]["regions"].as_array_of_tables().unwrap();
        let region = regions.get(0).unwrap();
        assert_eq!(region["column"].as_integer(), Some(3));
        assert_eq!(region["touch"].as_str(), Some("cross"));
        assert_eq!(region["click"].as_str(), Some("square"));
        let keyboard = crate::bindings::keyboard_binding(&doc, "touch:1:3")
            .unwrap()
            .unwrap();
        assert_eq!(keyboard["tap"].as_str(), Some("KEY_SPACE"));
        assert_eq!(doc["gyro"]["binding"]["input"].as_str(), Some("touch:1:3"));
        assert_eq!(doc["gyro"]["binding"]["mode"].as_str(), Some("hold"));
    }

    #[test]
    #[ignore = "requires a Wayland compositor and the built bridge"]
    fn stick_keyboard_dialog_validates_and_updates_the_profile() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let bridge = PathBuf::from(
            std::env::var_os("JOYSHOCKRUST_TEST_BRIDGE")
                .expect("set JOYSHOCKRUST_TEST_BRIDGE to the built joyshockrust-linux binary"),
        );
        let editor = Editor::open(bridge, PathBuf::new(), None).unwrap();
        let app = adw::Application::builder()
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let window = adw::ApplicationWindow::builder().application(&app).build();
        let ctx = Context {
            editor: Rc::new(RefCell::new(editor)),
            window: window.downgrade(),
            overlay: glib::WeakRef::new(),
        };
        window.present();
        let dialog = crate::stick_keys::mapping_dialog(&ctx, "left", "Left stick", None).unwrap();
        dialog.present(Some(&window));
        let content = dialog.child().unwrap();
        let pick = |group_title: &str, row_title: &str, label: &str| {
            let group = descendants::<adw::PreferencesGroup>(&content)
                .into_iter()
                .find(|group| group.title() == group_title)
                .unwrap();
            let row = descendants::<adw::ComboRow>(group.upcast_ref())
                .into_iter()
                .find(|row| row.title() == row_title)
                .unwrap();
            let model = row.model().unwrap().downcast::<gtk::StringList>().unwrap();
            let index = (0..model.n_items())
                .find(|&index| model.string(index).as_deref() == Some(label))
                .unwrap();
            row.set_selected(index);
        };
        pick("Directions", "Up", "W");
        pick("Directions", "Left", "A");
        pick("Inner zone", "Key", "Left Shift");
        pick("Outer zone", "Key", "R");
        pick("Outer zone", "Mode", "Keep sprint");
        let threshold = descendants::<adw::PreferencesGroup>(&content)
            .into_iter()
            .find(|group| group.title() == "Inner zone")
            .and_then(|group| {
                descendants::<adw::EntryRow>(group.upcast_ref())
                    .into_iter()
                    .find(|row| row.title() == "Engage radius")
            })
            .unwrap();
        let update = descendants::<gtk::Button>(&content)
            .into_iter()
            .find(|button| button.label().as_deref() == Some("Update Mapping"))
            .unwrap();
        threshold.set_text("0.1");
        update.emit_clicked();
        assert!(ctx.document().unwrap().get("keyboard").is_none());
        assert!(
            descendants::<gtk::Label>(&content)
                .iter()
                .any(|label| label.is_visible() && label.text().contains("inner zone requires"))
        );
        threshold.set_text("0,5");
        update.emit_clicked();
        let doc = ctx.document().unwrap();
        let stick = &doc["keyboard"]["sticks"][0];
        assert_eq!(stick["up"].as_str(), Some("KEY_W"));
        assert_eq!(stick["left"].as_str(), Some("KEY_A"));
        assert_eq!(stick["inner"]["key"].as_str(), Some("KEY_LEFTSHIFT"));
        assert_eq!(stick["outer"]["mode"].as_str(), Some("keep_sprint"));
        window.close();
    }
}

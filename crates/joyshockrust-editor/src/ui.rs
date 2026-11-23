use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
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

    let profile_controls = gtk::Box::builder()
        .spacing(12)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    let profiles = gio::Menu::new();
    let chooser = gtk::MenuButton::builder()
        .tooltip_text("Choose a profile to edit")
        .menu_model(&profiles)
        .build();
    let profile_name = gtk::Entry::builder()
        .placeholder_text("Profile name")
        .width_chars(12)
        .hexpand(true)
        .build();
    profile_name.update_property(&[gtk::accessible::Property::Label("Profile name")]);
    profile_controls.append(&chooser);
    profile_controls.append(&profile_name);
    toolbar.add_top_bar(&profile_controls);

    sync_profiles(&ctx, &profiles, &chooser, &profile_name);
    let name_ctx = ctx.clone();
    profile_name
        .connect_changed(move |entry| name_ctx.editor.borrow_mut().name = entry.text().into());

    let open = gio::SimpleAction::new("open-profile", Some(glib::VariantTy::BYTE_STRING));
    let open_ctx = ctx.clone();
    let open_profiles = profiles.clone();
    let open_chooser = chooser.clone();
    let open_name = profile_name.clone();
    let open_stack = stack.downgrade();
    open.connect_activate(move |_, value| {
        let Some(bytes) = value.and_then(|value| value.get::<Vec<u8>>()) else {
            return;
        };
        let path = (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes)));
        let editor = open_ctx.editor.borrow();
        if path.is_some() && editor.path == path {
            return;
        }
        let unsaved = editor.has_unsaved_changes();
        drop(editor);
        let switch = {
            let ctx = open_ctx.clone();
            let profiles = open_profiles.clone();
            let chooser = open_chooser.clone();
            let name = open_name.clone();
            let stack = open_stack.clone();
            move || {
                let result = ctx.editor.borrow_mut().switch_profile(path.clone());
                if let Err(error) = result {
                    ctx.report(Err(format!("Could not open the profile: {error}")));
                    return;
                }
                sync_profiles(&ctx, &profiles, &chooser, &name);
                if let Some(stack) = stack.upgrade() {
                    refresh_page(&stack, &ctx);
                }
            }
        };
        if unsaved {
            if let Some(window) = open_ctx.window.upgrade() {
                confirm_discard(&window).connect_response(Some("discard"), move |_, _| switch());
            }
        } else {
            switch();
        }
    });
    window.add_action(&open);

    for (action_name, apply, shortcut) in [
        ("save", false, "<Primary>s"),
        ("save-apply", true, "<Primary><Shift>s"),
    ] {
        let action = gio::SimpleAction::new(action_name, None);
        let save_ctx = ctx.clone();
        let profiles = profiles.clone();
        let chooser = chooser.clone();
        let entry = profile_name.clone();
        action.connect_activate(move |_, _| {
            let result = save_ctx.editor.borrow_mut().save_and_apply(apply);
            sync_profiles(&save_ctx, &profiles, &chooser, &entry);
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
        if confirmed.get() || !close_ctx.editor.borrow().has_unsaved_changes() {
            return glib::Propagation::Proceed;
        }
        let window_ref = window.downgrade();
        let confirmed = confirmed.clone();
        confirm_discard(window).connect_response(Some("discard"), move |_, _| {
            confirmed.set(true);
            if let Some(window) = window_ref.upgrade() {
                window.close();
            }
        });
        glib::Propagation::Stop
    });
    window.present();
}

fn confirm_discard(window: &adw::ApplicationWindow) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(
        Some("Discard unsaved changes?"),
        Some("Your edits to this profile will be lost."),
    );
    dialog.add_responses(&[("cancel", "Keep Editing"), ("discard", "Discard")]);
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.present(Some(window));
    dialog
}

fn sync_profiles(ctx: &Context, menu: &gio::Menu, chooser: &gtk::MenuButton, entry: &gtk::Entry) {
    let editor = ctx.editor.borrow();
    let profile_name = editor.path.as_ref().and_then(|path| path.file_name());
    let text = profile_name.map_or("New profile".into(), |name| name.to_string_lossy());
    chooser.set_label(&text);
    let is_new = editor.path.is_none();
    let draft_name = editor.name.clone();
    let paths = editor.profile_paths();
    drop(editor);

    entry.set_visible(is_new);
    entry.set_text(if is_new { &draft_name } else { "" });
    menu.remove_all();
    for path in paths {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let item = gio::MenuItem::new(Some(&name.replace('_', "__")), None);
        item.set_action_and_target_value(
            Some("win.open-profile"),
            Some(&path.as_os_str().as_bytes().to_variant()),
        );
        menu.append_item(&item);
    }
    let create = gio::MenuItem::new(Some("New profile"), None);
    create.set_action_and_target_value(
        Some("win.open-profile"),
        Some(&Vec::<u8>::new().to_variant()),
    );
    let section = gio::Menu::new();
    section.append_item(&create);
    menu.append_section(None, &section);
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
        editor.invalid_gyro_sensitivity = [None, None];
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
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::*;

    pub(crate) fn descendants<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Vec<T> {
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
    fn profile_picker_switches_files_protects_drafts_and_lists_new_profiles() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let config = tempfile::tempdir().unwrap();
        let directory = config.path().join("profiles");
        std::fs::create_dir(&directory).unwrap();
        let startup = config.path().join("profile.toml");
        let game = directory.join("game_one.toml");
        std::fs::write(&startup, crate::NEW_PROFILE).unwrap();
        std::fs::write(&game, "version = 1\n# Game profile\n").unwrap();
        let editor = Rc::new(RefCell::new(
            Editor::open("/bin/true".into(), directory.clone(), Some(startup.clone())).unwrap(),
        ));
        let app = adw::Application::builder()
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        open_window(&app, editor.clone());
        let window = app
            .active_window()
            .unwrap()
            .downcast::<adw::ApplicationWindow>()
            .unwrap();
        let chooser = descendants::<gtk::MenuButton>(window.upcast_ref())
            .into_iter()
            .find(|button| button.tooltip_text().as_deref() == Some("Choose a profile to edit"))
            .unwrap();
        let menu = chooser.menu_model().unwrap();
        let target = |index| menu.item_attribute_value(index, "target", None).unwrap();
        assert_eq!(
            target(0).get::<Vec<u8>>().unwrap(),
            startup.as_os_str().as_bytes()
        );
        let open = window.lookup_action("open-profile").unwrap();
        let game_target = target(1);
        open.activate(Some(&game_target));
        assert_eq!(editor.borrow().path.as_deref(), Some(game.as_path()));
        assert!(editor.borrow().text.contains("# Game profile"));

        let new_profile = menu.item_link(2, "section").unwrap();
        let new_target = new_profile.item_attribute_value(0, "target", None).unwrap();
        editor.borrow_mut().text.push_str("# Unsaved change\n");
        let choose_draft_action = |label| {
            open.activate(Some(&new_target));
            let dialog = window.visible_dialog().unwrap();
            let button = descendants::<gtk::Button>(dialog.upcast_ref())
                .into_iter()
                .find(|button| button.label().as_deref() == Some(label))
                .unwrap();
            button.emit_clicked();
        };
        choose_draft_action("Keep Editing");
        assert!(editor.borrow().text.contains("# Unsaved change"));
        choose_draft_action("Discard");
        assert_eq!(editor.borrow().path, None);
        assert_eq!(editor.borrow().text, crate::NEW_PROFILE);

        let name = descendants::<gtk::Entry>(window.upcast_ref())
            .into_iter()
            .find(|entry| entry.placeholder_text().as_deref() == Some("Profile name"))
            .unwrap();
        name.set_text("New Game");
        window.lookup_action("save").unwrap().activate(None);
        let saved = directory.join("New Game.toml");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), crate::NEW_PROFILE);
        assert_eq!(
            target(1).get::<Vec<u8>>().unwrap(),
            saved.as_os_str().as_bytes()
        );
        assert!(!gtk::prelude::WidgetExt::is_visible(&name));
        window.close();
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

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn gyro_calibration_shortcut_can_be_set_and_removed() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let mut editor = Editor::open(PathBuf::new(), PathBuf::new(), None).unwrap();
        editor
            .text
            .push_str("[gyro.calibration_hold]\nbutton = 'l3'\nhold_ms = 2500\n");
        let ctx = Context {
            editor: Rc::new(RefCell::new(editor)),
            window: glib::WeakRef::new(),
            overlay: glib::WeakRef::new(),
        };
        let page = sensitivity::gyro_page(&ctx);
        let row = descendants::<adw::ComboRow>(&page)
            .into_iter()
            .find(|row| row.title() == "Hold button")
            .unwrap();
        let model = row.model().unwrap().downcast::<gtk::StringList>().unwrap();
        let ps = (0..model.n_items())
            .find(|&index| model.string(index).as_deref() == Some("PS button"))
            .unwrap();
        row.set_selected(ps);
        let doc = ctx.document().unwrap();
        assert_eq!(
            doc["gyro"]["calibration_hold"]["button"].as_str(),
            Some("ps")
        );
        assert_eq!(
            doc["gyro"]["calibration_hold"]["hold_ms"].as_integer(),
            Some(2500)
        );
        row.set_selected(0);
        assert!(
            ctx.document()
                .unwrap()
                .get("gyro")
                .and_then(|gyro| gyro.get("calibration_hold"))
                .is_none()
        );
    }

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn gyro_sliders_and_typed_values_use_independent_ranges() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let mut editor = Editor::open(PathBuf::new(), PathBuf::new(), None).unwrap();
        editor
            .text
            .push_str("[gyro]\nsensitivity_x = 42.125 # existing profile\nsensitivity_y = 4.0\n");
        let initial_text = editor.text.clone();
        let ctx = Context {
            editor: Rc::new(RefCell::new(editor)),
            window: glib::WeakRef::new(),
            overlay: glib::WeakRef::new(),
        };
        let controls = |page: &gtk::Widget, axis: &str| {
            let row = descendants::<adw::ActionRow>(page)
                .into_iter()
                .find(|row| row.title() == axis)
                .unwrap();
            let entry = descendants::<gtk::Entry>(row.upcast_ref()).pop().unwrap();
            let scale = descendants::<gtk::Scale>(row.upcast_ref()).pop().unwrap();
            (entry, scale)
        };
        let page = sensitivity::gyro_page(&ctx);
        let (entry, scale) = controls(&page, "Horizontal");
        assert_eq!(entry.text(), "42.125");
        assert_eq!(scale.adjustment().upper(), 20.0);
        assert_eq!(scale.value(), 20.0);
        assert_eq!(ctx.editor.borrow().text, initial_text);

        entry.set_text(" 26,875 ");
        let doc = ctx.document().unwrap();
        assert_eq!(doc["gyro"]["sensitivity_x"].as_float(), Some(26.875));
        assert_eq!(doc["gyro"]["sensitivity_y"].as_float(), Some(4.0));
        assert!(doc.to_string().contains("# existing profile"));
        assert_eq!(scale.value(), 20.0);
        entry.set_text("100");
        assert_eq!(
            ctx.document().unwrap()["gyro"]["sensitivity_x"].as_float(),
            Some(100.0)
        );

        entry.set_text("100.1");
        assert_eq!(
            ctx.editor.borrow().validate().unwrap_err(),
            sensitivity::GYRO_SENSITIVITY_ERROR
        );
        assert_eq!(
            ctx.document().unwrap()["gyro"]["sensitivity_x"].as_float(),
            Some(100.0)
        );
        let page = sensitivity::gyro_page(&ctx);
        let (entry, scale) = controls(&page, "Horizontal");
        assert_eq!(entry.text(), "100.1");
        assert!(entry.has_css_class("error"));
        scale.set_value(19.27);
        assert_eq!(entry.text(), "19.3");
        assert_eq!(
            ctx.document().unwrap()["gyro"]["sensitivity_x"].as_float(),
            Some(19.3)
        );
        assert!(ctx.editor.borrow().draft_error().is_none());

        let (vertical_entry, vertical_scale) = controls(&page, "Vertical");
        vertical_entry.set_text("8.125");
        assert_eq!(vertical_scale.value(), 8.125);
        assert_eq!(
            ctx.document().unwrap()["gyro"]["sensitivity_y"].as_float(),
            Some(8.125)
        );

        let window = gtk::Window::builder()
            .default_width(560)
            .default_height(640)
            .child(&page)
            .build();
        window.present();
        let main = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while scale.width() <= 0 || vertical_scale.width() <= 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "sliders were not allocated"
            );
            while main.pending() {
                main.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(scale.width(), vertical_scale.width());
        assert_eq!(
            scale.compute_bounds(&page).unwrap().x(),
            vertical_scale.compute_bounds(&page).unwrap().x(),
        );

        let link_switch = |page: &gtk::Widget| {
            descendants::<adw::SwitchRow>(page)
                .into_iter()
                .find(|row| row.title() == "Link horizontal and vertical")
                .unwrap()
        };
        let toggle = link_switch(&page);
        assert!(!toggle.is_active());
        toggle.set_active(true);
        assert_eq!(descendants::<gtk::Scale>(&page).len(), 1);
        let assert_axes = |horizontal, vertical| {
            let doc = ctx.document().unwrap();
            assert_eq!(doc["gyro"]["sensitivity_x"].as_float(), Some(horizontal));
            assert_eq!(doc["gyro"]["sensitivity_y"].as_float(), Some(vertical));
        };
        assert_axes(19.3, 19.3);
        let (entry, scale) = controls(&page, "Both axes");
        scale.set_value(7.26);
        assert_axes(7.3, 7.3);
        entry.set_text("31,25");
        assert_axes(31.25, 31.25);
        assert_eq!(scale.value(), 20.0);
        entry.set_text("invalid");
        assert!(!toggle.is_sensitive());
        assert!(ctx.editor.borrow().draft_error().is_some());

        let reopened = sensitivity::gyro_page(&ctx);
        let toggle = link_switch(&reopened);
        assert!(toggle.is_active());
        let (entry, _) = controls(&reopened, "Both axes");
        assert_eq!(entry.text(), "invalid");
        entry.set_text("31.25");
        toggle.set_active(false);
        assert_eq!(descendants::<gtk::Scale>(&reopened).len(), 2);
        let (entry, _) = controls(&reopened, "Vertical");
        entry.set_text("9.5");
        assert_axes(31.25, 9.5);
        let preview = descendants::<adw::PreferencesGroup>(&reopened)
            .into_iter()
            .find(|group| group.title() == "Flick Stick")
            .unwrap();
        assert_eq!(
            descendants::<gtk::DrawingArea>(preview.upcast_ref()).len(),
            2
        );
        for title in ["Physical input", "Flick Stick response"] {
            assert!(
                descendants::<gtk::Label>(preview.upcast_ref())
                    .iter()
                    .any(|label| label.text() == title)
            );
        }

        let acceleration = descendants::<adw::SwitchRow>(&reopened)
            .into_iter()
            .find(|row| row.title() == "Gyro acceleration")
            .unwrap();
        let settings = descendants::<adw::SpinRow>(&reopened);
        let setting = |title| settings.iter().find(|row| row.title() == title).unwrap();
        assert!(!acceleration.is_active());
        assert!(!setting("Acceleration start").is_sensitive());
        assert!(setting("Minimum speed").is_sensitive());
        acceleration.set_active(true);
        assert!(setting("Acceleration start").is_sensitive());
        assert_eq!(
            ctx.document().unwrap()["gyro"]["accel_enabled"].as_bool(),
            Some(true)
        );
        for (title, value) in [
            ("Minimum speed", 2.0),
            ("Acceleration start", 25.0),
            ("Acceleration end", 120.0),
            ("Acceleration multiplier", 2.5),
            ("Axial threshold", 12.0),
            ("Steering sensitivity", 0.35),
        ] {
            setting(title).set_value(value);
        }
        acceleration.set_active(false);
        assert!(!setting("Acceleration end").is_sensitive());
        assert!(!setting("Acceleration multiplier").is_sensitive());
        assert!(setting("Minimum speed").is_sensitive());
        assert_eq!(
            ctx.document().unwrap()["gyro"]["accel_enabled"].as_bool(),
            Some(false)
        );
        acceleration.set_active(true);
        assert_eq!(setting("Acceleration multiplier").value(), 2.5);
        acceleration.set_active(false);
        let doc = ctx.document().unwrap();
        for (key, value) in [
            ("threshold_dps", 2.0),
            ("accel_start_dps", 25.0),
            ("accel_end_dps", 120.0),
            ("accel_multiplier", 2.5),
            ("flick_axial_threshold_deg", 12.0),
            ("flick_steering_sensitivity", 0.35),
        ] {
            assert_eq!(doc["gyro"][key].as_float(), Some(value));
        }
        window.close();
    }

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn virtual_gamepad_type_and_trigger_stages_write_separate_bindings() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let mut editor = Editor::open(PathBuf::new(), PathBuf::new(), None).unwrap();
        editor.text.push_str("[bridge]\nrumble = true\n");
        editor.selected = "l2";
        let ctx = Context {
            editor: Rc::new(RefCell::new(editor)),
            window: glib::WeakRef::new(),
            overlay: glib::WeakRef::new(),
        };
        let page = controller::page(&ctx);
        let select = |row: &adw::ComboRow, label: &str| {
            let model = row.model().unwrap().downcast::<gtk::StringList>().unwrap();
            let index = (0..model.n_items())
                .find(|&index| model.string(index).as_deref() == Some(label))
                .unwrap();
            row.set_selected(index);
        };
        let row = descendants::<adw::ComboRow>(&page)
            .into_iter()
            .find(|row| row.title() == "Virtual gamepad type")
            .unwrap();
        select(&row, "Xbox Series X|S");
        let groups = descendants::<adw::PreferencesGroup>(&page);
        for (stage, key) in [("Partial pull", "Q"), ("Full pull", "E")] {
            let group = groups.iter().find(|group| group.title() == stage).unwrap();
            let row = descendants::<adw::ComboRow>(group.upcast_ref())
                .into_iter()
                .find(|row| row.title() == "Hold")
                .unwrap();
            select(&row, key);
        }
        let keep_partial = |page: &gtk::Widget| {
            descendants::<adw::SwitchRow>(page)
                .into_iter()
                .find(|row| row.title() == "Keep partial action at full pull")
                .unwrap()
        };
        let l2 = keep_partial(&page);
        assert!(!l2.is_active());
        l2.set_active(true);
        ctx.editor.borrow_mut().selected = "r2";
        let r2_page = controller::page(&ctx);
        let r2 = keep_partial(&r2_page);
        assert!(!r2.is_active());
        r2.set_active(true);
        l2.set_active(false);

        let doc = ctx.document().unwrap();
        assert_eq!(doc["keyboard"]["keep_partial"]["l2"].as_bool(), Some(false));
        assert_eq!(doc["keyboard"]["keep_partial"]["r2"].as_bool(), Some(true));
        assert_eq!(doc["bridge"]["gamepad"].as_str(), Some("xbox-series"));
        assert_eq!(doc["bridge"]["rumble"].as_bool(), Some(true));
        let bindings = doc["keyboard"]["bindings"].as_array_of_tables().unwrap();
        let partial = bindings.get(0).unwrap();
        let full = bindings.get(1).unwrap();
        assert_eq!(partial["trigger"].as_str(), Some("l2:0.45:0.35:0.95:0.85"));
        assert_eq!(full["trigger"].as_str(), Some("l2:0.95:0.85"));
        assert_eq!(partial["hold"].as_str(), Some("KEY_Q"));
        assert_eq!(full["hold"].as_str(), Some("KEY_E"));

        select(&row, "Use bridge default");
        let doc = ctx.document().unwrap();
        assert!(doc["bridge"].get("gamepad").is_none());
        assert_eq!(doc["bridge"]["rumble"].as_bool(), Some(true));
    }
}

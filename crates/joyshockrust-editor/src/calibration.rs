use std::{
    cell::{Cell, RefCell},
    error::Error,
    ffi::{OsStr, OsString},
    path::PathBuf,
    rc::Rc,
};

use adw::{gio, glib, gtk, prelude::*};

pub fn run(bridge: PathBuf, profile: PathBuf, node: PathBuf) -> Result<(), Box<dyn Error>> {
    gtk::gdk::set_allowed_backends("wayland");
    let application = adw::Application::builder()
        .application_id("io.github.aapokaapo.JoyShockRust.Calibration")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_activate(move |application| {
        open_window(application, bridge.clone(), profile.clone(), node.clone());
    });
    if application.run_with_args(&["joyshockrust-editor"]) != glib::ExitCode::SUCCESS {
        return Err("Could not start calibration in a Wayland session".into());
    }
    Ok(())
}

struct Calibration {
    bridge: PathBuf,
    profile: PathBuf,
    node: PathBuf,
    window: glib::WeakRef<adw::ApplicationWindow>,
    page: glib::WeakRef<adw::PreferencesPage>,
    group: RefCell<glib::WeakRef<adw::PreferencesGroup>>,
    worker: RefCell<Option<gio::Subprocess>>,
    closing: Cell<bool>,
    saving: Cell<bool>,
}

fn open_window(
    application: &adw::Application,
    bridge: PathBuf,
    profile: PathBuf,
    node: PathBuf,
) -> adw::ApplicationWindow {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Game Sensitivity Calibration")
        .default_width(560)
        .default_height(390)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    toolbar.set_content(Some(&page));
    window.set_content(Some(&toolbar));
    let session = Rc::new(Calibration {
        bridge,
        profile,
        node,
        window: window.downgrade(),
        page: page.downgrade(),
        group: RefCell::new(glib::WeakRef::new()),
        worker: RefCell::new(None),
        closing: Cell::new(false),
        saving: Cell::new(false),
    });
    let on_close = session.clone();
    window.connect_close_request(move |_| {
        if on_close.saving.get() {
            return glib::Propagation::Stop;
        }
        if let Some(worker) = on_close.worker.borrow().as_ref() {
            on_close.closing.set(true);
            worker.force_exit();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    let sensitivity = std::fs::read_to_string(&session.profile)
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| {
            let value = doc.get("gyro")?.get("in_game_sens")?;
            value
                .as_float()
                .or_else(|| value.as_integer().map(|number| number as f64))
        })
        .unwrap_or(1.0);
    session.setup(sensitivity, None);
    window.present();
    window
}

impl Calibration {
    fn group(&self, title: &str, description: &str) -> Option<adw::PreferencesGroup> {
        let page = self.page.upgrade()?;
        // Each step adds a single group. Keep the scroll container and header in place.
        if let Some(previous) = self.group.borrow().upgrade() {
            page.remove(&previous);
        }
        let group = adw::PreferencesGroup::builder()
            .title(title)
            .description(description)
            .build();
        page.add(&group);
        self.group.replace(group.downgrade());
        Some(group)
    }

    fn setup(self: &Rc<Self>, sensitivity: f64, message: Option<&str>) {
        let filename = self
            .profile
            .file_name()
            .unwrap_or(self.profile.as_os_str())
            .to_string_lossy();
        let Some(group) = self.group("Prepare calibration", &format!("Profile “{filename}”"))
        else {
            return;
        };
        let entry = input(
            &group,
            "In-game mouse sensitivity",
            &sensitivity.to_string(),
        );
        let warning = detail(&group, message.unwrap_or_default(), true);
        warning.set_visible(message.is_some());
        let start = button(&group, "Start Measurement", "suggested-action");
        let session = self.clone();
        start.connect_clicked(move |_| {
            let text = entry.text().trim().replace(',', ".");
            let Some(sensitivity) = positive_decimal(&text) else {
                warning
                    .set_text("Enter the game's mouse sensitivity as a number greater than zero.");
                warning.set_visible(true);
                return;
            };
            session.measure(sensitivity);
        });
    }

    fn measure(self: &Rc<Self>, sensitivity: f64) {
        let Some(group) = self.group(
            "Measure whole turns",
            "Focus the game. Aim at a landmark and press Options with the right stick centered.",
        ) else {
            return;
        };
        detail(
            &group,
            "Tilt the right stick up, then sweep it around its edge until the camera completes one or more whole turns. At the landmark, center the stick and press Options again. Remember how many turns you completed.",
            false,
        );
        let status = adw::ActionRow::builder()
            .title("Measurement status")
            .subtitle("Opening the controller and calibration mouse…")
            .build();
        group.add(&status);
        let cancel = button(&group, "Cancel", "");
        let window = self.window.clone();
        cancel.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
        let args = [
            OsString::from("--measure-stick"),
            sensitivity.to_string().into(),
            self.node.as_os_str().to_owned(),
            std::process::id().to_string().into(),
        ];
        let session = self.clone();
        self.launch(
            &args,
            move |line| {
                let message = match line {
                    "ready" => "Ready. Center the right stick and press Options.",
                    "started" => "Measuring. Sweep the stick around its edge.",
                    "center-stick" => "Center the right stick, then press Options again.",
                    "no-motion" => "No movement recorded. Turn the camera before confirming.",
                    _ => return false,
                };
                status.set_subtitle(message);
                true
            },
            move |reply| match reply.and_then(|line| {
                line.strip_prefix("counts ")
                    .and_then(|counts| counts.parse::<i32>().ok())
                    .filter(|&counts| counts > 0)
                    .ok_or_else(|| "The measurement returned no mouse counts.".to_owned())
            }) {
                Ok(counts) => session.confirm(sensitivity, counts, 1, None),
                Err(error) => session.setup(sensitivity, Some(&error)),
            },
        );
    }

    fn confirm(self: &Rc<Self>, sensitivity: f64, counts: i32, turns: u32, message: Option<&str>) {
        let Some(group) = self.group(
            "Enter whole turns",
            "Enter the number of complete camera turns you counted.",
        ) else {
            return;
        };
        let entry = input(&group, "Complete turns", &turns.to_string());
        let warning = detail(&group, message.unwrap_or_default(), true);
        warning.set_visible(message.is_some());
        let save = button(&group, "Save Calibration", "suggested-action");
        let restart = button(&group, "Measure Again", "");
        let session = self.clone();
        restart.connect_clicked(move |_| session.setup(sensitivity, None));
        let session = self.clone();
        save.connect_clicked(move |_| {
            let Some(turns) = entry
                .text()
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|&turns| turns > 0)
            else {
                warning.set_text("Enter a positive whole number.");
                warning.set_visible(true);
                return;
            };
            session.save(sensitivity, counts, turns);
        });
    }

    fn save(self: &Rc<Self>, sensitivity: f64, counts: i32, turns: u32) {
        self.group(
            "Saving calibration",
            "Writing the measured sensitivity to the profile.",
        );
        self.saving.set(true);
        let args = [
            OsString::from("--save-measurement"),
            self.profile.as_os_str().to_owned(),
            counts.to_string().into(),
            sensitivity.to_string().into(),
            turns.to_string().into(),
        ];
        let session = self.clone();
        self.launch(
            &args,
            |_| false,
            move |reply| {
                session.saving.set(false);
                match reply.and_then(|text| {
                    positive_decimal(text.trim())
                        .ok_or_else(|| "The result could not be read.".to_owned())
                }) {
                    Ok(calibration) => session.finished(calibration, sensitivity),
                    Err(error) => session.confirm(sensitivity, counts, turns, Some(&error)),
                }
            },
        );
    }

    fn finished(&self, calibration: f64, sensitivity: f64) {
        let Some(group) = self.group("Calibration saved", "Close the window to resume the controller. Select the updated profile from the tray to apply it.") else { return };
        for (title, value) in [
            ("Real-world calibration", calibration),
            ("In-game mouse sensitivity", sensitivity),
        ] {
            group.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(value.to_string())
                    .build(),
            );
        }
        let done = button(&group, "Done", "suggested-action");
        let window = self.window.clone();
        done.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }

    fn launch(
        self: &Rc<Self>,
        args: &[OsString],
        mut progress: impl FnMut(&str) -> bool + 'static,
        finished: impl FnOnce(Result<String, String>) + 'static,
    ) {
        let argv: Vec<&OsStr> = std::iter::once(self.bridge.as_os_str())
            .chain(args.iter().map(OsString::as_os_str))
            .collect();
        let flags = gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE;
        let worker = match gio::Subprocess::newv(&argv, flags) {
            Ok(worker) => worker,
            Err(error) => {
                finished(Err(error.to_string()));
                return;
            }
        };
        self.worker.replace(Some(worker.clone()));
        let session = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let stream = gio::DataInputStream::new(&worker.stdout_pipe().expect("piped stdout"));
            let mut last = String::new();
            let result = loop {
                match stream.read_line_utf8_future(glib::Priority::DEFAULT).await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if !line.is_empty() && !progress(line) {
                            last = line.to_owned();
                        }
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => {
                        worker.force_exit();
                        break Err(error);
                    }
                }
            };
            // Drain the stream and reap the process before the tray may resume outputs.
            let waited = worker.wait_future().await;
            session.worker.take();
            if session.closing.get() {
                if let Some(window) = session.window.upgrade() {
                    window.close();
                }
                return;
            }
            finished(
                result
                    .and(waited)
                    .map_err(|error| error.to_string())
                    .and_then(|()| {
                        if worker.is_successful() {
                            Ok(last)
                        } else if last.is_empty() {
                            Err("Calibration process exited.".into())
                        } else {
                            Err(last)
                        }
                    }),
            );
        });
    }
}

fn positive_decimal(text: &str) -> Option<f64> {
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn input(group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::EntryRow {
    let input = adw::EntryRow::builder()
        .title(title)
        .input_purpose(gtk::InputPurpose::Number)
        .build();
    input.set_text(value);
    group.add(&input);
    input
}

fn detail(group: &adw::PreferencesGroup, text: &str, error: bool) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .margin_top(10)
        .margin_bottom(10)
        .build();
    if error {
        label.add_css_class("error");
    }
    group.add(&label);
    label
}

fn button(group: &adw::PreferencesGroup, text: &str, style: &str) -> gtk::Button {
    let button = gtk::Button::with_label(text);
    if !style.is_empty() {
        button.add_css_class(style);
    }
    button.set_halign(gtk::Align::End);
    button.set_margin_top(12);
    group.add(&button);
    button
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::ui::tests::descendants;

    fn wait_for(condition: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "calibration window did not advance"
            );
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn calibration_window_completes_and_cancels() {
        gtk::gdk::set_allowed_backends("wayland");
        adw::init().unwrap();
        let application = adw::Application::builder()
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        application.register(None::<&gio::Cancellable>).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let helper = directory.path().join("calibration-helper");
        fs::write(
            &helper,
            r#"#!/bin/sh
wait_for_gate() {
    attempts=0
    while [ ! -f "$1" ] && [ "$attempts" -lt 250 ]; do
        sleep 0.02
        attempts=$((attempts + 1))
    done
    [ -f "$1" ] || exit 1
}
case "$1" in
    --measure-stick)
        [ "$2" = 2 ] || exit 1
        if [ "$3" = cancel ]; then exec sleep 60; fi
        printf 'ready\n'
        wait_for_gate "$3.ready"
        printf 'center-stick\n'
        wait_for_gate "$3.centered"
        printf 'started\n'
        wait_for_gate "$3.swept"
        printf 'no-motion\n'
        wait_for_gate "$3.confirmed"
        printf 'counts 28800\n'
        ;;
    --save-measurement)
        [ "$3" = 28800 ] && [ "$4" = 2 ] && [ "$5" = 4 ] || exit 1
        printf '40.0\n'
        ;;
    *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        let profile = directory.path().join("profile.toml");
        fs::write(&profile, "version = 1\n[gyro]\nin_game_sens = 2.0\n").unwrap();

        let open =
            |node: &str| open_window(&application, helper.clone(), profile.clone(), node.into());
        let find_button = |window: &adw::ApplicationWindow, name| {
            descendants::<gtk::Button>(window.upcast_ref())
                .into_iter()
                .find(|button| button.label().as_deref() == Some(name))
        };
        let node = directory.path().join("measure");
        let window = open(node.to_str().unwrap());
        find_button(&window, "Start Measurement")
            .unwrap()
            .emit_clicked();
        for (message, gate) in [
            ("Ready.", "ready"),
            ("Center the right stick", "centered"),
            ("Measuring.", "swept"),
            ("No movement recorded.", "confirmed"),
        ] {
            wait_for(|| {
                descendants::<adw::ActionRow>(window.upcast_ref())
                    .iter()
                    .any(|row| {
                        row.title() == "Measurement status"
                            && row
                                .subtitle()
                                .is_some_and(|subtitle| subtitle.starts_with(message))
                    })
            });
            assert!(find_button(&window, "Save Calibration").is_none());
            fs::write(node.with_extension(gate), "").unwrap();
        }
        wait_for(|| find_button(&window, "Save Calibration").is_some());
        let turns = descendants::<adw::EntryRow>(window.upcast_ref())
            .into_iter()
            .find(|row| row.title() == "Complete turns")
            .unwrap();
        turns.set_text("4");
        find_button(&window, "Save Calibration")
            .unwrap()
            .emit_clicked();
        wait_for(|| find_button(&window, "Done").is_some());
        find_button(&window, "Done").unwrap().emit_clicked();
        assert!(!window.is_visible());

        let window = open("cancel");
        find_button(&window, "Start Measurement")
            .unwrap()
            .emit_clicked();
        find_button(&window, "Cancel").unwrap().emit_clicked();
        wait_for(|| !window.is_visible());
        assert!(find_button(&window, "Save Calibration").is_none());
    }
}

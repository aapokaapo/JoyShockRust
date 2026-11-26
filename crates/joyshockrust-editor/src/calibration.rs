use std::{
    cell::{Cell, RefCell},
    error::Error,
    path::{Path, PathBuf},
    rc::Rc,
};

use adw::{gio, glib, gtk, prelude::*};
use joyshockrust_ipc::calibration::{
    BridgeMessage, ClientMessage, CloseReason, MAX_LINE_BYTES, Progress, SessionKind,
};
use joyshockrust_motion::game_calibration::calculate;

use crate::calibration_navigation::{self, Control};

/// Run a calibration window that drives a bridge calibration session at `socket`.
pub fn run(
    socket: PathBuf,
    game: Option<GameProfiles>,
    node: PathBuf,
) -> Result<(), Box<dyn Error>> {
    gtk::gdk::set_allowed_backends("wayland");
    let application = adw::Application::builder()
        .application_id("io.github.aapokaapo.JoyShockRust.Calibration")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    application.connect_activate(move |application| {
        let (window, session) =
            open_window(application, socket.clone(), game.clone(), node.clone());
        session.watch_controller(&window);
    });
    if application.run_with_args(&["joyshockrust-editor"]) != glib::ExitCode::SUCCESS {
        return Err("Could not start calibration in a Wayland session".into());
    }
    Ok(())
}

#[derive(Clone)]
pub struct GameProfiles {
    pub active: Option<PathBuf>,
    pub available: Vec<PathBuf>,
}

impl GameProfiles {
    fn gyro_number(&self, key: &str) -> Option<f64> {
        let text = std::fs::read_to_string(self.active.as_ref()?).ok()?;
        let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
        let value = doc.get("gyro")?.get(key)?;
        value
            .as_float()
            .or_else(|| value.as_integer().map(|number| number as f64))
    }

    fn choices(&self, previous: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for path in previous
            .into_iter()
            .chain(self.active.as_deref())
            .chain(self.available.iter().map(PathBuf::as_path))
        {
            if path.is_file() && !paths.iter().any(|candidate: &PathBuf| candidate == path) {
                paths.push(path.to_owned());
            }
        }
        paths
    }
}

/// The window's end of a bridge calibration session.
#[derive(Clone)]
struct Connection {
    stream: gio::SocketConnection,
    input: gio::DataInputStream,
}

struct Calibration {
    socket: PathBuf,
    game: Option<GameProfiles>,
    node: PathBuf,
    window: glib::WeakRef<adw::ApplicationWindow>,
    page: glib::WeakRef<adw::PreferencesPage>,
    group: RefCell<glib::WeakRef<adw::PreferencesGroup>>,
    connection: RefCell<Option<Connection>>,
    saving: Cell<bool>,
    controls: RefCell<Vec<Control>>,
    navigation_hint: glib::WeakRef<gtk::Label>,
}

/// How a session request ended.
enum Outcome {
    Reply(BridgeMessage),
    /// The request failed and the session continues.
    Failed(String),
}

fn open_window(
    application: &adw::Application,
    socket: PathBuf,
    game: Option<GameProfiles>,
    node: PathBuf,
) -> (adw::ApplicationWindow, Rc<Calibration>) {
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title(if game.is_some() {
            "Game Sensitivity Calibration"
        } else {
            "Controller Gyro Calibration"
        })
        .default_width(560)
        .default_height(390)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();
    toolbar.set_content(Some(&page));
    let hint = gtk::Label::builder()
        .label(if game.is_some() {
            "D-pad: move or change value · L1/R1: fine sensitivity · Cross: select · Circle: close"
        } else {
            "D-pad: move · Cross: select · Circle: close"
        })
        .wrap(true)
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(8)
        .css_classes(["dim-label"])
        .build();
    toolbar.add_bottom_bar(&hint);
    window.set_content(Some(&toolbar));
    let session = Rc::new(Calibration {
        socket,
        game,
        node,
        window: window.downgrade(),
        page: page.downgrade(),
        group: RefCell::new(glib::WeakRef::new()),
        connection: RefCell::new(None),
        saving: Cell::new(false),
        controls: RefCell::new(Vec::new()),
        navigation_hint: hint.downgrade(),
    });
    let on_close = session.clone();
    window.connect_close_request(move |_| {
        if on_close.saving.get() {
            return glib::Propagation::Stop;
        }
        // Closing the connection ends the session. The bridge stops any
        // measurement and restores the outputs it paused.
        on_close.disconnect();
        glib::Propagation::Proceed
    });
    session.begin();
    window.present();
    (window, session)
}

impl Calibration {
    fn watch_controller(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        let session = Rc::downgrade(self);
        let hint = self.navigation_hint.clone();
        let unavailable = move |error: &std::io::Error| {
            if let Some(hint) = hint.upgrade() {
                hint.set_text(&format!("Controller navigation unavailable. {error}"));
            }
        };
        if let Err(error) = calibration_navigation::watch(
            window,
            &self.node,
            move |command| {
                if let Some(session) = session.upgrade() {
                    session.navigate(command);
                }
            },
            unavailable.clone(),
        ) {
            unavailable(&error);
        }
    }

    fn navigate(&self, command: &str) {
        if let Some(window) = self.window.upgrade()
            && window.is_active()
        {
            self.navigate_window(&window, command);
        }
    }

    fn navigate_window(&self, window: &adw::ApplicationWindow, command: &str) {
        // Release the borrow before an action can replace the current group.
        let controls = self.controls.borrow().clone();
        calibration_navigation::navigate(window, &controls, command);
    }

    fn button(&self, group: &adw::PreferencesGroup, title: &str, style: &str) -> gtk::Button {
        let button = button(group, title, style);
        self.controls
            .borrow_mut()
            .push(Control::Button(button.downgrade()));
        button
    }

    fn group(&self, title: &str, description: &str) -> Option<adw::PreferencesGroup> {
        let page = self.page.upgrade()?;
        self.controls.borrow_mut().clear();
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

    fn is_open(&self) -> bool {
        self.window
            .upgrade()
            .is_some_and(|window| window.is_visible())
    }

    fn disconnect(&self) {
        if let Some(connection) = self.connection.take() {
            let _ = connection.stream.close(None::<&gio::Cancellable>);
        }
    }

    /// Open the bridge session, which pauses outputs for the calibration.
    fn begin(self: &Rc<Self>) {
        if self
            .group(
                "Preparing the controller",
                "Pausing virtual outputs for calibration…",
            )
            .is_none()
        {
            return;
        }
        let kind = if self.game.is_some() {
            SessionKind::Game
        } else {
            SessionKind::Gyro
        };
        let session = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = session.connect(kind).await;
            if !session.is_open() {
                session.disconnect();
                return;
            }
            match result {
                Ok(()) => {
                    if let Some(game) = &session.game {
                        let sensitivity = game.gyro_number("in_game_sens").unwrap_or(1.0);
                        session.setup(sensitivity, None);
                    } else {
                        session.setup_gyro(None);
                    }
                    // The first step arrives after the window was shown, so
                    // give it the initial focus the window would have chosen.
                    let first = session.controls.borrow().first().cloned();
                    if let Some(first) = first {
                        first.focus();
                    }
                }
                Err(error) => session.ended("Calibration unavailable", &error),
            }
        });
    }

    async fn connect(&self, kind: SessionKind) -> Result<(), String> {
        let unavailable = |error: &dyn std::fmt::Display| {
            format!("Could not reach the bridge. Start JoyShockRust and try again. ({error})")
        };
        let begin = ClientMessage::Begin {
            kind,
            node: std::path::absolute(&self.node).map_err(|error| error.to_string())?,
        }
        .encode()
        .map_err(str::to_owned)?;
        let address = gio::UnixSocketAddress::new(&self.socket);
        let stream = gio::SocketClient::new()
            .connect_future(&address)
            .await
            .map_err(|error| unavailable(&error))?;
        let input = gio::DataInputStream::new(&stream.input_stream());
        let connection = Connection { stream, input };
        let reply = exchange(&connection, &begin, |_| {}).await;
        match reply {
            Ok(BridgeMessage::Started { .. }) => {
                self.connection.replace(Some(connection));
                Ok(())
            }
            Ok(BridgeMessage::Busy) => Err("Another calibration is already in progress.".into()),
            Ok(BridgeMessage::Rejected(reason) | BridgeMessage::Failed(reason)) => Err(reason),
            Ok(_) => Err("The bridge sent an unexpected reply.".into()),
            Err(error) => Err(unavailable(&error)),
        }
    }

    /// Send one request and wait for its result, forwarding progress.
    fn request(
        self: &Rc<Self>,
        message: ClientMessage,
        progress: impl FnMut(Progress) + 'static,
        finished: impl FnOnce(Outcome) + 'static,
    ) {
        let line = match message.encode() {
            Ok(line) => line,
            Err(error) => {
                finished(Outcome::Failed(error.to_owned()));
                return;
            }
        };
        let session = self.clone();
        glib::MainContext::default().spawn_local(async move {
            // Keep the session reachable so closing the window can end it
            // while a request is waiting.
            let Some(connection) = session.connection.borrow().clone() else {
                session.ended(
                    "Calibration ended",
                    "The calibration session is no longer active.",
                );
                return;
            };
            let reply = exchange(&connection, &line, progress).await;
            if !session.is_open() {
                return;
            }
            match reply {
                Ok(BridgeMessage::Closed(CloseReason::Idle)) => session.ended(
                    "Calibration ended",
                    "The session was idle for too long. Virtual outputs were restored.",
                ),
                Ok(reply) => {
                    let outcome = match reply {
                        BridgeMessage::Rejected(reason) | BridgeMessage::Failed(reason) => {
                            Outcome::Failed(reason)
                        }
                        BridgeMessage::Cancelled => {
                            Outcome::Failed("The measurement was cancelled.".into())
                        }
                        reply => Outcome::Reply(reply),
                    };
                    finished(outcome);
                }
                Err(_) => session.ended(
                    "Calibration ended",
                    "The bridge stopped, so calibration was cancelled. Nothing was saved from this step.",
                ),
            }
        });
    }

    /// Show a final page after the session can no longer continue.
    fn ended(&self, title: &str, message: &str) {
        self.saving.set(false);
        self.disconnect();
        if let Some(group) = self.group(title, "") {
            detail(&group, message, true);
            self.close_button(&group, "Close", "suggested-action");
        }
    }

    fn setup_gyro(self: &Rc<Self>, error: Option<&str>) {
        let Some(group) = self.group("Prepare the controller", "Set the controller down on a flat, stable surface. Leave it untouched while the gyroscope is measured.") else { return };
        if let Some(error) = error {
            detail(&group, error, true);
        }
        let start = self.button(&group, "Start Gyro Calibration", "suggested-action");
        let session = self.clone();
        start.connect_clicked(move |_| session.measure_gyro());
    }

    fn measure_gyro(self: &Rc<Self>) {
        let Some(group) = self.group(
            "Measuring the gyroscope",
            "Keep the controller still until the measurement completes.",
        ) else {
            return;
        };
        let status = detail(&group, "Opening the controller…", false);
        self.close_button(&group, "Cancel", "");
        let session = self.clone();
        self.request(
            ClientMessage::MeasureGyro,
            move |progress| {
                if progress == Progress::Ready {
                    status.set_text(&format!(
                        "Waiting for {} seconds of steady readings.",
                        joyshockrust_motion::StationaryCalibration::SAMPLE_DURATION.as_secs(),
                    ));
                }
            },
            move |outcome| match outcome {
                Outcome::Reply(BridgeMessage::GyroMeasured { measurement, .. }) => {
                    session.confirm_gyro(measurement, None)
                }
                Outcome::Reply(_) => {
                    session.setup_gyro(Some("The controller did not return a gyro correction."))
                }
                Outcome::Failed(error) => session.setup_gyro(Some(&error)),
            },
        );
    }

    fn confirm_gyro(self: &Rc<Self>, measurement: u64, error: Option<&str>) {
        let Some(group) = self.group("Measurement complete", "Save the correction for this controller. It will be used when the bridge resumes gyro output.") else { return };
        if let Some(error) = error {
            detail(&group, error, true);
        }
        self.close_button(&group, "Cancel", "");
        let save = self.button(&group, "Save Gyro Calibration", "suggested-action");
        let session = self.clone();
        save.connect_clicked(move |_| {
            session.group(
                "Saving gyro calibration",
                "Writing the correction for this controller.",
            );
            session.saving.set(true);
            let done = session.clone();
            session.request(
                ClientMessage::SaveGyro { measurement },
                |_| {},
                move |outcome| {
                    done.saving.set(false);
                    match outcome {
                        Outcome::Reply(BridgeMessage::GyroSaved) => {
                            if let Some(group) = done.group(
                                "Gyro calibration saved",
                                "Select Done to end calibration and restore virtual outputs.",
                            ) {
                                done.close_button(&group, "Done", "suggested-action");
                            }
                        }
                        Outcome::Reply(_) => done.confirm_gyro(
                            measurement,
                            Some("The bridge sent an unexpected reply."),
                        ),
                        Outcome::Failed(error) => done.confirm_gyro(measurement, Some(&error)),
                    }
                },
            );
        });
    }

    fn close_button(&self, group: &adw::PreferencesGroup, title: &str, style: &str) {
        let close = self.button(group, title, style);
        let window = self.window.clone();
        close.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }

    fn setup(self: &Rc<Self>, sensitivity: f64, message: Option<&str>) {
        let Some(group) = self.group(
            "Prepare calibration",
            "You can choose where to save the calibration after measuring.",
        ) else {
            return;
        };
        let entry = input(
            &group,
            "In-game mouse sensitivity",
            &sensitivity.to_string(),
        );
        self.controls
            .borrow_mut()
            .push(Control::Sensitivity(entry.downgrade()));
        let warning = detail(&group, message.unwrap_or_default(), true);
        warning.set_visible(message.is_some());
        let start = self.button(&group, "Start Measurement", "suggested-action");
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
        self.close_button(&group, "Cancel", "");
        let session = self.clone();
        self.request(
            ClientMessage::MeasureGame { sensitivity },
            move |progress| {
                status.set_subtitle(match progress {
                    Progress::Ready => "Ready. Center the right stick and press Options.",
                    Progress::Started => "Measuring. Sweep the stick around its edge.",
                    Progress::CenterStick => "Center the right stick, then press Options again.",
                    Progress::NoMotion => {
                        "No movement recorded. Turn the camera before confirming."
                    }
                });
            },
            move |outcome| match outcome {
                Outcome::Reply(BridgeMessage::GameMeasured {
                    measurement,
                    counts,
                }) if counts > 0 => {
                    let measured = Measured {
                        id: measurement,
                        sensitivity,
                        counts,
                    };
                    session.confirm(measured, 1, None, None)
                }
                Outcome::Reply(_) => session.setup(
                    sensitivity,
                    Some("The measurement returned no mouse counts."),
                ),
                Outcome::Failed(error) => session.setup(sensitivity, Some(&error)),
            },
        );
    }

    fn confirm(
        self: &Rc<Self>,
        measured: Measured,
        turns: u32,
        previous: Option<&Path>,
        message: Option<&str>,
    ) {
        let Some(group) = self.group(
            "Save game calibration",
            "Enter the number of complete camera turns and choose a profile.",
        ) else {
            return;
        };
        let Measured {
            sensitivity,
            counts,
            ..
        } = measured;
        let entry = input(&group, "Complete turns", &turns.to_string());
        self.controls
            .borrow_mut()
            .push(Control::Turns(entry.downgrade()));
        let result = adw::ActionRow::builder()
            .title("Real-world calibration")
            .build();
        let refresh_result = move |entry: &adw::EntryRow, result: &adw::ActionRow| {
            let value = measured_calibration(counts, sensitivity, entry.text().as_str())
                .map_or_else(
                    |_| "—".to_owned(),
                    |(_, calibration)| calibration.to_string(),
                );
            result.set_subtitle(&value);
        };
        refresh_result(&entry, &result);
        group.add(&result);
        let result = result.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(result) = result.upgrade() {
                refresh_result(entry, &result);
            }
        });
        group.add(
            &adw::ActionRow::builder()
                .title("In-game mouse sensitivity")
                .subtitle(sensitivity.to_string())
                .build(),
        );
        let game = self.game.as_ref().expect("game calibration");
        let profiles = game.choices(previous);
        let mut labels = vec!["Choose a profile".to_owned()];
        labels.extend(profiles.iter().map(|path| {
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            let duplicate = profiles
                .iter()
                .filter(|other| other.file_name() == path.file_name())
                .count()
                > 1;
            let name = if duplicate {
                path.display().to_string()
            } else {
                name.into_owned()
            };
            if Some(path) == game.active.as_ref() {
                format!("{name} (Active)")
            } else {
                name
            }
        }));
        let selected = previous
            .or(game.active.as_deref())
            .and_then(|path| profiles.iter().position(|candidate| candidate == path))
            .map_or(0, |index| index as u32 + 1);
        let labels: Vec<_> = labels.iter().map(String::as_str).collect();
        let destination = adw::ComboRow::builder()
            .title("Profile")
            .model(&gtk::StringList::new(&labels))
            .selected(selected)
            .build();
        destination.set_visible(!profiles.is_empty());
        group.add(&destination);
        self.controls
            .borrow_mut()
            .push(Control::Profile(destination.downgrade()));
        let warning = detail(&group, message.unwrap_or_default(), true);
        warning.set_visible(message.is_some());
        let save = self.button(&group, "Save Calibration", "suggested-action");
        save.set_visible(!profiles.is_empty());
        save.set_sensitive(selected != 0);
        let save_ref = save.downgrade();
        destination.connect_selected_notify(move |row| {
            if let Some(save) = save_ref.upgrade() {
                save.set_sensitive(row.selected() != 0);
            }
        });
        let confirm_turns = move || match measured_calibration(counts, sensitivity, &entry.text()) {
            Ok((turns, _)) => Some(turns),
            Err(error) => {
                warning.set_text(error);
                warning.set_visible(true);
                None
            }
        };
        let restart = self.button(&group, "Measure Again", "");
        let session = self.clone();
        restart.connect_clicked(move |_| session.setup(sensitivity, None));
        if profiles.is_empty() {
            detail(
                &group,
                "Use Browse Profiles with the keyboard or mouse to choose an existing profile.",
                false,
            );
        }
        let browse = button(&group, "Browse Profiles…", "");
        let session = self.clone();
        let browse_turns = confirm_turns.clone();
        browse.connect_clicked(move |_| {
            if let Some(turns) = browse_turns() {
                session.browse(measured, turns);
            }
        });
        let session = self.clone();
        save.connect_clicked(move |_| {
            if let Some(path) = destination
                .selected()
                .checked_sub(1)
                .and_then(|index| profiles.get(index as usize))
                && let Some(turns) = confirm_turns()
            {
                session.save(measured, turns, path.clone());
            }
        });
    }

    fn browse(self: &Rc<Self>, measured: Measured, turns: u32) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let chooser = gtk::FileChooserNative::new(
            Some("Choose a Profile"),
            Some(&window),
            gtk::FileChooserAction::Open,
            Some("Save Calibration"),
            Some("Cancel"),
        );
        chooser.set_modal(true);
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("TOML profiles"));
        filter.add_pattern("*.toml");
        chooser.add_filter(&filter);
        let session = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if chooser.run_future().await == gtk::ResponseType::Accept
                && session
                    .window
                    .upgrade()
                    .is_some_and(|window| window.is_visible())
                && let Some(path) = chooser.file().and_then(|file| file.path())
            {
                session.save(measured, turns, path);
            }
        });
    }

    fn save(self: &Rc<Self>, measured: Measured, turns: u32, path: PathBuf) {
        if self
            .group(
                "Saving calibration",
                "Writing the measured sensitivity to the profile.",
            )
            .is_none()
        {
            return;
        }
        self.saving.set(true);
        let session = self.clone();
        let profile = std::path::absolute(&path).unwrap_or_else(|_| path.clone());
        self.request(
            ClientMessage::SaveGame {
                measurement: measured.id,
                turns,
                profile,
            },
            |_| {},
            move |outcome| {
                session.saving.set(false);
                match outcome {
                    Outcome::Reply(BridgeMessage::GameSaved { calibration }) => {
                        session.finished(calibration, measured.sensitivity, &path)
                    }
                    Outcome::Reply(_) => session.confirm(
                        measured,
                        turns,
                        Some(&path),
                        Some("The result could not be read."),
                    ),
                    Outcome::Failed(error) => {
                        session.confirm(measured, turns, Some(&path), Some(&error))
                    }
                }
            },
        );
    }

    fn finished(&self, calibration: f64, sensitivity: f64, path: &Path) {
        let Some(group) = self.group(
            "Calibration saved",
            &format!(
                "Saved to {}. Select Done to resume the controller.",
                path.display(),
            ),
        ) else {
            return;
        };
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
        let done = self.button(&group, "Done", "suggested-action");
        let window = self.window.clone();
        done.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
}

/// A completed RWC measurement recorded by the bridge.
#[derive(Clone, Copy)]
struct Measured {
    id: u64,
    sensitivity: f64,
    counts: i32,
}

/// Write one request line and read replies until one is not progress.
async fn exchange(
    connection: &Connection,
    line: &str,
    mut progress: impl FnMut(Progress),
) -> Result<BridgeMessage, glib::Error> {
    connection
        .stream
        .output_stream()
        .write_all_future(line.as_bytes().to_vec(), glib::Priority::DEFAULT)
        .await
        .map_err(|(_, error)| error)?;
    loop {
        let line = connection
            .input
            .read_line_utf8_future(glib::Priority::DEFAULT)
            .await?
            .ok_or_else(|| glib::Error::new(gio::IOErrorEnum::BrokenPipe, "session closed"))?;
        if line.len() >= MAX_LINE_BYTES {
            return Err(glib::Error::new(
                gio::IOErrorEnum::InvalidData,
                "calibration reply too long",
            ));
        }
        match BridgeMessage::decode(line.trim()) {
            Ok(BridgeMessage::Progress(step)) => progress(step),
            Ok(reply) => return Ok(reply),
            Err(error) => return Err(glib::Error::new(gio::IOErrorEnum::InvalidData, error)),
        }
    }
}

fn measured_calibration(
    counts: i32,
    sensitivity: f64,
    text: &str,
) -> Result<(u32, f64), &'static str> {
    let turns = text
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|&turns| turns > 0)
        .ok_or("Enter a positive whole number.")?;
    Ok((turns, calculate(counts, sensitivity, f64::from(turns))?))
}

pub(crate) fn positive_decimal(text: &str) -> Option<f64> {
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
        io::{BufRead, BufReader, Write},
        os::unix::net::{UnixListener, UnixStream},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
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

    fn wait_for_gate(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "gate {} not opened",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A scripted bridge session server. The node's file name selects the
    /// behavior. Counts sessions that have ended.
    fn fake_bridge(socket: &Path, ended: Arc<AtomicUsize>) {
        let listener = UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let ended = ended.clone();
                std::thread::spawn(move || {
                    let stream = stream.unwrap();
                    serve_fake(&stream);
                    ended.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
    }

    fn serve_fake(stream: &UnixStream) {
        let mut reader = BufReader::new(stream);
        let mut writer = stream;
        let mut reply =
            |message: BridgeMessage| writer.write_all(message.encode().as_bytes()).is_ok();
        let mut next = || {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(ClientMessage::decode(line.trim_end()).unwrap()),
            }
        };
        let Some(ClientMessage::Begin { node, .. }) = next() else {
            return;
        };
        let name = node.file_name().unwrap().to_str().unwrap().to_owned();
        if name == "busy" {
            reply(BridgeMessage::Busy);
            return;
        }
        reply(BridgeMessage::Started { session: 1 });
        let mut measurement = 0;
        while let Some(message) = next() {
            let answered = match message {
                ClientMessage::MeasureGame { sensitivity } => {
                    assert_eq!(sensitivity, 2.0);
                    if name == "cancel" {
                        // Never finish. The window ends the session.
                        continue;
                    }
                    for (progress, gate) in [
                        (Progress::Ready, "ready"),
                        (Progress::CenterStick, "centered"),
                        (Progress::Started, "swept"),
                        (Progress::NoMotion, "confirmed"),
                    ] {
                        reply(BridgeMessage::Progress(progress));
                        wait_for_gate(&node.with_extension(gate));
                    }
                    measurement += 1;
                    reply(BridgeMessage::GameMeasured {
                        measurement,
                        counts: 28_800,
                    })
                }
                ClientMessage::SaveGame {
                    measurement: saved,
                    turns,
                    profile,
                } => {
                    assert_eq!((saved, turns), (measurement, 4));
                    fs::write(node.with_file_name("rwc-saved"), profile.to_str().unwrap()).unwrap();
                    reply(BridgeMessage::GameSaved { calibration: 40.0 })
                }
                ClientMessage::MeasureGyro => {
                    if name == "drop" {
                        return;
                    }
                    reply(BridgeMessage::Progress(Progress::Ready));
                    measurement += 1;
                    reply(BridgeMessage::GyroMeasured {
                        measurement,
                        controller: "00:11:22:33:44:55".into(),
                        bias_dps: [0.25, 0.0, -0.1],
                    })
                }
                ClientMessage::SaveGyro { measurement: saved } => {
                    assert_eq!(saved, measurement);
                    fs::write(node.with_file_name("gyro-saved"), "").unwrap();
                    reply(BridgeMessage::GyroSaved)
                }
                ClientMessage::End => return,
                other => panic!("unexpected request {other:?}"),
            };
            if !answered {
                return;
            }
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
        let socket = directory.path().join("reload.sock.calibration");
        let ended = Arc::new(AtomicUsize::new(0));
        fake_bridge(&socket, ended.clone());
        let profile = directory.path().join("profile.toml");
        fs::write(
            &profile,
            "version = 1\n[gyro]\nin_game_sens = 2.0\nflick_axial_threshold_deg = 12\n",
        )
        .unwrap();

        let alternate_dir = directory.path().join("other");
        fs::create_dir(&alternate_dir).unwrap();
        let alternate = alternate_dir.join("profile.toml");
        fs::write(&alternate, "version = 1\n").unwrap();
        let rwc_saved = directory.path().join("rwc-saved");

        let find_button = |window: &adw::ApplicationWindow, name| {
            descendants::<gtk::Button>(window.upcast_ref())
                .into_iter()
                .find(|button| button.label().as_deref() == Some(name))
        };
        let open_with = |socket: &Path, game: bool, node: &str| {
            let (window, session) = open_window(
                &application,
                socket.to_owned(),
                game.then(|| GameProfiles {
                    active: Some(profile.clone()),
                    available: vec![alternate.clone()],
                }),
                directory.path().join(node),
            );
            let start = if game {
                "Start Measurement"
            } else {
                "Start Gyro Calibration"
            };
            wait_for(|| {
                find_button(&window, start).is_some() || find_button(&window, "Close").is_some()
            });
            (window, session)
        };
        let open = |node: &str| open_with(&socket, true, node);
        let wait_ended = |count| wait_for(|| ended.load(Ordering::SeqCst) == count);

        let node = directory.path().join("measure");
        let (window, session) = open("measure");
        if !window.is_active() {
            session.navigate("back");
            assert!(window.is_visible());
        }
        let sensitivity = descendants::<adw::EntryRow>(window.upcast_ref())
            .into_iter()
            .find(|row| row.title() == "In-game mouse sensitivity")
            .unwrap();
        session.navigate_window(&window, "right");
        assert_eq!(sensitivity.text(), "2.1");
        session.navigate_window(&window, "fine-left");
        assert_eq!(sensitivity.text(), "2.09");
        session.navigate_window(&window, "fine-right");
        session.navigate_window(&window, "left");
        assert_eq!(sensitivity.text(), "2");
        session.navigate_window(&window, "down");
        session.navigate_window(&window, "accept");
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
        let pick_profile = |window: &adw::ApplicationWindow| {
            descendants::<adw::ComboRow>(window.upcast_ref())
                .into_iter()
                .find(|row| row.title() == "Profile")
                .unwrap()
        };
        let finish_measurement =
            |window: &adw::ApplicationWindow, session: &Calibration, select_alternate| {
                let turns = descendants::<adw::EntryRow>(window.upcast_ref())
                    .into_iter()
                    .find(|row| row.title() == "Complete turns")
                    .unwrap();
                for _ in 0..3 {
                    session.navigate_window(window, "right");
                }
                assert_eq!(turns.text(), "4");
                let value = |title| {
                    descendants::<adw::ActionRow>(window.upcast_ref())
                        .into_iter()
                        .find(|row| row.title() == title)
                        .unwrap()
                        .subtitle()
                        .unwrap()
                };
                assert_eq!(value("Real-world calibration"), "40");
                assert_eq!(value("In-game mouse sensitivity"), "2");
                session.navigate_window(window, "down");
                if select_alternate {
                    session.navigate_window(window, "right");
                    assert_eq!(pick_profile(window).selected(), 2);
                }
                session.navigate_window(window, "down");
                session.navigate_window(window, "accept");
                wait_for(|| find_button(window, "Done").is_some());
                session.navigate_window(window, "accept");
                assert!(!window.is_visible());
            };
        let picker = pick_profile(&window);
        assert_eq!(picker.selected(), 1);
        finish_measurement(&window, &session, false);
        assert_eq!(
            fs::read_to_string(&rwc_saved).unwrap(),
            profile.to_str().unwrap()
        );
        // Closing the window ends the bridge session.
        wait_ended(1);

        let (window, session) = open("measure");
        session.navigate_window(&window, "down");
        session.navigate_window(&window, "accept");
        wait_for(|| find_button(&window, "Save Calibration").is_some());
        finish_measurement(&window, &session, true);
        assert_eq!(
            fs::read_to_string(&rwc_saved).unwrap(),
            alternate.to_str().unwrap()
        );
        wait_ended(2);

        let (window, session) = open("cancel");
        session.navigate_window(&window, "down");
        session.navigate_window(&window, "accept");
        assert!(find_button(&window, "Cancel").is_some());
        session.navigate_window(&window, "back");
        wait_for(|| !window.is_visible());
        assert!(find_button(&window, "Save Calibration").is_none());
        // Cancelling mid-measurement releases the session at once.
        wait_ended(3);

        let record = directory.path().join("gyro-saved");
        let open_gyro = |node: &str| open_with(&socket, false, node);
        let (window, session) = open_gyro("gyro");
        session.navigate_window(&window, "accept");
        wait_for(|| find_button(&window, "Save Gyro Calibration").is_some());
        assert!(!record.exists());
        session.navigate_window(&window, "back");
        assert!(!window.is_visible());
        assert!(!record.exists());
        wait_ended(4);

        let (window, session) = open_gyro("gyro");
        session.navigate_window(&window, "accept");
        wait_for(|| find_button(&window, "Save Gyro Calibration").is_some());
        let save = find_button(&window, "Save Gyro Calibration").unwrap();
        for _ in 0..2 {
            if gtk::prelude::GtkWindowExt::focus(&window).as_ref() == Some(save.upcast_ref()) {
                break;
            }
            session.navigate_window(&window, "down");
        }
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&window).as_ref(),
            Some(save.upcast_ref())
        );
        session.navigate_window(&window, "accept");
        wait_for(|| find_button(&window, "Done").is_some());
        assert!(record.exists());
        session.navigate_window(&window, "accept");
        assert!(!window.is_visible());
        wait_ended(5);

        // A bridge restart ends the session and says so.
        let (window, session) = open_gyro("drop");
        session.navigate_window(&window, "accept");
        wait_for(|| find_button(&window, "Close").is_some());
        assert!(find_button(&window, "Save Gyro Calibration").is_none());
        session.navigate_window(&window, "accept");
        assert!(!window.is_visible());
        wait_ended(6);

        // Overlapping and unreachable sessions show why calibration cannot start.
        let (window, session) = open_gyro("busy");
        assert!(find_button(&window, "Close").is_some());
        session.navigate_window(&window, "accept");
        assert!(!window.is_visible());
        let (window, session) = open_with(&directory.path().join("missing"), false, "gyro");
        assert!(find_button(&window, "Close").is_some());
        assert!(find_button(&window, "Start Gyro Calibration").is_none());
        session.navigate_window(&window, "accept");
        assert!(!window.is_visible());
    }
}

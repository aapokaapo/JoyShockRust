use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

use eframe::egui;
use toml_edit::DocumentMut;

mod bindings;
mod controller;
mod sensitivity;

const NEW_PROFILE: &str = "version = 1\n";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Buttons,
    Sticks,
    Gyro,
    Advanced,
}

fn profile_filename(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err("Enter a profile name without path separators or control characters.".into());
    }
    Ok(if name.ends_with(".toml") {
        name.to_owned()
    } else {
        format!("{name}.toml")
    })
}

fn run_bridge(bridge: &Path, args: &[&Path]) -> Result<(), String> {
    let output = Command::new(bridge)
        .args(args)
        .output()
        .map_err(|error| format!("{}: {error}", bridge.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr);
    if message.trim().is_empty() {
        Err(format!("JoyShockRust exited with {}.", output.status))
    } else {
        Err(message.trim().into())
    }
}

fn stage(directory: Option<&Path>, text: &str) -> io::Result<tempfile::NamedTempFile> {
    let mut file = if let Some(directory) = directory {
        fs::create_dir_all(directory)?;
        tempfile::NamedTempFile::new_in(directory)?
    } else {
        tempfile::NamedTempFile::new()?
    };
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    Ok(file)
}

struct Editor {
    bridge: PathBuf,
    directory: PathBuf,
    path: Option<PathBuf>,
    original: String,
    text: String,
    name: String,
    selected: &'static str,
    page: Page,
    calibration: sensitivity::CalibrationDraft,
    notice: Option<Result<String, String>>,
    confirm_close: bool,
    closing: bool,
}

impl Editor {
    fn open(bridge: PathBuf, directory: PathBuf, path: Option<PathBuf>) -> io::Result<Self> {
        let original = path
            .as_ref()
            .map_or_else(|| Ok(NEW_PROFILE.to_owned()), fs::read_to_string)?;
        Ok(Self {
            bridge,
            directory,
            path,
            text: original.clone(),
            original,
            name: String::new(),
            selected: "cross",
            page: Page::Buttons,
            calibration: sensitivity::CalibrationDraft::default(),
            notice: None,
            confirm_close: false,
            closing: false,
        })
    }

    fn validate(&self) -> Result<String, String> {
        if let Some(error) = self.calibration.error {
            return Err(error.into());
        }
        let staged = stage(None, &self.text).map_err(|error| error.to_string())?;
        run_bridge(&self.bridge, &[Path::new("--check-profile"), staged.path()])?;
        Ok("Profile is valid.".into())
    }

    fn save(&mut self) -> Result<PathBuf, String> {
        if let Some(error) = self.calibration.error {
            return Err(error.into());
        }
        if let Some(path) = &self.path {
            let current =
                fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
            if current != self.original {
                return Err("The profile changed on disk. Reopen it before editing again.".into());
            }
            let staged = stage(None, &self.text).map_err(|error| error.to_string())?;
            run_bridge(
                &self.bridge,
                &[Path::new("--install-profile"), staged.path(), path],
            )?;
            self.original.clone_from(&self.text);
            return Ok(path.clone());
        }
        let path = self.directory.join(profile_filename(&self.name)?);
        let staged = stage(Some(&self.directory), &self.text).map_err(|error| error.to_string())?;
        run_bridge(&self.bridge, &[Path::new("--check-profile"), staged.path()])?;
        staged
            .persist_noclobber(&path)
            .map_err(|error| format!("{}: {}", path.display(), error.error))?;
        self.path = Some(path.clone());
        self.original.clone_from(&self.text);
        Ok(path)
    }

    fn save_and_apply(&mut self, apply: bool) -> Result<String, String> {
        let path = self.save()?;
        if apply {
            run_bridge(&self.bridge, &[Path::new("--load-profile"), &path])
                .map_err(|error| format!("Profile saved. Could not apply it: {error}"))?;
            return Ok("Profile saved and applied.".into());
        }
        Ok("Profile saved.".into())
    }
}

impl eframe::App for Editor {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested())
            && (self.text != self.original || self.calibration.error.is_some())
            && !self.closing
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        egui::TopBottomPanel::top("commands").show(ctx, |ui| {
            ui.heading("JoyShockRust Profile Editor");
            ui.horizontal(|ui| {
                if let Some(path) = &self.path {
                    ui.label(path.display().to_string());
                } else {
                    ui.label("Profile name");
                    ui.text_edit_singleline(&mut self.name);
                }
            });
            ui.horizontal_wrapped(|ui| {
                let previous = self.page;
                for (page, label) in [
                    (Page::Buttons, "Buttons"),
                    (Page::Sticks, "Sticks"),
                    (Page::Gyro, "Gyro"),
                    (Page::Advanced, "Advanced TOML"),
                ] {
                    ui.selectable_value(&mut self.page, page, label);
                }
                if previous == Page::Gyro
                    && self.page != previous
                    && self.calibration.error.is_none()
                {
                    self.calibration.reset();
                }
                ui.separator();
                if ui.button("Validate").clicked() {
                    self.notice = Some(self.validate());
                }
                if ui.button("Save").clicked() {
                    self.notice = Some(self.save_and_apply(false));
                }
                if ui.button("Save and apply").clicked() {
                    self.notice = Some(self.save_and_apply(true));
                }
            });
        });
        egui::TopBottomPanel::bottom("feedback").show(ctx, |ui| {
            if let Some(notice) = &self.notice {
                match notice {
                    Ok(message) => {
                        ui.label(message);
                    }
                    Err(message) => {
                        ui.colored_label(ui.visuals().error_fg_color, message);
                    }
                }
            }
        });
        if self.page == Page::Advanced {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Profile TOML");
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.text)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(24),
                        );
                    });
            });
        } else {
            match self.text.parse::<DocumentMut>() {
                Ok(mut doc) if self.page == Page::Buttons => {
                    egui::SidePanel::right("binding-inspector")
                        .exact_width(323.0)
                        .resizable(false)
                        .show(ctx, |ui| {
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                if sensitivity::card(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    controller::inspector(ui, &mut doc, self.selected)
                                }) {
                                    self.text = doc.to_string();
                                    self.notice = None;
                                }
                            });
                        });
                    egui::CentralPanel::default().show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            sensitivity::card(ui, |ui| {
                                controller::layout(ui, &doc, &mut self.selected);
                            });
                        });
                    });
                }
                Ok(mut doc) => {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            let changed = match self.page {
                                Page::Sticks => sensitivity::sticks(ui, &mut doc),
                                Page::Gyro => {
                                    sensitivity::gyro(ui, &mut doc, &mut self.calibration)
                                }
                                _ => unreachable!("settings page"),
                            };
                            if changed {
                                self.text = doc.to_string();
                                self.notice = None;
                                ui.ctx().request_repaint();
                            }
                        });
                    });
                }
                Err(error) => {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.heading("Profile needs attention");
                        ui.label(error.to_string());
                        if ui.button("Open Advanced TOML").clicked() {
                            self.page = Page::Advanced;
                        }
                    });
                }
            }
        }
        if self.confirm_close {
            egui::Window::new("Unsaved profile changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if ui.button("Keep editing").clicked() {
                        self.confirm_close = false;
                    }
                    if ui.button("Discard changes and close").clicked() {
                        self.closing = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let path = args.next().map(PathBuf::from);
    if args.next().is_some() {
        return Err("usage: joyshockrust-editor [PROFILE]".into());
    }
    let directory = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|home| home.join(".config"))
        })
        .ok_or("expected an absolute XDG_CONFIG_HOME or HOME")?
        .join("joyshockrust/profiles");
    let bridge = env::current_exe()?.with_file_name("joyshockrust-linux");
    let editor = Editor::open(bridge, directory, path)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1010.0, 630.0])
            .with_min_inner_size([810.0, 430.0]),
        ..Default::default()
    };
    eframe::run_native(
        "JoyShockRust Profile Editor",
        options,
        Box::new(move |context| {
            let mut visuals = egui::Visuals::light();
            visuals.panel_fill = egui::Color32::from_rgb(247, 246, 252);
            visuals.selection.bg_fill = egui::Color32::from_rgb(231, 224, 250);
            visuals.selection.stroke.color = egui::Color32::from_rgb(78, 52, 158);
            visuals.widgets.active.bg_fill = egui::Color32::from_rgb(235, 228, 252);
            visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(241, 233, 252);
            context.egui_ctx.set_visuals(visuals);
            context.egui_ctx.style_mut(|style| {
                style.spacing.item_spacing = egui::vec2(8.0, 8.0);
                style.spacing.button_padding = egui::vec2(9.0, 5.0);
            });
            Ok(Box::new(editor))
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_new_profiles_in_the_profile_directory() {
        assert_eq!(profile_filename("  My Game  ").unwrap(), "My Game.toml");
        assert_eq!(profile_filename("Game.toml").unwrap(), "Game.toml");
        for name in ["", "../test", "/tmp/escape", "a\\b", ".hidden", "a\nb"] {
            assert!(profile_filename(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_profile_changed_outside_the_editor_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("game.toml");
        fs::write(&path, NEW_PROFILE).unwrap();
        let mut editor = Editor::open(
            directory.path().join("unused-bridge"),
            directory.path().into(),
            Some(path.clone()),
        )
        .unwrap();
        editor.text.push_str("# Editor change\n");
        let external = "version = 1\n# External change\n";
        fs::write(&path, external).unwrap();
        assert!(editor.save().unwrap_err().contains("changed on disk"));
        assert_eq!(fs::read_to_string(path).unwrap(), external);
    }
}

use std::{env, process::Command};

use eframe::egui::{self, Color32, CornerRadius, Stroke, Theme, vec2};

pub const BLUE: Color32 = Color32::from_rgb(28, 113, 216);

#[derive(Clone, Copy)]
pub struct Palette {
    pub window: Color32,
    pub card: Color32,
    pub entry: Color32,
    pub border: Color32,
    pub foreground: Color32,
    pub accent: Color32,
    pub selected: Color32,
    pub button: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub shell: Color32,
    pub touchpad: Color32,
    pub neutral_line: Color32,
    pub error: Color32,
}

impl Palette {
    const LIGHT: Self = Self {
        window: rgb(0xfafafb),
        card: Color32::WHITE,
        entry: rgb(0xebebed),
        border: rgb(0xe4e4e6),
        foreground: rgb(0x252529),
        accent: rgb(0x1a5fb4),
        selected: rgb(0xdceafa),
        button: rgb(0xefeff1),
        hover: rgb(0xe4e4e7),
        pressed: rgb(0xd8d8dc),
        shell: rgb(0xdedee3),
        touchpad: rgb(0xc3c3cc),
        neutral_line: rgb(0x74747e),
        error: rgb(0xa51d2d),
    };
    const DARK: Self = Self {
        window: rgb(0x222226),
        card: rgb(0x343438),
        entry: rgb(0x1d1d20),
        border: rgb(0x45454b),
        foreground: rgb(0xf6f5f4),
        accent: rgb(0x99c1f1),
        selected: rgb(0x304b6e),
        button: rgb(0x414147),
        hover: rgb(0x505056),
        pressed: rgb(0x5b5b62),
        shell: rgb(0x54545d),
        touchpad: rgb(0x3a3a42),
        neutral_line: rgb(0xa2a2ad),
        error: rgb(0xff9d9d),
    };
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub fn palette(ui: &egui::Ui) -> Palette {
    if ui.visuals().dark_mode {
        Palette::DARK
    } else {
        Palette::LIGHT
    }
}

pub fn configure(ctx: &egui::Context) {
    for (theme, colors) in [(Theme::Light, Palette::LIGHT), (Theme::Dark, Palette::DARK)] {
        let mut visuals = match theme {
            Theme::Light => egui::Visuals::light(),
            Theme::Dark => egui::Visuals::dark(),
        };
        visuals.panel_fill = colors.window;
        visuals.window_fill = colors.card;
        visuals.window_stroke = Stroke::new(1.0_f32, colors.border);
        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.menu_corner_radius = CornerRadius::same(10);
        visuals.extreme_bg_color = colors.entry;
        visuals.text_edit_bg_color = Some(colors.entry);
        visuals.selection.bg_fill = colors.selected;
        visuals.selection.stroke = Stroke::new(1.0_f32, colors.accent);
        visuals.error_fg_color = colors.error;
        for (widgets, fill) in [
            (&mut visuals.widgets.inactive, colors.button),
            (&mut visuals.widgets.hovered, colors.hover),
            (&mut visuals.widgets.active, colors.pressed),
            (&mut visuals.widgets.open, colors.selected),
        ] {
            widgets.corner_radius = CornerRadius::same(8);
            widgets.bg_fill = fill;
            widgets.weak_bg_fill = fill;
            widgets.bg_stroke.color = colors.border;
            widgets.fg_stroke.color = colors.foreground;
        }
        visuals.widgets.noninteractive.fg_stroke.color = colors.foreground;
        visuals.widgets.noninteractive.bg_stroke.color = colors.border;
        ctx.set_visuals_of(theme, visuals);
    }
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 8.0);
        style.spacing.button_padding = vec2(10.0, 6.0);
    });
    ctx.options_mut(|options| options.fallback_theme = fallback_theme());
    ctx.set_theme(egui::ThemePreference::System);
}

fn fallback_theme() -> Theme {
    if let Some(theme) = env::var_os("GTK_THEME") {
        return if theme.to_string_lossy().ends_with(":dark") {
            Theme::Dark
        } else {
            Theme::Light
        };
    }
    let dark = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "'prefer-dark'"
        });
    if dark { Theme::Dark } else { Theme::Light }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_adwaita_styles_keep_their_own_surface_colors() {
        let ctx = egui::Context::default();
        configure(&ctx);
        for (theme, palette) in [(Theme::Light, Palette::LIGHT), (Theme::Dark, Palette::DARK)] {
            ctx.set_theme(theme);
            let style = ctx.style();
            assert_eq!(style.visuals.panel_fill, palette.window);
            assert_eq!(style.visuals.window_fill, palette.card);
            assert_eq!(style.visuals.selection.stroke.color, palette.accent);
        }
    }
}

use adw::{gtk, prelude::*};

pub struct Curve {
    pub input_max: f64,
    pub output_max: f64,
    pub points: Vec<(f64, f64)>,
    pub sample: Option<(f64, f64)>,
}

pub fn add(
    group: &adw::PreferencesGroup,
    title: &str,
    output: &str,
    input: &str,
    live: bool,
    curve: impl Fn() -> Curve + 'static,
) -> gtk::DrawingArea {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
    panel.set_margin_top(12);
    panel.set_margin_bottom(12);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("heading");
    panel.append(&heading);
    let legend = gtk::Label::new(Some("Solid · Configured       Dashed · Linear"));
    legend.add_css_class("dim-label");
    legend.set_wrap(true);
    panel.append(&legend);
    if live {
        let markers = gtk::Label::new(Some("Gray · Input    Blue · Response"));
        markers.add_css_class("dim-label");
        markers.set_wrap(true);
        panel.append(&markers);
    }
    let output = gtk::Label::new(Some(output));
    output.set_xalign(0.0);
    output.add_css_class("caption");
    panel.append(&output);
    let area = gtk::DrawingArea::builder().content_height(146).build();
    area.update_property(&[gtk::accessible::Property::Label(title)]);
    area.set_draw_func(move |area, cr, width, height| draw(area, cr, width, height, curve()));
    panel.append(&area);
    let input = gtk::Label::new(Some(input));
    input.add_css_class("caption");
    panel.append(&input);
    group.add(&panel);
    area
}

fn draw(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: i32, height: i32, curve: Curve) {
    let left = 36.0;
    let top = 8.0;
    let width = (f64::from(width) - left - 24.0).max(1.0);
    let height = (f64::from(height) - top - 22.0).max(1.0);
    let pos = |x: f64, y: f64| {
        (
            left + width * x / curve.input_max,
            top + height * (1.0 - y / curve.output_max),
        )
    };
    let line = |from: (f64, f64), to: (f64, f64)| {
        cr.move_to(from.0, from.1);
        cr.line_to(to.0, to.1);
    };
    let style = area.style_context();
    let fg = style.color();
    let accent = style.lookup_color("accent_color").unwrap_or(fg);
    let color = |rgba: gtk::gdk::RGBA, alpha| {
        cr.set_source_rgba(
            rgba.red().into(),
            rgba.green().into(),
            rgba.blue().into(),
            alpha,
        );
    };
    cr.set_font_size(11.0);
    cr.set_line_width(1.0);
    for tick in 0..=4 {
        let x = curve.input_max * f64::from(tick) / 4.0;
        let y = curve.output_max * f64::from(tick) / 4.0;
        color(fg, 0.17);
        line(pos(x, 0.0), pos(x, curve.output_max));
        line(pos(0.0, y), pos(curve.input_max, y));
        let _ = cr.stroke();
        color(fg, 0.83);
        let (label_x, bottom) = pos(x, 0.0);
        cr.move_to(label_x - 5.0, bottom + 16.0);
        let _ = cr.show_text(&format!("{x:.0}"));
        let (_, label_y) = pos(0.0, y);
        cr.move_to(2.0, label_y + 4.0);
        let _ = cr.show_text(&format!("{y:.0}"));
    }
    color(fg, 0.66);
    cr.set_dash(&[4.0, 4.0], 0.0);
    line(pos(0.0, 0.0), pos(curve.input_max, curve.input_max));
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    color(accent, 1.0);
    cr.set_line_width(2.5);
    for (i, (x, y)) in curve.points.into_iter().enumerate() {
        let (x, y) = pos(x, y);
        if i == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();
    if let Some((input, output)) = curve.sample {
        draw_markers(cr, [pos(input, input), pos(input, output)]);
    }
}

pub fn draw_markers(cr: &gtk::cairo::Context, [input, output]: [(f64, f64); 2]) {
    // The larger input dot remains visible as a ring if both values coincide.
    for ((x, y), color, radius) in [
        (input, gtk::gdk::RGBA::new(0.60, 0.60, 0.60, 1.0), 6.0),
        (output, gtk::gdk::RGBA::new(0.24, 0.56, 0.96, 1.0), 4.0),
    ] {
        cr.new_sub_path();
        cr.arc(x, y, radius, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
        );
        let _ = cr.fill();
    }
}

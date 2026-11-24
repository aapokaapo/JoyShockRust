use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use adw::{gtk, prelude::*};

use crate::{controller_input, triggers};

const PREVIEW_MESSAGE: &[u8] = b"\0preview-trigger-travel\n";

#[derive(Clone)]
pub struct Chart {
    area: gtk::DrawingArea,
    thresholds: Rc<Cell<[u8; 4]>>,
    keep_partial: Rc<Cell<bool>>,
    side: u8,
}

impl Chart {
    pub fn set_thresholds(&self, thresholds: [u8; 4]) {
        self.thresholds.set(thresholds);
        self.area.queue_draw();
        if is_active(&self.area) {
            send(self.side, thresholds);
        }
    }

    pub fn set_keep_partial(&self, keep: bool) {
        self.keep_partial.set(keep);
        self.area.queue_draw();
    }
}

pub fn add(
    group: &adw::PreferencesGroup,
    side: &str,
    initial: [u8; 4],
    keep_partial: bool,
) -> Chart {
    let side = u8::from(side == "r2");
    let thresholds = Rc::new(Cell::new(initial));
    let keep_partial = Rc::new(Cell::new(keep_partial));
    let live = Rc::new(Cell::new(None));
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    panel.set_margin_top(8);
    panel.set_margin_bottom(12);
    let area = gtk::DrawingArea::builder().content_height(86).build();
    area.set_widget_name("trigger-travel-chart");
    area.update_property(&[gtk::accessible::Property::Label(
        "Trigger travel and stage thresholds",
    )]);
    area.set_draw_func({
        let thresholds = thresholds.clone();
        let keep_partial = keep_partial.clone();
        let live = live.clone();
        move |area, cr, width, _| {
            draw(
                area,
                cr,
                width,
                thresholds.get(),
                keep_partial.get(),
                live.get(),
            )
        }
    });
    panel.append(&area);
    let legend = gtk::Label::new(Some(
        "Solid · Pull    Dashed · Release    Marker · Live travel",
    ));
    legend.set_wrap(true);
    legend.add_css_class("caption");
    panel.append(&legend);
    let status = gtk::Label::new(Some("Waiting for controller input…"));
    status.add_css_class("dim-label");
    panel.append(&status);
    group.add(&panel);

    let armed = Rc::new(Cell::new(false));
    let refresh_at = Cell::new(Instant::now());
    controller_input::watch_status(&area, "trigger_travel=", Duration::from_millis(33), {
        let thresholds = thresholds.clone();
        let chart = area.downgrade();
        let armed = armed.clone();
        move |result| {
            let Some(area) = chart.upgrade() else {
                return;
            };
            let now = Instant::now();
            let active = is_active(&area);
            let was_active = armed.replace(active);
            if active && (!was_active || now >= refresh_at.get()) {
                send(side, thresholds.get());
                refresh_at.set(now + Duration::from_millis(500));
            } else if was_active && !active {
                stop();
            }
            let unavailable = result.is_err();
            let travel = result
                .ok()
                .flatten()
                .as_deref()
                .and_then(|value| read_travel(value, side));
            if live.replace(travel) != travel {
                area.queue_draw();
            }
            if let Some(travel) = travel {
                status.set_text(&format!("Live travel: {:.1}%", travel * 100.0));
            } else if unavailable {
                status.set_text("Bridge status is unavailable");
            } else {
                status.set_text("Waiting for controller input…");
            }
        }
    });
    area.connect_unmap(move |_| {
        if armed.replace(false) {
            stop();
        }
    });
    Chart {
        area,
        thresholds,
        keep_partial,
        side,
    }
}

fn is_active(area: &gtk::DrawingArea) -> bool {
    area.is_mapped()
        && area
            .root()
            .and_downcast::<gtk::Window>()
            .is_some_and(|window| window.is_active())
}

fn stop() {
    let _ = triggers::send_preview_message(PREVIEW_MESSAGE, &[]);
}

fn send(side: u8, [partial, partial_release, full, full_release]: [u8; 4]) {
    let _ = triggers::send_preview_message(
        PREVIEW_MESSAGE,
        &[side, partial, partial_release, full, full_release],
    );
}

fn read_travel(value: &str, side: u8) -> Option<f64> {
    let (left, right) = value.split_once(',')?;
    let travel = if side == 0 { left } else { right }.parse::<f64>().ok()?;
    (0.0..=1.0).contains(&travel).then_some(travel)
}

fn draw(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    [partial, partial_release, full, full_release]: [u8; 4],
    keep_partial: bool,
    live: Option<f64>,
) {
    let left = 57.0;
    let end = (f64::from(width) - 10.0).max(left + 1.0);
    let x = |fraction: f64| left + (end - left) * fraction;
    let style = area.style_context();
    let foreground = style.color();
    let accent = style.lookup_color("accent_color").unwrap_or(foreground);
    let color = |rgba: gtk::gdk::RGBA, alpha| {
        cr.set_source_rgba(
            rgba.red().into(),
            rgba.green().into(),
            rgba.blue().into(),
            alpha,
        );
    };
    let text = |label: &str, x, y| {
        cr.move_to(x, y);
        let _ = cr.show_text(label);
    };

    cr.set_font_size(11.0);
    let partial_end = if keep_partial { 100 } else { full };
    for (label, top, pull, release, active_end) in [
        ("Partial", 12.0, partial, partial_release, partial_end),
        ("Full", 40.0, full, full_release, 100),
    ] {
        let pull = x(f64::from(pull) / 100.0);
        let release = x(f64::from(release) / 100.0);
        let active_end = x(f64::from(active_end) / 100.0);
        color(foreground, 0.1);
        cr.rectangle(left, top, end - left, 18.0);
        let _ = cr.fill();
        color(accent, 0.22);
        cr.rectangle(release, top, pull - release, 18.0);
        let _ = cr.fill();
        color(accent, 0.48);
        cr.rectangle(pull, top, active_end - pull, 18.0);
        let _ = cr.fill();
        color(foreground, 0.22);
        cr.rectangle(active_end, top, end - active_end, 18.0);
        let _ = cr.fill();
        color(accent, 1.0);
        cr.set_line_width(1.5);
        for (marker, dash) in [(release, &[3.0, 2.0][..]), (pull, &[][..])] {
            cr.set_dash(dash, 0.0);
            cr.move_to(marker, top - 3.0);
            cr.line_to(marker, top + 21.0);
            let _ = cr.stroke();
        }
        color(foreground, 0.9);
        text(label, 0.0, top + 13.0);
    }
    color(foreground, 0.85);
    for (label, position, offset) in [
        ("0%", left, 0.0),
        ("50%", x(0.5), 12.0),
        ("100%", end, 27.0),
    ] {
        text(label, position - offset, 79.0);
    }
    if let Some(live) = live {
        let marker = x(live);
        color(foreground, 1.0);
        cr.set_line_width(2.0);
        cr.move_to(marker, 10.0);
        cr.line_to(marker, 62.0);
        let _ = cr.stroke();
        cr.arc(marker, 7.0, 3.0, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}

#[cfg(test)]
mod tests {
    use super::read_travel;

    #[test]
    fn live_travel_selects_the_correct_side_and_rejects_invalid_values() {
        assert_eq!(read_travel("0.300000,1.000000", 0), Some(0.3));
        assert_eq!(read_travel("0.300000,1.000000", 1), Some(1.0));
        assert_eq!(read_travel("NaN,-0.1", 0), None);
        assert_eq!(read_travel("NaN,-0.1", 1), None);
        assert_eq!(read_travel("0.3", 0), None);
    }
}

use joyshockrust_core::trigger::TriggerSide;
use joyshockrust_ipc::control::TravelPreview;
use joyshockrust_ipc::status::Field;
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use adw::{gtk, prelude::*};

use crate::{controller_input, response_plot, triggers};

#[derive(Clone)]
pub struct Chart {
    area: gtk::DrawingArea,
    thresholds: Rc<Cell<[u8; 4]>>,
    keep_partial: Rc<Cell<bool>>,
    stages: Rc<Cell<[bool; 2]>>,
    range: Rc<Cell<(u8, bool)>>,
    side: u8,
}

impl Chart {
    pub fn set_thresholds(&self, thresholds: [u8; 4]) {
        self.thresholds.set(thresholds);
        self.area.queue_draw();
        if is_active(&self.area) {
            send(self.side, thresholds, self.stages.get(), self.range.get());
        }
    }

    pub fn set_keep_partial(&self, keep: bool) {
        self.keep_partial.set(keep);
        self.area.queue_draw();
    }

    pub fn set_stages(&self, stages: [bool; 2]) {
        self.stages.set(stages);
        self.area.set_content_height(
            58 + 28 * stages.into_iter().filter(|visible| *visible).count() as i32,
        );
        self.area.queue_draw();
        if is_active(&self.area) {
            send(self.side, self.thresholds.get(), stages, self.range.get());
        }
    }

    pub fn set_range(&self, range: (u8, bool)) {
        self.range.set(range);
        self.area.queue_draw();
        if is_active(&self.area) {
            send(self.side, self.thresholds.get(), self.stages.get(), range);
        }
    }
}

pub fn add(
    group: &adw::PreferencesGroup,
    side: &str,
    initial: [u8; 4],
    keep_partial: bool,
    range: (u8, bool),
) -> Chart {
    let side = u8::from(side == "r2");
    let thresholds = Rc::new(Cell::new(initial));
    let keep_partial = Rc::new(Cell::new(keep_partial));
    let stages = Rc::new(Cell::new([false; 2]));
    let range = Rc::new(Cell::new(range));
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
        let stages = stages.clone();
        let range = range.clone();
        let live = live.clone();
        move |area, cr, width, _| {
            draw(
                area,
                cr,
                width,
                Drawing {
                    thresholds: thresholds.get(),
                    keep_partial: keep_partial.get(),
                    stages: stages.get(),
                    range: range.get(),
                    live: live.get(),
                },
            )
        }
    });
    panel.append(&area);
    let legend = gtk::Label::new(Some(
        "Gray · Physical    Blue · Scaled    Solid · Pull    Dashed · Release",
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
    controller_input::watch_status(&area, Field::TriggerTravel, Duration::from_millis(33), {
        let thresholds = thresholds.clone();
        let stages = stages.clone();
        let range = range.clone();
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
                send(side, thresholds.get(), stages.get(), range.get());
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
                status.set_text(&format!(
                    "Physical: {:.1}%    Scaled: {:.1}%",
                    travel * 100.0,
                    scaled_travel(travel, range.get().0) * 100.0,
                ));
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
        stages,
        range,
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
    let _ = triggers::send_preview_message(&TravelPreview::encode(None));
}

fn send(
    side: u8,
    [partial, partial_release, full, full_release]: [u8; 4],
    [lower, upper]: [bool; 2],
    (limit, relative): (u8, bool),
) {
    let request = TravelPreview {
        side: if side == 0 {
            TriggerSide::Left
        } else {
            TriggerSide::Right
        },
        thresholds: [partial, partial_release, full, full_release],
        stages: [lower, upper],
        threshold_scale: if relative { limit } else { 100 },
        travel_limit: Some(limit),
    };
    let _ = triggers::send_preview_message(&TravelPreview::encode(Some(request)));
}

fn read_travel(value: &str, side: u8) -> Option<f64> {
    let (left, right) = value.split_once(',')?;
    let travel = if side == 0 { left } else { right }.parse::<f64>().ok()?;
    (0.0..=1.0).contains(&travel).then_some(travel)
}

fn scaled_travel(travel: f64, limit: u8) -> f64 {
    (travel / (f64::from(limit) / 100.0)).min(1.0)
}

struct Drawing {
    thresholds: [u8; 4],
    keep_partial: bool,
    stages: [bool; 2],
    range: (u8, bool),
    live: Option<f64>,
}

fn draw(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    Drawing {
        thresholds: [partial, partial_release, full, full_release],
        keep_partial,
        stages,
        range: (limit, relative),
        live,
    }: Drawing,
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
    let cap = x(f64::from(limit) / 100.0);
    let scale = f64::from(if relative { limit } else { 100 }) / 100.0;
    color(foreground, 0.1);
    cr.rectangle(left, 12.0, end - left, 18.0);
    let _ = cr.fill();
    color(accent, 0.48);
    cr.rectangle(left, 12.0, cap - left, 18.0);
    let _ = cr.fill();
    color(accent, 1.0);
    cr.set_line_width(1.5);
    cr.move_to(cap, 9.0);
    cr.line_to(cap, 33.0);
    let _ = cr.stroke();
    color(foreground, 0.9);
    text("Travel", 0.0, 25.0);
    let mut top = 40.0;
    for ((label, pull, release, active_end), _) in [
        ("Partial", partial, partial_release, partial_end),
        ("Full", full, full_release, 100),
    ]
    .into_iter()
    .zip(stages)
    .filter(|(_, visible)| *visible)
    {
        let pull = x(f64::from(pull) / 100.0 * scale);
        let release = x(f64::from(release) / 100.0 * scale);
        let active_end = x(f64::from(active_end) / 100.0 * scale);
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
        top += 28.0;
    }
    color(foreground, 0.85);
    for (label, position, offset) in [
        ("0%", left, 0.0),
        ("50%", x(0.5), 12.0),
        ("100%", end, 27.0),
    ] {
        text(label, position - offset, top + 11.0);
    }
    if let Some(live) = live {
        let marker = x(live);
        color(foreground, 1.0);
        cr.set_line_width(2.0);
        cr.move_to(marker, 10.0);
        cr.line_to(marker, top - 6.0);
        let _ = cr.stroke();
        response_plot::draw_markers(cr, [(marker, 7.0), (x(scaled_travel(live, limit)), 7.0)]);
    }
}

#[cfg(test)]
mod tests {
    use super::{read_travel, scaled_travel};

    #[test]
    fn scaled_marker_reaches_one_at_the_limit_and_stays_there() {
        assert_eq!(scaled_travel(0.0, 60), 0.0);
        assert_eq!(scaled_travel(0.3, 60), 0.5);
        assert_eq!(scaled_travel(0.6, 60), 1.0);
        assert_eq!(scaled_travel(0.9, 60), 1.0);
        assert_eq!(scaled_travel(0.3, 100), 0.3);
    }

    #[test]
    fn live_travel_selects_the_correct_side_and_rejects_invalid_values() {
        assert_eq!(read_travel("0.300000,1.000000", 0), Some(0.3));
        assert_eq!(read_travel("0.300000,1.000000", 1), Some(1.0));
        assert_eq!(read_travel("NaN,-0.1", 0), None);
        assert_eq!(read_travel("NaN,-0.1", 1), None);
        assert_eq!(read_travel("0.3", 0), None);
    }
}

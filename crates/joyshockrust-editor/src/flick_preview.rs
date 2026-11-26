use joyshockrust_ipc::telemetry::preview::{Cursor, Preview};
use std::{
    cell::{Cell, RefCell},
    f64::consts::{FRAC_PI_2, TAU},
    rc::Rc,
};

use adw::{gtk, prelude::*};
use joyshockrust_core::{Stick, StickSide};
use joyshockrust_motion::aim_response::{
    FLICK_RELEASE_RADIUS, FlickDirection, FlickUpdate, snap_flick_angle,
};

use crate::{live_preview::LivePreview, response_plot};

#[derive(Clone, Copy, Default, PartialEq)]
struct PreviewSample {
    stick: Stick,
    turn: Option<[f64; 2]>,
}

impl PreviewSample {
    fn update(&mut self, stick: Stick, direction: &mut FlickDirection) {
        self.stick = stick;
        match direction.update(stick.x, stick.y) {
            FlickUpdate::Idle => self.turn = None,
            FlickUpdate::Started(angle) => self.turn = Some([angle, 0.0]),
            FlickUpdate::Steered(delta) => {
                if let Some([_, steered]) = &mut self.turn {
                    *steered += delta;
                }
            }
        }
    }

    fn adjusted_marker(self, threshold: f64, multiplier: f64) -> (f64, f64) {
        let x = f64::from(self.stick.x);
        let y = f64::from(self.stick.y);
        let Some([initial, steered]) = self.turn else {
            return (0.0, 0.0);
        };
        let radius = x.hypot(y);
        let (sin, cos) = (snap_flick_angle(initial, threshold) + steered * multiplier).sin_cos();
        (radius * sin, -radius * cos)
    }
}

struct Tracker {
    cursor: Cursor,
    direction: FlickDirection,
    settings: (StickSide, f64),
    sample: Option<PreviewSample>,
}

impl Tracker {
    fn new(settings: (StickSide, f64)) -> Self {
        Self {
            cursor: Cursor::default(),
            direction: FlickDirection::new(settings.1),
            settings,
            sample: None,
        }
    }

    fn update(
        &mut self,
        frame: Option<&Preview>,
        settings: (StickSide, f64),
    ) -> Option<PreviewSample> {
        if settings != self.settings {
            *self = Self::new(settings);
        }
        let Some(frame) = frame else {
            *self = Self::new(settings);
            return None;
        };
        let (reset, samples) = self.cursor.advance(frame);
        if reset {
            self.direction.reset();
            self.sample = None;
        }
        for report in samples {
            self.sample
                .get_or_insert_with(PreviewSample::default)
                .update(
                    settings.0.select(report.sticks[0], report.sticks[1]),
                    &mut self.direction,
                );
        }
        self.sample
    }
}

pub fn add(
    telemetry: &LivePreview,
    group: &adw::PreferencesGroup,
    adjustment: &gtk::Adjustment,
    steering: &gtk::Adjustment,
    engagement: &gtk::Adjustment,
    side: Rc<Cell<StickSide>>,
) -> gtk::Box {
    let sample = Rc::new(Cell::new(None::<PreviewSample>));
    let chart = gtk::DrawingArea::builder().content_height(172).build();
    chart.update_property(&[gtk::accessible::Property::Label(
        "Flick Stick input and response",
    )]);
    chart.set_draw_func({
        let adjustment = adjustment.clone();
        let steering = steering.clone();
        let sample = sample.clone();
        let engagement = engagement.clone();
        move |area, cr, width, height| {
            let threshold = adjustment.value().to_radians();
            let markers = sample.get().map(|sample| {
                [
                    (f64::from(sample.stick.x), f64::from(sample.stick.y)),
                    sample.adjusted_marker(threshold, steering.value()),
                ]
            });
            draw(
                area,
                cr,
                (width, height),
                threshold,
                engagement.value(),
                markers,
            );
        }
    });
    for setting in [adjustment, steering, engagement] {
        let chart = chart.downgrade();
        setting.connect_value_changed(move |_| {
            if let Some(chart) = chart.upgrade() {
                chart.queue_draw();
            }
        });
    }

    let status = gtk::Label::builder()
        .label("Waiting for controller input…")
        .wrap(true)
        .css_classes(["dim-label"])
        .margin_bottom(12)
        .build();
    let preview = gtk::Box::new(gtk::Orientation::Vertical, 6);
    preview.set_margin_top(12);
    let legend = gtk::Label::new(Some("Gray · Physical input    Blue · Flick Stick response"));
    legend.set_wrap(true);
    legend.add_css_class("dim-label");
    preview.append(&legend);
    preview.append(&chart);
    preview.append(&status);
    group.add(&preview);

    let engagement = engagement.clone();
    let tracker = RefCell::new(Tracker::new((side.get(), engagement.value())));
    telemetry.watch(&preview, move |frame| {
        let latest = tracker
            .borrow_mut()
            .update(frame, (side.get(), engagement.value()));
        status.set_text(match frame {
            None => "Bridge telemetry is unavailable",
            Some(frame) if frame.history.device.is_none() => "Waiting for an active controller…",
            Some(_) if latest.is_none() => "Waiting for controller input…",
            Some(_) => "",
        });
        if sample.replace(latest) != latest {
            chart.queue_draw();
        }
    });
    preview
}

fn draw(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    (width, height): (i32, i32),
    threshold: f64,
    engagement: f64,
    markers: Option<[(f64, f64); 2]>,
) {
    // Leave room for a diagonal's travel when it snaps onto a cardinal axis.
    let radius =
        (f64::from(width.min(height)) * 0.5 - 9.0).max(1.0) * std::f64::consts::FRAC_1_SQRT_2;
    let x = f64::from(width) / 2.0;
    let y = f64::from(height) / 2.0;
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

    if threshold > 0.0 {
        color(accent, 0.23);
        for quadrant in 0..4 {
            let axis = f64::from(quadrant) * FRAC_PI_2;
            cr.move_to(x, y);
            cr.arc(x, y, radius, axis - threshold, axis + threshold);
            cr.close_path();
        }
        let _ = cr.fill();
    }
    color(fg, 0.26);
    cr.set_line_width(1.0);
    cr.arc(x, y, radius, 0.0, TAU);
    cr.move_to(x - radius, y);
    cr.line_to(x + radius, y);
    cr.move_to(x, y - radius);
    cr.line_to(x, y + radius);
    let _ = cr.stroke();

    color(fg, 0.35);
    cr.set_dash(&[3.0, 3.0], 0.0);
    for ring in [f64::from(FLICK_RELEASE_RADIUS), engagement / 100.0] {
        cr.new_sub_path();
        cr.arc(x, y, radius * ring, 0.0, TAU);
    }
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);

    if let Some(markers) = markers {
        response_plot::draw_markers(
            cr,
            markers.map(|(dx, dy)| (x + dx * radius, y + dy * radius)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intermediate_neutral_report_rearms_flick_and_a_gap_clears_held_steering() {
        use joyshockrust_config::controller::{ModelSelection, Selection};
        use joyshockrust_ipc::telemetry::{
            TouchPoints,
            preview::{Device, History, Sample},
        };

        let settings = (StickSide::Right, 85.0);
        let mut tracker = Tracker::new(settings);
        let input = |x, y| Sample {
            buttons: Default::default(),
            sticks: [Stick::default(), Stick { x, y }],
            triggers: [0.0; 2],
            touch: TouchPoints([None; 2]),
        };
        let mut frame = Preview {
            instance: 1,
            history: History {
                session: 1,
                device: Some(Device {
                    model: ModelSelection::DualSense,
                    transport: Selection::Usb,
                }),
                first: 0,
                samples: vec![input(0.0, 0.0)],
                imu: None,
            },
        };
        tracker.update(Some(&frame), settings);
        frame.history.samples.push(input(1.0, 0.0));
        let right = tracker
            .update(Some(&frame), settings)
            .unwrap()
            .adjusted_marker(0.0, 0.5);
        assert!(right.0 > 0.99);
        frame
            .history
            .samples
            .extend([input(0.0, 0.0), input(0.0, -1.0)]);
        let up = tracker
            .update(Some(&frame), settings)
            .unwrap()
            .adjusted_marker(0.0, 0.5);
        assert_eq!(up, (0.0, -1.0));

        frame.history.first = 100;
        frame.history.samples = vec![input(1.0, 0.0)];
        let after_gap = tracker
            .update(Some(&frame), settings)
            .unwrap()
            .adjusted_marker(0.0, 0.5);
        assert_eq!(after_gap, (0.0, 0.0));
        assert_eq!(
            tracker.update(None, settings).map(|sample| sample.stick),
            None
        );
    }

    #[test]
    fn preview_shares_the_runtime_snap_boundary_and_preserves_travel() {
        let mut sample = PreviewSample::default();
        let mut direction = FlickDirection::default();
        sample.update(Stick::default(), &mut direction);
        sample.update(Stick { x: 0.08, y: -0.99 }, &mut direction);
        let threshold = 8.0_f64.to_radians();
        assert_eq!(sample.adjusted_marker(threshold, 0.5).0, 0.0);
        sample.update(Stick { x: 0.1, y: -0.99 }, &mut direction);
        let slow = sample.adjusted_marker(threshold, 0.5);
        let full = sample.adjusted_marker(threshold, 1.0);
        assert!(slow.0 > 0.0 && full.0 > slow.0);
        assert!((slow.0.hypot(slow.1) - full.0.hypot(full.1)).abs() < 1e-9);
        sample.update(Stick::default(), &mut direction);
        assert_eq!(sample.adjusted_marker(threshold, 0.5), (0.0, 0.0));
    }
}

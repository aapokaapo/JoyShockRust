use joyshockrust_ipc::status::Field;
use std::{
    cell::{Cell, RefCell},
    f64::consts::{FRAC_PI_2, TAU},
    fs::File,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use adw::{gio, glib, gtk, prelude::*};
use joyshockrust_core::{Stick, StickSide};
use joyshockrust_motion::aim_response::{
    FLICK_RELEASE_RADIUS, FlickDirection, FlickUpdate, snap_flick_angle,
};

use crate::{controller_input, response_plot};

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

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

pub fn add(
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

    let polling = Rc::new(RefCell::new(None::<glib::JoinHandle<()>>));
    let on_map = polling.clone();
    let engagement = engagement.clone();
    preview.connect_map(move |_| {
        let task = glib::MainContext::default().spawn_local(poll_input(
            sample.clone(),
            chart.clone(),
            status.clone(),
            side.clone(),
            engagement.clone(),
        ));
        on_map.replace(Some(task));
    });
    preview.connect_unmap(move |_| {
        if let Some(task) = polling.borrow_mut().take() {
            task.abort();
        }
    });
    preview
}

async fn active_node(
    client: &gio::SocketClient,
    address: &gio::UnixSocketAddress,
) -> Result<Option<PathBuf>, glib::Error> {
    Ok(
        controller_input::status_value(client, address, Field::ActiveHidraw)
            .await?
            .as_deref()
            .and_then(hidraw_path),
    )
}

fn hidraw_path(value: &str) -> Option<PathBuf> {
    joyshockrust_ipc::status::hidraw_device_name(value).then(|| PathBuf::from(value))
}

async fn poll_input(
    sample: Rc<Cell<Option<PreviewSample>>>,
    chart: gtk::DrawingArea,
    status: gtk::Label,
    side: Rc<Cell<StickSide>>,
    engagement: gtk::Adjustment,
) {
    let update = |latest| {
        if sample.replace(latest) != latest {
            chart.queue_draw();
        }
    };
    update(None);
    let Some(address) = controller_input::status_address() else {
        status.set_text("Bridge status is unavailable");
        return;
    };
    let client = gio::SocketClient::new();
    client.set_timeout(1);
    let mut next_check = Instant::now();
    let mut input: Option<(PathBuf, File)> = None;
    let mut buffer = [0; 16_384];
    let mut engage_value = engagement.value();
    let mut direction = FlickDirection::new(engage_value);
    let mut current_side = side.get();
    loop {
        if current_side != side.get() || engage_value != engagement.value() {
            current_side = side.get();
            engage_value = engagement.value();
            direction = FlickDirection::new(engage_value);
            update(None);
        }
        if Instant::now() >= next_check {
            match active_node(&client, &address).await {
                Ok(Some(node)) if input.as_ref().is_some_and(|(path, _)| *path == node) => {}
                Ok(Some(node)) => {
                    update(None);
                    direction.reset();
                    input = match controller_input::open(&node) {
                        Ok(file) => {
                            status.set_text("Waiting for controller input…");
                            Some((node, file))
                        }
                        Err(err) => {
                            status.set_text(&format!("Controller input unavailable: {err}"));
                            None
                        }
                    };
                }
                Ok(None) => {
                    input = None;
                    update(None);
                    status.set_text("Waiting for an active controller…");
                }
                Err(_) => {
                    input = None;
                    update(None);
                    status.set_text("Bridge status is unavailable");
                }
            }
            next_check = Instant::now() + RECONNECT_INTERVAL;
        }
        if let Some((_, file)) = &mut input {
            let mut latest = sample.get().unwrap_or_default();
            let mut received = false;
            match controller_input::read_reports(file, &mut buffer, 64, |controls| {
                latest.update(
                    current_side.select(controls.left_stick, controls.right_stick),
                    &mut direction,
                );
                received = true;
                true
            }) {
                Ok(()) => {
                    if received {
                        if sample.get().is_none() {
                            status.set_text("");
                        }
                        update(Some(latest));
                    }
                }
                Err(_) => {
                    input = None;
                    update(None);
                    status.set_text("Waiting for an active controller…");
                }
            }
        }
        let delay = if input.is_some() {
            FRAME_INTERVAL
        } else {
            next_check.saturating_duration_since(Instant::now())
        };
        glib::timeout_future(delay).await;
    }
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
    fn preview_can_read_the_active_controller_from_the_status_socket() {
        use std::{io::Write, os::unix::net::UnixListener};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("status.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut guest, _) = listener.accept().unwrap();
            guest
                .write_all(b"version=1\ncontroller=connected\nactive_hidraw=/dev/hidraw6\n")
                .unwrap();
        });
        let context = glib::MainContext::new();
        let node = context
            .with_thread_default(|| {
                let client = gio::SocketClient::new();
                client.set_timeout(1);
                context.block_on(active_node(&client, &gio::UnixSocketAddress::new(&path)))
            })
            .unwrap()
            .unwrap();
        server.join().unwrap();
        assert_eq!(node, Some(PathBuf::from("/dev/hidraw6")));
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
        assert_eq!(
            hidraw_path("/dev/hidraw6"),
            Some(PathBuf::from("/dev/hidraw6"))
        );
        assert_eq!(hidraw_path("none"), None);
    }
}

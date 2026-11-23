use std::{
    cell::{Cell, RefCell},
    f64::consts::{FRAC_PI_2, TAU},
    fs::File,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use adw::{gio, glib, gtk, prelude::*};
use joyshockrust_core::Stick;
use joyshockrust_motion::aim_response::{FlickDirection, FlickUpdate, snap_flick_angle};

use crate::controller_input;

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
        let [initial, steered] = self.turn.unwrap_or_else(|| [x.atan2(-y), 0.0]);
        let radius = x.hypot(y);
        let (sin, cos) = (snap_flick_angle(initial, threshold) + steered * multiplier).sin_cos();
        (radius * sin, -radius * cos)
    }
}

pub fn add(
    group: &adw::PreferencesGroup,
    adjustment: &gtk::Adjustment,
    steering: &gtk::Adjustment,
) {
    let sample = Rc::new(Cell::new(None::<PreviewSample>));
    let plots = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    plots.set_homogeneous(true);
    plots.set_margin_top(12);
    plots.set_margin_start(12);
    plots.set_margin_end(12);

    let charts = ["Physical input", "Flick Stick response"].map(|title| {
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let label = gtk::Label::new(Some(title));
        label.set_wrap(true);
        panel.append(&label);
        let chart = gtk::DrawingArea::builder().content_height(152).build();
        chart.update_property(&[gtk::accessible::Property::Label(title)]);
        panel.append(&chart);
        plots.append(&panel);
        chart
    });
    for (index, chart) in charts.iter().enumerate() {
        let adjustment = adjustment.clone();
        let steering = steering.clone();
        let sample = sample.clone();
        chart.set_draw_func(move |area, cr, width, height| {
            let threshold = adjustment.value().to_radians();
            let adjusted = index != 0;
            let marker = sample.get().map(|sample| {
                if adjusted {
                    sample.adjusted_marker(threshold, steering.value())
                } else {
                    (f64::from(sample.stick.x), f64::from(sample.stick.y))
                }
            });
            draw(area, cr, width, height, threshold, marker, adjusted);
        });
    }
    for setting in [adjustment, steering] {
        let chart_refs = charts.each_ref().map(|chart| chart.downgrade());
        setting.connect_value_changed(move |_| {
            for chart in chart_refs.iter().filter_map(glib::WeakRef::upgrade) {
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
    preview.append(&plots);
    preview.append(&status);
    group.add(&preview);

    let polling = Rc::new(RefCell::new(None::<glib::JoinHandle<()>>));
    let on_map = polling.clone();
    preview.connect_map(move |_| {
        let task = glib::MainContext::default().spawn_local(poll_input(
            sample.clone(),
            charts.clone(),
            status.clone(),
        ));
        on_map.replace(Some(task));
    });
    preview.connect_unmap(move |_| {
        if let Some(task) = polling.borrow_mut().take() {
            task.abort();
        }
    });
}

async fn active_node(
    client: &gio::SocketClient,
    address: &gio::UnixSocketAddress,
) -> Result<Option<PathBuf>, glib::Error> {
    let connection = client.connect_future(address).await?;
    let stream = gio::DataInputStream::new(&connection.input_stream());
    for _ in 0..64 {
        let Some(line) = stream
            .read_line_utf8_future(glib::Priority::DEFAULT)
            .await?
        else {
            break;
        };
        if let Some(node) = line.strip_prefix("active_hidraw=") {
            return Ok(hidraw_path(node));
        }
    }
    Ok(None)
}

fn hidraw_path(value: &str) -> Option<PathBuf> {
    let suffix = value.strip_prefix("/dev/hidraw")?;
    (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| PathBuf::from(value))
}

async fn poll_input(
    sample: Rc<Cell<Option<PreviewSample>>>,
    charts: [gtk::DrawingArea; 2],
    status: gtk::Label,
) {
    let update = |latest| {
        if sample.replace(latest) != latest {
            for chart in &charts {
                chart.queue_draw();
            }
        }
    };
    update(None);
    let Some(runtime) =
        std::env::var_os("XDG_RUNTIME_DIR").filter(|path| Path::new(path).is_absolute())
    else {
        status.set_text("Bridge status is unavailable");
        return;
    };
    let address =
        gio::UnixSocketAddress::new(&Path::new(&runtime).join("joyshockrust/status.sock"));
    let client = gio::SocketClient::new();
    client.set_timeout(1);
    let mut next_check = Instant::now();
    let mut input: Option<(PathBuf, File)> = None;
    let mut buffer = [0; 16_384];
    let mut direction = FlickDirection::default();
    loop {
        if Instant::now() >= next_check {
            match active_node(&client, &address).await {
                Ok(Some(node)) if input.as_ref().is_some_and(|(path, _)| *path == node) => {}
                Ok(Some(node)) => {
                    update(None);
                    direction = FlickDirection::default();
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
                latest.update(controls.right_stick, &mut direction);
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
    width: i32,
    height: i32,
    threshold: f64,
    marker: Option<(f64, f64)>,
    adjusted: bool,
) {
    // Leave room for a diagonal's travel when it snaps onto a cardinal axis.
    let radius = (f64::from(width.min(height)) * 0.5 - 9.0).max(1.0) * 0.86;
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

    if let Some((dx, dy)) = marker {
        let color_value = if adjusted { accent } else { fg };
        color(color_value, 0.5);
        cr.set_line_width(2.0);
        cr.move_to(x, y);
        cr.line_to(x + dx * radius, y + dy * radius);
        let _ = cr.stroke();
        color(color_value, 1.0);
        cr.arc(x + dx * radius, y + dy * radius, 4.5, 0.0, TAU);
        let _ = cr.fill();
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

use joyshockrust_ipc::telemetry::ImuReading;

use adw::{gtk, prelude::*};

const VELOCITY_DISPLAY_LIMIT: f64 = 180.0;

pub fn group() -> (
    adw::PreferencesGroup,
    impl Fn(Result<Option<ImuReading>, ()>),
) {
    let group = adw::PreferencesGroup::builder()
        .title("Live calibrated IMU")
        .description("Controller axes with factory calibration and any measured gyro bias applied.")
        .build();
    let velocity = ["X", "Y", "Z"].map(|axis| {
        let row = adw::ActionRow::builder()
            .title(format!("{axis} angular velocity (°/s)"))
            .subtitle("—")
            .build();
        let scale = gtk::Scale::with_range(
            gtk::Orientation::Horizontal,
            -VELOCITY_DISPLAY_LIMIT,
            VELOCITY_DISPLAY_LIMIT,
            0.01,
        );
        scale.set_value(0.0);
        scale.set_draw_value(false);
        scale.set_has_origin(false);
        scale.add_mark(0.0, gtk::PositionType::Bottom, None);
        scale.set_width_request(120);
        scale.set_hexpand(true);
        scale.set_valign(gtk::Align::Center);
        scale.set_can_target(false);
        scale.set_focusable(false);
        scale.update_property(&[gtk::accessible::Property::Label(axis)]);
        row.add_suffix(&scale);
        group.add(&row);
        (row, scale)
    });
    let acceleration = adw::ActionRow::builder()
        .title("Acceleration (g)")
        .subtitle("—")
        .build();
    group.add(&acceleration);
    let status = gtk::Label::builder()
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    group.add(&status);

    let update = move |result: Result<Option<ImuReading>, ()>| {
        let (sample, message) = match result {
            Ok(Some(sample)) => (Some(sample), ""),
            Ok(None) => (None, "Waiting for calibrated reports…"),
            Err(()) => (None, "Bridge status is unavailable."),
        };
        status.set_text(message);
        status.set_visible(!message.is_empty());
        for (index, (row, scale)) in velocity.iter().enumerate() {
            let value = sample.map(|sample| sample.gyro_dps[index]);
            scale.set_value(f64::from(value.unwrap_or_default()));
            scale.set_opacity(if value.is_some() { 1.0 } else { 0.45 });
            row.set_subtitle(&value.map_or_else(|| "—".into(), |value| format!("{value:+.3}")));
        }
        acceleration.set_subtitle(&sample.map_or_else(
            || "—".into(),
            |sample| {
                let [x, y, z] = sample.accel_g;
                format!("X {x:+.3}    Y {y:+.3}    Z {z:+.3}")
            },
        ));
    };
    (group, update)
}

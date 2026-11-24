use std::{cell::RefCell, rc::Rc, time::Duration};

use adw::{gio, glib, gtk, prelude::*};

use crate::controller_input;

pub fn group() -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Live calibrated IMU")
        .description("Controller axes with factory calibration and any measured gyro bias applied.")
        .build();
    let rows = ["Angular velocity (°/s)", "Acceleration (g)"].map(|title| {
        let row = adw::ActionRow::builder().title(title).subtitle("—").build();
        group.add(&row);
        row
    });
    let status = gtk::Label::builder()
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    group.add(&status);

    let polling = Rc::new(RefCell::new(None::<glib::JoinHandle<()>>));
    let on_map = polling.clone();
    group.connect_map(move |_| {
        let task = glib::MainContext::default().spawn_local(poll(rows.clone(), status.clone()));
        on_map.replace(Some(task));
    });
    group.connect_unmap(move |_| {
        if let Some(task) = polling.borrow_mut().take() {
            task.abort();
        }
    });
    group
}

async fn poll(rows: [adw::ActionRow; 2], status: gtk::Label) {
    let update = |sample: Option<[[f32; 3]; 2]>, message: &str| {
        status.set_text(message);
        status.set_visible(!message.is_empty());
        for (index, row) in rows.iter().enumerate() {
            let text = sample.map_or_else(
                || "—".into(),
                |sample| {
                    let [x, y, z] = sample[index];
                    format!("X {x:+.3}    Y {y:+.3}    Z {z:+.3}")
                },
            );
            row.set_subtitle(&text);
        }
    };
    update(None, "Waiting for calibrated reports…");
    let Some(address) = controller_input::status_address() else {
        update(None, "Bridge status is unavailable.");
        return;
    };
    let client = gio::SocketClient::new();
    client.set_timeout(1);
    loop {
        match controller_input::status_value(&client, &address, "imu=").await {
            Ok(value) => match value.as_deref().and_then(parse_sample) {
                Some(sample) => update(Some(sample), ""),
                None => update(None, "Waiting for calibrated reports…"),
            },
            Err(_) => update(None, "Bridge status is unavailable."),
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
}

fn parse_sample(value: &str) -> Option<[[f32; 3]; 2]> {
    let mut fields = value.split(',');
    let mut sample = [[0.0; 3]; 2];
    for axis in sample.iter_mut().flatten() {
        *axis = fields
            .next()?
            .parse::<f32>()
            .ok()
            .filter(|x| x.is_finite())?;
    }
    fields.next().is_none().then_some(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imu_requires_six_finite_values_in_device_axis_order() {
        assert_eq!(
            parse_sample("1,-2,3,0,1.01,-0.5"),
            Some([[1.0, -2.0, 3.0], [0.0, 1.01, -0.5]])
        );
        for invalid in ["1,2,3", "1,2,3,4,5,6,7", "NaN,2,3,4,5,6"] {
            assert!(parse_sample(invalid).is_none());
        }
    }
}

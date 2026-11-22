use joyshockrust_core::Button;
use joyshockrust_input::{
    DUALSENSE_EDGE_USB_REPORT_LEN, DeviceModel, Timebase, Transport, parse_report,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Only for this synthetic trace. Hardware needs a measured gap limit.
    let mut timebase = Timebase::new(10_000)?;
    let mut frame = [0; DUALSENSE_EDGE_USB_REPORT_LEN];
    frame[0] = 0x01;
    frame[1..=4].fill(128);

    for (ticks, cross_pressed) in [(0_u32, false), (3_000, true), (6_000, false)] {
        frame[8] = if cross_pressed { 0x28 } else { 0x08 };
        frame[28..32].copy_from_slice(&ticks.to_le_bytes());
        let report = parse_report(DeviceModel::DualSenseEdge, Transport::Usb, &frame, None)?;
        let timestamp = timebase.observe(report.imu.device_ticks);
        println!(
            "cross={} ticks={} time={timestamp:?}",
            report.controls.buttons.contains(Button::South),
            report.imu.device_ticks
        );
    }
    Ok(())
}

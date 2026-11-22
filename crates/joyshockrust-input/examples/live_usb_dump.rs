use std::{
    env,
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
};

use joyshockrust_input::{DeviceModel, Timebase, TimestampUpdate, Transport, parse_report};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let path = match (args.next(), args.next()) {
        (Some(path), None) => PathBuf::from(path),
        _ => return Err("usage: live_usb_dump /dev/hidrawN (wired DualSense Edge)".into()),
    };
    let mut device =
        File::open(&path).map_err(|err| format!("could not open {}: {err}", path.display()))?;
    let mut timebase = Timebase::new(3_000_000)?;
    // hidraw consumes one packet per read and can truncate a packet if the
    // buffer is too small. Use the kernel's default maximum HID buffer size.
    let mut bytes = [0; 16_384];
    let mut stdout = io::stdout().lock();
    loop {
        let len = match device.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(len) => len,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("read from {} failed: {err}", path.display()).into()),
        };

        let report = match parse_report(
            DeviceModel::DualSenseEdge,
            Transport::Usb,
            &bytes[..len],
            None,
        ) {
            Ok(report) => report,
            Err(err) => {
                eprintln!("{}: {err}", path.display());
                continue;
            }
        };
        let timestamp = timebase.observe(report.imu.device_ticks);
        write!(
            stdout,
            "ticks={} {:?} gyro_counts={:?} accel_counts={:?} time={timestamp:?}",
            report.imu.device_ticks,
            report.controls,
            report.imu.gyro_counts,
            report.imu.accel_counts,
        )?;
        match timestamp {
            TimestampUpdate::Advanced { delta, .. } => {
                writeln!(stdout, " sample_rate={:.1} Hz", 1.0 / delta.as_secs_f64())?;
            }
            _ => writeln!(stdout, " sample_rate=unavailable")?,
        }
    }
}

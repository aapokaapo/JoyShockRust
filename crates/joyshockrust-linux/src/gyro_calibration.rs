use std::{
    error::Error,
    ffi::OsString,
    fs::{self, File},
    io::{self, Write},
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::mpsc::{SyncSender, sync_channel},
    thread,
    time::{Duration, Instant},
};

use joyshockrust_input::{FactoryCalibration, RawImuSample, Timebase, TimestampUpdate};
use joyshockrust_motion::{
    Calibration, ImuSample, MotionConfig, MotionEngine, StationaryCalibration,
};
use serde::{Deserialize, Serialize};

use crate::{
    ReportTimers, SessionEvent,
    controller_config::ControllerConfig,
    discovery, forward_reports,
    haptics::{Cue, Haptics},
    hidraw::{self, ControllerId},
    profile,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredBias {
    gyro_bias_dps: [f32; 3],
}

fn bias_path(id: ControllerId) -> io::Result<PathBuf> {
    Ok(profile::default_path()?
        .with_file_name("gyro-calibration")
        .join(format!("{id}.toml")))
}

fn validated_bias(gyro_bias_dps: [f32; 3]) -> Result<Calibration, &'static str> {
    let bias = Calibration { gyro_bias_dps };
    MotionEngine::new(MotionConfig::default(), bias).map_err(|_| "Invalid gyro bias.")?;
    Ok(bias)
}

pub fn load(device: &File) -> Option<Calibration> {
    let id = hidraw::controller_id(device).ok().flatten()?;
    let result = (|| -> Result<Calibration, Box<dyn Error>> {
        let path = bias_path(id)?;
        let contents = fs::read_to_string(path)?;
        let bias: StoredBias = toml::from_str(&contents)?;
        Ok(validated_bias(bias.gyro_bias_dps)?)
    })();
    match result {
        Ok(bias) => Some(bias),
        Err(error) => {
            if !error
                .downcast_ref::<io::Error>()
                .is_some_and(|err| err.kind() == io::ErrorKind::NotFound)
            {
                eprintln!("Could not load controller gyro calibration: {error}");
            }
            None
        }
    }
}

pub fn run(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let mode = args.next();
    match mode.as_deref().and_then(|arg| arg.to_str()) {
        Some("--measure") => {
            let node = args.next().ok_or("expected a controller node")?;
            let parent = parse_number(args.next())?;
            if args.next().is_some() || parent <= 1 {
                return Err("expected a controller node and calibration window process ID".into());
            }
            nix::sys::prctl::set_pdeathsig(nix::sys::signal::Signal::SIGKILL)?;
            if nix::unistd::getppid().as_raw() != parent {
                return Err("calibration window has closed".into());
            }
            measure(Path::new(&node))?;
        }
        Some("--save") => {
            let id = args
                .next()
                .as_deref()
                .and_then(|arg| arg.to_str())
                .and_then(ControllerId::parse)
                .ok_or("expected a controller ID")?;
            let bias = [
                parse_number(args.next())?,
                parse_number(args.next())?,
                parse_number(args.next())?,
            ];
            if args.next().is_some() {
                return Err("expected a controller ID and three bias values".into());
            }
            save(&bias_path(id)?, bias)?;
        }
        _ => return Err("expected --measure or --save".into()),
    }
    Ok(())
}

fn save(path: &Path, bias: [f32; 3]) -> Result<(), Box<dyn Error>> {
    validated_bias(bias)?;
    let text = toml::to_string(&StoredBias {
        gyro_bias_dps: bias,
    })?;
    profile::replace_file(path, text.as_bytes())?;
    Ok(())
}

pub struct BiasWriter(SyncSender<Calibration>);

impl BiasWriter {
    pub fn open(input: &File) -> io::Result<Option<Self>> {
        let Some(id) = hidraw::controller_id(input)? else {
            return Ok(None);
        };
        let path = bias_path(id)?;
        let (sender, receiver) = sync_channel::<Calibration>(1);
        thread::Builder::new()
            .name("gyro-calibration-save".into())
            .spawn(move || {
                while let Ok(calibration) = receiver.recv() {
                    if let Err(error) = save(&path, calibration.gyro_bias_dps) {
                        eprintln!("Could not save controller gyro calibration: {error}");
                    }
                }
            })?;
        Ok(Some(Self(sender)))
    }

    pub fn send(&self, calibration: Calibration) {
        let _ = self.0.try_send(calibration);
    }
}

fn parse_number<T: std::str::FromStr>(arg: Option<OsString>) -> Result<T, &'static str> {
    arg.as_deref()
        .and_then(|arg| arg.to_str())
        .and_then(|value| value.parse().ok())
        .ok_or("invalid or missing calibration number")
}

fn measure(node: &Path) -> Result<(), Box<dyn Error>> {
    let (mut device, identity) =
        discovery::open_controller(node, ControllerConfig::load().filter())?;
    let id = hidraw::controller_id(&device)?
        .ok_or("This controller has no stable identifier for calibration.")?;
    let factory = hidraw::read_factory_calibration(&device, identity.transport)?;
    let mut sampler = Sampler::new(factory);
    let mut measured = None;
    let haptics = Haptics::open(&device, identity.transport);
    println!("ready");
    io::stdout().flush()?;
    if let Some(haptics) = &haptics {
        haptics.send(Cue::CalibrationStart);
    }
    let settle_at = Instant::now()
        + if haptics.is_some() {
            Duration::from_millis(500)
        } else {
            Duration::ZERO
        };
    forward_reports(
        &mut device,
        identity,
        None,
        None,
        None,
        ReportTimers {
            stop_at: Some(Instant::now() + Duration::from_secs(15)),
            ..Default::default()
        },
        |event| {
            if let SessionEvent::Report(report) = event
                && Instant::now() >= settle_at
                && let Some(bias) = sampler.observe(report.imu)
            {
                measured = Some(bias);
                return Ok(ControlFlow::Break(()));
            }
            Ok(ControlFlow::Continue(()))
        },
    )?;
    let [x, y, z] = measured
        .ok_or("Controller disconnected before gyro calibration finished.")?
        .gyro_bias_dps;
    if let Some(haptics) = haptics {
        haptics.finish(Cue::CalibrationDone);
    }
    println!("bias {id} {x} {y} {z}");
    Ok(())
}

struct Sampler {
    factory: FactoryCalibration,
    clock: Timebase,
    estimate: StationaryCalibration,
}

impl Sampler {
    fn new(factory: FactoryCalibration) -> Self {
        Self {
            factory,
            clock: Timebase::new(150_000).expect("valid 50 ms sensor gap"),
            estimate: StationaryCalibration::default(),
        }
    }

    fn observe(&mut self, sample: RawImuSample) -> Option<Calibration> {
        let elapsed = match self.clock.observe(sample.device_ticks) {
            TimestampUpdate::FirstSample => Duration::ZERO,
            TimestampUpdate::Advanced { elapsed, .. } => elapsed,
            TimestampUpdate::Gap { .. } => {
                self.estimate.reset();
                Duration::ZERO
            }
            TimestampUpdate::Repeated { .. } | TimestampUpdate::OutOfOrder { .. } => return None,
        };
        let calibrated = self.factory.convert(sample);
        self.estimate.observe(ImuSample {
            timestamp_ns: elapsed.as_nanos() as u64,
            gyro_dps: calibrated.gyro_dps,
            accel_g: calibrated.accel_g,
        })
    }
}

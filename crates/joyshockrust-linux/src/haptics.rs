use std::{
    ffi::OsString,
    fs::File,
    io::{self, Write},
    ops::ControlFlow,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use joyshockrust_input::Transport;

use crate::{
    ReportTimers, SessionEvent,
    controller_config::ControllerConfig,
    discovery,
    flick_stick::FlickStick,
    forward_reports,
    game_calibration::Dial,
    hidraw::{self, Identity},
};

#[derive(Clone, Copy)]
pub enum Cue {
    CalibrationStart,
    CalibrationDone,
    Detent,
}

pub struct Haptics {
    sender: SyncSender<Cue>,
    worker: JoinHandle<io::Result<()>>,
}

impl Haptics {
    pub fn open(input: &File, transport: Transport) -> Option<Self> {
        if transport != Transport::Usb {
            return None;
        }
        match Self::open_usb(input) {
            Ok(haptics) => Some(haptics),
            Err(error) => {
                eprintln!("DualSense USB calibration haptics unavailable: {error}");
                None
            }
        }
    }

    fn open_usb(input: &File) -> io::Result<Self> {
        Self::open_pcm(playback_device(input)?)
    }

    fn open_pcm(device: String) -> io::Result<Self> {
        let (sender, receiver) = sync_channel(1);
        let worker = thread::Builder::new()
            .name("calibration-haptics".into())
            .spawn(move || {
                let waveforms = Waveforms::new();
                while let Ok(cue) = receiver.recv() {
                    if let Err(error) = play(&device, &receiver, &waveforms, cue) {
                        eprintln!("DualSense USB calibration haptics unavailable: {error}");
                        return Err(error);
                    }
                }
                Ok(())
            })?;
        Ok(Self { sender, worker })
    }

    pub fn send(&self, cue: Cue) {
        // Coalesce fast detents instead of delaying the next tactile click.
        let _ = self.sender.try_send(cue);
    }
    // Standalone calibration can wait after releasing its report reader.
    pub fn finish(self, cue: Cue) {
        let _ = self.sender.send(cue);
        let _ = self.drain();
    }

    fn drain(self) -> io::Result<()> {
        let Self { sender, worker } = self;
        drop(sender);
        worker
            .join()
            .map_err(|_| io::Error::other("haptics worker stopped unexpectedly"))?
    }
}

const TEST_USAGE: &str = "usage: joyshockrust-linux --test-haptics [--pattern all|start|done|detent|sweep] [--pcm ALSA_DEVICE] [/dev/hidrawN]";

pub fn test_cli(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let mut pattern = String::from("all");
    let mut pcm = None;
    let mut node = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help") => {
                println!("{TEST_USAGE}");
                return Ok(());
            }
            Some("--pattern") => {
                pattern = args
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, TEST_USAGE))?
            }
            Some("--pcm") if pcm.is_none() => {
                pcm = Some(
                    args.next()
                        .and_then(|value| value.into_string().ok())
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, TEST_USAGE))?,
                );
            }
            _ if node.is_none() && !arg.as_encoded_bytes().starts_with(b"-") => {
                node = Some(PathBuf::from(arg))
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, TEST_USAGE)),
        }
    }
    let cues: &[(&str, Cue)] = match pattern.as_str() {
        "start" => &[("Start pulse", Cue::CalibrationStart)],
        "done" => &[("Completion double pulse", Cue::CalibrationDone)],
        "detent" => &[("Detent clicks", Cue::Detent)],
        "all" => &[
            ("Start pulse", Cue::CalibrationStart),
            ("Completion double pulse", Cue::CalibrationDone),
            ("Detent clicks", Cue::Detent),
        ],
        "sweep" => &[],
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, TEST_USAGE)),
    };
    let mut controller = None;
    if pcm.is_none() || node.is_some() || pattern == "sweep" {
        let filter = discovery::ControllerFilter {
            transport: discovery::Selection::Usb,
            ..Default::default()
        };
        let (file, identity, path) = if let Some(path) = node {
            let (file, identity) = discovery::open_controller(&path, filter).map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })?;
            (file, identity, path)
        } else {
            let (file, candidate) =
                discovery::open_first_controller(filter, ControllerConfig::load().preferred())?;
            (file, candidate.identity, candidate.path)
        };
        println!(
            "Controller: {} ({} USB)",
            path.display(),
            identity.model_name()
        );
        if pcm.is_none() {
            pcm = Some(playback_device(&file)?);
        }
        controller = Some((file, identity));
    }
    let device = pcm.expect("resolved ALSA playback device");
    println!("ALSA playback: {device}, 48000 Hz, 16-bit, 4 channels (actuators on 3–4)");
    if pattern == "sweep" {
        let (file, identity) = controller.expect("sweep controller");
        return test_sweep(file, identity, device);
    }

    let waveforms = Waveforms::new();
    for (index, &(description, cue)) in cues.iter().enumerate() {
        if index != 0 {
            thread::sleep(Duration::from_millis(450));
        }
        println!("Playing: {description}");
        let (sender, receiver) = sync_channel(1);
        if matches!(cue, Cue::Detent) {
            let repeating = thread::spawn(move || {
                for _ in 0..5 {
                    thread::sleep(Duration::from_millis(95));
                    if sender.send(Cue::Detent).is_err() {
                        break;
                    }
                }
            });
            let result = play(&device, &receiver, &waveforms, cue);
            drop(receiver);
            let _ = repeating.join();
            result?;
        } else {
            drop(sender);
            play(&device, &receiver, &waveforms, cue)?;
        }
    }
    println!("PCM playback completed. Check whether you felt the cues on the controller.");
    Ok(())
}

fn test_sweep(mut input: File, identity: Identity, device: String) -> io::Result<()> {
    let haptics = Haptics::open_pcm(device)?;
    let mut flick = FlickStick::new(1.0);
    let mut dial = Dial::default();
    let mut steps = 0;
    println!("Center the right stick, then sweep it slowly around the edge for 12 seconds.");
    let result = forward_reports(
        &mut input,
        identity,
        None,
        None,
        None,
        ReportTimers {
            stop_at: Some(Instant::now() + Duration::from_secs(12)),
            ..Default::default()
        },
        |event| {
            if let SessionEvent::Report(report) = event {
                flick.advance_with_rotation(
                    report.controls.right_stick,
                    report.imu.device_ticks,
                    |angle| {
                        if dial.rotate(angle) {
                            steps += 1;
                            haptics.send(Cue::Detent);
                        }
                    },
                );
            }
            Ok(ControlFlow::Continue(()))
        },
    );
    let playback = haptics.drain();
    println!("Stick detents detected: {steps}");
    match result {
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
        Err(error) => return Err(error),
        Ok(()) => {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Controller disconnected during the sweep.",
            ));
        }
    }
    playback?;
    if steps == 0 {
        return Err(io::Error::other(
            "No stick detents detected. Start centered and rotate the stick along its edge.",
        ));
    }
    println!("PCM playback completed. Check whether you felt the detents on the controller.");
    Ok(())
}

fn playback_device(input: &File) -> io::Result<String> {
    let usb = hidraw::hid_parent(input)?
        .parent_with_subsystem_devtype("usb", "usb_device")?
        .ok_or_else(|| io::Error::other("could not find the controller's USB device"))?;
    let mut devices = udev::Enumerator::new()?;
    devices.match_parent(&usb)?;
    devices.match_subsystem("sound")?;
    devices
        .scan_devices()?
        .find_map(|device| pcm_address(device.sysname().to_str()?))
        .ok_or_else(|| io::Error::other("the selected controller has no ALSA playback device"))
}

fn pcm_address(name: &str) -> Option<String> {
    let (card, device) = name
        .strip_prefix("pcmC")?
        .strip_suffix('p')?
        .split_once('D')?;
    Some(format!(
        "hw:{},{}",
        card.parse::<u32>().ok()?,
        device.parse::<u32>().ok()?
    ))
}

struct Waveforms {
    start: Vec<u8>,
    done: Vec<u8>,
    detent: Vec<u8>,
}

impl Waveforms {
    fn new() -> Self {
        Self {
            start: waveform(Cue::CalibrationStart),
            done: waveform(Cue::CalibrationDone),
            detent: waveform(Cue::Detent),
        }
    }

    fn samples(&self, cue: Cue) -> &[u8] {
        match cue {
            Cue::CalibrationStart => &self.start,
            Cue::CalibrationDone => &self.done,
            Cue::Detent => &self.detent,
        }
    }
}

fn play(device: &str, receiver: &Receiver<Cue>, waveforms: &Waveforms, cue: Cue) -> io::Result<()> {
    let mut player = Command::new("aplay")
        .args([
            "-q",
            "-N",
            "-D",
            device,
            "-t",
            "raw",
            "-c",
            "4",
            "-r",
            "48000",
            "-f",
            "S16_LE",
            "--buffer-time=32000",
            "--period-time=8000",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not launch aplay for {device}: {error}"),
            )
        })?;
    let mut pipe = player.stdin.take().expect("piped aplay stdin");
    let result = stream_cues(&mut pipe, receiver, waveforms, cue);
    drop(pipe);
    let status = player.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "aplay on {device} exited with {status}"
        )));
    }
    result
}

fn stream_cues(
    mut output: impl Write,
    receiver: &Receiver<Cue>,
    waveforms: &Waveforms,
    mut cue: Cue,
) -> io::Result<()> {
    const PERIOD_MS: usize = 8;
    const PERIOD_BYTES: usize = SAMPLE_RATE / 1_000 * PERIOD_MS * 8;
    let period = Duration::from_millis(PERIOD_MS as u64);
    let mut next = Instant::now();
    let mut idle_until = next + Duration::from_millis(175);
    let mut samples = waveforms.samples(cue);
    loop {
        thread::sleep(next.saturating_duration_since(Instant::now()));
        let now = Instant::now();
        match receiver.try_recv() {
            Ok(incoming)
                if !matches!(incoming, Cue::Detent)
                    || samples.is_empty()
                    || matches!(cue, Cue::Detent) =>
            {
                cue = incoming;
                samples = waveforms.samples(cue);
                idle_until = now + Duration::from_millis(175);
            }
            Ok(_) | Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) if samples.is_empty() => break,
            Err(TryRecvError::Disconnected) => {}
        }
        if samples.is_empty() && now >= idle_until {
            break;
        }
        let mut block = [0; PERIOD_BYTES];
        let length = block.len().min(samples.len());
        block[..length].copy_from_slice(&samples[..length]);
        samples = &samples[length..];
        output.write_all(&block)?;
        next = (next + period).max(Instant::now());
    }
    Ok(())
}

const SAMPLE_RATE: usize = 48_000;

fn waveform(cue: Cue) -> Vec<u8> {
    let (length_ms, frequency, level) = match cue {
        Cue::CalibrationStart => (86, 125.0, 0.42),
        Cue::CalibrationDone => (150, 180.0, 0.44),
        Cue::Detent => (15, 210.0, 0.36),
    };
    let frames = length_ms * SAMPLE_RATE / 1000;
    let mut bytes = Vec::with_capacity(frames * 8);
    for frame in 0..frames {
        let time = frame as f64 / SAMPLE_RATE as f64;
        let progress = frame as f64 / frames as f64;
        let envelope = match cue {
            Cue::CalibrationDone => (std::f64::consts::TAU * progress).sin().powi(2),
            _ => (std::f64::consts::PI * progress).sin().powi(2),
        };
        let signal = (std::f64::consts::TAU * frequency * time).sin() * envelope * level;
        let sample = (signal * f64::from(i16::MAX)) as i16;
        // USB audio channels 1–2 are audible output. Only 3–4 drive the actuators.
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&sample.to_le_bytes());
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_cues_use_only_the_actuator_channels() {
        for cue in [Cue::CalibrationStart, Cue::CalibrationDone, Cue::Detent] {
            let pcm = waveform(cue);
            let (frames, remaining) = pcm.as_chunks::<8>();
            assert!(remaining.is_empty());
            assert!(frames.iter().all(|frame| frame[..4] == [0; 4]));
            assert!(frames.iter().any(|frame| frame[4..] != [0; 4]));
        }
        assert_eq!(pcm_address("pcmC2D0p").as_deref(), Some("hw:2,0"));
        assert_eq!(pcm_address("pcmC2D0c"), None);
    }

    #[test]
    fn pcm_stream_drains_the_final_cue_and_pads_the_last_period() {
        let (sender, receiver) = sync_channel(1);
        drop(sender);
        let waveforms = Waveforms {
            start: Vec::new(),
            done: Vec::new(),
            detent: waveform(Cue::Detent),
        };
        let mut output = Vec::new();
        stream_cues(&mut output, &receiver, &waveforms, Cue::Detent).unwrap();
        assert!(output.starts_with(&waveforms.detent));
        assert_eq!(output.len(), SAMPLE_RATE / 1000 * 16 * 8);
        assert!(
            output[waveforms.detent.len()..]
                .iter()
                .all(|&byte| byte == 0)
        );
    }
}

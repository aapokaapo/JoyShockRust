//! Bridge-owned calibration sessions.
//!
//! One thread serves the calibration socket. A connected client owns the
//! controller until its connection closes. The thread asks the main loop for
//! an output lease through a channel and wake descriptor, so the report path
//! gains no work. The main loop pauses outputs and records the previous
//! state, and restores that state when the lease is released.
//!
//! Measurements run in worker processes started from the bridge executable
//! (`--gyro-calibration --measure` and `--calibrate-game --measure-stick`).
//! Killing a worker makes cancellation and timeouts reliable, and the
//! workers keep the existing measurement code and CLI entry points.

use std::{
    io::{self, PipeReader, Read, Write},
    os::{
        fd::AsFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use joyshockrust_ipc::calibration::{
    BridgeMessage, ClientMessage, CloseReason, MAX_LINE_BYTES, Progress, SessionKind,
};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};

/// Main-loop operations requested by the session thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseAction {
    /// Pause outputs for the session and remember the previous state.
    Acquire(u64),
    /// Restore the output state remembered by the matching acquire.
    Release(u64),
}

pub(super) struct LeaseRequest {
    pub(super) action: LeaseAction,
    pub(super) reply: SyncSender<bool>,
}

/// The main loop's side of the session thread.
pub(super) struct Server {
    pub(super) wake: UnixStream,
    requests: Receiver<LeaseRequest>,
}

impl Server {
    pub(super) fn start(listener: UnixListener, config: Config) -> io::Result<Self> {
        let (wake, notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        let (sender, requests) = sync_channel(1);
        let leases = Leases {
            sender,
            notify,
            timeout: config.timeouts.lease,
        };
        thread::Builder::new()
            .name("jsr-calibration".into())
            .spawn(move || serve(&listener, &leases, &config))?;
        Ok(Self { wake, requests })
    }

    pub(super) fn receive(&self) -> io::Result<Option<LeaseRequest>> {
        match (&self.wake).read(&mut [0]) {
            Ok(0) => return Err(io::Error::other("calibration session thread stopped")),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(err) => return Err(err),
        }
        Ok(self.requests.try_recv().ok())
    }
}

/// Persistence used by sessions. Tests replace the files with a record.
pub(super) trait Store: Send + 'static {
    fn save_gyro(&self, controller: &str, bias_dps: [f32; 3]) -> Result<(), String>;
    fn save_game(
        &self,
        profile: &Path,
        counts: i32,
        sensitivity: f64,
        turns: u32,
    ) -> Result<f64, String>;
}

pub(super) struct Files;

impl Store for Files {
    fn save_gyro(&self, controller: &str, bias_dps: [f32; 3]) -> Result<(), String> {
        crate::gyro_calibration::save_for(controller, bias_dps).map_err(|err| err.to_string())
    }

    fn save_game(
        &self,
        profile: &Path,
        counts: i32,
        sensitivity: f64,
        turns: u32,
    ) -> Result<f64, String> {
        crate::game_calibration::save_measurement(profile, counts, sensitivity, turns)
            .map_err(|err| err.to_string())
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Timeouts {
    /// Time for a new client to send its `begin` request.
    pub(super) handshake: Duration,
    /// Time for the main loop to grant or release a lease.
    pub(super) lease: Duration,
    /// Upper bound for a stationary gyro measurement.
    pub(super) gyro: Duration,
    /// Upper bound for an RWC stick sweep, which waits on the user.
    pub(super) game: Duration,
    /// Session lifetime without a client request while nothing is measured.
    pub(super) idle: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(5),
            lease: Duration::from_secs(5),
            gyro: Duration::from_secs(30),
            game: Duration::from_secs(10 * 60),
            idle: Duration::from_secs(10 * 60),
        }
    }
}

pub(super) struct Config {
    /// Executable providing the measurement worker modes.
    pub(super) program: PathBuf,
    pub(super) timeouts: Timeouts,
    pub(super) store: Box<dyn Store>,
}

struct Leases {
    sender: SyncSender<LeaseRequest>,
    notify: UnixStream,
    timeout: Duration,
}

impl Leases {
    fn request(&self, action: LeaseAction) -> Result<bool, &'static str> {
        let (reply, response) = sync_channel(1);
        self.sender
            .send(LeaseRequest { action, reply })
            .map_err(|_| "the bridge main loop stopped")?;
        (&self.notify)
            .write_all(&[1])
            .map_err(|_| "the bridge main loop stopped")?;
        match response.recv_timeout(self.timeout) {
            Ok(granted) => Ok(granted),
            // A dropped request means the main loop had nothing to apply.
            Err(RecvTimeoutError::Disconnected) => Ok(false),
            Err(RecvTimeoutError::Timeout) => Err("the bridge did not respond in time"),
        }
    }
}

fn serve(listener: &UnixListener, leases: &Leases, config: &Config) {
    if let Err(err) = listener.set_nonblocking(true) {
        eprintln!("Calibration sessions unavailable: {err}");
        return;
    }
    // Start from a clock-derived value so ids differ across bridge restarts.
    let mut next_session = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |time| time.as_secs() << 16);
    loop {
        if let Err(err) = wait_readable(listener, None) {
            eprintln!("Calibration sessions stopped: {err}");
            return;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(err) if is_retryable(&err) => continue,
            Err(err) => {
                eprintln!("Calibration sessions stopped: {err}");
                return;
            }
        };
        next_session = next_session.wrapping_add(1).max(1);
        if let Err(err) = run_session(stream, listener, leases, config, next_session) {
            eprintln!("Calibration session {next_session} ended: {err}");
        }
    }
}

fn is_retryable(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
    )
}

fn wait_readable(fd: impl AsFd, deadline: Option<Instant>) -> io::Result<bool> {
    let mut descriptors = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
    loop {
        match poll(&mut descriptors, poll_timeout(deadline)) {
            Ok(0) => return Ok(false),
            Ok(_) => return Ok(true),
            Err(Errno::EINTR) => continue,
            Err(err) => return Err(err.into()),
        }
    }
}

fn poll_timeout(deadline: Option<Instant>) -> PollTimeout {
    deadline.map_or(PollTimeout::NONE, |deadline| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        PollTimeout::try_from(remaining.as_nanos().div_ceil(1_000_000)).unwrap_or(PollTimeout::MAX)
    })
}

/// Turn away connections that arrive while a session owns the controller.
fn refuse_waiting(listener: &UnixListener) {
    while let Ok((mut stream, _)) = listener.accept() {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let _ = stream.write_all(BridgeMessage::Busy.encode().as_bytes());
    }
}

/// Splits a byte stream into bounded lines.
#[derive(Default)]
struct Lines {
    buffer: Vec<u8>,
}

impl Lines {
    /// Read once. Returns `false` at end of stream.
    fn fill(&mut self, mut input: impl Read) -> io::Result<bool> {
        let mut bytes = [0; 1024];
        let len = loop {
            match input.read(&mut bytes) {
                Ok(len) => break len,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        };
        self.buffer.extend_from_slice(&bytes[..len]);
        if self.buffer.len() > MAX_LINE_BYTES && !self.buffer.contains(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "calibration line too long",
            ));
        }
        Ok(len > 0)
    }

    fn next(&mut self) -> Option<String> {
        let end = self.buffer.iter().position(|&byte| byte == b'\n')?;
        let line: Vec<u8> = self.buffer.drain(..=end).collect();
        Some(String::from_utf8_lossy(&line[..end]).trim().to_owned())
    }

    /// The unterminated remainder after end of stream.
    fn rest(&mut self) -> Option<String> {
        let line = String::from_utf8_lossy(&std::mem::take(&mut self.buffer))
            .trim()
            .to_owned();
        (!line.is_empty()).then_some(line)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Measurement {
    Gyro,
    Game { sensitivity: f64 },
}

#[derive(Debug, PartialEq)]
enum Measured {
    Gyro {
        id: u64,
        controller: String,
        bias_dps: [f32; 3],
    },
    Game {
        id: u64,
        counts: i32,
        sensitivity: f64,
    },
}

struct Worker {
    child: Child,
    output: PipeReader,
    lines: Lines,
    measurement: Measurement,
    deadline: Instant,
    result: Option<Result<Measured, String>>,
    last: Option<String>,
}

impl Worker {
    fn spawn(config: &Config, node: &Path, measurement: Measurement) -> io::Result<Self> {
        let (output, writer) = io::pipe()?;
        let mut command = Command::new(&config.program);
        let limit = match measurement {
            Measurement::Gyro => {
                command.args(["--gyro-calibration", "--measure"]);
                config.timeouts.gyro
            }
            Measurement::Game { sensitivity } => {
                command.args([
                    "--calibrate-game".to_owned(),
                    "--measure-stick".to_owned(),
                    sensitivity.to_string(),
                ]);
                config.timeouts.game
            }
        };
        // The worker exits with the bridge thread that started it.
        command
            .arg(node)
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer);
        let child = command.spawn()?;
        // Close the parent's write ends so end of stream marks worker exit.
        drop(command);
        Ok(Self {
            child,
            output,
            lines: Lines::default(),
            measurement,
            deadline: Instant::now() + limit,
            result: None,
            last: None,
        })
    }

    /// Interpret a worker output line. Returns progress to forward.
    fn line(&mut self, line: String, id: u64) -> Option<Progress> {
        if line.is_empty() {
            return None;
        }
        if let Some(progress) = Progress::parse(&line) {
            return Some(progress);
        }
        let mut fields = line.split_ascii_whitespace();
        let result = match (self.measurement, fields.next()) {
            (Measurement::Gyro, Some("bias")) => {
                let controller = fields.next().map(str::to_owned);
                let mut bias = [0.0f32; 3];
                let mut valid = controller.is_some();
                for value in &mut bias {
                    match fields.next().and_then(|text| text.parse().ok()) {
                        Some(number) => *value = number,
                        None => valid = false,
                    }
                }
                valid.then(|| Measured::Gyro {
                    id,
                    controller: controller.unwrap_or_default(),
                    bias_dps: bias,
                })
            }
            (Measurement::Game { sensitivity }, Some("counts")) => fields
                .next()
                .and_then(|text| text.parse().ok())
                .filter(|&counts: &i32| counts > 0)
                .map(|counts| Measured::Game {
                    id,
                    counts,
                    sensitivity,
                }),
            _ => None,
        };
        match result {
            Some(result) if fields.next().is_none() => self.result = Some(Ok(result)),
            _ => self.last = Some(worker_error(&line)),
        }
        None
    }

    fn finish(mut self) -> Result<Measured, String> {
        let status = self.child.wait();
        let failure = || {
            self.last
                .clone()
                .unwrap_or_else(|| "The calibration process exited.".to_owned())
        };
        match status {
            Ok(status) if status.success() => self.result.take().unwrap_or_else(|| {
                Err(match self.measurement {
                    Measurement::Gyro => "The controller did not return a gyro correction.",
                    Measurement::Game { .. } => "The measurement returned no mouse counts.",
                }
                .to_owned())
            }),
            Ok(_) => Err(failure()),
            Err(err) => Err(err.to_string()),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Session<'a> {
    id: u64,
    kind: SessionKind,
    node: PathBuf,
    client: UnixStream,
    config: &'a Config,
    worker: Option<Worker>,
    measured: Option<Measured>,
    next_measurement: u64,
}

enum Flow {
    Continue,
    End,
}

fn send(client: &UnixStream, message: &BridgeMessage) -> io::Result<()> {
    (&*client).write_all(message.encode().as_bytes())
}

fn run_session(
    client: UnixStream,
    listener: &UnixListener,
    leases: &Leases,
    config: &Config,
    id: u64,
) -> io::Result<()> {
    client.set_nonblocking(false)?;
    client.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut lines = Lines::default();
    let deadline = Instant::now() + config.timeouts.handshake;
    let begin = loop {
        if let Some(line) = lines.next() {
            break line;
        }
        if !wait_readable(&client, Some(deadline))? {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no begin request"));
        }
        if !lines.fill(&client)? {
            return Ok(());
        }
    };
    let Ok(ClientMessage::Begin { kind, node }) = ClientMessage::decode(&begin) else {
        send(
            &client,
            &BridgeMessage::Rejected("expected a begin request".into()),
        )?;
        return Ok(());
    };
    match leases.request(LeaseAction::Acquire(id)) {
        Ok(true) => {}
        Ok(false) => {
            send(
                &client,
                &BridgeMessage::Rejected("The bridge could not release the controller.".into()),
            )?;
            return Ok(());
        }
        Err(reason) => {
            // A late grant must not leave outputs paused without a session.
            let _ = leases.request(LeaseAction::Release(id));
            send(&client, &BridgeMessage::Rejected(reason.into()))?;
            return Ok(());
        }
    }
    eprintln!("Calibration session {id} started for {}", node.display());
    let mut session = Session {
        id,
        kind,
        node,
        client,
        config,
        worker: None,
        measured: None,
        next_measurement: 1,
    };
    let result = session.run(listener, lines);
    // Stop measurement before handing the controller back.
    session.worker.take();
    match leases.request(LeaseAction::Release(id)) {
        Ok(true) => eprintln!("Calibration session {id} ended; outputs restored"),
        Ok(false) => eprintln!("Calibration session {id} ended; the lease was already released"),
        Err(reason) => {
            eprintln!("Calibration session {id} ended; could not restore outputs: {reason}")
        }
    }
    result
}

impl Session<'_> {
    fn run(&mut self, listener: &UnixListener, mut lines: Lines) -> io::Result<()> {
        send(&self.client, &BridgeMessage::Started { session: self.id })?;
        let mut idle_at = Instant::now() + self.config.timeouts.idle;
        loop {
            while let Some(line) = lines.next() {
                idle_at = Instant::now() + self.config.timeouts.idle;
                if let Flow::End = self.request(&line)? {
                    return Ok(());
                }
            }
            let deadline = self
                .worker
                .as_ref()
                .map_or(idle_at, |worker| worker.deadline);
            let client = self.client.try_clone()?;
            let mut descriptors = vec![
                PollFd::new(client.as_fd(), PollFlags::POLLIN),
                PollFd::new(listener.as_fd(), PollFlags::POLLIN),
            ];
            if let Some(worker) = &self.worker {
                descriptors.push(PollFd::new(worker.output.as_fd(), PollFlags::POLLIN));
            }
            let ready = loop {
                match poll(&mut descriptors, poll_timeout(Some(deadline))) {
                    Ok(ready) => break ready,
                    Err(Errno::EINTR) => continue,
                    Err(err) => return Err(err.into()),
                }
            };
            let events: Vec<_> = descriptors
                .iter()
                .map(|descriptor| descriptor.revents().unwrap_or_else(PollFlags::empty))
                .collect();
            drop(descriptors);
            if ready == 0 {
                if self.worker.take().is_some() {
                    send(
                        &self.client,
                        &BridgeMessage::Failed("The measurement timed out.".into()),
                    )?;
                    idle_at = Instant::now() + self.config.timeouts.idle;
                    continue;
                }
                send(&self.client, &BridgeMessage::Closed(CloseReason::Idle))?;
                return Ok(());
            }
            if !events[1].is_empty() {
                refuse_waiting(listener);
            }
            if events.get(2).is_some_and(|flags| !flags.is_empty()) {
                self.worker_output()?;
                idle_at = Instant::now() + self.config.timeouts.idle;
            }
            if !events[0].is_empty() && !lines.fill(&self.client)? {
                return Ok(());
            }
        }
    }

    fn worker_output(&mut self) -> io::Result<()> {
        let Some(worker) = &mut self.worker else {
            return Ok(());
        };
        let open = worker.lines.fill(&worker.output)?;
        let id = self.next_measurement;
        let mut progress = Vec::new();
        while let Some(line) = worker.lines.next() {
            progress.extend(worker.line(line, id));
        }
        if !open && let Some(line) = worker.lines.rest() {
            progress.extend(worker.line(line, id));
        }
        for progress in progress {
            send(&self.client, &BridgeMessage::Progress(progress))?;
        }
        if open {
            return Ok(());
        }
        let worker = self.worker.take().expect("running worker");
        let reply = match worker.finish() {
            Ok(measured) => {
                self.next_measurement += 1;
                let reply = match &measured {
                    Measured::Gyro {
                        id,
                        controller,
                        bias_dps,
                    } => BridgeMessage::GyroMeasured {
                        measurement: *id,
                        controller: controller.clone(),
                        bias_dps: *bias_dps,
                    },
                    Measured::Game { id, counts, .. } => BridgeMessage::GameMeasured {
                        measurement: *id,
                        counts: *counts,
                    },
                };
                self.measured = Some(measured);
                reply
            }
            Err(reason) => BridgeMessage::Failed(reason),
        };
        send(&self.client, &reply)
    }

    fn request(&mut self, line: &str) -> io::Result<Flow> {
        let reply = match ClientMessage::decode(line) {
            Err(reason) => BridgeMessage::Rejected(reason.into()),
            Ok(ClientMessage::End) => return Ok(Flow::End),
            Ok(ClientMessage::Begin { .. }) => {
                BridgeMessage::Rejected("The session has already started.".into())
            }
            Ok(ClientMessage::Cancel) => match self.worker.take() {
                Some(_) => BridgeMessage::Cancelled,
                None => BridgeMessage::Rejected("No measurement is running.".into()),
            },
            Ok(ClientMessage::MeasureGyro) => match self.measure(Measurement::Gyro) {
                Some(reply) => reply,
                None => return Ok(Flow::Continue),
            },
            Ok(ClientMessage::MeasureGame { sensitivity }) => {
                match self.measure(Measurement::Game { sensitivity }) {
                    Some(reply) => reply,
                    None => return Ok(Flow::Continue),
                }
            }
            Ok(ClientMessage::SaveGyro { measurement }) => match &self.measured {
                _ if self.worker.is_some() => busy_measuring(),
                Some(Measured::Gyro {
                    id,
                    controller,
                    bias_dps,
                }) if *id == measurement => {
                    match self.config.store.save_gyro(controller, *bias_dps) {
                        Ok(()) => BridgeMessage::GyroSaved,
                        Err(reason) => BridgeMessage::Failed(reason),
                    }
                }
                _ => stale(),
            },
            Ok(ClientMessage::SaveGame {
                measurement,
                turns,
                profile,
            }) => match &self.measured {
                _ if self.worker.is_some() => busy_measuring(),
                Some(Measured::Game {
                    id,
                    counts,
                    sensitivity,
                }) if *id == measurement => {
                    if turns == 0 {
                        BridgeMessage::Rejected("Enter a positive whole number.".into())
                    } else {
                        match self
                            .config
                            .store
                            .save_game(&profile, *counts, *sensitivity, turns)
                        {
                            Ok(calibration) => BridgeMessage::GameSaved { calibration },
                            Err(reason) => BridgeMessage::Failed(reason),
                        }
                    }
                }
                _ => stale(),
            },
        };
        send(&self.client, &reply)?;
        Ok(Flow::Continue)
    }

    /// Start a worker. Progress and the result follow from its output.
    fn measure(&mut self, measurement: Measurement) -> Option<BridgeMessage> {
        let expected = match measurement {
            Measurement::Gyro => SessionKind::Gyro,
            Measurement::Game { .. } => SessionKind::Game,
        };
        if expected != self.kind {
            return Some(BridgeMessage::Rejected(
                "This session does not offer that measurement.".into(),
            ));
        }
        if self.worker.is_some() {
            return Some(busy_measuring());
        }
        // A new measurement invalidates confirmation of the previous one.
        self.measured = None;
        match Worker::spawn(self.config, &self.node, measurement) {
            Ok(worker) => {
                self.worker = Some(worker);
                None
            }
            Err(err) => Some(BridgeMessage::Failed(format!(
                "Could not start the measurement: {err}"
            ))),
        }
    }
}

/// The text of a worker's `Error: "..."` exit line, or the line itself.
fn worker_error(line: &str) -> String {
    let text = line.strip_prefix("Error: ").unwrap_or(line);
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
        .to_owned()
}

fn busy_measuring() -> BridgeMessage {
    BridgeMessage::Rejected("A measurement is running.".into())
}

fn stale() -> BridgeMessage {
    BridgeMessage::Rejected("The measurement is stale or unknown. Measure again.".into())
}

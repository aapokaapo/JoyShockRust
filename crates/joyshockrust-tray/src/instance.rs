//! Single tray instance and the launcher request that reaches it.
//!
//! The running tray holds an exclusive lock on a file in the user runtime
//! directory for its whole lifetime and serves a Unix socket next to it.
//! A second tray sees the lock and exits. The desktop launcher connects to
//! the socket to ask the running tray to start the bridge and show the
//! profile editor, starting the tray first when none is running.

use std::{
    env,
    fs::{self, File, TryLockError},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Kept outside the bridge's runtime directory, which systemd removes when
/// the bridge stops while the tray keeps running.
const LOCK_FILE: &str = "joyshockrust-tray.lock";
const SOCKET_FILE: &str = "joyshockrust-tray.sock";
const TRAY_SERVICE: &str = "joyshockrust-tray.service";
const OPEN_EDITOR: &str = "open-editor";
const ACCEPTED: &str = "ok";
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const START_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    OpenEditor,
}

impl Request {
    fn name(self) -> &'static str {
        match self {
            Self::OpenEditor => OPEN_EDITOR,
        }
    }

    fn parse(line: &str) -> Option<Self> {
        (line.trim_end() == OPEN_EDITOR).then_some(Self::OpenEditor)
    }
}

/// Proof that this process is the only tray for the runtime directory.
pub struct Instance {
    _lock: File,
    socket: PathBuf,
}

impl Instance {
    /// Return `None` when another tray already holds the lock.
    pub fn acquire(runtime: &Path) -> io::Result<Option<Self>> {
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(runtime.join(LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error),
        }
        Ok(Some(Self {
            _lock: lock,
            socket: runtime.join(SOCKET_FILE),
        }))
    }

    /// Serve launcher requests on a background thread until `deliver`
    /// reports that the tray has stopped listening.
    pub fn listen(&self, deliver: impl Fn(Request) -> bool + Send + 'static) -> io::Result<()> {
        // Holding the lock means any existing socket belongs to a tray that
        // has exited.
        if let Err(error) = fs::remove_file(&self.socket)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        let listener = UnixListener::bind(&self.socket)?;
        thread::Builder::new()
            .name("launcher-requests".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    match serve(stream) {
                        Ok(Some(request)) if !deliver(request) => break,
                        _ => {}
                    }
                }
            })?;
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

fn serve(stream: UnixStream) -> io::Result<Option<Request>> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = Request::parse(&read_line(&stream)?);
    if request.is_some() {
        (&stream).write_all(format!("{ACCEPTED}\n").as_bytes())?;
    }
    Ok(request)
}

/// Read one short line without letting the peer grow the buffer.
fn read_line(stream: &UnixStream) -> io::Result<String> {
    let mut line = String::new();
    BufReader::new(stream.take(64)).read_line(&mut line)?;
    Ok(line)
}

fn send(socket: &Path, request: Request) -> io::Result<()> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    (&stream).write_all(format!("{}\n", request.name()).as_bytes())?;
    if read_line(&stream)?.trim_end() == ACCEPTED {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the tray rejected the request",
        ))
    }
}

/// Ask the running tray to start the bridge and show the editor. Start the
/// tray through its user unit, or directly without one, when none answers.
pub fn open_editor(runtime: &Path) -> io::Result<()> {
    let socket = runtime.join(SOCKET_FILE);
    if send(&socket, Request::OpenEditor).is_ok() {
        return Ok(());
    }
    let unit = Command::new("systemctl")
        .args(["--user", "start", TRAY_SERVICE])
        .stdin(Stdio::null())
        .status();
    if !unit.as_ref().is_ok_and(|status| status.success()) {
        // Without the unit, the tray runs as a child that outlives us.
        Command::new(env::current_exe()?)
            .stdin(Stdio::null())
            .spawn()?;
    }
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match send(&socket, Request::OpenEditor) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("the tray did not answer at {}: {error}", socket.display()),
                ));
            }
            Err(_) => thread::sleep(RETRY),
        }
    }
}

/// Forward each request to the tray loop as `action`.
pub fn forward<A: Send + 'static>(
    actions: mpsc::Sender<A>,
    action: impl Fn(Request) -> A + Send + 'static,
) -> impl Fn(Request) -> bool + Send + 'static {
    move |request| actions.send(action(request)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_tray_holds_the_runtime_lock() {
        let runtime = tempfile::tempdir().unwrap();
        let first = Instance::acquire(runtime.path()).unwrap().unwrap();
        assert!(Instance::acquire(runtime.path()).unwrap().is_none());
        drop(first);
        assert!(Instance::acquire(runtime.path()).unwrap().is_some());
    }

    #[test]
    fn launcher_request_reaches_the_running_tray() {
        let runtime = tempfile::tempdir().unwrap();
        let instance = Instance::acquire(runtime.path()).unwrap().unwrap();
        let (sender, receiver) = mpsc::channel();
        instance.listen(forward(sender, |request| request)).unwrap();
        open_editor(runtime.path()).unwrap();
        assert_eq!(
            receiver.recv_timeout(IO_TIMEOUT).unwrap(),
            Request::OpenEditor
        );
    }

    #[test]
    fn a_new_tray_replaces_the_socket_of_one_that_exited() {
        let runtime = tempfile::tempdir().unwrap();
        // A socket file left behind without a listener.
        drop(UnixListener::bind(runtime.path().join(SOCKET_FILE)).unwrap());
        let instance = Instance::acquire(runtime.path()).unwrap().unwrap();
        let (sender, receiver) = mpsc::channel();
        instance.listen(forward(sender, |request| request)).unwrap();
        send(&runtime.path().join(SOCKET_FILE), Request::OpenEditor).unwrap();
        assert_eq!(
            receiver.recv_timeout(IO_TIMEOUT).unwrap(),
            Request::OpenEditor
        );
    }

    #[test]
    fn unknown_requests_are_ignored() {
        assert_eq!(Request::parse("open-editor\n"), Some(Request::OpenEditor));
        assert_eq!(Request::parse("quit\n"), None);
    }
}

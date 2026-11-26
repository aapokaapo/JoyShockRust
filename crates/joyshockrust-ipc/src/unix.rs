//! Linux transport for the bridge's request/reply datagrams.

use std::{
    fs::OpenOptions,
    io::{self, Read},
    os::{
        linux::net::SocketAddrExt,
        unix::{
            fs::OpenOptionsExt,
            net::{SocketAddr, UnixDatagram},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::control::{
    ControlCommand, MAX_PROFILE_BYTES, Reply, load_socket_path, profile_path_payload,
};

pub struct ControlClient {
    path: PathBuf,
}

impl ControlClient {
    pub fn new(reload_path: PathBuf) -> Self {
        Self {
            path: load_socket_path(&reload_path),
        }
    }

    pub fn send(&self, command: ControlCommand<'_>) -> io::Result<()> {
        self.request(&command.encode(), command.operation())
    }

    pub fn load_profile(&self, source: &Path) -> io::Result<()> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(source)
            .map_err(|err| io::Error::new(err.kind(), format!("{}: {err}", source.display())))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular profile file",
            ));
        }
        let mut profile = Vec::new();
        file.take(MAX_PROFILE_BYTES as u64 + 1)
            .read_to_end(&mut profile)?;
        self.load_profile_contents(source, &profile)
    }

    /// Activate the exact contents already read or written by the caller.
    pub fn load_profile_contents(&self, source: &Path, profile: &[u8]) -> io::Result<()> {
        if profile.len() > MAX_PROFILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "profile exceeds 64 KiB",
            ));
        }
        let payload = profile_path_payload(&std::path::absolute(source)?, profile)?;
        self.request(&payload, "profile reload")
    }

    fn request(&self, payload: &[u8], operation: &str) -> io::Result<()> {
        static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);
        let id = format!(
            "joyshockrust-profile-{}-{}",
            std::process::id(),
            NEXT_REQUEST.fetch_add(1, Ordering::Relaxed),
        );
        let socket = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(id.as_bytes())?)?;
        socket.connect(&self.path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("could not connect to {}: {err}", self.path.display()),
            )
        })?;
        let timeout = Some(Duration::from_secs(2));
        socket.set_write_timeout(timeout)?;
        socket.set_read_timeout(timeout)?;
        socket.send(payload)?;
        let mut response = [0; 32];
        let len = socket.recv(&mut response)?;
        Reply::check(&response[..len], operation)
    }
}

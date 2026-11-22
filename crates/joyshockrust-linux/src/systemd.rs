use std::{
    env, io,
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixDatagram},
    },
    path::Path,
};

pub fn notify_ready() -> io::Result<()> {
    let Some(socket) = env::var_os("NOTIFY_SOCKET").filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let address = match socket.as_encoded_bytes().strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name)?,
        None => SocketAddr::from_pathname(Path::new(&socket))?,
    };
    let sender = UnixDatagram::unbound()?;
    sender.set_nonblocking(true)?;
    sender.send_to_addr(b"READY=1", &address).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("could not notify systemd of readiness: {err}"),
        )
    })?;
    Ok(())
}

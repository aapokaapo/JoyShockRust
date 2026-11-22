use std::{
    io::{self, Write},
    path::Path,
};

use joyshockrust_input::Transport;

use crate::discovery;

pub fn run(path: Option<&Path>, transport: Transport, mut output: impl Write) -> io::Result<()> {
    if let Some(path) = path {
        check_edge(path, transport, &mut output)?;
    } else {
        let nodes = discovery::edge_nodes(transport)?;
        if nodes.is_empty() {
            writeln!(
                output,
                "No {} DualSense Edge hidraw nodes found.",
                discovery::transport_name(transport)
            )?;
        }
        for node in &nodes {
            check_edge(node, transport, &mut output)?;
        }
    }
    // Opening the builder checks the same permissions as startup. The
    // virtual device is created only if build() is called.
    let result = evdev::uinput::VirtualDevice::builder();
    match result {
        Ok(_) => writeln!(output, "/dev/uinput: read-write open OK"),
        Err(err) => writeln!(output, "/dev/uinput: {err}"),
    }
}

fn check_edge(path: &Path, transport: Transport, output: &mut impl Write) -> io::Result<()> {
    match discovery::open_edge(path, transport) {
        Ok(_) => writeln!(
            output,
            "{}: {} DualSense Edge, readable",
            path.display(),
            discovery::transport_name(transport)
        ),
        Err(err) => writeln!(output, "{}: {err}", path.display()),
    }
}

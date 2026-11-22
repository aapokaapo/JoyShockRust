use std::{
    io::{self, Write},
    path::Path,
};

use crate::discovery;

pub fn run(path: Option<&Path>, mut output: impl Write) -> io::Result<()> {
    if let Some(path) = path {
        check_edge(path, &mut output)?;
    } else {
        let nodes = discovery::wired_edge_nodes()?;
        if nodes.is_empty() {
            writeln!(output, "No wired DualSense Edge hidraw nodes found.")?;
        }
        for node in &nodes {
            check_edge(node, &mut output)?;
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

fn check_edge(path: &Path, output: &mut impl Write) -> io::Result<()> {
    match discovery::open_wired_edge(path) {
        Ok(_) => writeln!(output, "{}: wired DualSense Edge, readable", path.display()),
        Err(err) => writeln!(output, "{}: {err}", path.display()),
    }
}

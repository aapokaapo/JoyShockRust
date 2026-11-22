use std::{
    io::{self, Write},
    path::Path,
};

use crate::discovery::{self, Selection};

pub fn run(path: Option<&Path>, selection: Selection, mut output: impl Write) -> io::Result<()> {
    if let Some(path) = path {
        check_edge(path, selection, &mut output)?;
    } else {
        let nodes = discovery::edge_nodes(selection)?;
        if nodes.is_empty() {
            writeln!(output, "No {} hidraw nodes found.", selection.description())?;
        }
        for (node, _) in &nodes {
            check_edge(node, selection, &mut output)?;
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

fn check_edge(path: &Path, selection: Selection, output: &mut impl Write) -> io::Result<()> {
    match discovery::open_edge(path, selection) {
        Ok((_, transport)) => writeln!(
            output,
            "{}: {} DualSense Edge, readable",
            path.display(),
            discovery::transport_name(transport)
        ),
        Err(err) => writeln!(output, "{}: {err}", path.display()),
    }
}

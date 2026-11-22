use std::{
    io::{self, Write},
    path::Path,
};

use crate::discovery::{self, ControllerFilter, Selection};

pub fn run(path: Option<&Path>, selection: Selection, mut output: impl Write) -> io::Result<()> {
    let filter = ControllerFilter {
        transport: selection,
        ..ControllerFilter::default()
    };
    if let Some(path) = path {
        check_controller(path, filter, &mut output)?;
    } else {
        let nodes = discovery::controller_nodes(filter)?;
        if nodes.is_empty() {
            writeln!(output, "No {filter} hidraw nodes found.")?;
        }
        for node in &nodes {
            check_controller(&node.path, filter, &mut output)?;
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

fn check_controller(
    path: &Path,
    filter: ControllerFilter,
    output: &mut impl Write,
) -> io::Result<()> {
    match discovery::open_controller(path, filter) {
        Ok((_, identity)) => writeln!(
            output,
            "{}: {} {}, readable",
            path.display(),
            discovery::transport_name(identity.transport),
            identity.model_name(),
        ),
        Err(err) => writeln!(output, "{}: {err}", path.display()),
    }
}

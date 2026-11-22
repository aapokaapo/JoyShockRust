use std::{
    io::{self, Write},
    path::Path,
};

use crate::{
    discovery::{self, ControllerFilter, ModelSelection, Selection},
    hidraw::Identity,
};

const INVENTORY: ControllerFilter = ControllerFilter {
    transport: Selection::Auto,
    model: ModelSelection::Any,
};

pub fn run(
    path: Option<&Path>,
    filter: ControllerFilter,
    show_profile_filter: bool,
    mut output: impl Write,
) -> io::Result<()> {
    if show_profile_filter {
        writeln!(output, "Controller filter: {filter}")?;
    }
    let scan = if show_profile_filter {
        INVENTORY
    } else {
        filter
    };
    if let Some(path) = path {
        check_controller(path, scan, filter, &mut output)?;
    } else {
        let nodes = discovery::controller_nodes(scan)?;
        let mut matched = false;
        for node in nodes {
            if filter.accepts(node.identity) {
                matched = true;
                check_controller(&node.path, filter, filter, &mut output)?;
            } else {
                describe_controller(&node.path, node.identity, filter, &mut output)?;
            }
        }
        if !matched {
            writeln!(output, "No {filter} hidraw nodes found.")?;
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
    probe_filter: ControllerFilter,
    requested_filter: ControllerFilter,
    output: &mut impl Write,
) -> io::Result<()> {
    match discovery::open_controller(path, probe_filter) {
        Ok((_, identity)) => describe_controller(path, identity, requested_filter, output),
        Err(err) => writeln!(output, "{}: {err}", path.display()),
    }
}

fn describe_controller(
    path: &Path,
    identity: Identity,
    filter: ControllerFilter,
    output: &mut impl Write,
) -> io::Result<()> {
    let result = if filter.accepts(identity) {
        "readable"
    } else {
        "excluded by profile"
    };
    writeln!(
        output,
        "{}: {} {}, {result}",
        path.display(),
        discovery::transport_name(identity.transport),
        identity.model_name(),
    )
}

#[cfg(test)]
mod tests {
    use joyshockrust_input::{DeviceModel, Transport};

    use super::*;

    #[test]
    fn inventory_labels_matching_and_excluded_controllers() {
        let filter = ControllerFilter {
            transport: Selection::Bluetooth,
            model: ModelSelection::DualSenseEdge,
        };
        for (model, transport, expected) in [
            (
                DeviceModel::DualSenseEdge,
                Transport::Bluetooth,
                "Bluetooth DualSense Edge, readable",
            ),
            (
                DeviceModel::DualSenseEdge,
                Transport::Usb,
                "wired DualSense Edge, excluded by profile",
            ),
            (
                DeviceModel::DualSense,
                Transport::Bluetooth,
                "Bluetooth DualSense, excluded by profile",
            ),
        ] {
            let mut output = Vec::new();
            describe_controller(
                Path::new("/dev/hidraw0"),
                Identity { model, transport },
                filter,
                &mut output,
            )
            .unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("/dev/hidraw0: {expected}\n")
            );
        }
    }
}

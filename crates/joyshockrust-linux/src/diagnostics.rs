use std::{
    io::{self, Write},
    path::Path,
};

use crate::{
    discovery::{self, ControllerFilter, ModelSelection, Selection},
    hidraw::{self, ControllerId, Identity},
};

const INVENTORY: ControllerFilter = ControllerFilter {
    transport: Selection::Auto,
    model: ModelSelection::Any,
    controller_id: None,
};

pub fn run(
    path: Option<&Path>,
    filter: ControllerFilter,
    show_profile_filter: bool,
    show_id: bool,
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
        check_controller(path, scan, filter, show_id, &mut output)?;
    } else {
        let nodes = discovery::controller_nodes(scan)?;
        let mut matched = false;
        for node in nodes {
            if filter.accepts(node.identity) || show_id {
                matched |= check_controller(&node.path, scan, filter, show_id, &mut output)?
                    || !show_profile_filter;
            } else {
                describe_controller(&node.path, node.identity, None, filter, false, &mut output)?;
            }
        }
        if !matched {
            if show_profile_filter {
                writeln!(output, "No readable controller matches the profile.")?;
            } else {
                writeln!(output, "No {filter} hidraw nodes found.")?;
            }
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
    show_id: bool,
    output: &mut impl Write,
) -> io::Result<bool> {
    match discovery::open_controller(path, probe_filter) {
        Ok((file, identity)) => {
            let controller_id = if show_id || requested_filter.controller_id.is_some() {
                match hidraw::controller_id(&file) {
                    Ok(id) => id,
                    Err(err) => {
                        writeln!(output, "{}: could not read HID ID: {err}", path.display())?;
                        None
                    }
                }
            } else {
                None
            };
            describe_controller(
                path,
                identity,
                controller_id,
                requested_filter,
                show_id,
                output,
            )
        }
        Err(err) => {
            writeln!(output, "{}: {err}", path.display())?;
            Ok(false)
        }
    }
}

fn describe_controller(
    path: &Path,
    identity: Identity,
    controller_id: Option<ControllerId>,
    filter: ControllerFilter,
    show_id: bool,
    output: &mut impl Write,
) -> io::Result<bool> {
    let matched = filter.accepts(identity) && filter.accepts_id(controller_id);
    let result = if matched {
        "readable"
    } else {
        "excluded by profile"
    };
    write!(
        output,
        "{}: {} {}, {result}",
        path.display(),
        discovery::transport_name(identity.transport),
        identity.model_name(),
    )?;
    if show_id {
        let id: &dyn std::fmt::Display = match &controller_id {
            Some(id) => id,
            None => &"unavailable",
        };
        write!(output, ", controller_id={id}")?;
    }
    writeln!(output)?;
    Ok(matched)
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
            controller_id: None,
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
                None,
                filter,
                false,
                &mut output,
            )
            .unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                format!("/dev/hidraw0: {expected}\n")
            );
        }
    }

    #[test]
    fn profile_can_distinguish_two_controllers_of_the_same_model_without_printing_their_ids() {
        let filter_for = |id| {
            crate::profile::parse(&format!(
                "version = 1\n[bridge]\nmodel = 'dualsense-edge'\ncontroller_id = '{id}'"
            ))
            .unwrap()
            .bridge
            .unwrap()
            .controller_filter()
        };
        let filter = filter_for("00:11:22:33:44:aa");
        let other_id = filter_for("00:11:22:33:44:bb").controller_id;
        let identity = Identity {
            model: DeviceModel::DualSenseEdge,
            transport: Transport::Usb,
        };
        for (id, expected) in [
            (filter.controller_id, true),
            (other_id, false),
            (None, false),
        ] {
            let mut output = Vec::new();
            let matched = describe_controller(
                Path::new("/dev/hidraw0"),
                identity,
                id,
                filter,
                false,
                &mut output,
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert_eq!(matched, expected);
            assert!(!output.contains("00:11"));
            assert!(output.contains(if expected {
                "readable"
            } else {
                "excluded by profile"
            }));
        }
        let mut output = Vec::new();
        describe_controller(
            Path::new("/dev/hidraw0"),
            identity,
            other_id,
            filter,
            true,
            &mut output,
        )
        .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("controller_id=00:11:22:33:44:bb")
        );
    }
}

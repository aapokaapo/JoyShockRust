#[cfg(target_os = "linux")]
mod adaptive_trigger;
#[cfg(target_os = "linux")]
mod benchmark;
#[cfg(target_os = "linux")]
mod cli;
#[cfg(target_os = "linux")]
mod controller_config;
#[cfg(target_os = "linux")]
mod diagnostics;
#[cfg(target_os = "linux")]
mod discovery;
#[cfg(target_os = "linux")]
mod game_calibration;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
mod gyro_calibration;
#[cfg(target_os = "linux")]
mod gyro_mouse;
#[cfg(target_os = "linux")]
mod haptics;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod hidraw;
#[cfg(target_os = "linux")]
mod inventory;
#[cfg(target_os = "linux")]
mod keyboard;
#[cfg(target_os = "linux")]
mod launch;
#[cfg(all(test, target_os = "linux"))]
mod live_uinput_test;
#[cfg(target_os = "linux")]
mod profile;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod rumble;
#[cfg(target_os = "linux")]
mod status;
#[cfg(target_os = "linux")]
mod systemd;
#[cfg(target_os = "linux")]
mod touchpad;
#[cfg(target_os = "linux")]
mod trigger_travel;

#[cfg(target_os = "linux")]
use hidraw::Identity;
#[cfg(target_os = "linux")]
use joyshockrust_input::Transport;
#[cfg(target_os = "linux")]
use joyshockrust_mapping::gyro_gate;
#[cfg(target_os = "linux")]
use std::ops::ControlFlow;

#[cfg(target_os = "linux")]
fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    cli::run()
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Emulation {
    master: bool,
    gyro_override: Option<bool>,
}

#[cfg(target_os = "linux")]
impl Default for Emulation {
    fn default() -> Self {
        Self {
            master: true,
            gyro_override: None,
        }
    }
}

#[cfg(target_os = "linux")]
impl Emulation {
    fn gamepad(self, selected: bool) -> bool {
        self.master && selected
    }

    fn gyro(self, configured: Option<gyro_mouse::Options>) -> Option<gyro_mouse::Options> {
        if !self.master {
            return None;
        }
        match self.gyro_override {
            Some(true) => Some(configured.unwrap_or_default()),
            Some(false) => None,
            None => configured,
        }
    }

    fn changed(
        self,
        target: reload::OutputTarget,
        enabled: bool,
        gamepad: bool,
    ) -> Option<(Self, bool)> {
        use reload::OutputTarget;
        if !self.master && target != OutputTarget::Master {
            return None;
        }
        let (gamepad, gyro_override) = match target {
            OutputTarget::Gamepad => (enabled, self.gyro_override),
            OutputTarget::GyroMouse => (gamepad, Some(enabled)),
            OutputTarget::Both => (enabled, Some(enabled)),
            OutputTarget::Master => (gamepad, self.gyro_override),
        };
        let master = if target == OutputTarget::Master {
            enabled
        } else {
            self.master
        };
        Some((
            Self {
                master,
                gyro_override,
            },
            gamepad,
        ))
    }

    fn publish(self, status: Option<&status::StatusSocket>, gamepad: bool) {
        if let Some(status) = status {
            status.set_gamepad_enabled(gamepad);
            status.set_gyro_override(self.gyro_override);
            status.set_emulation_master(self.master);
        }
    }
}

#[cfg(target_os = "linux")]
fn open_mouse(
    input: &std::fs::File,
    transport: Transport,
    options: Option<gyro_mouse::Options>,
) -> std::io::Result<Option<gyro_mouse::GyroMouse>> {
    let Some(options) = options else {
        return Ok(None);
    };
    let mouse = gyro_mouse::GyroMouse::open(input, transport, options)
        .map_err(|err| std::io::Error::other(format!("could not start gyro mouse: {err}")))?;
    eprintln!("JoyShockRust Gyro Mouse ready");
    if options.calibrate {
        eprintln!(
            "Set the controller down and keep it still for {} seconds to calibrate the gyroscope",
            joyshockrust_motion::StationaryCalibration::SAMPLE_DURATION.as_secs(),
        );
    }
    Ok(Some(mouse))
}

#[cfg(target_os = "linux")]
struct OpenGamepad {
    device: evdev::uinput::VirtualDevice,
    rumble: Option<rumble::Rumble>,
    feedback: Option<std::os::fd::OwnedFd>,
}

#[cfg(target_os = "linux")]
fn open_gamepad(
    input: &std::fs::File,
    bridge: profile::BridgeOptions,
) -> std::io::Result<OpenGamepad> {
    use std::os::fd::AsFd;

    let rumble = if bridge.rumble {
        match rumble::Rumble::open(input) {
            Ok(rumble) => Some(rumble),
            Err(err) => {
                eprintln!("Rumble forwarding unavailable: {err}");
                None
            }
        }
    } else {
        None
    };
    let output = gamepad::open(bridge.gamepad, rumble.is_some())
        .map_err(|err| std::io::Error::other(format!("could not create uinput gamepad: {err}")))?;
    let feedback = rumble
        .as_ref()
        .map(|_| output.as_fd().try_clone_to_owned())
        .transpose()?;
    Ok(OpenGamepad {
        device: output,
        rumble,
        feedback,
    })
}

#[cfg(target_os = "linux")]
struct SessionDevice<'a> {
    identity: Identity,
    filter: discovery::ControllerFilter,
    path: &'a std::path::Path,
    grab_touchpad: bool,
    outputs_paused: Option<&'a std::cell::Cell<bool>>,
    emulation: &'a mut Emulation,
}

#[cfg(target_os = "linux")]
fn serve_session(
    input: &mut std::fs::File,
    runtime: &mut profile::RuntimeOptions,
    reload: Option<&reload::Reload>,
    status: Option<&status::StatusSocket>,
    SessionDevice {
        identity,
        filter,
        path,
        grab_touchpad,
        outputs_paused,
        emulation,
    }: SessionDevice<'_>,
    bridge: &mut profile::BridgeOptions,
    mut measurement: Option<&mut benchmark::Benchmark>,
) -> std::io::Result<Option<discovery::ControllerFilter>> {
    use std::{
        cell::{Cell, RefCell},
        time::Instant,
    };

    let mut active_gyro = emulation.gyro(runtime.gyro);
    let mut adaptive = adaptive_trigger::AdaptiveTriggers::new(identity.transport);
    let mut travel_preview: Option<trigger_travel::Preview> = None;
    let mut travel_haptics = None;
    let mut mouse = open_mouse(input, identity.transport, active_gyro)?;
    let mut keyboard = keyboard::Keyboard::open(runtime.keyboard.as_ref(), status)?;
    let keyboard_deadline = Cell::new(None);
    let reload_input = reload.map(|_| input.try_clone()).transpose()?;
    let (mut output, mut rumble, feedback) = if emulation.gamepad(bridge.virtual_gamepad) {
        let OpenGamepad {
            device,
            rumble,
            feedback,
        } = open_gamepad(input, *bridge)?;
        (Some(device), rumble, feedback)
    } else {
        (None, None, None)
    };
    let feedback = RefCell::new(feedback);
    let mut state = gamepad::GamepadState::new(bridge.gamepad);
    let mut layer_state = gamepad::RemapState::default();
    let mut touch_click = touchpad::ClickState::default();
    let touchpad_requested = grab_touchpad || runtime.grab_touchpad;
    let mut native_touchpad = touchpad_requested
        .then(|| touchpad::try_grab_native(input))
        .flatten();
    adaptive.update(path, input, runtime.adaptive_triggers);
    if let Some(status) = status {
        status.update(Some(identity), runtime.gyro.is_some());
        let (factory, bias) = mouse.as_ref().map_or_else(
            || {
                let factory = hidraw::read_factory_calibration(input, identity.transport).ok();
                let bias = factory
                    .and_then(|_| gyro_calibration::load(input))
                    .unwrap_or_default();
                (factory, bias)
            },
            |mouse| {
                (
                    Some(mouse.factory_calibration()),
                    mouse.stationary_calibration(),
                )
            },
        );
        status.set_imu_calibration(factory, bias);
        status.set_active_hidraw(path);
        status.set_gamepad_device(output.as_mut());
        status.set_touchpad_policy(touchpad_requested, native_touchpad.is_some());
    }
    if output.is_some()
        && let Some(mouse) = &mouse
    {
        mouse.signal_outputs_ready();
    }
    let resource_start = measurement
        .as_ref()
        .and_then(|_| benchmark::ResourceSnapshot::capture());
    let mut switch = None;
    let result = forward_reports(
        input,
        identity,
        reload,
        status,
        Some(&feedback),
        ReportTimers {
            measurement: measurement.as_deref_mut(),
            keyboard: Some(&keyboard_deadline),
            ..Default::default()
        },
        |event| {
            let report = match event {
                SessionEvent::Report(report) => report,
                SessionEvent::KeyboardDeadline(now) => {
                    keyboard_deadline.set(None);
                    if let Some(keyboard) = &mut keyboard {
                        keyboard_deadline.set(keyboard.tick(now)?);
                    }
                    return Ok(ControlFlow::Continue(()));
                }
                SessionEvent::Feedback => {
                    if let (Some(rumble), Some(output)) = (&mut rumble, &mut output) {
                        rumble.service(output)?;
                    }
                    return Ok(ControlFlow::Continue(()));
                }
                SessionEvent::Reload(reload) => {
                    let both_ready = output.is_some() && mouse.is_some();
                    let current_filter = filter;
                    let next = match reload.resolve(current_filter) {
                        Some(reload::Update::Profile(next)) => next,
                        Some(reload::Update::TriggerPreview(preview)) => {
                            adaptive.preview(
                                path,
                                reload_input.as_ref().expect("reload descriptor"),
                                runtime.adaptive_triggers,
                                preview,
                            );
                            return Ok(ControlFlow::Continue(()));
                        }
                        Some(reload::Update::TriggerTravel(settings)) => {
                            if settings.is_some() && travel_haptics.is_none() {
                                travel_haptics = haptics::Haptics::open_controller(
                                    reload_input.as_ref().expect("reload descriptor"),
                                    identity.transport,
                                );
                            }
                            trigger_travel::Preview::refresh(&mut travel_preview, settings);
                            return Ok(ControlFlow::Continue(()));
                        }
                        Some(reload::Update::Selection(selection))
                            if selection != current_filter =>
                        {
                            switch = Some((Some(selection), reload));
                            return Ok(ControlFlow::Break(()));
                        }
                        Some(reload::Update::Selection(_)) => {
                            reload.applied(status);
                            return Ok(ControlFlow::Continue(()));
                        }
                        Some(reload::Update::OutputsPaused(paused)) => {
                            if paused && outputs_paused.is_some() {
                                switch = Some((None, reload));
                                return Ok(ControlFlow::Break(()));
                            }
                            reload.applied(status);
                            return Ok(ControlFlow::Continue(()));
                        }
                        Some(reload::Update::Emulation(target, enabled)) => {
                            let Some((next_emulation, gamepad_selection)) =
                                emulation.changed(target, enabled, bridge.virtual_gamepad)
                            else {
                                reload.rejected("emulation is disabled by the master switch");
                                return Ok(ControlFlow::Continue(()));
                            };
                            let gamepad = next_emulation.gamepad(gamepad_selection);
                            let next_gyro = next_emulation.gyro(runtime.gyro);
                            let input = reload_input.as_ref().expect("reload descriptor");
                            let replacements = (|| {
                                let next_mouse = (next_gyro != active_gyro)
                                    .then(|| open_mouse(input, identity.transport, next_gyro))
                                    .transpose()?;
                                let next_gamepad = (gamepad && output.is_none())
                                    .then(|| open_gamepad(input, *bridge))
                                    .transpose()?;
                                Ok::<_, std::io::Error>((next_mouse, next_gamepad))
                            })();
                            let (next_mouse, next_gamepad) = match replacements {
                                Ok(replacements) => replacements,
                                Err(error) => {
                                    reload.rejected(&error);
                                    return Ok(ControlFlow::Continue(()));
                                }
                            };
                            if let Some(next_mouse) = next_mouse {
                                mouse = next_mouse;
                            }
                            if let Some(OpenGamepad {
                                device,
                                rumble: forwarder,
                                feedback: descriptor,
                            }) = next_gamepad
                            {
                                output = Some(device);
                                rumble = forwarder;
                                feedback.replace(descriptor);
                            } else if !gamepad && output.is_some() {
                                feedback.take();
                                if let Some(mut device) = output.take()
                                    && let Err(err) = state
                                        .publish(joyshockrust_core::Controls::default(), |events| {
                                            device.emit(events)
                                        })
                                {
                                    eprintln!(
                                        "Could not release virtual gamepad before removing it: {err}"
                                    );
                                }
                                rumble = None;
                                state = gamepad::GamepadState::new(bridge.gamepad);
                                layer_state = gamepad::RemapState::default();
                                touch_click = touchpad::ClickState::default();
                            }
                            bridge.virtual_gamepad = gamepad_selection;
                            *emulation = next_emulation;
                            active_gyro = next_gyro;
                            emulation.publish(status, gamepad_selection);
                            if let Some(status) = status {
                                status.set_gamepad_device(output.as_mut());
                            }
                            if !both_ready
                                && output.is_some()
                                && let Some(mouse) = &mouse
                            {
                                mouse.signal_outputs_ready();
                            }
                            reload.applied(status);
                            return Ok(ControlFlow::Continue(()));
                        }
                        None => return Ok(ControlFlow::Continue(())),
                    };
                    let next_kind = reload.profile_gamepad(&next);
                    let replacement_gamepad = if output.is_some() && bridge.gamepad != next_kind {
                        let input = reload_input.as_ref().expect("reload descriptor");
                        match open_gamepad(
                            input,
                            profile::BridgeOptions {
                                gamepad: next_kind,
                                ..*bridge
                            },
                        ) {
                            Ok(gamepad) => Some(gamepad),
                            Err(error) => {
                                reload.rejected(&error);
                                return Ok(ControlFlow::Continue(()));
                            }
                        }
                    } else {
                        None
                    };
                    let replacement_keyboard = if runtime.keyboard != next.keyboard {
                        match keyboard::Keyboard::open(next.keyboard.as_ref(), status) {
                            Ok(device) => Some(device),
                            Err(err) => {
                                reload.rejected(&err);
                                return Ok(ControlFlow::Continue(()));
                            }
                        }
                    } else {
                        None
                    };
                    let next_gyro = emulation.gyro(next.gyro);
                    if let (Some(mouse), Some(current), Some(next)) =
                        (&mut mouse, active_gyro, next_gyro)
                        && current.can_reconfigure(next)
                    {
                        mouse.reconfigure(next);
                    } else if active_gyro != next_gyro {
                        let input = reload_input.as_ref().expect("reload descriptor");
                        match open_mouse(input, identity.transport, next_gyro) {
                            Ok(next_mouse) => mouse = next_mouse,
                            Err(err) => {
                                reload.rejected(&err);
                                return Ok(ControlFlow::Continue(()));
                            }
                        }
                    }
                    if runtime.remaps != next.remaps
                        || runtime.trigger_stages != next.trigger_stages
                    {
                        layer_state = gamepad::RemapState::default();
                    }
                    if runtime.touchpad != next.touchpad {
                        touch_click = touchpad::ClickState::default();
                    }
                    if let Some(replacement) = replacement_keyboard {
                        if let Some(previous) = &mut keyboard
                            && let Err(err) = previous.release()
                        {
                            reload.rejected(&err);
                            return Err(err);
                        }
                        keyboard = replacement;
                        keyboard_deadline.set(None);
                    }
                    if let Some(replacement) = replacement_gamepad {
                        feedback.take();
                        if let Some(mut previous) = output.take()
                            && let Err(error) = state
                                .publish(joyshockrust_core::Controls::default(), |events| {
                                    previous.emit(events)
                                })
                        {
                            eprintln!("Could not release the previous virtual gamepad: {error}");
                        }
                        output = Some(replacement.device);
                        rumble = replacement.rumble;
                        feedback.replace(replacement.feedback);
                        if let Some(status) = status {
                            status.set_gamepad_device(output.as_mut());
                        }
                    }
                    if bridge.gamepad != next_kind {
                        state = gamepad::GamepadState::new(next_kind);
                    }
                    bridge.gamepad = next_kind;
                    if let Some(status) = status {
                        status.set_gamepad_kind(next_kind);
                    }
                    *runtime = *next;
                    adaptive.update(
                        path,
                        reload_input.as_ref().expect("reload descriptor"),
                        runtime.adaptive_triggers,
                    );
                    active_gyro = next_gyro;
                    let touchpad_requested = grab_touchpad || runtime.grab_touchpad;
                    if !touchpad_requested {
                        native_touchpad.take();
                    } else if native_touchpad.is_none() {
                        let input = reload_input.as_ref().expect("reload descriptor");
                        native_touchpad = touchpad::try_grab_native(input);
                    }
                    if let Some(status) = status {
                        status.update(Some(identity), runtime.gyro.is_some());
                        status.set_touchpad_policy(touchpad_requested, native_touchpad.is_some());
                    }
                    if !both_ready
                        && output.is_some()
                        && let Some(mouse) = &mouse
                    {
                        mouse.signal_outputs_ready();
                    }
                    reload.applied(status);
                    return Ok(ControlFlow::Continue(()));
                }
            };
            if let Some(status) = status {
                status.observe_battery(report.battery);
            }
            if let Some(keyboard) = &mut keyboard {
                let now = Instant::now();
                if keyboard_deadline
                    .get()
                    .is_some_and(|deadline| deadline <= now)
                {
                    // A deadline can fall between two reports in the same read batch.
                    keyboard.tick(now)?;
                }
                keyboard_deadline.set(keyboard.publish(report, now)?);
            }
            let gamepad_batch = if let Some(output) = &mut output {
                let controls = map_gamepad_controls(
                    runtime,
                    active_gyro,
                    &report,
                    &mut layer_state,
                    &mut touch_click,
                );
                state.publish(controls, |events| output.emit(events))?
            } else {
                false
            };
            let mouse_batch = if let Some(mouse) = &mut mouse {
                mouse.publish(report)?
            } else {
                false
            };
            if let Some(status) = status {
                status.observe_touch(report.touch_contacts);
                status.observe_analog(&report.controls);
                status.observe_imu(
                    report.imu,
                    mouse
                        .as_ref()
                        .map(gyro_mouse::GyroMouse::stationary_calibration),
                    mouse
                        .as_ref()
                        .and_then(gyro_mouse::GyroMouse::projected_interval),
                );
                if gamepad_batch || mouse_batch {
                    status.observe_outputs(gamepad_batch, mouse_batch);
                }
            }
            if let Some(preview) = &mut travel_preview
                && preview.crossed(&report.controls)
                && let Some(haptics) = &travel_haptics
            {
                haptics.send(haptics::Cue::Detent);
            }
            Ok(ControlFlow::Continue(()))
        },
    );
    if let Some(measurement) = measurement {
        measurement.finish_resources(resource_start);
    }
    let release_gamepad = if let Some(output) = &mut output {
        state
            .publish(joyshockrust_core::Controls::default(), |events| {
                output.emit(events)
            })
            .map(|_| ())
    } else {
        Ok(())
    };
    let release_keyboard = if let Some(keyboard) = &mut keyboard {
        keyboard.release()
    } else {
        Ok(())
    };
    let result = result.and(release_gamepad).and(release_keyboard);
    drop(native_touchpad);
    drop(adaptive);
    if let Some((selection, request)) = switch {
        if let Err(err) = &result {
            request.rejected(err);
        } else {
            // Close all old virtual outputs before acknowledging the new filter.
            drop(feedback);
            drop(output);
            drop(mouse);
            drop(keyboard);
            drop(rumble);
            if let Some(status) = status {
                status.update(None, runtime.gyro.is_some());
            }
            if selection.is_none()
                && let Some(outputs_paused) = outputs_paused
            {
                outputs_paused.set(true);
                if let Some(status) = status {
                    status.set_outputs_paused(true);
                }
            }
            request.applied(status);
            return Ok(Some(selection.unwrap_or(filter)));
        }
    }
    if let Some(status) = status {
        status.update(None, runtime.gyro.is_some());
    }
    result.map(|_| None)
}

#[cfg(target_os = "linux")]
fn map_gamepad_controls(
    runtime: &profile::RuntimeOptions,
    active_gyro: Option<gyro_mouse::Options>,
    report: &joyshockrust_input::InputReport,
    layer_state: &mut gamepad::RemapState,
    touch_click: &mut touchpad::ClickState,
) -> joyshockrust_core::Controls {
    gamepad::GamepadMapping {
        stick_curve: runtime.stick_curve,
        flick_stick: active_gyro
            .filter(|options| options.flick_stick.is_some())
            .map(|options| options.flick_stick_side),
        suppressed: runtime
            .keyboard
            .as_ref()
            .map_or_else(Default::default, |keyboard| keyboard.gamepad_mask.buttons),
        touchpad: runtime.touchpad.as_ref(),
        trigger_stages: runtime.trigger_stages,
        trigger_full_travel: runtime.trigger_full_travel,
        trigger_analog_routes: runtime.trigger_analog_routes,
        remaps: &runtime.remaps,
    }
    .apply(
        report.controls,
        report.touch_contacts,
        layer_state,
        touch_click,
    )
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("joyshockrust-linux requires Linux hidraw and uinput");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
enum SessionEvent<'a> {
    Report(joyshockrust_input::InputReport),
    Feedback,
    Reload(reload::Request<'a>),
    KeyboardDeadline(std::time::Instant),
}

#[cfg(target_os = "linux")]
const REPORT_BATCH_LIMIT: u64 = 32;

#[cfg(target_os = "linux")]
#[derive(Default)]
struct ReportTimers<'a> {
    measurement: Option<&'a mut benchmark::Benchmark>,
    stop_at: Option<std::time::Instant>,
    keyboard: Option<&'a std::cell::Cell<Option<std::time::Instant>>>,
}

#[cfg(target_os = "linux")]
fn forward_reports<'a>(
    input: &mut std::fs::File,
    identity: Identity,
    reload: Option<&'a reload::Reload>,
    status: Option<&status::StatusSocket>,
    feedback: Option<&std::cell::RefCell<Option<std::os::fd::OwnedFd>>>,
    timers: ReportTimers<'_>,
    mut publish: impl FnMut(SessionEvent<'a>) -> std::io::Result<ControlFlow<()>>,
) -> std::io::Result<()> {
    use joyshockrust_input::parse_report;
    use nix::poll::PollFlags;
    use std::{
        io::{self, Read},
        os::fd::{AsFd, AsRawFd},
        time::Instant,
    };

    // One read consumes one complete HID report. A larger buffer preserves
    // length validation when a node delivers a larger report than expected.
    let mut report = [0; 16_384];
    let mut measurement = timers.measurement;
    let benchmark_deadline = measurement
        .as_ref()
        .map(|measurement| Instant::now() + measurement.timeout);
    loop {
        let deadline = benchmark_deadline
            .into_iter()
            .chain(timers.keyboard.and_then(std::cell::Cell::get))
            .chain(timers.stop_at)
            .min();
        let ((ready, request, mut feedback_ready), polled_feedback) = {
            let feedback = feedback.map(std::cell::RefCell::borrow);
            let descriptor = feedback
                .as_deref()
                .and_then(Option::as_ref)
                .map(AsFd::as_fd);
            (
                reload::wait(input.as_fd(), reload, status, descriptor, deadline)?,
                descriptor.map(|fd| fd.as_raw_fd()),
            )
        };
        if let Some(request) = request {
            if publish(SessionEvent::Reload(request))?.is_break() {
                return Ok(());
            }
            let current_feedback =
                feedback.and_then(|socket| socket.borrow().as_ref().map(AsRawFd::as_raw_fd));
            // A control request can replace the polled descriptor. Read feedback on the next wake.
            if current_feedback != polled_feedback {
                feedback_ready = PollFlags::empty();
            }
        }
        if ready.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
            return Ok(());
        }
        if timers
            .stop_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Keep the controller still on a stable surface, then retry.",
            ));
        }
        if let (Some(deadline), Some(measurement)) =
            (benchmark_deadline, measurement.as_deref_mut())
            && Instant::now() >= deadline
        {
            measurement.timed_out = true;
            return Ok(());
        }
        if let Some(deadline) = timers.keyboard.and_then(std::cell::Cell::get) {
            let now = Instant::now();
            if now >= deadline && publish(SessionEvent::KeyboardDeadline(now))?.is_break() {
                return Ok(());
            }
        }
        if feedback_ready.intersects(PollFlags::POLLERR | PollFlags::POLLHUP | PollFlags::POLLNVAL)
        {
            return Err(io::Error::other("virtual force feedback descriptor lost"));
        }
        if feedback_ready.contains(PollFlags::POLLIN) && publish(SessionEvent::Feedback)?.is_break()
        {
            return Ok(());
        }
        if !ready.contains(PollFlags::POLLIN) {
            continue;
        }
        let mut decoded = 0;
        let mut rejected = 0;
        for _ in 0..REPORT_BATCH_LIMIT {
            let len = match input.read(&mut report) {
                Ok(0) => return Ok(()),
                Ok(len) => len,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err)
                    if matches!(err.raw_os_error(), Some(nix::libc::EIO | nix::libc::ENODEV)) =>
                {
                    return Ok(());
                }
                Err(err) => return Err(err),
            };
            match parse_report(identity.model, identity.transport, &report[..len], None) {
                Ok(report) => {
                    decoded += 1;
                    let (progress, complete) = if let Some(measurement) = measurement.as_deref_mut()
                    {
                        let started = Instant::now();
                        let progress = publish(SessionEvent::Report(report))?;
                        measurement.record(started, started.elapsed(), report.imu.device_ticks);
                        (progress, measurement.complete())
                    } else {
                        (publish(SessionEvent::Report(report))?, false)
                    };
                    if progress.is_break() || complete {
                        if let Some(status) = status {
                            status.observe_reports(decoded, rejected);
                        }
                        return Ok(());
                    }
                }
                Err(_) => {
                    rejected += 1;
                    if let Some(measurement) = measurement.as_deref_mut() {
                        measurement.rejected += 1;
                    }
                }
            }
        }
        if let Some(status) = status
            && (decoded > 0 || rejected > 0)
        {
            status.observe_reports(decoded, rejected);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        fs::File,
        io,
        os::fd::OwnedFd,
        os::unix::net::{UnixDatagram, UnixStream},
    };

    use super::Identity;
    use joyshockrust_core::Button;
    use joyshockrust_input::{DeviceModel, Transport};

    const USB_EDGE: Identity = Identity {
        model: DeviceModel::DualSenseEdge,
        transport: Transport::Usb,
    };

    #[test]
    fn readiness_loop_ends_when_input_disconnects() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(sender);
        let mut input = File::from(OwnedFd::from(receiver));
        super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers::default(),
            |_| panic!("disconnected input published controls"),
        )
        .unwrap();
    }

    #[test]
    fn readiness_loop_services_feedback_when_input_is_idle() {
        let (_sender, receiver) = UnixDatagram::pair().unwrap();
        let (game, feedback) = UnixDatagram::pair().unwrap();
        let feedback = std::cell::RefCell::new(Some(feedback.into()));
        game.send(b"feedback").unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let result = super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            Some(&feedback),
            super::ReportTimers::default(),
            |event| {
                assert!(matches!(event, super::SessionEvent::Feedback));
                Err(io::ErrorKind::BrokenPipe.into())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn steady_state_report_replay_does_not_allocate() {
        const FRAMES: usize = 64;
        let profile = super::profile::parse(
            r#"
            version = 1
            stick_curve = 1.5
            remap_buttons = ["cross=triangle"]
            layer_remaps = ["l1:cross=circle"]
            [touchpad]
            rows = 1
            columns = 1
            [[touchpad.regions]]
            row = 1
            column = 1
            touch = "square"
            click = "r3"
            "#,
        )
        .unwrap();
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[37] = 0x80;
        for index in 0..FRAMES {
            let active = index % 2 == 0;
            frame[1] = if active { 196 } else { 128 };
            frame[8] = if active { 0x28 } else { 8 };
            frame[9] = u8::from(active);
            frame[10] = if active { 2 } else { 0 };
            frame[33] = if active { 1 } else { 0x80 };
            sender.send(&frame).unwrap();
        }
        // A zero-length datagram exits the read loop like end of input.
        sender.send(&[]).unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let mut pad = super::gamepad::GamepadState::default();
        let mut layer = super::gamepad::RemapState::default();
        let mut click = super::touchpad::ClickState::default();
        let mut reports = 0;
        let mut batches = 0;
        let allocations = allocation_counter::measure(|| {
            super::forward_reports(
                &mut input,
                USB_EDGE,
                None,
                None,
                None,
                super::ReportTimers::default(),
                |event| {
                    let super::SessionEvent::Report(report) = event else {
                        unreachable!()
                    };
                    let controls = super::map_gamepad_controls(
                        &profile,
                        profile.gyro,
                        &report,
                        &mut layer,
                        &mut click,
                    );
                    if pad.publish(controls, |events| {
                        std::hint::black_box(events);
                        Ok(())
                    })? {
                        batches += 1;
                    }
                    reports += 1;
                    Ok(super::ControlFlow::Continue(()))
                },
            )
            .unwrap();
        });
        assert_eq!(reports, FRAMES);
        assert_eq!(batches, FRAMES);
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
    }

    #[test]
    fn exclusive_button_outputs_consume_only_their_physical_sources() {
        use joyshockrust_core::{Button, DPad};
        let source = r#"
version = 1
remap_buttons = ["cross=triangle", "circle=cross", "dpad-up=square"]
[keyboard.button_output]
cross = "keyboard"
l2 = "mouse"
dpad-up = "keyboard"
[[keyboard.bindings]]
button = "cross"
tap = "KEY_SPACE"
[[keyboard.bindings]]
trigger = "l2:0.95:0.85"
hold = "BTN_LEFT"
"#;
        let runtime = super::profile::parse(source).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[5] = 255;
        frame[8] = 0x61;
        frame[9] = 0x04;
        let report =
            joyshockrust_input::parse_report(USB_EDGE.model, USB_EDGE.transport, &frame, None)
                .unwrap();
        let controls = super::map_gamepad_controls(
            &runtime,
            runtime.gyro,
            &report,
            &mut super::gamepad::RemapState::default(),
            &mut super::touchpad::ClickState::default(),
        );
        assert!(!controls.buttons.contains(Button::North));
        assert!(!controls.buttons.contains(Button::West));
        assert!(controls.buttons.contains(Button::South));
        assert!(!controls.buttons.contains(Button::LeftTrigger));
        assert_eq!(controls.left_trigger, 0.0);
        assert_eq!(controls.dpad, DPad::Right);
        assert!(super::profile::parse(&source.replace("BTN_LEFT", "KEY_A")).is_err());
    }

    #[test]
    fn dpad_remaps_reach_button_and_hat_events_without_chaining() {
        use evdev::{AbsoluteAxisCode, EventType, KeyCode};
        use joyshockrust_core::DPad;

        let runtime = super::profile::parse(
            r#"
version = 1
remap_buttons = ["dpad-up=cross", "circle=dpad-left", "dpad-left=dpad-right"]
[touchpad]
rows = 1
columns = 1
[[touchpad.regions]]
row = 1
column = 1
touch = "dpad-down"
"#,
        )
        .unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[37] = 0x80;
        let mut state = super::gamepad::GamepadState::new(super::gamepad::OutputKind::Xbox360);
        let mut layer = super::gamepad::RemapState::default();
        let mut click = super::touchpad::ClickState::default();
        let hat_x = AbsoluteAxisCode::ABS_HAT0X.0;
        let hat_y = AbsoluteAxisCode::ABS_HAT0Y.0;
        let south = KeyCode::BTN_SOUTH.0;
        let hat = |code, value| (EventType::ABSOLUTE, code, value);
        let key = |value| (EventType::KEY, south, value);
        for (bits, touch, expected_hat, expected_events) in [
            (0x01, false, DPad::Right, vec![key(1), hat(hat_x, 1)]),
            (0x41, false, DPad::Neutral, vec![hat(hat_x, 0)]),
            (0x48, false, DPad::Left, vec![key(0), hat(hat_x, -1)]),
            (
                0x06,
                true,
                DPad::DownRight,
                vec![hat(hat_x, 1), hat(hat_y, 1)],
            ),
            (
                0x08,
                false,
                DPad::Neutral,
                vec![hat(hat_x, 0), hat(hat_y, 0)],
            ),
        ] {
            frame[8] = bits;
            frame[33] = if touch { 0 } else { 0x80 };
            let report =
                joyshockrust_input::parse_report(USB_EDGE.model, USB_EDGE.transport, &frame, None)
                    .unwrap();
            let controls = super::map_gamepad_controls(
                &runtime,
                runtime.gyro,
                &report,
                &mut layer,
                &mut click,
            );
            assert_eq!(controls.dpad, expected_hat);
            let mut actual = Vec::new();
            state
                .publish(controls, |events| {
                    actual.extend(
                        events
                            .iter()
                            .map(|event| (event.event_type(), event.code(), event.value())),
                    );
                    Ok(())
                })
                .unwrap();
            assert_eq!(actual, expected_events);
        }
    }

    #[test]
    fn virtual_trigger_stages_keep_analog_travel_and_route_digital_targets() {
        use joyshockrust_core::{Button, DPad};
        let runtime = super::profile::parse(
            r#"
version = 1
[triggers.r2]
thresholds = [30, 20, 80, 70]
partial_button = "cross"
full_button = "dpad-left"
keep_partial = true
"#,
        )
        .unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[8] = 8;
        frame[33] = 0x80;
        frame[37] = 0x80;
        let mut layers = super::gamepad::RemapState::default();
        let mut click = super::touchpad::ClickState::default();
        for (travel, cross, hat) in [
            (80, true, DPad::Neutral),
            (204, true, DPad::Left),
            (180, true, DPad::Left),
            (178, true, DPad::Neutral),
            (51, false, DPad::Neutral),
        ] {
            frame[6] = travel;
            frame[9] = if travel > 0 { 0x08 } else { 0 };
            let report =
                joyshockrust_input::parse_report(USB_EDGE.model, USB_EDGE.transport, &frame, None)
                    .unwrap();
            let mapped =
                super::map_gamepad_controls(&runtime, None, &report, &mut layers, &mut click);
            assert_eq!(mapped.buttons.contains(Button::South), cross);
            assert!(!mapped.buttons.contains(Button::RightTrigger));
            assert_eq!(mapped.right_trigger, report.controls.right_trigger);
            assert_eq!(mapped.dpad, hat);
        }
    }

    #[test]
    fn virtual_trigger_travel_saturates_at_the_configured_physical_travel() {
        let runtime = super::profile::parse(
            "version = 1\n[triggers.l2]\nfull_travel_percent = 60\nfull_button = 'cross'\n",
        )
        .unwrap();
        for invalid in [0, 101] {
            assert!(
                super::profile::parse(&format!(
                    "version = 1\n[triggers.l2]\nfull_travel_percent = {invalid}\n"
                ))
                .is_err()
            );
        }
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[8] = 8;
        frame[6] = 102;
        let mut layers = super::gamepad::RemapState::default();
        let mut click = super::touchpad::ClickState::default();
        for (left, expected, action) in [
            (0, 0.0, false),
            (102, 2.0 / 3.0, false),
            (153, 1.0, false),
            (255, 1.0, true),
        ] {
            frame[5] = left;
            let report =
                joyshockrust_input::parse_report(USB_EDGE.model, USB_EDGE.transport, &frame, None)
                    .unwrap();
            let mapped =
                super::map_gamepad_controls(&runtime, None, &report, &mut layers, &mut click);
            assert!((mapped.left_trigger - expected).abs() < 0.0001);
            assert_eq!(mapped.right_trigger, report.controls.right_trigger);
            assert_eq!(
                mapped.buttons.contains(joyshockrust_core::Button::South),
                action
            );
        }
    }

    #[test]
    fn exclusive_full_pull_routes_either_digital_stages_or_an_analog_axis() {
        let profile = |target: &str, partial: &str| {
            super::profile::parse(&format!(
                "version = 1\n[triggers.l2]\nfull_pull_exclusive = true\ntravel_limit_percent = 60\nthresholds = [30, 20, 80, 70]\nfull_button = '{target}'\n{partial}"
            ))
        };
        assert!(profile("r2", "partial_button = 'cross'").is_err());
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[8] = 8;
        frame[9] = 4;
        let mut layers = super::gamepad::RemapState::default();
        let mut click = super::touchpad::ClickState::default();
        let mut map = |runtime: &super::profile::RuntimeOptions, left: u8| {
            frame[5] = left;
            let report =
                joyshockrust_input::parse_report(USB_EDGE.model, USB_EDGE.transport, &frame, None)
                    .unwrap();
            super::map_gamepad_controls(runtime, None, &report, &mut layers, &mut click)
        };

        let analog = profile("r2", "").unwrap();
        let mapped = map(&analog, 153);
        assert_eq!([mapped.left_trigger, mapped.right_trigger], [0.0, 1.0]);
        assert!(
            !mapped
                .buttons
                .contains(joyshockrust_core::Button::LeftTrigger)
        );

        let digital = profile("cross", "partial_button = 'square'").unwrap();
        for (physical, partial, full) in [
            (120, true, false),
            (123, false, true),
            (110, false, true),
            (107, true, false),
        ] {
            let mapped = map(&digital, physical);
            assert_eq!(mapped.left_trigger, 0.0);
            assert_eq!(
                mapped.buttons.contains(joyshockrust_core::Button::West),
                partial
            );
            assert_eq!(
                mapped.buttons.contains(joyshockrust_core::Button::South),
                full
            );
        }
    }

    #[test]
    fn flick_stick_centers_only_the_selected_virtual_stick() {
        use joyshockrust_core::Stick;
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].copy_from_slice(&[255, 128, 0, 128]);
        let report =
            joyshockrust_input::parse_report(USB_EDGE.model, USB_EDGE.transport, &frame, None)
                .unwrap();
        for (side, expected_left, expected_right) in [
            ("", report.controls.left_stick, Stick::default()),
            (
                "flick_stick_side = 'left'",
                Stick::default(),
                report.controls.right_stick,
            ),
        ] {
            let runtime =
                super::profile::parse(&format!("version = 1\n[gyro]\nflick_stick = 1.0\n{side}"))
                    .unwrap();
            let controls = super::map_gamepad_controls(
                &runtime,
                runtime.gyro,
                &report,
                &mut super::gamepad::RemapState::default(),
                &mut super::touchpad::ClickState::default(),
            );
            assert_eq!(controls.left_stick, expected_left);
            assert_eq!(controls.right_stick, expected_right);
        }
    }

    #[test]
    fn benchmark_stops_at_the_requested_number_of_decoded_reports() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut valid = [0; 64];
        valid[0] = 1;
        sender.send(&[1; 65]).unwrap();
        for _ in 0..3 {
            sender.send(&valid).unwrap();
        }
        let mut input = File::from(OwnedFd::from(receiver));
        let mut measurement =
            super::benchmark::Benchmark::new(2, super::benchmark::DEFAULT_TIMEOUT);
        let mut published = 0;
        super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers {
                measurement: Some(&mut measurement),
                ..Default::default()
            },
            |event| {
                assert!(matches!(event, super::SessionEvent::Report(_)));
                published += 1;
                Ok(super::ControlFlow::Continue(()))
            },
        )
        .unwrap();
        assert_eq!(published, 2);
        assert_eq!(measurement.rejected, 1);
        assert!(measurement.complete());
    }

    #[test]
    fn benchmark_times_out_with_an_open_but_silent_input() {
        use std::{sync::mpsc, thread, time::Duration};

        let (sender, receiver) = UnixStream::pair().unwrap();
        let (finish, guard) = mpsc::channel::<()>();
        let watchdog = thread::spawn(move || {
            // Close the stream if a regression leaves the test stuck in poll.
            let _ = guard.recv_timeout(Duration::from_secs(1));
            drop(sender);
        });
        let mut input = File::from(OwnedFd::from(receiver));
        let mut measurement = super::benchmark::Benchmark::new(2, Duration::from_millis(20));
        let resource_start = super::benchmark::ResourceSnapshot::capture();
        let result = super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers {
                measurement: Some(&mut measurement),
                ..Default::default()
            },
            |_| panic!("silent input produced an event"),
        );
        drop(finish);
        watchdog.join().unwrap();
        result.unwrap();
        assert!(measurement.timed_out);
        assert!(!measurement.complete());
        measurement.finish_resources(resource_start);
        let mut report = Vec::new();
        measurement.write(USB_EDGE, false, &mut report).unwrap();
        let report = String::from_utf8(report).unwrap();
        assert!(report.lines().any(|line| line == "benchmark_end=timeout"));
        assert!(report.lines().any(|line| line == "reports_measured=0"));
        assert!(
            report
                .lines()
                .any(|line| line.starts_with("benchmark_cpu_percent="))
        );
    }

    #[test]
    fn readiness_loop_services_keyboard_deadlines_while_input_is_idle() {
        use std::{
            cell::Cell,
            time::{Duration, Instant},
        };
        let (_sender, receiver) = UnixDatagram::pair().unwrap();
        let mut input = File::from(OwnedFd::from(receiver));
        let mut benchmark = super::benchmark::Benchmark::new(2, Duration::from_millis(500));
        let deadline = Instant::now() + Duration::from_millis(20);
        let keyboard = Cell::new(Some(deadline));
        let mut fired = false;
        super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers {
                measurement: Some(&mut benchmark),
                keyboard: Some(&keyboard),
                ..Default::default()
            },
            |event| {
                let super::SessionEvent::KeyboardDeadline(now) = event else {
                    unreachable!()
                };
                assert!(now >= deadline);
                fired = true;
                Ok(super::ControlFlow::Break(()))
            },
        )
        .unwrap();
        assert!(fired);
    }

    #[test]
    fn readiness_loop_decodes_complete_reports_in_order() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        sender.send(&[1; 65]).unwrap();
        let mut frame = [0; 64];
        frame[0] = 1;
        frame[1..=4].fill(128);
        frame[8] = 0x28;
        sender.send(&frame).unwrap();
        frame[8] = 8;
        sender.send(&frame).unwrap();

        let mut input = File::from(OwnedFd::from(receiver));
        let mut presses = Vec::new();
        let result = super::forward_reports(
            &mut input,
            USB_EDGE,
            None,
            None,
            None,
            super::ReportTimers::default(),
            |event| {
                let super::SessionEvent::Report(report) = event else {
                    unreachable!()
                };
                presses.push(report.controls.buttons.contains(Button::South));
                if presses.len() == 2 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                Ok(super::ControlFlow::Continue(()))
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(presses, [true, false]);
    }
}

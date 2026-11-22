# HIDRAW input and hardware research

## Chosen interface
Use `/dev/hidraw*` directly; discovery/hotplug through udev. Open nonblocking, register fd with `epoll` (or an equivalently measured Linux event API), read complete HID reports, and decode based on device identity, transport, report ID and length. Treat errors and device removal explicitly.

## Current evidence

The stationary USB capture covers the wired report layout. The Linux bridge
has been tested with a wired Edge. It uses USB ancestry and `HIDIOCGRAWINFO`
to select a node, then waits for reads with `poll`. Annotated control and
motion captures, versioned calibration evidence, and Bluetooth validation
remain open. See the [hardware test record](TESTING_AND_PERFORMANCE.md).

## Hardware verification checklist
- Confirm actual DualSense Edge VID/PID, USB interface selection and report formats against Linux `hid-playstation`, kernel headers, USB captures and real hardware. **Do not hard-code previously discussed report IDs/lengths without verification.**
- Identify what hidraw exposes when `hid-playstation` is bound, and whether simultaneous kernel input devices cause double input.
- Determine the controller's IMU sample cadence, report timestamps, rollover behavior, calibration feature reports and conversion units.
- Check Bluetooth report framing, integrity checks and connection-specific differences.
- Confirm whether controller reports contain multiple sensor samples; preserve all valid samples.
- Check udev permissions, `uaccess`, logind seat behavior and service execution without root.
- Confirm read semantics, error recovery, hotplug and exclusive-access limitations.

## Parser contract
Pure parser takes `(device_model, transport, &[u8], optional_receive_timestamp)` and returns a validated normalized report or structured error. No I/O, no allocations per report. Parse raw units separately from calibrated physical units. Verify CRC for formats that require it.

## Timing
Prefer device timestamps when reliable; map them into a monotonic timebase with explicit rollover and resynchronization logic. Otherwise use `CLOCK_MONOTONIC` receive timestamps, noting added acquisition jitter. Never infer sample rate solely from daemon wakeup rate.

## Security and robustness
Validate lengths before indexing, check ranges and malformed packets, cap work per read cycle, and never trust device-reported counts without bounds. Detect hotplug and recover without stale device references.

## References to inspect
- https://github.com/torvalds/linux/blob/master/drivers/hid/hid-playstation.c
- https://docs.kernel.org/hid/hidraw.html
- https://github.com/aapokaapo/JoyShockMapper-systemd

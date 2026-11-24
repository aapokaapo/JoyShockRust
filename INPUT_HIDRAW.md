# HIDRAW input and hardware research

## Chosen interface
Use `/dev/hidraw*` directly; discovery/hotplug through udev. Open nonblocking, wait for fd readiness with `poll`, read complete HID reports, and decode based on device identity, transport, report ID and length. Treat errors and device removal explicitly.

## Current evidence

The bridge supports DualSense (`054c:0ce6`) and DualSense Edge
(`054c:0df2`) over USB and Bluetooth. Discovery checks candidates through
udev and verifies the opened node with `HIDIOCGRAWINFO`. The parser accepts
full USB input reports (`0x01`, 64 bytes) and full Bluetooth input reports
(`0x31`, 78 bytes with CRC). Battery and touch contacts are decoded on
both transports. Factory calibration uses the transport-specific feature
report parser. [Input parser](crates/joyshockrust-input/src/lib.rs) · [Factory calibration](crates/joyshockrust-input/src/calibration.rs) · [hidraw adapter](crates/joyshockrust-linux/src/hidraw.rs)

The recorded Edge USB fixture contains stationary reports. Synthetic tests
cover active controls, Bluetooth framing and timebase behavior. The user
verified both controller models and validated USB and Bluetooth reconnect
paths on their hardware. Labelled input and motion captures, firmware-specific
calibration comparisons and broader permission checks remain open.
[Hardware evidence](TESTING_AND_PERFORMANCE.md#current-acceptance-priorities)

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
Motion uses the device clock through the deterministic timebase. Device wrap, repeated or backward timestamps and gaps are explicit events. Structurally valid reports can still update controls when an IMU timestamp is rejected. Host receive time is maintained separately for gesture timing, report-rate estimates and benchmarks. [Timebase](crates/joyshockrust-input/src/timebase.rs)

## Security and robustness
Validate lengths before indexing, check ranges and malformed packets, cap work per read cycle, and never trust device-reported counts without bounds. Detect hotplug and recover without stale device references.

## References to inspect
- https://github.com/torvalds/linux/blob/master/drivers/hid/hid-playstation.c
- https://docs.kernel.org/hid/hidraw.html
- https://github.com/aapokaapo/JoyShockMapper-systemd

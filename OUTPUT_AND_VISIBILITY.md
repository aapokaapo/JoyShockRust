# Virtual output and physical controller visibility

## Output
- Use Linux `uinput` for relative mouse, keyboard and standard virtual gamepad capabilities.
- Investigate `UHID` when a faithful HID protocol device is required; do not assume it is equivalent to generic uinput.
- Virtual Xbox-like gamepad behavior and native DualSense emulation are separate compatibility projects.
- Diff output state and avoid redundant writes, but preserve required release events and synchronization boundaries.
- Test correct output lifetime, reconnect, stuck-key prevention and virtual-device identification.

The bridge emits a generic virtual gamepad and optional relative mouse.
The [test record](TESTING_AND_PERFORMANCE.md) covers uinput press/release and
Steam USB reconnect. Per-game visibility still needs its own acceptance run.

## Physical controller visibility
The daemon reads physical devices through hidraw. Games may also discover the physical controller through evdev, SDL, Steam Input, Wine/Proton or other paths, causing duplicate input.

**Decision:** default to *per-game environment-based filtering* when supported, not global device hiding. This is a compatibility feature of launch tooling, not part of the low-latency engine.

Candidate SDL variable: `SDL_GAMECONTROLLER_IGNORE_DEVICES` with a **verified** VID/PID; this only affects consumers honoring that SDL setting and is **not guaranteed to hide evdev/HID devices from Proton or native applications**. Do not claim it works universally. Verify behavior with the actual game, Proton build and input backend. Do not automatically use the physical controller's VID/PID for the virtual controller.

CLI should eventually offer `visibility inspect`, `visibility launch-options` and a guided test, showing physical and virtual IDs, affected APIs, limitations and results. Avoid privileged device isolation by default; document optional fallback only after verifying feasibility and safety.

## Acceptance
A tested game receives input from the virtual device and not the physical device; provide a repeatable verification method. A successful launch variable alone does not count as proof.

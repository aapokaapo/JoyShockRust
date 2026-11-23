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

The Linux launcher appends Sony IDs `054c:0ce6` and `054c:0df2` to
`SDL_GAMECONTROLLER_IGNORE_DEVICES` for the launched command. The filter
applies to SDL consumers that honor this hint. Steam Input, Proton and
consumers using evdev or HID directly have their own device discovery.
The launcher preserves the existing ignore list, including SDL's
`@file` syntax, and reports a conflict if an SDL allow list is set.
[SDL ignore-list format](https://github.com/libsdl-org/SDL/blob/402fc52af4e731184ad6a704068b5ccd27d8f1b8/include/SDL3/SDL_hints.h#L897-L929)

```sh
joyshockrust-linux --launch-sdl -- GAME [ARGS...]
```

The bridge runs separately and continues reading the physical controller.
See the [SDL visibility test](TESTING_AND_PERFORMANCE.md#per-game-sdl-visibility-acceptance).

## Acceptance
A tested game receives input from the virtual device and not the physical device; provide a repeatable verification method. A successful launch variable alone does not count as proof.

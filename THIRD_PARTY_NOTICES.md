# Third-party notices

JoyShockRust takes inspiration from JoyShockMapper and JoyShockMapper-systemd.
Their source was consulted for features such as Flick Stick and game
sensitivity calibration. Both source revisions carry the same MIT notice,
reproduced below. [JoyShockMapper](https://github.com/Electronicks/JoyShockMapper/blob/e148efe0b6ff4ad2ce0fc8c2560fa1028097b1a7/LICENSE.md)
· [JoyShockMapper-systemd](https://github.com/aapokaapo/JoyShockMapper-systemd/blob/bb69784488937e0a5e21988b966eccd9f04d504e/LICENSE.md)

The [reference analysis](docs/REFERENCE_ANALYSIS.md) identifies the
controller protocol and platform sources. The [USB replay fixture](crates/joyshockrust-input/tests/fixtures/dualsense_edge_usb.bin)
is a local DualSense Edge capture contributed by Aapo Kinnunen. [Capture commit](https://github.com/aapokaapo/JoyShockRust/commit/d7da53b)

Dependencies declared in Cargo manifests and native libraries used by
the binaries remain under their respective licenses. For a binary
distribution, include the notices and other license materials required
by the dependencies included in that distribution.

## MIT License

Copyright 2018-2021 Julian "Jibb" Smart; Copyright 2021- Nicolas Lessard.

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

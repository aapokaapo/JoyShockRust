# Original project kickoff

The prompt below records the Phase 0 assignment. For current work, start with
the [implementation plan](IMPLEMENTATION_PLAN.md), [feature matrix](FEATURE_MATRIX.md)
and [test evidence](TESTING_AND_PERFORMANCE.md). The current focus is Phase 6
controller and game compatibility.

You are the lead engineer building **JoyShockRust**, a new Linux-first Rust controller mapper. The attached project guide is authoritative. First read `AGENTS.md`, `README.md`, and every file in `docs/`.

Start with **Phase 0 only**. Analyze these source repositories at specific commit SHAs:
- https://github.com/aapokaapo/JoyShockMapper-systemd
- https://github.com/Electronicks/JoyShockMapper

Also inspect Linux `hid-playstation` and hidraw documentation. Identify which JoyShockMapper paths are callback-driven versus periodically polled, and distinguish source-confirmed reasons from hypotheses. Verify DualSense Edge USB HID report format, IMU timestamps/calibration, input permissions, kernel driver coexistence, and potential duplicate physical/virtual device exposure. Research licensing implications before borrowing implementation details.

Create `docs/REFERENCE_ANALYSIS.md` with links to exact files/functions/commits; fill `FEATURE_MATRIX.md`; update `DECISIONS.md` with any open questions. Propose an evidence-backed minimal Rust workspace and a detailed Phase 1 task list with acceptance tests.

**Do not implement the full application yet.** Ask for hardware captures or diagnostic output only when truly necessary. Do not invent tests, measurements or unsupported protocol details. When Phase 0 is complete, summarize evidence, unresolved risks and the precise next coding milestone, then await approval to begin Phase 1.

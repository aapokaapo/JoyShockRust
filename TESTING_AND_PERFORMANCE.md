# Testing, instrumentation and performance

## Initial targets (provisional, one controller)
| Metric | Target | Definition |
|---|---|---|
| Idle CPU | <0.5% of one core | Daemon idle, controller connected; document sampling window |
| Active CPU | <2% of one core | Representative gameplay, one controller |
| Resident memory | <20 MB | Headless daemon RSS |
| Processing latency | p99 <0.5 ms | From normalized input availability to output submission; excludes physical I/O |
| Hot-path allocations | 0 | Steady-state per-report processing |
| Input cadence | Preserve device reports | No intentional gyro sample dropping |

These are **engineering targets**, not established measurements. Measure USB and Bluetooth separately and distinguish per-report processing, scheduling delay, HID acquisition and kernel output delivery.

## Test pyramid
- Unit: parsers, units, timebase rollover, mapping transitions, fusion math.
- Property/fuzz: malformed HID reports, state machine invariants, bounded timers.
- Replay: timestamped IMU and button traces; same input yields same output.
- Integration: virtual output events, permissions, hotplug, IPC reload.
- Hardware-in-loop: wired DualSense Edge first, then Bluetooth and others.
- Compatibility: Steam/Proton/native game discovery and filtering.

## Benchmarks
Use a repeatable workload and document CPU model, kernel, Rust version, power profile, controller firmware/transport, report rate, and whether debug/release. Record p50, p95, p99, max, samples, and any drops. Include CPU contention, idle and reconnect cases. Avoid misleading comparisons between processing-only latency and physical end-to-end latency.

## Observability
Counters for reports received/rejected, timestamp gaps, output writes, queue/backlog, reconnects and timer deadlines. Diagnostics must be buffered or collected outside the hot path; no synchronous per-sample logging.

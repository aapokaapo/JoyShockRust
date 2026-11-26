use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

use joyshockrust_input::{Timebase, TimestampUpdate};
use nix::sys::{
    resource::{UsageWho, getrusage},
    time::TimeValLike,
};

use crate::{discovery, hidraw::Identity};

pub const MAX_REPORTS: usize = 100_000;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
pub const MAX_TIMEOUT_SECONDS: u64 = 3600;
// Match the 50 ms motion gap at three DualSense sensor ticks per microsecond.
const DEVICE_GAP_TICKS: u32 = joyshockrust_input::ticks_in_nanoseconds(50_000_000) as u32;

pub struct Benchmark {
    target: usize,
    pub timeout: Duration,
    pub timed_out: bool,
    processing_ns: Vec<u64>,
    interval_ns: Vec<u64>,
    device_interval_ns: Vec<u64>,
    timebase: Timebase,
    repeated: usize,
    out_of_order: usize,
    gaps: usize,
    wraps: usize,
    first: Option<Instant>,
    last: Option<Instant>,
    resources: Option<ResourceTotals>,
    pub rejected: u64,
}

impl Benchmark {
    pub fn new(target: usize, timeout: Duration) -> Self {
        Self {
            target,
            timeout,
            timed_out: false,
            processing_ns: Vec::with_capacity(target),
            interval_ns: Vec::with_capacity(target - 1),
            device_interval_ns: Vec::with_capacity(target - 1),
            timebase: Timebase::new(DEVICE_GAP_TICKS).expect("valid benchmark gap limit"),
            repeated: 0,
            out_of_order: 0,
            gaps: 0,
            wraps: 0,
            first: None,
            last: None,
            resources: None,
            rejected: 0,
        }
    }

    pub fn record(&mut self, started: Instant, processing: Duration, device_ticks: u32) {
        self.first.get_or_insert(started);
        if let Some(last) = self.last.replace(started) {
            self.interval_ns.push(nanos(started.duration_since(last)));
        }
        self.processing_ns.push(nanos(processing));
        match self.timebase.observe(device_ticks) {
            TimestampUpdate::FirstSample => {}
            TimestampUpdate::Advanced { delta, wrapped, .. } => {
                self.device_interval_ns.push(nanos(delta));
                self.wraps += usize::from(wrapped);
            }
            TimestampUpdate::Repeated { .. } => self.repeated += 1,
            TimestampUpdate::OutOfOrder { .. } => self.out_of_order += 1,
            TimestampUpdate::Gap { .. } => self.gaps += 1,
        }
    }

    pub fn complete(&self) -> bool {
        self.processing_ns.len() == self.target
    }

    pub fn finish_resources(&mut self, start: Option<ResourceSnapshot>) {
        self.resources =
            start.and_then(|start| ResourceSnapshot::capture().map(|end| end.since(start)));
    }

    pub fn write(
        &mut self,
        identity: Identity,
        failed: bool,
        mut output: impl Write,
    ) -> io::Result<()> {
        let reason = if failed {
            "error"
        } else if self.complete() {
            "complete"
        } else if self.timed_out {
            "timeout"
        } else {
            "disconnected"
        };
        writeln!(output, "benchmark_complete={}", self.complete())?;
        writeln!(output, "benchmark_end={reason}")?;
        writeln!(output, "benchmark_timeout_s={}", self.timeout.as_secs())?;
        writeln!(output, "benchmark_model={}", identity.model_name())?;
        writeln!(
            output,
            "benchmark_transport={}",
            discovery::transport_name(identity.transport)
        )?;
        writeln!(output, "reports_target={}", self.target)?;
        writeln!(output, "reports_measured={}", self.processing_ns.len())?;
        writeln!(output, "reports_rejected={}", self.rejected)?;
        if let Some(resources) = &self.resources {
            resources.write(&mut output)?;
        } else {
            writeln!(output, "benchmark_resources=unavailable")?;
        }
        if let Some((first, last)) = self.first.zip(self.last)
            && last > first
        {
            let rate = self.interval_ns.len() as f64 / last.duration_since(first).as_secs_f64();
            writeln!(output, "report_rate_hz={rate:.1}")?;
        } else {
            writeln!(output, "report_rate_hz=none")?;
        }
        write_distribution(&mut output, "processing", &mut self.processing_ns)?;
        write_distribution(&mut output, "host_interval", &mut self.interval_ns)?;
        writeln!(output, "device_gap_threshold_ticks={DEVICE_GAP_TICKS}")?;
        writeln!(output, "device_timestamp_repeated={}", self.repeated)?;
        writeln!(
            output,
            "device_timestamp_out_of_order={}",
            self.out_of_order
        )?;
        writeln!(output, "device_timestamp_gaps={}", self.gaps)?;
        writeln!(output, "device_timestamp_wraps={}", self.wraps)?;
        write_distribution(&mut output, "device_interval", &mut self.device_interval_ns)
    }
}

pub struct ResourceSnapshot {
    timestamp: Instant,
    user_us: i64,
    system_us: i64,
    peak_rss_kib: u64,
}

impl ResourceSnapshot {
    pub fn capture() -> Option<Self> {
        let timestamp = Instant::now();
        let usage = getrusage(UsageWho::RUSAGE_SELF).ok()?;
        Some(Self {
            timestamp,
            user_us: usage.user_time().num_microseconds(),
            system_us: usage.system_time().num_microseconds(),
            peak_rss_kib: u64::try_from(usage.max_rss()).ok()?,
        })
    }

    fn since(self, start: Self) -> ResourceTotals {
        ResourceTotals {
            wall: self.timestamp.duration_since(start.timestamp),
            user_us: self.user_us - start.user_us,
            system_us: self.system_us - start.system_us,
            peak_rss_kib: self.peak_rss_kib,
        }
    }
}

struct ResourceTotals {
    wall: Duration,
    user_us: i64,
    system_us: i64,
    peak_rss_kib: u64,
}

impl ResourceTotals {
    fn write(&self, output: &mut impl Write) -> io::Result<()> {
        writeln!(output, "benchmark_wall_ns={}", nanos(self.wall))?;
        writeln!(output, "benchmark_cpu_user_us={}", self.user_us)?;
        writeln!(output, "benchmark_cpu_system_us={}", self.system_us)?;
        if self.wall.is_zero() {
            writeln!(output, "benchmark_cpu_percent=none")?;
        } else {
            let cpu_seconds = (self.user_us + self.system_us) as f64 / 1_000_000.0;
            let percent = 100.0 * cpu_seconds / self.wall.as_secs_f64();
            writeln!(output, "benchmark_cpu_percent={percent:.2}")?;
        }
        writeln!(output, "benchmark_peak_rss_kib={}", self.peak_rss_kib)
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn write_distribution(output: &mut impl Write, name: &str, samples: &mut [u64]) -> io::Result<()> {
    writeln!(output, "{name}_samples={}", samples.len())?;
    if samples.is_empty() {
        return Ok(());
    }
    samples.sort_unstable();
    for percentile in [50, 95, 99, 100] {
        let label = if percentile == 100 {
            "max".to_owned()
        } else {
            format!("p{percentile}")
        };
        let index = (samples.len() * percentile).div_ceil(100) - 1;
        writeln!(output, "{name}_{label}_ns={}", samples[index])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributions_use_nearest_rank_and_report_partial_runs() {
        use joyshockrust_input::{DeviceModel, Transport};

        let mut benchmark = Benchmark::new(5, DEFAULT_TIMEOUT);
        let first = Instant::now();
        for (offset, processing, ticks) in [(0, 10, 0), (1, 40, 3000), (2, 20, 6000), (6, 30, 9000)]
        {
            benchmark.record(
                first + Duration::from_millis(offset),
                Duration::from_nanos(processing),
                ticks,
            );
        }
        benchmark.rejected = 1;
        let mut report = Vec::new();
        benchmark
            .write(
                Identity {
                    model: DeviceModel::DualSenseEdge,
                    transport: Transport::Usb,
                },
                false,
                &mut report,
            )
            .unwrap();
        let report = String::from_utf8(report).unwrap();
        for expected in [
            "benchmark_complete=false",
            "benchmark_end=disconnected",
            "reports_measured=4",
            "reports_rejected=1",
            "report_rate_hz=500.0",
            "processing_p50_ns=20",
            "processing_p95_ns=40",
            "processing_p99_ns=40",
            "processing_max_ns=40",
            "host_interval_samples=3",
            "host_interval_p50_ns=1000000",
            "host_interval_max_ns=4000000",
            "device_interval_samples=3",
            "device_interval_max_ns=1000000",
        ] {
            assert!(report.lines().any(|line| line == expected), "{report}");
        }
    }

    #[test]
    fn device_clock_classifies_discontinuities_without_changing_report_count() {
        let mut benchmark = Benchmark::new(8, DEFAULT_TIMEOUT);
        let start = Instant::now();
        for ticks in [
            u32::MAX - 2999,
            0,
            0,
            u32::MAX,
            3000,
            153_000,
            303_001,
            306_001,
        ] {
            benchmark.record(start, Duration::ZERO, ticks);
        }
        assert!(benchmark.complete());
        assert_eq!(benchmark.wraps, 1);
        assert_eq!(benchmark.repeated, 1);
        assert_eq!(benchmark.out_of_order, 1);
        assert_eq!(benchmark.gaps, 1);
        assert_eq!(
            benchmark.device_interval_ns,
            [1_000_000, 1_000_000, 50_000_000, 1_000_000]
        );
    }

    #[test]
    fn resource_report_uses_cpu_deltas_and_the_final_process_peak() {
        let timestamp = Instant::now();
        let start = ResourceSnapshot {
            timestamp,
            user_us: 500_000,
            system_us: 20_000,
            peak_rss_kib: 3500,
        };
        let end = ResourceSnapshot {
            timestamp: timestamp + Duration::from_secs(2),
            user_us: 516_000,
            system_us: 24_000,
            peak_rss_kib: 3900,
        };
        let mut output = Vec::new();
        end.since(start).write(&mut output).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "benchmark_wall_ns=2000000000\nbenchmark_cpu_user_us=16000\nbenchmark_cpu_system_us=4000\nbenchmark_cpu_percent=1.00\nbenchmark_peak_rss_kib=3900\n"
        );
    }
}

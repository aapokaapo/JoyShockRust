use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

use crate::{discovery, hidraw::Identity};

pub const MAX_REPORTS: usize = 100_000;

pub struct Benchmark {
    target: usize,
    processing_ns: Vec<u64>,
    interval_ns: Vec<u64>,
    first: Option<Instant>,
    last: Option<Instant>,
    pub rejected: u64,
}

impl Benchmark {
    pub fn new(target: usize) -> Self {
        Self {
            target,
            processing_ns: Vec::with_capacity(target),
            interval_ns: Vec::with_capacity(target - 1),
            first: None,
            last: None,
            rejected: 0,
        }
    }

    pub fn record(&mut self, started: Instant, processing: Duration) {
        self.first.get_or_insert(started);
        if let Some(last) = self.last.replace(started) {
            self.interval_ns.push(nanos(started.duration_since(last)));
        }
        self.processing_ns.push(nanos(processing));
    }

    pub fn complete(&self) -> bool {
        self.processing_ns.len() == self.target
    }

    pub fn write(&mut self, identity: Identity, mut output: impl Write) -> io::Result<()> {
        writeln!(output, "benchmark_complete={}", self.complete())?;
        writeln!(output, "benchmark_model={}", identity.model_name())?;
        writeln!(
            output,
            "benchmark_transport={}",
            discovery::transport_name(identity.transport)
        )?;
        writeln!(output, "reports_target={}", self.target)?;
        writeln!(output, "reports_measured={}", self.processing_ns.len())?;
        writeln!(output, "reports_rejected={}", self.rejected)?;
        if let Some((first, last)) = self.first.zip(self.last)
            && last > first
        {
            let rate = self.interval_ns.len() as f64 / last.duration_since(first).as_secs_f64();
            writeln!(output, "report_rate_hz={rate:.1}")?;
        } else {
            writeln!(output, "report_rate_hz=none")?;
        }
        write_distribution(&mut output, "processing", &mut self.processing_ns)?;
        write_distribution(&mut output, "host_interval", &mut self.interval_ns)
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

        let mut benchmark = Benchmark::new(5);
        let first = Instant::now();
        for (offset, processing) in [(0, 10), (1, 40), (2, 20), (6, 30)] {
            benchmark.record(
                first + Duration::from_millis(offset),
                Duration::from_nanos(processing),
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
                &mut report,
            )
            .unwrap();
        let report = String::from_utf8(report).unwrap();
        for expected in [
            "benchmark_complete=false",
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
        ] {
            assert!(report.lines().any(|line| line == expected), "{report}");
        }
    }
}

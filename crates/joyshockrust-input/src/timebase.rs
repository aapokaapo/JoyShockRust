use core::{fmt, time::Duration};

const HALF_COUNTER_RANGE: u32 = 1 << 31;
const TICKS_PER_MICROSECOND: u64 = 3;
const NANOS_PER_MICROSECOND: u64 = 1_000;

/// Classification of a timestamp from a successfully parsed report.
/// Convert a duration to whole device ticks, rounding down.
pub const fn ticks_in_nanoseconds(nanos: u64) -> u64 {
    nanos / NANOS_PER_MICROSECOND * TICKS_PER_MICROSECOND
        + nanos % NANOS_PER_MICROSECOND * TICKS_PER_MICROSECOND / NANOS_PER_MICROSECOND
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampUpdate {
    /// A new segment starts with zero elapsed time and no integration delta.
    FirstSample,
    /// A forward sample within the configured limit.
    Advanced {
        elapsed: Duration,
        delta: Duration,
        wrapped: bool,
    },
    /// A report with the same sensor timestamp. Digital controls may still change.
    Repeated { elapsed: Duration },
    /// A gap started a new segment. Its first sample has zero elapsed time.
    Gap { delta_ticks: u32 },
    /// A backward or ambiguous timestamp. The last accepted time is retained.
    OutOfOrder { previous_ticks: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidForwardLimit(pub u32);

impl fmt::Display for InvalidForwardLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "forward limit {} must be in 1..{HALF_COUNTER_RANGE} ticks",
            self.0
        )
    }
}

impl core::error::Error for InvalidForwardLimit {}

/// Extends the 32-bit sensor clock within one connection and continuous segment.
///
/// The observed clock alone cannot distinguish stale reports from some device
/// resets. Call [`Self::reset`] at a known reset, reconnect or prolonged host
/// stall that could make counter ordering ambiguous.
#[derive(Debug)]
pub struct Timebase {
    max_forward_ticks: u32,
    previous_ticks: Option<u32>,
    elapsed: Duration,
    remainder: u64,
}

impl Timebase {
    /// Set the largest forward tick delta allowed within a segment.
    /// The input source should choose this limit based on its observed cadence.
    pub fn new(max_forward_ticks: u32) -> Result<Self, InvalidForwardLimit> {
        if !(1..HALF_COUNTER_RANGE).contains(&max_forward_ticks) {
            return Err(InvalidForwardLimit(max_forward_ticks));
        }
        Ok(Self {
            max_forward_ticks,
            previous_ticks: None,
            elapsed: Duration::ZERO,
            remainder: 0,
        })
    }

    /// End the current segment. The next report starts at zero elapsed time.
    pub fn reset(&mut self) {
        self.previous_ticks = None;
        self.elapsed = Duration::ZERO;
        self.remainder = 0;
    }

    /// Consume a device timestamp after the caller validates the report.
    pub fn observe(&mut self, ticks: u32) -> TimestampUpdate {
        let Some(previous_ticks) = self.previous_ticks else {
            self.previous_ticks = Some(ticks);
            return TimestampUpdate::FirstSample;
        };

        let delta_ticks = ticks.wrapping_sub(previous_ticks);
        if delta_ticks == 0 {
            return TimestampUpdate::Repeated {
                elapsed: self.elapsed,
            };
        }
        if delta_ticks >= HALF_COUNTER_RANGE {
            return TimestampUpdate::OutOfOrder { previous_ticks };
        }
        if delta_ticks > self.max_forward_ticks {
            self.reset();
            self.previous_ticks = Some(ticks);
            return TimestampUpdate::Gap { delta_ticks };
        }

        // Accumulate fractional nanoseconds across reports. Three one-tick
        // updates then advance exactly one microsecond.
        let numerator = u64::from(delta_ticks) * NANOS_PER_MICROSECOND + self.remainder;
        let delta = Duration::from_nanos(numerator / TICKS_PER_MICROSECOND);
        self.remainder = numerator % TICKS_PER_MICROSECOND;
        self.elapsed += delta;
        self.previous_ticks = Some(ticks);
        TimestampUpdate::Advanced {
            elapsed: self.elapsed,
            delta,
            wrapped: ticks < previous_ticks,
        }
    }
}

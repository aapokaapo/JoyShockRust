use core::time::Duration;

use joyshockrust_input::{InvalidForwardLimit, Timebase, TimestampUpdate};

const HALF_RANGE: u32 = 1 << 31;

#[test]
fn rejects_ambiguous_forward_limits() {
    for limit in [0, HALF_RANGE, u32::MAX] {
        assert_eq!(
            Timebase::new(limit).unwrap_err(),
            InvalidForwardLimit(limit)
        );
    }
    assert!(Timebase::new(1).is_ok());
    assert!(Timebase::new(HALF_RANGE - 1).is_ok());
}

#[test]
fn advances_from_zero_and_carries_fractional_nanoseconds() {
    let mut timebase = Timebase::new(10).unwrap();
    assert_eq!(timebase.observe(0), TimestampUpdate::FirstSample);
    for (ticks, elapsed_ns, delta_ns) in [(1, 333, 333), (2, 666, 333), (3, 1000, 334)] {
        assert_eq!(
            timebase.observe(ticks),
            TimestampUpdate::Advanced {
                elapsed: Duration::from_nanos(elapsed_ns),
                delta: Duration::from_nanos(delta_ns),
                wrapped: false,
            }
        );
    }
    assert_eq!(
        timebase.observe(3),
        TimestampUpdate::Repeated {
            elapsed: Duration::from_micros(1)
        }
    );
}

#[test]
fn advances_over_wrap_within_the_forward_limit() {
    let mut timebase = Timebase::new(3).unwrap();
    assert_eq!(timebase.observe(u32::MAX - 1), TimestampUpdate::FirstSample);
    assert_eq!(
        timebase.observe(1),
        TimestampUpdate::Advanced {
            elapsed: Duration::from_micros(1),
            delta: Duration::from_micros(1),
            wrapped: true,
        }
    );
}

#[test]
fn backwards_and_half_range_deltas_preserve_the_accepted_timestamp() {
    let mut timebase = Timebase::new(3).unwrap();
    assert_eq!(timebase.observe(42), TimestampUpdate::FirstSample);
    for ticks in [41, 42 + HALF_RANGE] {
        assert_eq!(
            timebase.observe(ticks),
            TimestampUpdate::OutOfOrder { previous_ticks: 42 }
        );
    }
    assert_eq!(
        timebase.observe(45),
        TimestampUpdate::Advanced {
            elapsed: Duration::from_micros(1),
            delta: Duration::from_micros(1),
            wrapped: false,
        }
    );
}

#[test]
fn forward_gap_starts_a_new_segment_and_clears_remainder() {
    let mut timebase = Timebase::new(2).unwrap();
    assert_eq!(timebase.observe(0), TimestampUpdate::FirstSample);
    assert!(matches!(
        timebase.observe(2),
        TimestampUpdate::Advanced { .. }
    ));
    assert_eq!(timebase.observe(5), TimestampUpdate::Gap { delta_ticks: 3 });
    assert_eq!(
        timebase.observe(6),
        TimestampUpdate::Advanced {
            elapsed: Duration::from_nanos(333),
            delta: Duration::from_nanos(333),
            wrapped: false,
        }
    );
}

#[test]
fn explicit_reset_accepts_a_restarted_device_clock() {
    let mut timebase = Timebase::new(3).unwrap();
    assert_eq!(timebase.observe(100), TimestampUpdate::FirstSample);
    assert!(matches!(
        timebase.observe(102),
        TimestampUpdate::Advanced { .. }
    ));
    assert_eq!(
        timebase.observe(0),
        TimestampUpdate::OutOfOrder {
            previous_ticks: 102
        }
    );
    timebase.reset();
    assert_eq!(timebase.observe(0), TimestampUpdate::FirstSample);
    assert_eq!(
        timebase.observe(1),
        TimestampUpdate::Advanced {
            elapsed: Duration::from_nanos(333),
            delta: Duration::from_nanos(333),
            wrapped: false,
        }
    );
}

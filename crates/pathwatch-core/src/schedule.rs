//! Probe scheduling: a fixed slot grid per stream, phase-staggered, with
//! explicit accounting for slots that could not be attempted.
//!
//! Behaviour on a late wake-up matches what Tokio documents for
//! `MissedTickBehavior::Skip` — fire once now, then continue on the original
//! grid — and never `Burst`, which would send a catch-up burst of probes.
//! Unlike `tokio::time::Interval`, the skipped slots are reported.

use std::time::Duration;

/// Slot `k` of a stream is due at `phase + k * interval` on the session's
/// monotonic clock.
#[derive(Debug, Clone)]
pub struct SlotGrid {
    phase_ns: u64,
    interval_ns: u64,
    next: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    /// The slot to attempt now.
    pub fire: u64,
    /// Slots that were passed over because the timer fired late.
    pub skipped: std::ops::Range<u64>,
}

impl SlotGrid {
    pub fn new(phase: Duration, interval: Duration) -> Self {
        assert!(!interval.is_zero(), "interval must be positive");
        Self { phase_ns: phase.as_nanos() as u64, interval_ns: interval.as_nanos() as u64, next: 0 }
    }

    pub fn interval_ns(&self) -> u64 {
        self.interval_ns
    }

    pub fn slot_time_ns(&self, k: u64) -> u64 {
        self.phase_ns + k * self.interval_ns
    }

    /// The number of the next unattempted slot.
    pub fn next_index(&self) -> u64 {
        self.next
    }

    /// When the next unattempted slot is due.
    pub fn next_due_ns(&self) -> u64 {
        self.slot_time_ns(self.next)
    }

    /// Call when the timer for [`Self::next_due_ns`] fires. Returns `None` if
    /// `now` is still before the next slot (a spurious wake-up).
    pub fn on_wake(&mut self, now_ns: u64) -> Option<Wake> {
        if now_ns < self.next_due_ns() {
            return None;
        }
        let current = (now_ns - self.phase_ns) / self.interval_ns;
        let wake = Wake { fire: current, skipped: self.next..current };
        self.next = current + 1;
        Some(wake)
    }

    /// Jumps past every slot due before `now` without reporting them — used
    /// after an observation gap, during which no slots exist.
    pub fn resume_after_gap(&mut self, now_ns: u64) {
        if now_ns >= self.phase_ns {
            let current = (now_ns - self.phase_ns) / self.interval_ns;
            self.next = self.next.max(current + 1);
        }
    }
}

/// Spreads `n` streams sharing an interval evenly across it, in order, so runs
/// are reproducible and probes do not leave in synchronised bursts.
pub fn stagger_phase(index: usize, n: usize, interval: Duration) -> Duration {
    if n == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos((interval.as_nanos() as u64 / n as u64) * index as u64)
}

/// Global limit on probe transactions: `rate` per second sustained, `burst` at once.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    rate_per_ns: f64,
    last_ns: u64,
}

impl TokenBucket {
    pub fn new(rate_per_sec: u32, burst: u32) -> Self {
        Self { capacity: burst as f64, tokens: burst as f64, rate_per_ns: rate_per_sec as f64 / 1e9, last_ns: 0 }
    }

    pub fn try_take(&mut self, now_ns: u64) -> bool {
        let elapsed = now_ns.saturating_sub(self.last_ns);
        self.last_ns = self.last_ns.max(now_ns);
        self.tokens = (self.tokens + elapsed as f64 * self.rate_per_ns).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn fires_each_slot_on_time() {
        let mut g = SlotGrid::new(Duration::from_millis(100), Duration::from_secs(1));
        assert_eq!(g.next_due_ns(), 100 * MS);
        assert_eq!(g.on_wake(99 * MS), None);
        assert_eq!(g.on_wake(100 * MS), Some(Wake { fire: 0, skipped: 0..0 }));
        assert_eq!(g.next_due_ns(), 1100 * MS);
        assert_eq!(g.on_wake(1100 * MS + 3), Some(Wake { fire: 1, skipped: 1..1 }));
    }

    #[test]
    fn late_timer_skips_explicitly_and_keeps_phase() {
        let mut g = SlotGrid::new(Duration::from_millis(100), Duration::from_secs(1));
        g.on_wake(100 * MS).unwrap();
        // Timer fires 3.4 s late: slots 1..=3 were due; 1 and 2 are skipped, 3 fires now.
        let w = g.on_wake(3_500 * MS).unwrap();
        assert_eq!(w, Wake { fire: 3, skipped: 1..3 });
        // The grid is unchanged: the next slot is still on the original phase.
        assert_eq!(g.next_due_ns(), 4_100 * MS);
    }

    #[test]
    fn no_burst_after_a_long_pause() {
        let mut g = SlotGrid::new(Duration::ZERO, Duration::from_secs(1));
        g.on_wake(0).unwrap();
        let w = g.on_wake(600_000 * MS).unwrap();
        assert_eq!(w.fire, 600);
        assert_eq!(w.skipped.end - w.skipped.start, 599);
        // Exactly one send results; the following wake is a full interval later.
        assert_eq!(g.on_wake(600_000 * MS + 1), None);
    }

    #[test]
    fn resume_after_gap_reports_nothing() {
        let mut g = SlotGrid::new(Duration::ZERO, Duration::from_secs(1));
        g.on_wake(0).unwrap();
        g.resume_after_gap(10_250 * MS);
        assert_eq!(g.next_due_ns(), 11_000 * MS);
        assert_eq!(g.on_wake(11_000 * MS), Some(Wake { fire: 11, skipped: 11..11 }));
    }

    #[test]
    fn stagger_is_even_and_ordered() {
        let i = Duration::from_secs(1);
        let phases: Vec<_> = (0..9).map(|k| stagger_phase(k, 9, i).as_millis()).collect();
        assert_eq!(phases, vec![0, 111, 222, 333, 444, 555, 666, 777, 888]);
    }

    #[test]
    fn token_bucket_enforces_rate_and_burst() {
        let mut b = TokenBucket::new(25, 40);
        let taken = (0..100).filter(|_| b.try_take(0)).count();
        assert_eq!(taken, 40, "burst");
        assert!(!b.try_take(10 * MS));
        assert!(b.try_take(40 * MS), "one token after 40 ms at 25/s");
        let over_one_second = (0..1000).filter(|i| b.try_take(40 * MS + i * MS)).count();
        assert!((24..=26).contains(&over_one_second), "sustained rate, got {over_one_second}");
    }
}

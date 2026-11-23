//! Clocks and observation-gap detection.
//!
//! Durations and deadlines use a monotonic clock that does **not** advance while
//! the host is suspended (Tokio's clock, which follows `std::time::Instant`).
//! That is what keeps a resume from producing a burst of overdue timers — and
//! also why suspend has to be detected separately, by comparing against a clock
//! that does count suspended time.

use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;

use crate::model::GapCause;

pub trait Clock: Send + Sync + 'static {
    /// Monotonic nanoseconds since session start; excludes suspended time.
    fn mono_ns(&self) -> u64;
    /// Wall-clock UTC. Never used to compute a duration.
    fn wall(&self) -> Timestamp;
    /// Total time the host has spent suspended since session start, as far as
    /// the platform's clocks can tell. `None` when no suitable clock pair exists.
    fn suspended_ns(&self) -> Option<u64>;
    /// The timer instant corresponding to a session-monotonic time.
    fn instant_at(&self, mono_ns: u64) -> tokio::time::Instant;
}

pub fn utc_string(ts: Timestamp) -> String {
    format!("{ts:.3}")
}

/// The real clock.
pub struct SystemClock {
    start: tokio::time::Instant,
    pair0: Option<(u64, u64)>,
}

impl SystemClock {
    pub fn new() -> Self {
        Self { start: tokio::time::Instant::now(), pair0: crate::platform::suspend_clock_pair() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn mono_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    fn wall(&self) -> Timestamp {
        Timestamp::now()
    }

    fn suspended_ns(&self) -> Option<u64> {
        let (excl0, incl0) = self.pair0?;
        let (excl, incl) = crate::platform::suspend_clock_pair()?;
        Some(incl.saturating_sub(incl0).saturating_sub(excl.saturating_sub(excl0)))
    }

    fn instant_at(&self, mono_ns: u64) -> tokio::time::Instant {
        self.start + Duration::from_nanos(mono_ns)
    }
}

/// A clock for tests: monotonic time follows Tokio's (pausable) clock, wall
/// time is derived from it, and suspension and wall-clock steps are injected.
pub struct ManualClock {
    start: tokio::time::Instant,
    inner: Mutex<ManualInner>,
}

struct ManualInner {
    wall_base: Timestamp,
    suspended: Duration,
    wall_step_ms: i64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            start: tokio::time::Instant::now(),
            inner: Mutex::new(ManualInner {
                // 2026-01-01T00:00:00Z
                wall_base: Timestamp::from_second(1_767_225_600).expect("valid timestamp"),
                suspended: Duration::ZERO,
                wall_step_ms: 0,
            }),
        }
    }

    /// Simulates the host sleeping for `d`: wall time moves, monotonic time does not.
    pub fn suspend(&self, d: Duration) {
        self.inner.lock().unwrap().suspended += d;
    }

    /// Simulates a wall-clock adjustment (NTP step, manual change).
    pub fn step_wall(&self, ms: i64) {
        self.inner.lock().unwrap().wall_step_ms += ms;
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn mono_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }

    fn wall(&self) -> Timestamp {
        let g = self.inner.lock().unwrap();
        let ns = self.start.elapsed().as_nanos() as i128 + g.suspended.as_nanos() as i128 + g.wall_step_ms as i128 * 1_000_000;
        Timestamp::from_nanosecond(g.wall_base.as_nanosecond() + ns).expect("valid timestamp")
    }

    fn suspended_ns(&self) -> Option<u64> {
        Some(self.inner.lock().unwrap().suspended.as_nanos() as u64)
    }

    fn instant_at(&self, mono_ns: u64) -> tokio::time::Instant {
        self.start + Duration::from_nanos(mono_ns)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockFinding {
    /// The instrument was not observing for `duration_ns` of real time.
    Gap { cause: GapCause, start_ns: u64, end_ns: u64, duration_ns: u64 },
    /// Wall-clock time moved relative to monotonic time without a suspend.
    /// Durations are unaffected; this is reported as an event only.
    WallStep { delta_ms: i64 },
}

/// Call [`GapDetector::tick`] at a steady cadence. It reports host suspends,
/// collector stalls, and wall-clock steps.
pub struct GapDetector {
    period_ns: u64,
    stall_threshold_ns: u64,
    suspend_threshold_ns: u64,
    last_mono: u64,
    last_suspended: Option<u64>,
    last_offset_ms: i64,
}

impl GapDetector {
    pub const SUSPEND_THRESHOLD: Duration = Duration::from_secs(2);
    pub const WALL_STEP_THRESHOLD_MS: i64 = 500;

    pub fn new(clock: &dyn Clock, period: Duration, stall_threshold: Duration) -> Self {
        let mono = clock.mono_ns();
        Self {
            period_ns: period.as_nanos() as u64,
            stall_threshold_ns: stall_threshold.as_nanos() as u64,
            suspend_threshold_ns: Self::SUSPEND_THRESHOLD.as_nanos() as u64,
            last_mono: mono,
            last_suspended: clock.suspended_ns(),
            last_offset_ms: Self::offset_ms(clock, mono),
        }
    }

    fn offset_ms(clock: &dyn Clock, mono: u64) -> i64 {
        let wall_ms = clock.wall().as_millisecond();
        wall_ms - (mono / 1_000_000) as i64 - (clock.suspended_ns().unwrap_or(0) / 1_000_000) as i64
    }

    pub fn tick(&mut self, clock: &dyn Clock) -> Vec<ClockFinding> {
        let mut out = Vec::new();
        let mono = clock.mono_ns();
        let suspended = clock.suspended_ns();
        let offset_ms = Self::offset_ms(clock, mono);
        let mono_delta = mono.saturating_sub(self.last_mono);

        let slept = match (suspended, self.last_suspended) {
            (Some(now), Some(before)) => now.saturating_sub(before),
            _ => 0,
        };
        if slept >= self.suspend_threshold_ns {
            out.push(ClockFinding::Gap { cause: GapCause::Suspend, start_ns: self.last_mono, end_ns: mono, duration_ns: slept + mono_delta });
        } else if mono_delta > self.period_ns + self.stall_threshold_ns {
            // The process was running (the monotonic clock advanced) but this
            // task was not scheduled: the collector stalled.
            out.push(ClockFinding::Gap { cause: GapCause::CollectorStall, start_ns: self.last_mono, end_ns: mono, duration_ns: mono_delta });
        }

        let step = offset_ms - self.last_offset_ms;
        if step.abs() >= Self::WALL_STEP_THRESHOLD_MS {
            // Without a suspend-aware clock pair a suspend is indistinguishable
            // from a forward wall step; report it as a gap, which is the safer reading.
            if suspended.is_none() && step > Self::SUSPEND_THRESHOLD.as_millis() as i64 && out.is_empty() {
                out.push(ClockFinding::Gap { cause: GapCause::Suspend, start_ns: self.last_mono, end_ns: mono, duration_ns: step as u64 * 1_000_000 + mono_delta });
            } else {
                out.push(ClockFinding::WallStep { delta_ms: step });
            }
        }

        self.last_mono = mono;
        self.last_suspended = suspended;
        self.last_offset_ms = offset_ms;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn detects_suspend_without_monotonic_advance() {
        let clock = ManualClock::new();
        let mut d = GapDetector::new(&clock, Duration::from_millis(250), Duration::from_secs(2));
        tokio::time::advance(Duration::from_millis(250)).await;
        assert!(d.tick(&clock).is_empty());

        clock.suspend(Duration::from_secs(600));
        tokio::time::advance(Duration::from_millis(250)).await;
        let f = d.tick(&clock);
        assert_eq!(f.len(), 1);
        match &f[0] {
            ClockFinding::Gap { cause, start_ns, end_ns, duration_ns } => {
                assert_eq!(*cause, GapCause::Suspend);
                assert_eq!(end_ns - start_ns, 250_000_000);
                assert_eq!(*duration_ns, 600_250_000_000);
            }
            other => panic!("unexpected {other:?}"),
        }
        tokio::time::advance(Duration::from_millis(250)).await;
        assert!(d.tick(&clock).is_empty(), "a suspend is reported once");
    }

    #[tokio::test(start_paused = true)]
    async fn wall_step_is_an_event_not_a_gap() {
        let clock = ManualClock::new();
        let mut d = GapDetector::new(&clock, Duration::from_millis(250), Duration::from_secs(2));
        clock.step_wall(-90_000);
        tokio::time::advance(Duration::from_millis(250)).await;
        assert_eq!(d.tick(&clock), vec![ClockFinding::WallStep { delta_ms: -90_000 }]);
    }

    #[tokio::test(start_paused = true)]
    async fn detects_collector_stall() {
        let clock = ManualClock::new();
        let mut d = GapDetector::new(&clock, Duration::from_millis(250), Duration::from_secs(2));
        tokio::time::advance(Duration::from_secs(7)).await;
        let f = d.tick(&clock);
        assert!(matches!(f[0], ClockFinding::Gap { cause: GapCause::CollectorStall, duration_ns: 7_000_000_000, .. }));
    }

    #[test]
    fn utc_string_has_millisecond_precision() {
        let ts = Timestamp::from_millisecond(1_767_225_600_412).unwrap();
        assert_eq!(utc_string(ts), "2026-01-01T00:00:00.412Z");
    }
}

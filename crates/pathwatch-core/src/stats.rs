//! Window statistics. Every number here is defined in the metric dictionary,
//! `planning/01-design-spec.md` §5. Statistics are a pure function of the
//! samples handed in; nothing here knows about clocks or rendering.

use serde::Serialize;

use crate::model::Outcome;

/// The compact, per-slot history entry statistics are computed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub seq: u64,
    /// Send time, or scheduled time for a slot that was never sent (monotonic ns).
    pub at_ns: u64,
    pub outcome: Outcome,
    pub rtt_us: Option<u32>,
    pub epoch: u32,
    /// False for samples that must not enter latency statistics, such as the
    /// first request after a forced reconnect on a reused connection.
    pub latency_eligible: bool,
}

/// A statistic that may be withheld or marked when the sample count is small.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Gated {
    Ok(f64),
    /// Shown, but flagged as resting on few samples.
    LowN(f64),
    /// Too few samples to show a number at all.
    Insufficient,
}

impl Gated {
    pub fn value(self) -> Option<f64> {
        match self {
            Gated::Ok(v) | Gated::LowN(v) => Some(v),
            Gated::Insufficient => None,
        }
    }

    fn gate(value: f64, n: u64, min_n: u64, low_below: u64) -> Self {
        if n < min_n {
            Gated::Insufficient
        } else if n < low_below {
            Gated::LowN(value)
        } else {
            Gated::Ok(value)
        }
    }
}

pub const MIN_N_CENTRAL: u64 = 5;
pub const LOW_N_BELOW: u64 = 20;
pub const MIN_N_P95: u64 = 20;
pub const MIN_N_P99: u64 = 100;
pub const MIN_N_SD: u64 = 20;
pub const MIN_PAIRS_DELTA: u64 = 10;
pub const MIN_N_RATE: u64 = 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowStats {
    pub window_ms: u64,

    // Slot accounting. Invariants (asserted in tests):
    //   sent  = pending + replied + timed_out + unreachable + service_error + protocol_error + cancelled
    //   slots = sent + local_error + skipped
    pub slots: u64,
    pub sent: u64,
    pub pending: u64,
    pub replied: u64,
    pub timed_out: u64,
    pub unreachable: u64,
    pub service_error: u64,
    pub protocol_error: u64,
    pub cancelled: u64,
    pub local_error: u64,
    pub skipped: u64,
    pub late: u64,
    pub duplicates: u64,

    /// Number of round-trip times behind the latency statistics.
    pub n: u64,
    pub min_us: Option<u32>,
    pub max_us: Option<u32>,
    pub mean_us: Gated,
    pub p50_us: Gated,
    pub p95_us: Gated,
    pub p99_us: Gated,
    pub sd_us: Gated,
    /// Mean absolute change between consecutive successful probes.
    pub delta_us: Gated,
    pub delta_pairs: u64,

    /// replied + timed_out + unreachable + service_error + protocol_error.
    pub finalised: u64,
    /// timed_out + unreachable.
    pub unanswered: u64,
    /// `unanswered / finalised`, withheld below [`MIN_N_RATE`] finalised probes.
    pub no_reply_rate: Option<f64>,
    /// service_error + protocol_error.
    pub errors: u64,
    pub error_rate: Option<f64>,

    /// sent / (window ÷ interval), at most 1.
    pub coverage: f64,
    pub unanswered_run: u64,
    pub longest_unanswered_run: u64,
}

/// Nearest-rank percentile (inverse empirical CDF, Hyndman–Fan type 1): the
/// value at 1-based rank ⌈p·n⌉ of the sorted samples, with p = `num / den`.
/// Always returns an observed value.
pub fn nearest_rank(sorted: &[u32], num: u64, den: u64) -> Option<u32> {
    let n = sorted.len() as u64;
    if n == 0 {
        return None;
    }
    let rank = (num * n).div_ceil(den).clamp(1, n);
    Some(sorted[(rank - 1) as usize])
}

impl WindowStats {
    /// Sets everything that follows from how many probes are still waiting
    /// and from the window's length — and nothing else does: `pending`,
    /// `sent`, `slots`, the window itself and its coverage. Statistics
    /// computed once can be brought up to date with this alone while the
    /// finalised samples behind them are unchanged.
    pub fn set_pending(&mut self, pending: u64, window_ns: u64, interval_ns: u64) {
        self.window_ms = window_ns / 1_000_000;
        self.pending = pending;
        self.sent = pending + self.replied + self.timed_out + self.unreachable + self.service_error + self.protocol_error + self.cancelled;
        self.slots = self.sent + self.local_error + self.skipped;
        self.coverage = coverage(self.sent, window_ns, interval_ns);
    }
}

/// The share of a window's probe slots that were sent, at most 1.
pub fn coverage(sent: u64, window_ns: u64, interval_ns: u64) -> f64 {
    let expected = (window_ns / interval_ns.max(1)).max(1);
    (sent as f64 / expected as f64).min(1.0)
}

/// Computes statistics for one stream and window.
///
/// `samples` must be the finalised samples whose send time falls in the window,
/// in `seq` order, already restricted to the current destination epoch.
/// `gap_between(a, b)` reports whether an observation gap lies between two
/// monotonic times; it breaks RTT-variation adjacency and unanswered runs.
pub fn compute(samples: &[Sample], pending: u64, late: u64, duplicates: u64, window_ns: u64, interval_ns: u64, gap_between: impl Fn(u64, u64) -> bool) -> WindowStats {
    let mut s = WindowStats {
        window_ms: window_ns / 1_000_000,
        slots: 0,
        sent: 0,
        pending,
        replied: 0,
        timed_out: 0,
        unreachable: 0,
        service_error: 0,
        protocol_error: 0,
        cancelled: 0,
        local_error: 0,
        skipped: 0,
        late,
        duplicates,
        n: 0,
        min_us: None,
        max_us: None,
        mean_us: Gated::Insufficient,
        p50_us: Gated::Insufficient,
        p95_us: Gated::Insufficient,
        p99_us: Gated::Insufficient,
        sd_us: Gated::Insufficient,
        delta_us: Gated::Insufficient,
        delta_pairs: 0,
        finalised: 0,
        unanswered: 0,
        no_reply_rate: None,
        errors: 0,
        error_rate: None,
        coverage: 0.0,
        unanswered_run: 0,
        longest_unanswered_run: 0,
    };

    let mut rtts: Vec<u32> = Vec::with_capacity(samples.len());
    let mut delta_sum: u64 = 0;
    let mut prev_success: Option<&Sample> = None;
    let mut run: u64 = 0;
    let mut prev_at: Option<u64> = None;

    for sample in samples {
        match sample.outcome {
            Outcome::Success => s.replied += 1,
            Outcome::Timeout => s.timed_out += 1,
            Outcome::Unreachable => s.unreachable += 1,
            Outcome::ServiceError => s.service_error += 1,
            Outcome::ProtocolError => s.protocol_error += 1,
            Outcome::Cancelled => s.cancelled += 1,
            Outcome::LocalError => s.local_error += 1,
            Outcome::Skipped => s.skipped += 1,
        }

        // Runs of unanswered probes are broken by a reply or by a gap.
        if let Some(p) = prev_at
            && gap_between(p, sample.at_ns)
        {
            run = 0;
        }
        prev_at = Some(sample.at_ns);
        match sample.outcome {
            Outcome::Timeout | Outcome::Unreachable => {
                run += 1;
                s.longest_unanswered_run = s.longest_unanswered_run.max(run);
            }
            Outcome::Success | Outcome::ServiceError | Outcome::ProtocolError => run = 0,
            // Slots that were never on the wire neither extend nor break a run.
            Outcome::Cancelled | Outcome::LocalError | Outcome::Skipped => {}
        }

        let rtt = match (sample.outcome, sample.rtt_us, sample.latency_eligible) {
            (Outcome::Success, Some(rtt), true) => Some(rtt),
            _ => None,
        };
        match rtt {
            Some(rtt) => {
                rtts.push(rtt);
                // Adjacent in send order: consecutive seq, both successful, no gap between.
                if let Some(prev) = prev_success
                    && prev.seq + 1 == sample.seq
                    && !gap_between(prev.at_ns, sample.at_ns)
                {
                    delta_sum += u64::from(rtt.abs_diff(prev.rtt_us.unwrap_or(rtt)));
                    s.delta_pairs += 1;
                }
                prev_success = Some(sample);
            }
            None => prev_success = None,
        }
    }
    s.unanswered_run = run;

    s.set_pending(pending, window_ns, interval_ns);
    s.finalised = s.replied + s.timed_out + s.unreachable + s.service_error + s.protocol_error;
    s.unanswered = s.timed_out + s.unreachable;
    s.errors = s.service_error + s.protocol_error;
    if s.finalised >= MIN_N_RATE {
        s.no_reply_rate = Some(s.unanswered as f64 / s.finalised as f64);
        s.error_rate = Some(s.errors as f64 / s.finalised as f64);
    }

    rtts.sort_unstable();
    s.n = rtts.len() as u64;
    if let (Some(&lo), Some(&hi)) = (rtts.first(), rtts.last()) {
        s.min_us = Some(lo);
        s.max_us = Some(hi);
        let n = s.n;
        let mean = rtts.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        s.mean_us = Gated::gate(mean, n, MIN_N_CENTRAL, LOW_N_BELOW);
        s.p50_us = Gated::gate(nearest_rank(&rtts, 50, 100).unwrap() as f64, n, MIN_N_CENTRAL, LOW_N_BELOW);
        s.p95_us = Gated::gate(nearest_rank(&rtts, 95, 100).unwrap() as f64, n, MIN_N_P95, MIN_N_P95);
        s.p99_us = Gated::gate(nearest_rank(&rtts, 99, 100).unwrap() as f64, n, MIN_N_P99, MIN_N_P99);
        if n >= 2 {
            let var = rtts.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
            s.sd_us = Gated::gate(var.sqrt(), n, MIN_N_SD, MIN_N_SD);
        }
    }
    if s.delta_pairs >= MIN_PAIRS_DELTA {
        s.delta_us = Gated::Ok(delta_sum as f64 / s.delta_pairs as f64);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(seq: u64, rtt_us: u32) -> Sample {
        Sample { seq, at_ns: seq * 1_000_000_000, outcome: Outcome::Success, rtt_us: Some(rtt_us), epoch: 1, latency_eligible: true }
    }

    fn other(seq: u64, outcome: Outcome) -> Sample {
        Sample { seq, at_ns: seq * 1_000_000_000, outcome, rtt_us: None, epoch: 1, latency_eligible: true }
    }

    fn no_gap(_: u64, _: u64) -> bool {
        false
    }

    const SEC: u64 = 1_000_000_000;

    #[test]
    fn nearest_rank_matches_definition_at_small_n() {
        let v: Vec<u32> = (1..=20).collect();
        assert_eq!(nearest_rank(&v, 50, 100), Some(10));
        assert_eq!(nearest_rank(&v, 95, 100), Some(19), "19th of 20, not the maximum");
        assert_eq!(nearest_rank(&v[..19], 95, 100), Some(19), "below 20 samples p95 is the maximum");
        assert_eq!(nearest_rank(&v, 99, 100), Some(20));
        assert_eq!(nearest_rank(&[7], 50, 100), Some(7));
        assert_eq!(nearest_rank(&[], 50, 100), None);
        let h: Vec<u32> = (1..=100).collect();
        assert_eq!(nearest_rank(&h, 99, 100), Some(99));
        assert_eq!(nearest_rank(&h, 100, 100), Some(100));
    }

    #[test]
    fn nearest_rank_agrees_with_a_naive_reference() {
        // Deterministic pseudo-random inputs; the reference counts values instead of indexing.
        let mut x: u64 = 0x9e3779b97f4a7c15;
        for n in 1..200usize {
            let mut v: Vec<u32> = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x % 100_000) as u32
                })
                .collect();
            v.sort_unstable();
            for p in [1u64, 25, 50, 75, 90, 95, 99, 100] {
                let got = nearest_rank(&v, p, 100).unwrap();
                // Smallest value with at least p% of samples at or below it.
                let reference = *v.iter().find(|&&c| (v.iter().filter(|&&o| o <= c).count() as u64) * 100 >= p * n as u64).unwrap();
                assert_eq!(got, reference, "n={n} p={p}");
            }
        }
    }

    #[test]
    fn minimum_sample_gating() {
        let mk = |n: u64| compute(&(0..n).map(|i| ok(i, 10_000 + i as u32)).collect::<Vec<_>>(), 0, 0, 0, 300 * SEC, SEC, no_gap);
        let s1 = mk(1);
        assert_eq!((s1.min_us, s1.max_us), (Some(10_000), Some(10_000)));
        assert_eq!(s1.p50_us, Gated::Insufficient);
        assert_eq!(mk(4).mean_us, Gated::Insufficient);
        assert!(matches!(mk(5).p50_us, Gated::LowN(_)));
        assert!(matches!(mk(19).p50_us, Gated::LowN(_)));
        assert_eq!(mk(19).p95_us, Gated::Insufficient);
        assert!(matches!(mk(20).p50_us, Gated::Ok(_)));
        assert_eq!(mk(20).p95_us, Gated::Ok(10_018.0));
        assert_eq!(mk(99).p99_us, Gated::Insufficient);
        assert_eq!(mk(100).p99_us, Gated::Ok(10_098.0));
        assert_eq!(mk(19).no_reply_rate, None, "rate withheld below 20 finalised");
        assert_eq!(mk(20).no_reply_rate, Some(0.0));
    }

    #[test]
    fn no_reply_denominator_excludes_non_network_outcomes() {
        let mut v: Vec<Sample> = (0..18).map(|i| ok(i, 12_000)).collect();
        v.push(other(18, Outcome::Timeout));
        v.push(other(19, Outcome::Unreachable));
        v.push(other(20, Outcome::LocalError));
        v.push(other(21, Outcome::Skipped));
        v.push(other(22, Outcome::Cancelled));
        let s = compute(&v, 2, 0, 0, 300 * SEC, SEC, no_gap);
        assert_eq!(s.finalised, 20);
        assert_eq!(s.unanswered, 2);
        assert_eq!(s.no_reply_rate, Some(0.1));
        // Counter identities.
        assert_eq!(s.sent, s.pending + s.replied + s.timed_out + s.unreachable + s.service_error + s.protocol_error + s.cancelled);
        assert_eq!(s.slots, s.sent + s.local_error + s.skipped);
        assert_eq!((s.sent, s.slots), (23, 25));
    }

    #[test]
    fn timeouts_contribute_no_rtt() {
        let v = vec![ok(0, 10_000), other(1, Outcome::Timeout), ok(2, 30_000)];
        let s = compute(&v, 0, 0, 0, 300 * SEC, SEC, no_gap);
        assert_eq!(s.n, 2);
        assert_eq!((s.min_us, s.max_us), (Some(10_000), Some(30_000)));
    }

    #[test]
    fn rtt_variation_needs_adjacent_successes() {
        // 12 consecutive successes alternating 10 ms / 14 ms: 11 pairs, each |Δ| = 4 ms.
        let v: Vec<Sample> = (0..12).map(|i| ok(i, if i % 2 == 0 { 10_000 } else { 14_000 })).collect();
        let s = compute(&v, 0, 0, 0, 300 * SEC, SEC, no_gap);
        assert_eq!(s.delta_pairs, 11);
        assert_eq!(s.delta_us, Gated::Ok(4_000.0));

        // A timeout in the middle removes the two pairs that touch it.
        let mut w = v.clone();
        w[5] = other(5, Outcome::Timeout);
        assert_eq!(compute(&w, 0, 0, 0, 300 * SEC, SEC, no_gap).delta_pairs, 9);

        // A skipped slot (missing seq) also breaks adjacency.
        let mut x = v.clone();
        x.remove(5);
        assert_eq!(compute(&x, 0, 0, 0, 300 * SEC, SEC, no_gap).delta_pairs, 9);

        // A gap between seq 5 and 6 breaks exactly one pair.
        let gap = |a: u64, b: u64| a <= 5 * SEC && b >= 6 * SEC;
        assert_eq!(compute(&v, 0, 0, 0, 300 * SEC, SEC, gap).delta_pairs, 10);

        // Fewer than ten pairs: withheld.
        assert_eq!(compute(&v[..10], 0, 0, 0, 300 * SEC, SEC, no_gap).delta_us, Gated::Insufficient);
    }

    #[test]
    fn unanswered_runs() {
        let v =
            vec![ok(0, 1), other(1, Outcome::Timeout), other(2, Outcome::Timeout), other(3, Outcome::Skipped), other(4, Outcome::Timeout), ok(5, 1), other(6, Outcome::Timeout)];
        let s = compute(&v, 0, 0, 0, 300 * SEC, SEC, no_gap);
        assert_eq!(s.longest_unanswered_run, 3, "a skipped slot neither extends nor breaks a run");
        assert_eq!(s.unanswered_run, 1);
    }

    #[test]
    fn coverage_reflects_a_young_session() {
        let v: Vec<Sample> = (0..60).map(|i| ok(i, 1_000)).collect();
        let s = compute(&v, 0, 0, 0, 300 * SEC, SEC, no_gap);
        assert!((s.coverage - 0.2).abs() < 1e-9);
    }

    #[test]
    fn ineligible_samples_are_counted_but_not_timed() {
        let mut v: Vec<Sample> = (0..5).map(|i| ok(i, 20_000)).collect();
        v[0].latency_eligible = false;
        v[0].rtt_us = Some(90_000);
        let s = compute(&v, 0, 0, 0, 300 * SEC, SEC, no_gap);
        assert_eq!(s.replied, 5);
        assert_eq!(s.n, 4);
        assert_eq!(s.max_us, Some(20_000));
    }
}

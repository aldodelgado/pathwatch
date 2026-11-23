//! `sample --assert`: the opt-in way to turn what was measured into an exit
//! status, for scripted health checks.
//!
//! An assertion is `[<streams>:] <metric> <comparison> <value>`, for example
//! `no-reply <= 1%`, `cloudflare: p50 < 40ms`, or `*/dns-udp/4: replied >= 3`.
//! It is checked against every stream it selects, at the end of the sample,
//! over the same window as the summary.
//!
//! Two rules keep a passing check honest. An assertion that cannot be
//! evaluated — nothing finalised, too few samples for the statistic — does
//! not pass: it is reported as having no data, and the exit status is the
//! same as for one that is false. And a number is never asserted on that the
//! summary would withhold: the sample-count gates are the same ones.

use pathwatch_core::stats::{Gated, MIN_N_CENTRAL, MIN_N_P95, MIN_N_P99, MIN_N_RATE, WindowStats};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Sent,
    Replied,
    NoReply,
    ErrorResponses,
    LocalErrors,
    Skipped,
    Min,
    Mean,
    P50,
    P95,
    P99,
    Max,
}

impl Metric {
    const ALL: [(&'static str, Metric); 12] = [
        ("sent", Metric::Sent),
        ("replied", Metric::Replied),
        ("no-reply", Metric::NoReply),
        ("error-responses", Metric::ErrorResponses),
        ("local-errors", Metric::LocalErrors),
        ("skipped", Metric::Skipped),
        ("min", Metric::Min),
        ("mean", Metric::Mean),
        ("p50", Metric::P50),
        ("p95", Metric::P95),
        ("p99", Metric::P99),
        ("max", Metric::Max),
    ];

    fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, m)| *m == self).map_or("?", |(n, _)| n)
    }

    fn is_time(self) -> bool {
        matches!(self, Metric::Min | Metric::Mean | Metric::P50 | Metric::P95 | Metric::P99 | Metric::Max)
    }

    /// Whether the metric can also be asserted as a share of finalised probes.
    fn has_rate(self) -> bool {
        matches!(self, Metric::NoReply | Metric::ErrorResponses)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

impl Op {
    fn symbol(self) -> &'static str {
        match self {
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Eq => "==",
        }
    }

    fn holds(self, measured: f64, asserted: f64) -> bool {
        match self {
            Op::Lt => measured < asserted,
            Op::Le => measured <= asserted,
            Op::Gt => measured > asserted,
            Op::Ge => measured >= asserted,
            Op::Eq => measured == asserted,
        }
    }
}

/// What the asserted number is a number of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Count,
    Percent,
    Millis,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assertion {
    /// As written, for messages.
    pub text: String,
    /// A pattern over stream ids; `None` selects every stream that ran.
    pub selector: Option<String>,
    pub metric: Metric,
    pub op: Op,
    pub value: f64,
    pub unit: Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// It held; the text states what was measured.
    Held(String),
    /// It was evaluated and is false.
    Failed(String),
    /// It could not be evaluated; the text says why.
    NoData(String),
}

#[cfg(test)]
impl Verdict {
    fn held(&self) -> bool {
        matches!(self, Verdict::Held(_))
    }
}

const SYNTAX: &str = "write it as [<streams>:] <metric> <comparison> <value>, for example \"no-reply <= 1%\" or \"cloudflare: p50 < 40ms\"";

/// `*` matches any run of characters; everything else matches itself.
fn glob(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, rest)) => text.strip_prefix(head).is_some_and(|t| (0..=t.len()).filter(|i| t.is_char_boundary(*i)).any(|i| glob(rest, &t[i..]))),
    }
}

impl Assertion {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (selector, rest) = match text.split_once(':') {
            Some((s, rest)) => (Some(s.trim()), rest),
            None => (None, text),
        };
        if selector.is_some_and(str::is_empty) {
            return Err(format!("nothing before the colon; {SYNTAX}"));
        }
        // Two-character comparisons first, so "<=" is not read as "<".
        let found = [("<=", Op::Le), (">=", Op::Ge), ("==", Op::Eq), ("<", Op::Lt), (">", Op::Gt)]
            .into_iter()
            .filter_map(|(sym, op)| rest.find(sym).map(|at| (at, sym.len(), op)))
            .min_by_key(|(at, len, _)| (*at, std::cmp::Reverse(*len)));
        let Some((at, len, op)) = found else { return Err(format!("no comparison (<, <=, >, >=, ==); {SYNTAX}")) };
        let (name, value) = (rest[..at].trim(), rest[at + len..].trim());
        let Some(metric) = Metric::ALL.iter().find(|(n, _)| *n == name).map(|(_, m)| *m) else {
            let known: Vec<&str> = Metric::ALL.iter().map(|(n, _)| *n).collect();
            return Err(format!("unknown metric {name:?} (use {})", known.join(", ")));
        };
        let (number, unit) = if let Some(n) = value.strip_suffix('%') {
            (n, Unit::Percent)
        } else if let Some(n) = value.strip_suffix("ms") {
            (n, Unit::Millis)
        } else {
            (value, Unit::Count)
        };
        let value: f64 = number.trim().parse().map_err(|_| format!("{value:?} is not a number, a percentage (\"1%\"), or a time (\"40ms\")"))?;
        if !value.is_finite() || value < 0.0 {
            return Err(format!("{number:?} is not a usable value"));
        }
        match (metric, unit) {
            (m, Unit::Millis) if m.is_time() => {}
            (m, _) if m.is_time() => return Err(format!("{name} is a time: give the value in ms, for example \"{name} {} 40ms\"", op.symbol())),
            (_, Unit::Millis) => return Err(format!("{name} is a count, not a time")),
            (m, Unit::Percent) if !m.has_rate() => return Err(format!("{name} is a count; only no-reply and error-responses can be asserted as a percentage")),
            (_, Unit::Count) if value.fract() != 0.0 => return Err(format!("{name} without a % sign is a count of probes, and {number} is not a whole number")),
            _ => {}
        }
        Ok(Self { text: text.trim().to_string(), selector: selector.map(str::to_string), metric, op, value, unit })
    }

    /// Whether the assertion is about this stream. A pattern that stops at
    /// the end of a part of the id covers everything below it, so
    /// `cloudflare` selects every Cloudflare stream.
    pub fn selects(&self, stream: &str) -> bool {
        match &self.selector {
            None => true,
            Some(p) => glob(p, stream) || glob(&format!("{p}/*"), stream),
        }
    }

    fn asserted(&self) -> String {
        let v = self.value;
        match self.unit {
            Unit::Count => format!("{} {v}", self.op.symbol()),
            Unit::Percent => format!("{} {v} %", self.op.symbol()),
            Unit::Millis => format!("{} {v} ms", self.op.symbol()),
        }
    }

    /// Checks the assertion against one stream's statistics.
    pub fn judge(&self, s: &WindowStats) -> Verdict {
        let name = self.metric.name();
        let (measured, shown): (f64, String) = match (self.metric, self.unit) {
            (Metric::Sent, _) => (s.sent as f64, format!("{}", s.sent)),
            (Metric::Replied, _) => (s.replied as f64, format!("{} of {} sent", s.replied, s.sent)),
            (Metric::LocalErrors, _) => (s.local_error as f64, format!("{} of {} slots", s.local_error, s.slots)),
            (Metric::Skipped, _) => (s.skipped as f64, format!("{} of {} slots", s.skipped, s.slots)),
            (Metric::NoReply | Metric::ErrorResponses, unit) => {
                let (k, rate) = if self.metric == Metric::NoReply { (s.unanswered, s.no_reply_rate) } else { (s.errors, s.error_rate) };
                if s.finalised == 0 {
                    return Verdict::NoData(format!("{name}: no probe was finalised ({} sent, {} local errors, {} skipped)", s.sent, s.local_error, s.skipped));
                }
                match (unit, rate) {
                    (Unit::Percent, Some(r)) => (r * 100.0, format!("{:.1} % ({k} of {})", r * 100.0, s.finalised)),
                    (Unit::Percent, None) => {
                        return Verdict::NoData(format!(
                            "{name}: {k} of {} finalised probes; a percentage needs {MIN_N_RATE} — sample for longer, or assert a count (the same metric without the % sign)",
                            s.finalised
                        ));
                    }
                    _ => (k as f64, format!("{k} of {}", s.finalised)),
                }
            }
            (metric, _) => {
                let (value, needs) = match metric {
                    Metric::Min => (s.min_us.map(f64::from), 1),
                    Metric::Max => (s.max_us.map(f64::from), 1),
                    Metric::Mean => (s.mean_us.value(), MIN_N_CENTRAL),
                    Metric::P50 => (s.p50_us.value(), MIN_N_CENTRAL),
                    Metric::P95 => (s.p95_us.value(), MIN_N_P95),
                    _ => (s.p99_us.value(), MIN_N_P99),
                };
                let Some(us) = value else {
                    return Verdict::NoData(format!("{name}: {} round-trip time{} in the window; {name} needs {needs}", s.n, if s.n == 1 { "" } else { "s" }));
                };
                let low = matches!((metric, s.p50_us), (Metric::Mean | Metric::P50, Gated::LowN(_)));
                (us / 1000.0, format!("{:.1} ms (of {} round trips{})", us / 1000.0, s.n, if low { ", a low count" } else { "" }))
            }
        };
        if self.op.holds(measured, self.value) {
            Verdict::Held(format!("{name} {shown} {}", self.asserted()))
        } else {
            Verdict::Failed(format!("{name} {shown}, asserted {}", self.asserted()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathwatch_core::model::Outcome;
    use pathwatch_core::stats::{Sample, compute};

    /// `answered` round trips of 10, 11, 12… ms, then `unanswered` timeouts.
    fn stats(answered: u64, unanswered: u64) -> WindowStats {
        let samples: Vec<Sample> = (0..answered + unanswered)
            .map(|seq| {
                let ok = seq < answered;
                Sample {
                    seq,
                    at_ns: seq * 1_000_000_000,
                    outcome: if ok { Outcome::Success } else { Outcome::Timeout },
                    rtt_us: ok.then_some(10_000 + seq as u32 * 1_000),
                    epoch: 1,
                    latency_eligible: true,
                }
            })
            .collect();
        compute(&samples, 0, 0, 0, 300_000_000_000, 1_000_000_000, |_, _| false)
    }

    fn a(text: &str) -> Assertion {
        Assertion::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn assertions_parse_with_or_without_a_selector_and_with_the_right_unit() {
        let x = a("no-reply <= 1%");
        assert_eq!((x.selector.clone(), x.metric, x.op, x.value, x.unit), (None, Metric::NoReply, Op::Le, 1.0, Unit::Percent));
        let x = a(" cloudflare/*/icmp/4 :p50<40 ms ");
        assert_eq!((x.selector.as_deref(), x.metric, x.op, x.value, x.unit), (Some("cloudflare/*/icmp/4"), Metric::P50, Op::Lt, 40.0, Unit::Millis));
        assert_eq!(a("replied >= 3").unit, Unit::Count);
        assert_eq!(a("no-reply == 0").op, Op::Eq);
        assert_eq!(a("max > 2.5ms").value, 2.5);

        for (bad, says) in [
            ("no-reply", "no comparison"),
            ("loss <= 1%", "unknown metric"),
            ("p50 < 40", "give the value in ms"),
            ("p50 < 40%", "give the value in ms"),
            ("sent >= 10ms", "a count, not a time"),
            ("replied >= 50%", "only no-reply and error-responses"),
            ("no-reply <= 1.5", "not a whole number"),
            ("no-reply <= many", "is not a number"),
            ("no-reply <= -1", "not a usable value"),
            (": p50 < 40ms", "nothing before the colon"),
            ("p50 != 40ms", "no comparison"),
            ("p50 = 40ms", "no comparison"),
        ] {
            let e = Assertion::parse(bad).expect_err(bad);
            assert!(e.contains(says), "{bad}: {e}");
        }
    }

    #[test]
    fn a_selector_is_a_pattern_over_the_stream_id_and_covers_what_is_below_it() {
        let id = "cloudflare/resolver-v4/icmp/4";
        for yes in ["cloudflare", "cloudflare/resolver-v4", "cloudflare/*/icmp/4", "*/icmp/4", "*/icmp/*", "*", "cloudflare/resolver-v4/icmp/4", "cloud*"] {
            assert!(a(&format!("{yes}: sent >= 1")).selects(id), "{yes}");
        }
        for no in ["google", "cloud", "*/icmp/6", "*/dns-udp/*", "cloudflare/resolver-v4/icmp/4/fresh", "resolver-v4"] {
            assert!(!a(&format!("{no}: sent >= 1")).selects(id), "{no}");
        }
        assert!(a("sent >= 1").selects(id), "no selector is every stream");
        // The fresh cohort is below its stream's id, and can be picked out.
        assert!(a("*/fresh: sent >= 1").selects("cloudflare/doh/doh/4/fresh") && !a("*/fresh: sent >= 1").selects("cloudflare/doh/doh/4"));
    }

    #[test]
    fn counts_and_times_are_judged_on_what_was_measured() {
        let s = stats(28, 2);
        assert_eq!(a("sent >= 30").judge(&s), Verdict::Held("sent 30 >= 30".into()));
        assert_eq!(a("replied >= 29").judge(&s), Verdict::Failed("replied 28 of 30 sent, asserted >= 29".into()));
        assert_eq!(a("no-reply <= 2").judge(&s), Verdict::Held("no-reply 2 of 30 <= 2".into()));
        assert_eq!(a("no-reply <= 5%").judge(&s), Verdict::Failed("no-reply 6.7 % (2 of 30), asserted <= 5 %".into()));
        assert_eq!(a("no-reply <= 7%").judge(&s), Verdict::Held("no-reply 6.7 % (2 of 30) <= 7 %".into()));
        assert_eq!(a("error-responses == 0").judge(&s), Verdict::Held("error-responses 0 of 30 == 0".into()));
        // 10..=37 ms: the median by nearest rank is the 14th value.
        assert_eq!(a("p50 < 24ms").judge(&s), Verdict::Held("p50 23.0 ms (of 28 round trips) < 24 ms".into()));
        assert_eq!(a("p50 < 23ms").judge(&s), Verdict::Failed("p50 23.0 ms (of 28 round trips), asserted < 23 ms".into()));
        assert!(a("max <= 37ms").judge(&s).held() && a("min >= 10ms").judge(&s).held() && a("p95 <= 36ms").judge(&s).held());
        assert!(a("local-errors == 0").judge(&s).held() && a("skipped == 0").judge(&s).held());
    }

    #[test]
    fn what_cannot_be_evaluated_does_not_pass() {
        // Nothing finalised: neither "no losses" nor any time can be claimed.
        let none = stats(0, 0);
        for text in ["no-reply == 0", "no-reply <= 100%", "error-responses == 0", "p50 < 1000ms", "max < 1000ms"] {
            assert!(matches!(a(text).judge(&none), Verdict::NoData(_)), "{text}: {:?}", a(text).judge(&none));
        }
        // What was sent and answered is always known, and here it is nothing.
        assert_eq!(a("replied >= 1").judge(&none), Verdict::Failed("replied 0 of 0 sent, asserted >= 1".into()));
        assert!(a("sent == 0").judge(&none).held());

        // Six probes: counts are judged, a percentage is not, and the
        // message says how to ask instead.
        let few = stats(6, 0);
        assert!(a("no-reply == 0").judge(&few).held());
        let Verdict::NoData(why) = a("no-reply <= 1%").judge(&few) else { panic!("a rate on six probes") };
        assert!(why.contains("0 of 6 finalised probes; a percentage needs 20") && why.contains("assert a count"), "{why}");
        // The same gates as the summary: a median from 5, a p95 from 20, a p99 from 100.
        assert_eq!(a("p50 < 100ms").judge(&few), Verdict::Held("p50 12.0 ms (of 6 round trips, a low count) < 100 ms".into()));
        assert!(matches!(a("p50 < 100ms").judge(&stats(4, 0)), Verdict::NoData(_)));
        assert!(matches!(a("p95 < 100ms").judge(&stats(19, 0)), Verdict::NoData(_)) && a("p95 < 100ms").judge(&stats(20, 0)).held());
        assert!(matches!(a("p99 < 1000ms").judge(&stats(99, 0)), Verdict::NoData(_)) && a("p99 < 1000ms").judge(&stats(100, 0)).held());
        // Every probe unanswered: the rate is known, the times are not.
        let dead = stats(0, 30);
        assert_eq!(a("no-reply <= 1%").judge(&dead), Verdict::Failed("no-reply 100.0 % (30 of 30), asserted <= 1 %".into()));
        let Verdict::NoData(why) = a("p50 < 100ms").judge(&dead) else { panic!("a median of nothing") };
        assert_eq!(why, "p50: 0 round-trip times in the window; p50 needs 5");
    }
}

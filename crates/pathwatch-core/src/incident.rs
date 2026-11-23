//! Incident rules for a single stream, with hysteresis and cooldown.
//! See `planning/01-design-spec.md` §8.
//!
//! Every rule opens on one condition and closes on a stricter one, so a stream
//! at the edge does not flicker; a rule that has just closed cannot reopen
//! during the cooldown; and a stream that keeps cycling is reported once as
//! flapping. Events state counts, rates, and durations — never a cause.

use std::collections::VecDeque;

use crate::config::Incidents;
use crate::model::{Flag, Observation, Outcome, Protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// Consecutive probes unanswered.
    NoReplyRun = 0,
    /// The share of unanswered probes over the last minute.
    ElevatedNoReply = 1,
    /// The recent median against the preceding baseline.
    LatencyShift = 2,
    /// Error responses among the last ten DNS transactions.
    ErrorResponses = 3,
    /// Sends failing on this host.
    LocalError = 4,
}

#[derive(Debug, Clone)]
pub struct IncidentConfig {
    pub cooldown_ns: u64,
    pub run_open: u32,
    pub run_close: u32,
    pub dns_run_open: u32,
    pub dns_run_close: u32,
    pub elevated_window_ns: u64,
    pub elevated_open: f64,
    pub elevated_close: f64,
    pub elevated_hold_ns: u64,
    pub elevated_min: u32,
    pub shift_recent_ns: u64,
    pub shift_baseline_ns: u64,
    pub shift_open_ratio: f64,
    pub shift_open_min_delta_us: f64,
    pub shift_close_ratio: f64,
    pub shift_min_recent: usize,
    pub shift_min_baseline: usize,
    pub dns_errors_in_last_10: u32,
    pub flapping_cycles: usize,
    pub flapping_window_ns: u64,
    pub flapping_quiet_ns: u64,
}

const SEC: u64 = 1_000_000_000;

impl Default for IncidentConfig {
    fn default() -> Self {
        Self {
            cooldown_ns: 60 * SEC,
            run_open: 5,
            run_close: 10,
            dns_run_open: 3,
            dns_run_close: 3,
            elevated_window_ns: 60 * SEC,
            elevated_open: 0.05,
            elevated_close: 0.01,
            elevated_hold_ns: 30 * SEC,
            elevated_min: 30,
            shift_recent_ns: 60 * SEC,
            shift_baseline_ns: 900 * SEC,
            shift_open_ratio: 1.5,
            shift_open_min_delta_us: 10_000.0,
            shift_close_ratio: 1.2,
            shift_min_recent: 30,
            shift_min_baseline: 300,
            dns_errors_in_last_10: 3,
            flapping_cycles: 3,
            flapping_window_ns: 600 * SEC,
            flapping_quiet_ns: 300 * SEC,
        }
    }
}

impl From<&Incidents> for IncidentConfig {
    fn from(c: &Incidents) -> Self {
        let ns = |d: std::time::Duration| d.as_nanos() as u64;
        Self {
            cooldown_ns: ns(c.cooldown.0),
            run_open: c.no_reply_run.open_after_consecutive,
            run_close: c.no_reply_run.close_after_consecutive,
            dns_run_open: c.dns.open_after_consecutive_no_response,
            dns_run_close: c.dns.close_after_consecutive_success,
            elevated_window_ns: ns(c.elevated_no_reply.window.0),
            elevated_open: c.elevated_no_reply.open_at,
            elevated_close: c.elevated_no_reply.close_at,
            elevated_hold_ns: ns(c.elevated_no_reply.close_hold.0),
            elevated_min: c.elevated_no_reply.min_finalised,
            shift_recent_ns: ns(c.latency_shift.recent.0),
            shift_baseline_ns: ns(c.latency_shift.baseline.0),
            shift_open_ratio: c.latency_shift.open_ratio,
            shift_open_min_delta_us: c.latency_shift.open_min_delta.0.as_micros() as f64,
            shift_close_ratio: c.latency_shift.close_ratio,
            shift_min_recent: c.latency_shift.min_recent as usize,
            shift_min_baseline: c.latency_shift.min_baseline as usize,
            dns_errors_in_last_10: c.dns.error_responses_in_last_10,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentKind {
    Opened,
    Closed,
    /// The stream keeps opening and closing; individual changes are no longer listed.
    Flapping,
    FlappingEnded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentEvent {
    pub at_ns: u64,
    pub utc: Option<String>,
    pub stream: usize,
    pub rule: Rule,
    pub kind: IncidentKind,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
struct RuleState {
    open_since: Option<u64>,
    last_closed_ns: Option<u64>,
    /// Open times, for counting cycles.
    opens: VecDeque<u64>,
}

#[derive(Debug, Clone)]
struct StreamState {
    is_dns: bool,
    rules: [RuleState; 5],
    epoch: u32,
    // no-reply run
    run: u32,
    run_first_ns: u64,
    run_unanswered: u32,
    ok_run: u32,
    ok_first_ns: u64,
    // elevated no-reply: (send time, unanswered)
    recent: VecDeque<(u64, bool)>,
    calm_since: Option<u64>,
    // latency shift: (send time, round trip)
    rtts: VecDeque<(u64, u32)>,
    frozen_baseline: Option<f64>,
    settled_since: Option<u64>,
    // error responses
    last10: VecDeque<bool>,
    clean_streak: u32,
    // flapping
    flapping: bool,
    last_change_ns: u64,
    /// When the latest failed send was attempted.
    last_local_ns: u64,
}

pub struct IncidentTracker {
    config: IncidentConfig,
    streams: Vec<StreamState>,
    events: Vec<IncidentEvent>,
    /// Every open and close, including those not listed while flapping, so the
    /// state at any past moment can be recovered.
    transitions: Vec<(u64, usize, Rule, bool)>,
    /// A session time and the wall-clock time it corresponds to, from the
    /// latest observation that carries both. Lets an event about something
    /// that was never sent (and so has no send time) still be dated.
    anchor: Option<(u64, jiff::Timestamp)>,
}

fn median(values: impl Iterator<Item = u32>) -> Option<f64> {
    let mut v: Vec<u32> = values.collect();
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(f64::from(v[(v.len() - 1) / 2]))
}

impl IncidentTracker {
    pub fn new(protocols: impl IntoIterator<Item = Protocol>) -> Self {
        Self::with_config(protocols, IncidentConfig::default())
    }

    pub fn with_config(protocols: impl IntoIterator<Item = Protocol>, config: IncidentConfig) -> Self {
        let mut tracker = Self { config, streams: Vec::new(), events: Vec::new(), transitions: Vec::new(), anchor: None };
        tracker.add_streams(protocols);
        tracker
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    /// Starts tracking more streams, after those already tracked: for
    /// streams a configuration reload added.
    pub fn add_streams(&mut self, protocols: impl IntoIterator<Item = Protocol>) {
        self.streams.extend(protocols.into_iter().map(|p| StreamState {
            is_dns: p.is_dns(),
            rules: Default::default(),
            epoch: 0,
            run: 0,
            run_first_ns: 0,
            run_unanswered: 0,
            ok_run: 0,
            ok_first_ns: 0,
            recent: VecDeque::new(),
            calm_since: None,
            rtts: VecDeque::new(),
            frozen_baseline: None,
            settled_since: None,
            last10: VecDeque::new(),
            clean_streak: 0,
            flapping: false,
            last_change_ns: 0,
            last_local_ns: 0,
        }));
    }

    /// Whether `rule` is open on the stream right now.
    pub fn is_open(&self, stream: usize, rule: Rule) -> bool {
        self.streams.get(stream).is_some_and(|s| s.rules[rule as usize].open_since.is_some())
    }

    /// Whether `rule` was open on the stream at session time `at_ns` — what a
    /// view frozen at that moment must show.
    pub fn is_open_at(&self, stream: usize, rule: Rule, at_ns: u64) -> bool {
        self.transitions.iter().rev().find(|(t, s, r, _)| *s == stream && *r == rule && *t <= at_ns).is_some_and(|(_, _, _, opened)| *opened)
    }

    pub fn events(&self) -> &[IncidentEvent] {
        &self.events
    }

    /// An observation gap restarts run counters and the short windows: probes
    /// on either side of it are not consecutive, and a gap never opens anything.
    pub fn on_gap(&mut self) {
        for s in &mut self.streams {
            s.run = 0;
            s.ok_run = 0;
            s.recent.clear();
            s.calm_since = None;
        }
    }

    fn open(&mut self, stream: usize, rule: Rule, at: u64, utc: &Option<String>, text: String) -> bool {
        let cfg = &self.config;
        let s = &mut self.streams[stream];
        let st = &mut s.rules[rule as usize];
        if st.open_since.is_some() || st.last_closed_ns.is_some_and(|c| at < c + cfg.cooldown_ns) {
            return false;
        }
        st.open_since = Some(at);
        st.opens.push_back(at);
        while st.opens.front().is_some_and(|t| at.saturating_sub(*t) > cfg.flapping_window_ns) {
            st.opens.pop_front();
        }
        let cycles = st.opens.len();
        s.last_change_ns = at;
        self.transitions.push((at, stream, rule, true));
        if s.flapping {
            return true;
        }
        if cycles > cfg.flapping_cycles {
            s.flapping = true;
            let text = format!(
                "flapping: {cycles} open/close cycles in {} minutes; further changes are not listed until it has been quiet for {} minutes",
                cfg.flapping_window_ns / (60 * SEC),
                cfg.flapping_quiet_ns / (60 * SEC)
            );
            self.events.push(IncidentEvent { at_ns: at, utc: utc.clone(), stream, rule, kind: IncidentKind::Flapping, text });
        } else {
            self.events.push(IncidentEvent { at_ns: at, utc: utc.clone(), stream, rule, kind: IncidentKind::Opened, text });
        }
        true
    }

    fn close(&mut self, stream: usize, rule: Rule, at: u64, utc: &Option<String>, text: String) {
        let s = &mut self.streams[stream];
        let st = &mut s.rules[rule as usize];
        if st.open_since.take().is_none() {
            return;
        }
        st.last_closed_ns = Some(at);
        s.last_change_ns = at;
        self.transitions.push((at, stream, rule, false));
        if !s.flapping {
            self.events.push(IncidentEvent { at_ns: at, utc: utc.clone(), stream, rule, kind: IncidentKind::Closed, text });
        }
    }

    /// The wall-clock time of a session time, once any probe has been sent.
    /// Exact to the clocks' agreement; across a host suspend it is only as
    /// good as the nearest observation.
    pub fn utc_at(&self, at_ns: u64) -> Option<String> {
        let (anchor_ns, wall) = self.anchor?;
        let offset = jiff::SignedDuration::from_nanos(at_ns as i64 - anchor_ns as i64);
        Some(crate::clock::utc_string(wall.checked_add(offset).ok()?))
    }

    /// Feeds one finalised first-attempt observation.
    pub fn on_observation(&mut self, stream: usize, o: &Observation) {
        if o.attempt != 1 || stream >= self.streams.len() {
            return;
        }
        let at = o.at_ns();
        if let (Some(sent_ns), Some(Ok(wall))) = (o.sent_ns, o.sent_utc.as_deref().map(str::parse::<jiff::Timestamp>)) {
            self.anchor = Some((sent_ns, wall));
        }
        let utc = o.sent_utc.clone();
        let cfg = self.config.clone();

        {
            let s = &mut self.streams[stream];
            // A new destination is a new baseline; never compare across epochs.
            if o.epoch != s.epoch {
                s.epoch = o.epoch;
                s.rtts.clear();
                s.frozen_baseline = None;
                s.settled_since = None;
            }
            if s.flapping && at.saturating_sub(s.last_change_ns) >= cfg.flapping_quiet_ns {
                s.flapping = false;
                self.events.push(IncidentEvent {
                    at_ns: at,
                    utc: utc.clone(),
                    stream,
                    rule: Rule::NoReplyRun,
                    kind: IncidentKind::FlappingEnded,
                    text: format!("flapping ended: no change for {} minutes", cfg.flapping_quiet_ns / (60 * SEC)),
                });
            }
        }

        // Sends failing on this host say nothing about the target.
        if o.outcome == Outcome::LocalError {
            let detail = o.error.as_ref().and_then(|e| e.detail.clone()).unwrap_or_else(|| "send failed".into());
            self.streams[stream].last_local_ns = at;
            self.open(stream, Rule::LocalError, at, &utc, format!("sends are failing on this host: {detail} — nothing reached the network"));
            return;
        }
        // Only a send made after the latest failure shows that sends work
        // again. A probe that left earlier and is only now timing out does not.
        if o.outcome.was_sent() && at > self.streams[stream].last_local_ns && self.is_open(stream, Rule::LocalError) {
            self.close(stream, Rule::LocalError, at, &utc, "sends succeed again".into());
        }

        let unanswered = matches!(o.outcome, Outcome::Timeout | Outcome::Unreachable);
        let replied = matches!(o.outcome, Outcome::Success | Outcome::ServiceError | Outcome::ProtocolError);
        if !unanswered && !replied {
            return; // skipped or cancelled: never on the wire, or not finalised by the network
        }
        let is_dns = self.streams[stream].is_dns;
        let (run_open, run_close) = if is_dns { (cfg.dns_run_open, cfg.dns_run_close) } else { (cfg.run_open, cfg.run_close) };

        // ── consecutive unanswered probes ──
        if unanswered {
            let s = &mut self.streams[stream];
            if s.run == 0 {
                s.run_first_ns = at;
            }
            s.run += 1;
            s.ok_run = 0;
            if s.rules[Rule::NoReplyRun as usize].open_since.is_some() {
                s.run_unanswered += 1;
            } else if s.run >= run_open {
                let (run, first) = (s.run, s.run_first_ns);
                if self.open(stream, Rule::NoReplyRun, at, &utc, format!("no-reply run opened: {run} consecutive probes unanswered")) {
                    let s = &mut self.streams[stream];
                    s.rules[Rule::NoReplyRun as usize].open_since = Some(first);
                    s.run_unanswered = run;
                }
            }
        } else {
            // Any reply — including an error response — ends a run of silence.
            let s = &mut self.streams[stream];
            s.run = 0;
            if let Some(first) = s.rules[Rule::NoReplyRun as usize].open_since {
                if s.ok_run == 0 {
                    s.ok_first_ns = at;
                }
                s.ok_run += 1;
                if s.ok_run >= run_close {
                    let secs = s.ok_first_ns.saturating_sub(first) as f64 / 1e9;
                    let n = s.run_unanswered;
                    s.ok_run = 0;
                    self.close(stream, Rule::NoReplyRun, at, &utc, format!("replies resumed after {secs:.0} s; {n} probes went unanswered during the run"));
                }
            }
        }

        // ── share of unanswered probes over the last minute ──
        {
            let s = &mut self.streams[stream];
            s.recent.push_back((at, unanswered));
            let newest = s.recent.iter().map(|(t, _)| *t).max().unwrap_or(at);
            while s.recent.front().is_some_and(|(t, _)| newest.saturating_sub(*t) >= cfg.elevated_window_ns) {
                s.recent.pop_front();
            }
            let n = s.recent.len() as u32;
            let k = s.recent.iter().filter(|(_, u)| *u).count() as u32;
            if n >= cfg.elevated_min {
                let rate = f64::from(k) / f64::from(n);
                let open = s.rules[Rule::ElevatedNoReply as usize].open_since.is_some();
                let run_open_now = s.rules[Rule::NoReplyRun as usize].open_since.is_some();
                if !open && rate >= cfg.elevated_open && !run_open_now {
                    self.open(stream, Rule::ElevatedNoReply, at, &utc, format!("no-reply rate {:.0} % over the last minute ({k} of {n} probes)", rate * 100.0));
                } else if open {
                    if rate <= cfg.elevated_close {
                        let since = *s.calm_since.get_or_insert(at);
                        if at.saturating_sub(since) >= cfg.elevated_hold_ns {
                            s.calm_since = None;
                            self.close(stream, Rule::ElevatedNoReply, at, &utc, format!("no-reply rate back to {:.0} % over the last minute", rate * 100.0));
                        }
                    } else {
                        s.calm_since = None;
                    }
                }
            }
        }

        // ── recent median against the preceding baseline ──
        if o.outcome == Outcome::Success
            && !o.has_flag(Flag::AfterReconnect)
            && let Some(rtt) = o.rtt_us
        {
            let s = &mut self.streams[stream];
            s.rtts.push_back((at, rtt));
            let newest = s.rtts.iter().map(|(t, _)| *t).max().unwrap_or(at);
            while s.rtts.front().is_some_and(|(t, _)| newest.saturating_sub(*t) >= cfg.shift_recent_ns + cfg.shift_baseline_ns) {
                s.rtts.pop_front();
            }
            let cut = newest.saturating_sub(cfg.shift_recent_ns);
            let recent_n = s.rtts.iter().filter(|(t, _)| *t > cut).count();
            let base_n = s.rtts.len() - recent_n;
            let open = s.rules[Rule::LatencyShift as usize].open_since.is_some();
            // The baseline freezes while the incident is open, so a long
            // slowdown cannot become its own baseline.
            let baseline = if open {
                s.frozen_baseline
            } else if base_n >= cfg.shift_min_baseline {
                median(s.rtts.iter().filter(|(t, _)| *t <= cut).map(|(_, r)| *r))
            } else {
                None
            };
            if let (Some(b), true) = (baseline, recent_n >= cfg.shift_min_recent) {
                let r = median(s.rtts.iter().filter(|(t, _)| *t > cut).map(|(_, r)| *r)).unwrap_or(b);
                if !open && r > (b * cfg.shift_open_ratio).max(b + cfg.shift_open_min_delta_us) {
                    s.frozen_baseline = Some(b);
                    s.settled_since = None;
                    self.open(stream, Rule::LatencyShift, at, &utc, format!("median {:.1} ms over the last minute against a {:.1} ms baseline", r / 1000.0, b / 1000.0));
                } else if open {
                    if r < b * cfg.shift_close_ratio {
                        let since = *s.settled_since.get_or_insert(at);
                        if at.saturating_sub(since) >= cfg.shift_recent_ns {
                            s.settled_since = None;
                            s.frozen_baseline = None;
                            self.close(stream, Rule::LatencyShift, at, &utc, format!("median back to {:.1} ms against the {:.1} ms baseline", r / 1000.0, b / 1000.0));
                        }
                    } else {
                        s.settled_since = None;
                    }
                }
            }
        }

        // ── error responses among the last ten DNS transactions ──
        if is_dns {
            let s = &mut self.streams[stream];
            let is_error = matches!(o.outcome, Outcome::ServiceError | Outcome::ProtocolError);
            s.last10.push_back(is_error);
            if s.last10.len() > 10 {
                s.last10.pop_front();
            }
            s.clean_streak = if o.outcome == Outcome::Success { s.clean_streak + 1 } else { 0 };
            let errors = s.last10.iter().filter(|e| **e).count() as u32;
            let open = s.rules[Rule::ErrorResponses as usize].open_since.is_some();
            if !open && is_error && errors >= cfg.dns_errors_in_last_10 {
                let what = o.error.as_ref().and_then(|e| e.detail.clone()).unwrap_or_else(|| "error".into());
                self.open(
                    stream,
                    Rule::ErrorResponses,
                    at,
                    &utc,
                    format!("{errors} error responses in the last {} transactions (latest: {what}) — answers arrived; these are not timeouts", s_len(&self.streams[stream])),
                );
            } else if open && s.clean_streak >= 10 {
                self.close(stream, Rule::ErrorResponses, at, &utc, "10 consecutive transactions answered as expected".into());
            }
        }
    }
}

fn s_len(s: &StreamState) -> usize {
    s.last10.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Msg, Record};
    use crate::script::{self, Behaviour, Rule as ScriptRule, Scenario};
    use std::time::Duration;

    fn feed(sc: &Scenario) -> IncidentTracker {
        let mut t = IncidentTracker::new(sc.streams.iter().map(|s| s.info.protocol));
        let index: std::collections::HashMap<_, _> = sc.streams.iter().enumerate().map(|(i, s)| (s.info.id.clone(), i)).collect();
        for m in script::generate(sc, 7) {
            match m {
                Msg::Record(Record::Observation(o)) => t.on_observation(index[&o.stream], &o),
                Msg::Record(Record::Gap(_)) => t.on_gap(),
                _ => {}
            }
        }
        t
    }

    fn run(name: &str) -> (IncidentTracker, Scenario) {
        let sc = script::by_name(name).unwrap();
        (feed(&sc), sc)
    }

    fn of(t: &IncidentTracker, stream: usize, rule: Rule) -> Vec<(IncidentKind, u64)> {
        t.events().iter().filter(|e| e.stream == stream && e.rule == rule).map(|e| (e.kind, e.at_ns)).collect()
    }

    fn window(from_s: u64, to_s: u64, what: Behaviour) -> ScriptRule {
        ScriptRule { from: Duration::from_secs(from_s), to: Duration::from_secs(to_s), what }
    }

    #[test]
    fn isolated_failure_opens_once_and_closes_once() {
        let (t, _) = run("isolated-failure");
        let runs = of(&t, 3, Rule::NoReplyRun);
        assert_eq!(runs.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![IncidentKind::Opened, IncidentKind::Closed]);
        // Opened on the fifth unanswered probe (sent 64.75 s), closed after ten replies.
        assert_eq!(runs[0].1, 64_750_000_000);
        let texts: Vec<&str> = t.events().iter().filter(|e| e.rule == Rule::NoReplyRun).map(|e| e.text.as_str()).collect();
        assert!(texts[0].contains("5 consecutive") && texts[1].contains("40 probes went unanswered"), "{texts:?}");
        assert!(!t.is_open(3, Rule::NoReplyRun));
        // The state at a past moment is recoverable — what a frozen view needs.
        assert!(!t.is_open_at(3, Rule::NoReplyRun, 60_000_000_000));
        assert!(t.is_open_at(3, Rule::NoReplyRun, 90_000_000_000));
        assert!(!t.is_open_at(3, Rule::NoReplyRun, 170_000_000_000));
        // Nothing on the three healthy streams.
        assert!(t.events().iter().all(|e| e.stream == 3));
    }

    #[test]
    fn elevated_rate_has_hysteresis() {
        // Arelion: 40 s of silence, then clean. The rate rule opens at the third
        // unanswered probe (3 of ~60 is 5 %) and must not close the moment the
        // rate dips: it needs ≤ 1 % for a held 30 s.
        let (t, _) = run("isolated-failure");
        let ev = of(&t, 3, Rule::ElevatedNoReply);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0], (IncidentKind::Opened, 62_750_000_000));
        assert!(t.is_open(3, Rule::ElevatedNoReply), "still held open at 180 s: the last unanswered probe left the minute at ~160 s");

        // One unanswered probe every 12 s is 8 %: elevated without ever being a run.
        let mut sc = script::by_name("healthy").unwrap();
        sc.duration = Duration::from_secs(400);
        for k in 0..8 {
            sc.streams[0].rules.push(window(60 + k * 12, 61 + k * 12, Behaviour::Silent));
        }
        let t = feed(&sc);
        assert!(of(&t, 0, Rule::NoReplyRun).is_empty());
        let ev = of(&t, 0, Rule::ElevatedNoReply);
        assert_eq!(ev.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![IncidentKind::Opened, IncidentKind::Closed], "{ev:?}");
        // Last unanswered at 144 s; it leaves the minute at 204 s; closed 30 s later.
        assert!((233..=236).contains(&(ev[1].1 / SEC)), "{}", ev[1].1 / SEC);
    }

    #[test]
    fn latency_shift_needs_a_baseline_and_freezes_it() {
        let mut sc = script::by_name("healthy").unwrap();
        sc.duration = Duration::from_secs(700);
        // +25 ms on a 12 ms stream from 400 s to 520 s.
        sc.streams[0].rules.push(window(400, 520, Behaviour::Slower(25_000)));
        // The same shift at 60 s on another stream, before any baseline exists.
        sc.streams[1].rules.push(window(60, 120, Behaviour::Slower(25_000)));
        let t = feed(&sc);
        let ev = of(&t, 0, Rule::LatencyShift);
        assert_eq!(ev.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![IncidentKind::Opened, IncidentKind::Closed], "{ev:?}");
        // Opens once the last minute's median has moved: ~30 s into the shift.
        assert!((429..=433).contains(&(ev[0].1 / SEC)), "{}", ev[0].1 / SEC);
        // Closes a held minute after the median is back: 520 + ~30 + 60.
        assert!((608..=614).contains(&(ev[1].1 / SEC)), "{}", ev[1].1 / SEC);
        let text = &t.events().iter().find(|e| e.rule == Rule::LatencyShift).unwrap().text;
        assert!(text.contains("against a 12."), "the baseline is the pre-shift median: {text}");
        assert!(of(&t, 1, Rule::LatencyShift).is_empty(), "no baseline, no verdict");
    }

    #[test]
    fn dns_error_responses_are_their_own_incident() {
        let (t, sc) = run("dns-only");
        let dns = sc.streams.iter().position(|s| s.info.protocol == Protocol::DnsUdp).unwrap();
        // SERVFAIL from 40 s: the third (50.1 s) opens it. Ten clean answers from 80.1 s close it.
        let ev = of(&t, dns, Rule::ErrorResponses);
        assert_eq!(ev, vec![(IncidentKind::Opened, 50_100_000_000), (IncidentKind::Closed, 125_100_000_000)]);
        let text = &t.events().iter().find(|e| e.rule == Rule::ErrorResponses).unwrap().text;
        assert!(text.contains("SERVFAIL") && text.contains("not timeouts"), "{text}");
        // The silence that follows is a no-reply run with DNS thresholds (3 and 3).
        let runs = of(&t, dns, Rule::NoReplyRun);
        assert_eq!(runs[0], (IncidentKind::Opened, 70_100_000_000));
        assert_eq!(runs[1].0, IncidentKind::Closed);
        // ICMP streams are untouched.
        assert!(t.events().iter().all(|e| e.stream == dns));
    }

    #[test]
    fn local_send_failures_open_a_local_incident_only() {
        let (t, sc) = run("ipv6-missing");
        let v6 = sc.streams.len() - 1;
        assert_eq!(t.events().len(), 1);
        let e = &t.events()[0];
        assert_eq!((e.stream, e.rule, e.kind), (v6, Rule::LocalError, IncidentKind::Opened));
        assert!(e.text.contains("nothing reached the network"));
        assert!(!t.is_open(v6, Rule::NoReplyRun) && !t.is_open(v6, Rule::ElevatedNoReply));
    }

    #[test]
    fn quiet_scenarios_open_nothing() {
        for name in ["healthy", "correlated-spike", "collector-overload", "address-change", "sleep-resume", "needs-config", "late-dup-reorder"] {
            let (t, _) = run(name);
            let unexpected: Vec<_> = t.events().iter().filter(|e| !(name == "late-dup-reorder" && e.stream == 1)).collect();
            assert!(unexpected.is_empty(), "{name}: {unexpected:?}");
        }
    }

    #[test]
    fn cooldown_stops_an_immediate_reopen() {
        // Six unanswered probes every twenty seconds. The first burst opens and
        // closes; the next three fall inside the 60 s cooldown; the fifth opens again.
        let (t, _) = run("flapping");
        let runs = of(&t, 3, Rule::NoReplyRun);
        let kinds: Vec<IncidentKind> = runs.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec![IncidentKind::Opened, IncidentKind::Closed, IncidentKind::Opened, IncidentKind::Closed], "{runs:?}");
        assert_eq!(runs[0].1 / SEC, 34);
        assert_eq!(runs[2].1 / SEC, 114);
        // The rate rule covers the whole episode as one incident.
        assert_eq!(of(&t, 3, Rule::ElevatedNoReply).len(), 1);
    }

    #[test]
    fn repeated_cycles_collapse_into_one_flapping_event() {
        // With no cooldown, a burst every 30 s cycles the run rule; after three
        // cycles in ten minutes the fourth is reported once, as flapping.
        let mut sc = script::by_name("healthy").unwrap();
        sc.duration = Duration::from_secs(700);
        for k in 0..8 {
            sc.streams[0].rules.push(window(60 + k * 30, 66 + k * 30, Behaviour::Silent));
        }
        let mut t = IncidentTracker::with_config(sc.streams.iter().map(|s| s.info.protocol), IncidentConfig { cooldown_ns: 0, ..IncidentConfig::default() });
        let index: std::collections::HashMap<_, _> = sc.streams.iter().enumerate().map(|(i, s)| (s.info.id.clone(), i)).collect();
        for m in script::generate(&sc, 7) {
            if let Msg::Record(Record::Observation(o)) = m {
                t.on_observation(index[&o.stream], &o);
            }
        }
        let kinds: Vec<IncidentKind> = of(&t, 0, Rule::NoReplyRun).iter().map(|(k, _)| *k).collect();
        use IncidentKind::*;
        assert_eq!(kinds, vec![Opened, Closed, Opened, Closed, Opened, Closed, Flapping, FlappingEnded], "{kinds:?}");
        // The state underneath is still tracked while flapping.
        assert!(t.is_open_at(0, Rule::NoReplyRun, 245 * SEC));
    }
}

//! A deterministic observation source driven by scenario scripts.
//!
//! It produces the same messages a live collector would, from a seeded
//! generator and a virtual clock, with no network and no real time. Tests use
//! it now; `demo` and `replay` will reuse it. Scripted sessions carry their
//! own session id and must never be mixed into a live one.

use std::sync::Arc;
use std::time::Duration;

use crate::engine::StreamInfo;
use crate::model::{AnnotationRec, Backend, DnsInfo, ErrorCategory, ErrorInfo, Family, GapCause, GapRec, IcmpInfo, Msg, Observation, Outcome, Protocol, Record};

/// 2026-01-01T00:00:00Z, the fixed origin of scripted wall-clock time.
const WALL_ORIGIN_MS: i64 = 1_767_225_600_000;

#[derive(Debug, Clone, PartialEq)]
pub enum Behaviour {
    /// No reply at all.
    Silent,
    /// Add this much to the round-trip time.
    Slower(u32),
    /// The reply arrives this long after the deadline.
    Late(Duration),
    /// Every reply arrives twice.
    Duplicated,
    /// A valid response that is an error (a DNS `SERVFAIL`, say).
    ServiceError(ErrorCategory, &'static str),
    /// The local stack refuses the send.
    LocalError(ErrorCategory),
    /// The slot is skipped by the collector.
    Skipped(ErrorCategory),
    /// The destination address changes: a new epoch with a new baseline.
    NewEpoch(u32),
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub from: Duration,
    pub to: Duration,
    pub what: Behaviour,
}

#[derive(Debug, Clone)]
pub struct ScriptStream {
    pub info: StreamInfo,
    pub configured: &'static str,
    /// `collecting`, or a state in which nothing is sent.
    pub state: &'static str,
    pub phase: Duration,
    pub base_us: u32,
    pub jitter_us: u32,
    pub first_seq: u64,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
pub struct ScriptGap {
    pub at: Duration,
    pub cause: GapCause,
    /// Real time that passes. For a suspend the monotonic clock stands still.
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: &'static str,
    pub shows: &'static str,
    pub duration: Duration,
    pub streams: Vec<ScriptStream>,
    pub gaps: Vec<ScriptGap>,
}

impl Scenario {
    pub fn infos(&self) -> Vec<StreamInfo> {
        self.streams.iter().map(|s| s.info.clone()).collect()
    }

    pub fn session(&self) -> Arc<str> {
        format!("script-{}", self.name).into()
    }

    /// Provider and stream descriptions for building dashboard snapshots.
    pub fn metas(&self) -> (Vec<crate::snapshot::ProviderMeta>, Vec<crate::snapshot::StreamMeta>) {
        use crate::snapshot::{ProviderMeta, StreamMeta, StreamState};
        let streams: Vec<StreamMeta> = self
            .streams
            .iter()
            .enumerate()
            .map(|(idx, s)| StreamMeta {
                idx,
                provider: s.info.provider.clone(),
                protocol: s.info.protocol,
                family: s.info.family,
                fresh: false,
                configured: s.configured.into(),
                state: match s.state {
                    "collecting" => StreamState::Collecting,
                    "needs_config" => StreamState::NeedsConfig("no target configured".into()),
                    "unsupported" => StreamState::Unsupported("unprivileged ICMP is disabled on this host".into()),
                    _ => StreamState::Disabled,
                },
            })
            .collect();
        let providers = [("cloudflare", "Cloudflare"), ("google", "Google"), ("nextdns", "NextDNS"), ("arelion", "Arelion")]
            .into_iter()
            .enumerate()
            .map(|(i, (key, name))| {
                let mine: Vec<&StreamMeta> = streams.iter().filter(|s| &*s.provider == key).collect();
                let needs_config = (mine.is_empty() || mine.iter().all(|s| matches!(s.state, StreamState::NeedsConfig(_)))).then(|| "no target configured".to_string());
                ProviderMeta { key: key.into(), name: name.into(), slot: i as u8 + 1, needs_config, disabled: false }
            })
            .collect();
        (providers, streams)
    }
}

/// xorshift64*: small, fast, and identical on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Roughly bell-shaped noise in `[-range, range]`.
    fn jitter(&mut self, range: u32) -> i64 {
        if range == 0 {
            return 0;
        }
        let span = u64::from(range) + 1;
        ((self.next() % span) as i64 + (self.next() % span) as i64) - i64::from(range)
    }
}

fn utc(mono_ns: u64, hidden_ns: u64) -> String {
    let ms = WALL_ORIGIN_MS + ((mono_ns + hidden_ns) / 1_000_000) as i64;
    crate::clock::utc_string(jiff::Timestamp::from_millisecond(ms).expect("valid timestamp"))
}

/// Generates the scenario's messages in the order a collector would emit them.
pub fn generate(sc: &Scenario, seed: u64) -> Vec<Msg> {
    generate_timed(sc, seed).into_iter().map(|(_, m)| m).collect()
}

/// As [`generate`], with the session-monotonic time at which each message is
/// emitted, for playback against a clock.
pub fn generate_timed(sc: &Scenario, seed: u64) -> Vec<(u64, Msg)> {
    let session = sc.session();
    let end_ns = sc.duration.as_nanos() as u64;
    // (time, tie-breaker, message)
    let mut timeline: Vec<(u64, u64, Msg)> = Vec::new();
    let mut order = 0u64;
    let mut push = |at: u64, msg: Msg, timeline: &mut Vec<(u64, u64, Msg)>| {
        order += 1;
        timeline.push((at, order, msg));
    };

    // Gaps on the monotonic timeline: a suspend occupies no monotonic time, a
    // stall occupies its whole duration.
    let gaps: Vec<(u64, u64, u64, GapCause)> = sc
        .gaps
        .iter()
        .map(|g| {
            let start = g.at.as_nanos() as u64;
            let d = g.duration.as_nanos() as u64;
            let mono_len = if g.cause == GapCause::Suspend { 0 } else { d };
            (start, start + mono_len, d, g.cause)
        })
        .collect();
    let hidden_before = |t: u64| gaps.iter().filter(|g| g.1 <= t).map(|g| g.2 - (g.1 - g.0)).sum::<u64>();
    let in_gap = |t: u64| gaps.iter().any(|g| t >= g.0 && t < g.1);
    let gap_between = |a: u64, b: u64| gaps.iter().find(|g| g.0 > a && g.0 <= b).map(|g| g.0);
    for &(start, end, d, cause) in &gaps {
        let rec = GapRec { session: session.clone(), cause, start_ns: start, end_ns: end, duration_ms: d / 1_000_000, end_utc: utc(end, hidden_before(end)) };
        push(end, Msg::Record(Record::Gap(rec)), &mut timeline);
    }

    for (idx, s) in sc.streams.iter().enumerate() {
        if s.state != "collecting" {
            continue;
        }
        let mut rng = Rng(seed ^ (0x9e37_79b9_7f4a_7c15u64.wrapping_mul(idx as u64 + 1)) | 1);
        let interval = s.info.interval.as_nanos() as u64;
        let deadline = s.info.deadline.as_nanos() as u64;
        let mut k = 0u64;
        loop {
            let sched = s.phase.as_nanos() as u64 + k * interval;
            if sched >= end_ns {
                break;
            }
            let seq = s.first_seq + k;
            k += 1;
            if in_gap(sched) {
                continue; // no slots exist inside an observation gap
            }
            let active: Vec<&Behaviour> = s.rules.iter().filter(|r| sched >= r.from.as_nanos() as u64 && sched < r.to.as_nanos() as u64).map(|r| &r.what).collect();
            let epoch = 1 + s.rules.iter().filter(|r| matches!(r.what, Behaviour::NewEpoch(_)) && sched >= r.from.as_nanos() as u64).count() as u32;
            let base = s
                .rules
                .iter()
                .rev()
                .find_map(|r| match r.what {
                    Behaviour::NewEpoch(b) if sched >= r.from.as_nanos() as u64 => Some(b),
                    _ => None,
                })
                .unwrap_or(s.base_us);

            let mut o = Observation {
                session: session.clone(),
                config_rev: 1,
                provider: s.info.provider.clone(),
                target: s.info.target.clone(),
                stream: s.info.id.clone(),
                seq,
                transaction: seq,
                attempt: 1,
                protocol: s.info.protocol,
                family: s.info.family,
                configured: s.configured.into(),
                dest_ip: s.configured.parse().ok(),
                dest_port: None,
                epoch,
                src: None,
                sched_ns: sched,
                sent_ns: None,
                recv_ns: None,
                sent_utc: None,
                deadline_ms: s.info.deadline.as_millis() as u32,
                sched_lag_us: None,
                outcome: Outcome::Skipped,
                error: None,
                rtt_us: None,
                rtt_user_us: None,
                recv_delay_us: None,
                flags: Vec::new(),
                phase: None,
                icmp: (s.info.protocol == Protocol::Icmp).then_some(IcmpInfo { id: 0x4242, seq16: seq as u16, reply_ttl: None, icmp_type: None, code: None, payload: 56 }),
                dns: s.info.protocol.is_dns().then(|| {
                    // The default query set, rotating by slot as the collector does.
                    let (qname, qtype, expect) = [(".", "NS", "noerror"), ("example.com.", "A", "answer"), ("wikipedia.org.", "AAAA", "answer")][(seq % 3) as usize];
                    DnsInfo {
                        qname: qname.into(),
                        qtype: qtype.into(),
                        id: (seq as u16).wrapping_mul(40_503),
                        expect: expect.into(),
                        rcode: None,
                        tc: None,
                        answers: None,
                        size: None,
                        min_ttl: None,
                        ede: Vec::new(),
                        matched: None,
                    }
                }),
                conn: None,
                backend: Backend { name: "script".into(), version: env!("CARGO_PKG_VERSION").into(), ts_source: "scripted".into() },
            };

            if let Some(Behaviour::Skipped(why)) = active.iter().find(|b| matches!(b, Behaviour::Skipped(_))) {
                o.error = Some(ErrorInfo::new(*why));
                push(sched, Msg::obs(o), &mut timeline);
                continue;
            }
            if let Some(Behaviour::LocalError(why)) = active.iter().find(|b| matches!(b, Behaviour::LocalError(_))) {
                o.outcome = Outcome::LocalError;
                o.error = Some(ErrorInfo::new(*why));
                push(sched, Msg::obs(o), &mut timeline);
                continue;
            }

            let sent = sched;
            o.sent_ns = Some(sent);
            o.sent_utc = Some(utc(sent, hidden_before(sent)));
            o.sched_lag_us = Some(0);
            push(sent, Msg::Sent { stream: idx as u32, seq, sent_ns: sent }, &mut timeline);

            let extra: u32 = active.iter().map(|b| if let Behaviour::Slower(us) = b { *us } else { 0 }).sum();
            let rtt_us = (i64::from(base + extra) + rng.jitter(s.jitter_us)).max(50) as u32;
            let recv = sent + u64::from(rtt_us) * 1_000;
            let late = active.iter().find_map(|b| if let Behaviour::Late(by) = b { Some(by.as_nanos() as u64) } else { None });
            let silent = active.iter().any(|b| matches!(b, Behaviour::Silent));

            // A probe in flight when a gap begins is cancelled by it; one still
            // in flight when the session ends is cancelled by shutdown.
            let finalises_at = if silent || late.is_some() { sent + deadline } else { recv };
            if let Some(gap_at) = gap_between(sent, finalises_at) {
                o.outcome = Outcome::Cancelled;
                o.error = Some(ErrorInfo::new(ErrorCategory::Gap));
                push(gap_at, Msg::obs(o), &mut timeline);
                continue;
            }
            if finalises_at >= end_ns {
                o.outcome = Outcome::Cancelled;
                o.error = Some(ErrorInfo::new(ErrorCategory::Shutdown));
                push(end_ns, Msg::obs(o), &mut timeline);
                continue;
            }

            if silent || late.is_some() {
                // A timeout has no round-trip time.
                o.outcome = Outcome::Timeout;
                push(sent + deadline, Msg::obs(o), &mut timeline);
                if let Some(by) = late {
                    let at = sent + deadline + by;
                    if at < end_ns {
                        let a = AnnotationRec { session: session.clone(), stream: s.info.id.clone(), seq, at_ns: at, late_rtt_us: Some(((deadline + by) / 1_000) as u32) };
                        push(at, Msg::Record(Record::LateReply(a)), &mut timeline);
                    }
                }
                continue;
            }

            o.recv_ns = Some(recv);
            o.rtt_us = Some(rtt_us);
            match active.iter().find(|b| matches!(b, Behaviour::ServiceError(..))) {
                Some(Behaviour::ServiceError(cat, detail)) => {
                    o.outcome = Outcome::ServiceError;
                    o.error = Some(ErrorInfo::new(*cat).detail(*detail));
                    if let Some(d) = o.dns.as_mut() {
                        d.rcode = Some((*detail).into());
                        d.answers = Some(0);
                        d.matched = Some(false);
                    }
                }
                _ => {
                    o.outcome = Outcome::Success;
                    if let Some(i) = o.icmp.as_mut() {
                        i.reply_ttl = Some(57);
                    }
                    if let Some(d) = o.dns.as_mut() {
                        d.rcode = Some("NOERROR".into());
                        d.answers = Some(if d.qtype == "NS" { 13 } else { 1 });
                        d.matched = Some(true);
                    }
                }
            }
            push(recv, Msg::obs(o), &mut timeline);
            if active.iter().any(|b| matches!(b, Behaviour::Duplicated)) {
                let a = AnnotationRec { session: session.clone(), stream: s.info.id.clone(), seq, at_ns: recv + 400_000, late_rtt_us: None };
                push(recv + 400_000, Msg::Record(Record::Duplicate(a)), &mut timeline);
            }
        }
    }

    timeline.sort_by_key(|(at, order, _)| (*at, *order));
    timeline.into_iter().map(|(at, _, m)| (at, m)).collect()
}

// ───────────────────────────── the scenario set ─────────────────────────────

#[allow(clippy::too_many_arguments)] // a fixture builder: positional is clearer than a struct here
pub fn stream(
    provider: &'static str,
    target: &'static str,
    protocol: Protocol,
    family: Family,
    configured: &'static str,
    base_ms: u32,
    jitter_us: u32,
    phase_ms: u64,
) -> ScriptStream {
    let (interval, deadline) = if protocol == Protocol::Icmp { (Duration::from_secs(1), Duration::from_secs(2)) } else { (Duration::from_secs(5), Duration::from_secs(3)) };
    ScriptStream {
        info: StreamInfo {
            id: format!("{provider}/{target}/{}/{}", protocol.as_str(), family.as_str()).into(),
            provider: provider.into(),
            target: target.into(),
            protocol,
            family,
            interval,
            deadline,
        },
        configured,
        state: "collecting",
        phase: Duration::from_millis(phase_ms),
        base_us: base_ms * 1_000,
        jitter_us,
        first_seq: 0,
        rules: Vec::new(),
    }
}

/// The four provider ICMP streams over IPv4. Addresses for the last two are
/// documentation-range placeholders; scripted data is illustrative only.
pub fn four_icmp() -> Vec<ScriptStream> {
    vec![
        stream("cloudflare", "resolver-v4", Protocol::Icmp, Family::V4, "1.1.1.1", 12, 700, 0),
        stream("google", "resolver-v4", Protocol::Icmp, Family::V4, "8.8.8.8", 18, 1_100, 250),
        stream("nextdns", "anycast-v4", Protocol::Icmp, Family::V4, "203.0.113.28", 24, 1_400, 500),
        stream("arelion", "router", Protocol::Icmp, Family::V4, "198.51.100.7", 31, 2_200, 750),
    ]
}

pub fn rule(from_s: u64, to_s: u64, what: Behaviour) -> Rule {
    Rule { from: Duration::from_secs(from_s), to: Duration::from_secs(to_s), what }
}

fn scenario(name: &'static str, shows: &'static str, streams: Vec<ScriptStream>) -> Scenario {
    Scenario { name, shows, duration: Duration::from_secs(180), streams, gaps: Vec::new() }
}

/// Every scripted scenario, as listed in `planning/03-implementation-plan.md` §4.3.
pub fn all() -> Vec<Scenario> {
    let mut out = Vec::new();

    out.push(scenario("healthy", "four streams, stable latency, full coverage", four_icmp()));

    let mut s = four_icmp();
    s[3].rules.push(rule(60, 100, Behaviour::Silent));
    out.push(scenario("isolated-failure", "one target stops replying for 40 s; the other three are unaffected", s));

    let mut s = four_icmp();
    for st in &mut s {
        st.rules.push(rule(60, 75, Behaviour::Slower(40_000)));
    }
    out.push(scenario("correlated-spike", "all four rise together for 15 s", s));

    let mut s = four_icmp();
    for st in &mut s {
        st.rules.push(rule(60, 90, Behaviour::Silent));
    }
    s.push(stream("cloudflare", "resolver-v4", Protocol::DnsUdp, Family::V4, "1.1.1.1", 14, 900, 100));
    let mut g = stream("google", "resolver-v4", Protocol::DnsUdp, Family::V4, "8.8.8.8", 21, 1_200, 2_600);
    g.rules.push(rule(60, 90, Behaviour::Silent));
    s.push(g);
    out.push(scenario("correlated-outage", "all four ICMP targets stop; DNS partially answers", s));

    let mut s = four_icmp();
    let mut d = stream("cloudflare", "resolver-v4", Protocol::DnsUdp, Family::V4, "1.1.1.1", 14, 900, 100);
    d.rules.push(rule(40, 60, Behaviour::ServiceError(ErrorCategory::DnsRcode, "SERVFAIL")));
    d.rules.push(rule(60, 80, Behaviour::Silent));
    s.push(d);
    out.push(scenario("dns-only", "ICMP healthy; DNS returns SERVFAIL, then times out, then recovers", s));

    let mut s = four_icmp();
    for st in &mut s {
        st.state = "unsupported";
    }
    s.push(stream("cloudflare", "resolver-v4", Protocol::DnsUdp, Family::V4, "1.1.1.1", 14, 900, 100));
    out.push(scenario("missing-permission", "ICMP unsupported on the host; DNS runs", s));

    let mut s = four_icmp();
    s[2].state = "needs_config";
    s[3].state = "needs_config";
    out.push(scenario("needs-config", "providers 3 and 4 unconfigured; nothing is sent to them", s));

    let mut s = four_icmp();
    s[2].rules.push(rule(60, 180, Behaviour::NewEpoch(41_000)));
    out.push(scenario("address-change", "a hostname target moves to a new address; statistics restart", s));

    let mut s = four_icmp();
    for st in &mut s {
        st.rules.push(rule(50, 70, Behaviour::Skipped(ErrorCategory::Backpressure)));
    }
    out.push(scenario("collector-overload", "the processor falls behind: skipped slots, no network loss", s));

    let mut sc = scenario("sleep-resume", "a 12-minute host suspend: one gap, no burst, no loss", four_icmp());
    sc.gaps.push(ScriptGap { at: Duration::from_millis(60_100), cause: GapCause::Suspend, duration: Duration::from_secs(720) });
    out.push(sc);

    let mut s = four_icmp();
    s[0].first_seq = 65_500; // crosses the 16-bit sequence boundary
    s[1].rules.push(rule(20, 40, Behaviour::Late(Duration::from_millis(400))));
    s[2].rules.push(rule(40, 60, Behaviour::Duplicated));
    out.push(scenario("late-dup-reorder", "late replies, duplicates, and a 16-bit sequence wraparound", s));

    let mut s = four_icmp();
    for burst in 0..5u64 {
        s[3].rules.push(rule(30 + burst * 20, 36 + burst * 20, Behaviour::Silent));
    }
    out.push(scenario("flapping", "repeated short failures on one target", s));

    let mut s = four_icmp();
    let mut v6 = stream("cloudflare", "resolver-v6", Protocol::Icmp, Family::V6, "2606:4700:4700::1111", 13, 700, 125);
    v6.rules.push(rule(0, 180, Behaviour::LocalError(ErrorCategory::NoRoute)));
    s.push(v6);
    out.push(scenario("ipv6-missing", "IPv6 has no route: local errors while IPv4 is healthy", s));

    out
}

pub fn by_name(name: &str) -> Option<Scenario> {
    all().into_iter().find(|s| s.name == name)
}

/// Plays a scenario against a clock: feed it the current session time and it
/// ingests everything due by then. Used by `demo` and by dashboard tests.
pub struct Playback {
    end_ns: u64,
    pub processor: crate::processor::Processor,
    pub providers: Vec<crate::snapshot::ProviderMeta>,
    pub streams: Vec<crate::snapshot::StreamMeta>,
    msgs: Vec<(u64, Msg)>,
    next: usize,
}

impl Playback {
    pub fn new(scenario: Scenario, seed: u64) -> Self {
        let msgs = generate_timed(&scenario, seed);
        let (providers, streams) = scenario.metas();
        let processor = crate::processor::Processor::new(scenario.infos(), crate::engine::HistoryLimits::default());
        Self { end_ns: scenario.duration.as_nanos() as u64, processor, providers, streams, msgs, next: 0 }
    }

    /// Plays a recording instead of a scenario: the same processor, fed the
    /// recorded records at their recorded times.
    pub fn of_recording(replay: crate::recorder::Replay) -> Self {
        let processor = crate::processor::Processor::new(replay.infos, crate::engine::HistoryLimits::default());
        Self { end_ns: replay.end_ns, processor, providers: replay.providers, streams: replay.streams, msgs: replay.msgs, next: 0 }
    }

    pub fn end_ns(&self) -> u64 {
        self.end_ns
    }

    /// Ingests every message due at or before `now_ns`.
    pub fn advance_to(&mut self, now_ns: u64) {
        while let Some((at, msg)) = self.msgs.get(self.next) {
            if *at > now_ns {
                break;
            }
            self.processor.ingest(msg);
            // A recorded reload changes what the session runs from here on.
            if let Msg::Record(Record::Config(c)) = msg {
                let engine = &self.processor.engine;
                (self.providers, self.streams) = crate::snapshot::metas_from(&c.streams, &c.providers, &|id| engine.stream_index(id));
            }
            self.next += 1;
        }
    }

    pub fn snapshot(&self, view: &crate::snapshot::ViewSpec, now_ns: u64) -> crate::snapshot::Snapshot {
        self.processor.snapshot(&self.providers, &self.streams, view, now_ns.min(self.end_ns()))
    }
}

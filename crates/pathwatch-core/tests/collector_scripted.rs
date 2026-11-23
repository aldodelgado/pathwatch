//! Collector behaviour against a scripted ICMP transport and a paused clock:
//! deterministic, no network. These cover the milestone-1 acceptance criteria
//! that concern scheduling, outcome accounting, gaps, and isolation.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use pathwatch_core::clock::{Clock, ManualClock};
use pathwatch_core::collector::{self, IcmpSetup, PauseSwitch, Setup};
use pathwatch_core::config::Loaded;
use pathwatch_core::engine::{Engine, HistoryLimits};
use pathwatch_core::model::{ErrorCategory, GapCause, Line, Msg, Outcome, Record};
use pathwatch_core::testing::{FakeIcmpIo, FakeNet, FakeReply, Script, SentProbe};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const A: &str = "192.0.2.1";
const B: &str = "192.0.2.2";

/// Two ICMP-only IPv4 streams, phases 0 ms and 500 ms.
const CONFIG: &str = r#"
[general]
families = ["ipv4"]

[[providers.cloudflare.targets]]
id = "a"
address = "192.0.2.1"
protocols = ["icmp"]

[[providers.google.targets]]
id = "b"
address = "192.0.2.2"
protocols = ["icmp"]
"#;

struct Run {
    msgs: Vec<Msg>,
    sent: Vec<SentProbe>,
    engine: Engine,
    end_ns: u64,
}

impl Run {
    fn lines(&self) -> Vec<String> {
        self.msgs
            .iter()
            .filter_map(|m| match m {
                Msg::Record(r) => Some(Line::new(r.clone()).to_json()),
                Msg::Sent { .. } => None,
            })
            .collect()
    }

    fn observations(&self, stream: &str) -> Vec<&pathwatch_core::model::Observation> {
        self.msgs
            .iter()
            .filter_map(|m| match m {
                Msg::Record(Record::Observation(o)) if &*o.stream == stream => Some(&**o),
                _ => None,
            })
            .collect()
    }

    fn stats(&self, stream: &str) -> pathwatch_core::stats::WindowStats {
        let idx = self.engine.stream_index(stream).expect("stream exists");
        self.engine.stats(idx, Duration::from_secs(300), self.end_ns)
    }
}

/// Runs the collector for `phases` of simulated time, calling `between` after
/// each phase but the last (to inject a suspend, for example).
async fn run(config: &str, channel: usize, script: Script, phases: &[Duration], between: impl Fn(&ManualClock)) -> Run {
    run_with(config, channel, script, phases, |clock, _| between(clock)).await
}

/// As [`run`], with the user's stop/resume switch also handed to `between`.
async fn run_with(config: &str, channel: usize, script: Script, phases: &[Duration], between: impl Fn(&ManualClock, &PauseSwitch)) -> Run {
    run_net(config, channel, script, phases, |clock, pause, _| between(clock, pause)).await
}

/// As [`run_with`], with the host's network also handed to `between`, so a
/// test can change it under the collector.
async fn run_net(config: &str, channel: usize, script: Script, phases: &[Duration], between: impl Fn(&ManualClock, &PauseSwitch, &FakeNet)) -> Run {
    run_on(config, channel, script, phases, |_| {}, between).await
}

/// As [`run_net`], on a network prepared by `net_setup` before the plan is made.
async fn run_on(config: &str, channel: usize, script: Script, phases: &[Duration], net_setup: impl Fn(&FakeNet), between: impl Fn(&ManualClock, &PauseSwitch, &FakeNet)) -> Run {
    run_full(config, channel, script, phases, net_setup, |h| between(&h.clock, &h.pause, &h.net)).await
}

/// What a test can reach into while the collector runs.
struct Harness {
    clock: Arc<ManualClock>,
    pause: PauseSwitch,
    net: Arc<FakeNet>,
    reload: mpsc::Sender<collector::Reload>,
}

impl Harness {
    /// Asks the collector to run this configuration from now on, as the
    /// binary does after it has parsed and validated a reloaded file.
    fn reload(&self, config: &str) {
        let loaded = Loaded::from_layers(Some(config), None, &[]).expect("the reloaded configuration is valid");
        let mut plan = loaded.plan(&|_| None);
        plan.add_controls(&loaded.config, &pathwatch_core::netwatch::NetProbe::local(self.net.as_ref()));
        assert!(self.reload.try_send(collector::Reload { config: Arc::new(loaded.config.clone()), plan }).is_ok(), "the collector takes reloads");
    }
}

/// As [`run_on`], with everything a test can act on handed to `between`.
async fn run_full(config: &str, channel: usize, script: Script, phases: &[Duration], net_setup: impl Fn(&FakeNet), between: impl Fn(&Harness)) -> Run {
    let pause = PauseSwitch::default();
    let (reload_tx, reload_rx) = mpsc::channel(4);
    let net = Arc::new(FakeNet::default());
    net_setup(&net);
    let loaded = Loaded::from_layers(Some(config), None, &[]).expect("test configuration is valid");
    let mut plan = loaded.plan(&|_| None);
    // The controls, when the configuration enables them, as this network has them.
    plan.add_controls(&loaded.config, &pathwatch_core::netwatch::NetProbe::local(net.as_ref()));
    let clock = Arc::new(ManualClock::new());
    let (tx, mut rx) = mpsc::channel(channel);
    let cancel = CancellationToken::new();
    let (io, sent) = FakeIcmpIo::new(clock.clone(), script);
    let setup = Setup {
        config: Arc::new(loaded.config.clone()),
        plan: plan.clone(),
        session: "test-session".into(),
        config_rev: 1,
        clock: clock.clone(),
        tx,
        cancel: cancel.clone(),
        tls: None,
        resolver: None,
        dns_port: 53,
        pause: pause.clone(),
        net: Some(net.clone()),
        reload: Some(reload_rx),
    };
    let harness = Harness { clock: clock.clone(), pause, net, reload: reload_tx };
    let handle = tokio::spawn(collector::run(setup, IcmpSetup::Use(io), IcmpSetup::<FakeIcmpIo>::Unavailable("IPv6 is not under test".into())));
    for (i, d) in phases.iter().enumerate() {
        tokio::time::sleep(*d).await;
        if i + 1 < phases.len() {
            between(&harness);
        }
    }
    let end_ns = clock.mono_ns();
    cancel.cancel();
    let mut msgs = Vec::new();
    // Drain while the collector shuts down, so a small channel cannot block it.
    while let Some(m) = rx.recv().await {
        msgs.push(m);
    }
    handle.await.unwrap();
    let mut engine = Engine::new(collector::stream_infos(&plan), HistoryLimits::default());
    for m in &msgs {
        engine.ingest(m);
    }
    let sent = sent.lock().unwrap().clone();
    Run { msgs, sent, engine, end_ns }
}

fn replies(a_ms: Option<u64>, b_ms: Option<u64>) -> Script {
    Box::new(move |dest: IpAddr, _seq, _now| {
        let ms = if dest.to_string() == A { a_ms } else { b_ms };
        ms.map(|ms| vec![FakeReply::Echo(Duration::from_millis(ms))]).unwrap_or_default()
    })
}

const SA: &str = "cloudflare/a/icmp/4";
const SB: &str = "google/b/icmp/4";

fn assert_identities(s: &pathwatch_core::stats::WindowStats) {
    assert_eq!(s.sent, s.pending + s.replied + s.timed_out + s.unreachable + s.service_error + s.protocol_error + s.cancelled, "{s:?}");
    assert_eq!(s.slots, s.sent + s.local_error + s.skipped, "{s:?}");
}

#[tokio::test(start_paused = true)]
async fn identical_scenarios_produce_identical_records_and_statistics() {
    let first = run(CONFIG, 4096, replies(Some(12), Some(18)), &[Duration::from_millis(10_300)], |_| {}).await;
    let second = run(CONFIG, 4096, replies(Some(12), Some(18)), &[Duration::from_millis(10_300)], |_| {}).await;
    // One number is left out of the comparison: how many times the collector
    // checked its own lateness. The run ends on the very instant of such a
    // check, and which of the two comes first is the scheduler's choice. It
    // is a count about the instrument, not a measurement.
    let steady = |lines: Vec<String>| -> Vec<String> {
        lines
            .into_iter()
            .map(|l| match l.split_once("\"loop_lag_checks\":") {
                Some((before, after)) => format!("{before}\"loop_lag_checks\":_{}", after.trim_start_matches(|c: char| c.is_ascii_digit())),
                None => l,
            })
            .collect()
    };
    assert_eq!(steady(first.lines()), steady(second.lines()), "byte-identical JSONL for the same scenario");
    assert_eq!(first.stats(SA), second.stats(SA));

    let a = first.stats(SA);
    // Slots at 0..=10 s; the one at 10 s replied before the run ended at 10.3 s.
    assert_eq!((a.sent, a.replied, a.timed_out, a.pending), (11, 11, 0, 0));
    assert_eq!((a.min_us, a.max_us), (Some(12_000), Some(12_000)), "exact round-trip time under the scripted clock");
    assert_identities(&a);
    let b = first.stats(SB);
    assert_eq!((b.sent, b.replied), (10, 10), "phase 500 ms: slots at 0.5..=9.5 s");
    assert_eq!(b.min_us, Some(18_000));
}

#[tokio::test(start_paused = true)]
async fn probes_are_staggered_and_never_burst() {
    let r = run(CONFIG, 4096, replies(Some(12), Some(18)), &[Duration::from_millis(5_300)], |_| {}).await;
    let a: Vec<u64> = r.sent.iter().filter(|p| p.dest.to_string() == A).map(|p| p.at_ns / 1_000_000).collect();
    let b: Vec<u64> = r.sent.iter().filter(|p| p.dest.to_string() == B).map(|p| p.at_ns / 1_000_000).collect();
    assert_eq!(a, vec![0, 1000, 2000, 3000, 4000, 5000]);
    assert_eq!(b, vec![500, 1500, 2500, 3500, 4500]);
}

#[tokio::test(start_paused = true)]
async fn a_silent_target_does_not_delay_the_others_and_has_no_round_trip_time() {
    let r = run(CONFIG, 4096, replies(Some(12), None), &[Duration::from_millis(10_300)], |_| {}).await;
    let a = r.observations(SA);
    assert!(a.iter().all(|o| o.outcome == Outcome::Success && o.sched_lag_us == Some(0)), "the healthy stream sends exactly on schedule");

    let b = r.observations(SB);
    let timeouts: Vec<_> = b.iter().filter(|o| o.outcome == Outcome::Timeout).collect();
    assert_eq!(timeouts.len(), 8, "sent at 0.5..=7.5 s, each timing out 2 s later");
    for o in &timeouts {
        assert_eq!(o.rtt_us, None);
        assert_eq!(o.recv_ns, None);
    }
    // In the serialised form the field is absent — never 0, never the deadline.
    for line in r.lines().iter().filter(|l| l.contains("\"outcome\":\"timeout\"")) {
        assert!(!line.contains("rtt_us"), "{line}");
    }
    // Probes sent at 8.5 and 9.5 s were still in flight at shutdown: cancelled, not lost.
    let cancelled: Vec<_> = b.iter().filter(|o| o.outcome == Outcome::Cancelled).collect();
    assert_eq!(cancelled.len(), 2);
    assert!(cancelled.iter().all(|o| o.error.as_ref().unwrap().category == ErrorCategory::Shutdown));

    let s = r.stats(SB);
    assert_eq!((s.timed_out, s.cancelled, s.replied, s.finalised), (8, 2, 0, 8));
    assert_eq!(s.n, 0);
    assert_eq!((s.min_us, s.max_us), (None, None));
    assert_identities(&s);
}

#[tokio::test(start_paused = true)]
async fn stopping_collection_sends_nothing_and_is_recorded_as_one_gap() {
    // Run 3.3 s, stop for 5 s, resume for 3 s. Stream B never answers, so a
    // stop that was miscounted would show up as extra timeouts.
    let phases = [Duration::from_millis(3_300), Duration::from_millis(5_000), Duration::from_millis(3_000)];
    let r = run_with(CONFIG, 4096, replies(Some(12), None), &phases, |_, pause| pause.set(!pause.is_stopped())).await;
    let (stop_ns, resume_ns) = (3_300_000_000u64, 8_300_000_000u64);

    let gaps: Vec<_> = r
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::Gap(g)) => Some(g),
            _ => None,
        })
        .collect();
    assert_eq!(gaps.len(), 1, "one gap record: {gaps:?}");
    assert_eq!((gaps[0].cause, gaps[0].start_ns, gaps[0].end_ns, gaps[0].duration_ms), (GapCause::Stopped, stop_ns, resume_ns, 5_000));

    // Nothing left the socket while stopped, and nothing was "caught up" after.
    assert!(r.sent.iter().all(|p| p.at_ns <= stop_ns || p.at_ns >= resume_ns), "a probe was sent while stopped");
    // A's slots fall on whole seconds (0–3, then 9–11); B's half a second later (0.5–2.5, then 8.5–10.5).
    for (stream, dest, expected) in [(SA, A, 7), (SB, B, 6)] {
        let sent = r.sent.iter().filter(|p| p.dest.to_string() == dest).count();
        assert_eq!(sent, expected, "{stream}: only the slots outside the stop");
        let obs = r.observations(stream);
        assert!(obs.iter().all(|o| o.outcome != Outcome::Skipped), "{stream}: slots in a stop do not exist, so none is skipped");
        assert!(obs.iter().all(|o| o.sched_ns <= stop_ns || o.sched_ns >= resume_ns), "{stream}: a slot was scheduled inside the stop");
        // The grid keeps its phase: the first slot after resuming is on the original second boundaries.
        let phase = obs[0].sched_ns;
        assert!(obs.iter().all(|o| (o.sched_ns - phase) % 1_000_000_000 == 0), "{stream}: the slot grid moved");
        assert_identities(&r.stats(stream));
    }
    // A probe in flight when collection stopped still gets its verdict.
    let (a, b) = (r.stats(SA), r.stats(SB));
    assert_eq!((a.replied, a.unanswered), (7, 0), "{a:?}");
    assert_eq!(b.timed_out + b.pending + b.cancelled, 6, "{b:?}");
    assert!(b.timed_out >= 3, "the three sent before the stop time out normally: {b:?}");

    let events: Vec<&str> = r
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::Event(e)) => Some(e.event.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(events.iter().filter(|e| e.starts_with("collection_")).copied().collect::<Vec<_>>(), vec!["collection_stopped", "collection_resumed"]);
}

#[tokio::test(start_paused = true)]
async fn a_session_that_ends_while_stopped_still_records_the_gap() {
    let r = run_with(CONFIG, 4096, replies(Some(12), Some(18)), &[Duration::from_millis(2_300), Duration::from_millis(4_000)], |_, pause| pause.set(true)).await;
    let gap = r.msgs.iter().find_map(|m| match m {
        Msg::Record(Record::Gap(g)) => Some(g.clone()),
        _ => None,
    });
    let gap = gap.expect("the open stop is closed at shutdown");
    assert_eq!((gap.cause, gap.duration_ms), (GapCause::Stopped, 4_000));
    assert_eq!(r.sent.iter().filter(|p| p.at_ns > 2_300_000_000).count(), 0);
}

#[tokio::test(start_paused = true)]
async fn host_suspend_is_one_gap_with_no_burst_and_no_loss() {
    let r = run(CONFIG, 4096, replies(Some(12), None), &[Duration::from_millis(5_300), Duration::from_millis(5_000)], |clock| {
        clock.suspend(Duration::from_secs(600));
    })
    .await;
    let gaps: Vec<_> = r
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::Gap(g)) => Some(g),
            _ => None,
        })
        .collect();
    assert_eq!(gaps.len(), 1, "one gap record");
    assert_eq!(gaps[0].cause, GapCause::Suspend);
    assert!((600_000..=600_300).contains(&gaps[0].duration_ms), "{}", gaps[0].duration_ms);

    // No catch-up: the suspended ten minutes produced no probes at all.
    let a_sent = r.sent.iter().filter(|p| p.dest.to_string() == A).count();
    assert_eq!(a_sent, 11, "slots at 0..=10 s of monotonic time only");
    let a = r.stats(SA);
    assert_eq!((a.timed_out, a.unanswered), (0, 0), "the healthy stream shows no loss across the suspend");

    // Probes of the silent stream that were in flight when the host slept are
    // cancelled by the gap, not counted as timeouts.
    let b = r.observations(SB);
    let gap_cancelled = b.iter().filter(|o| o.outcome == Outcome::Cancelled && o.error.as_ref().unwrap().category == ErrorCategory::Gap).count();
    assert_eq!(gap_cancelled, 2, "sent at 3.5 s and 4.5 s, pending at the 5.3 s suspend");

    // The 5-minute window ending now reaches back across the gap on the real
    // timeline, so only post-resume probes are inside it and coverage is low.
    assert_eq!(a.sent, 5, "post-resume slots at 6..=10 s; earlier ones are 10 minutes old");
    assert!(a.coverage < 0.05, "{}", a.coverage);
    assert_eq!(r.engine.gaps().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn local_send_failures_are_not_network_loss() {
    let script: Script =
        Box::new(|dest: IpAddr, _seq, _now| if dest.to_string() == B { vec![FakeReply::SendError(libc_ehostunreach())] } else { vec![FakeReply::Echo(Duration::from_millis(12))] });
    let r = run(CONFIG, 4096, script, &[Duration::from_millis(30_300)], |_| {}).await;
    let b = r.observations(SB);
    assert!(!b.is_empty() && b.iter().all(|o| o.outcome == Outcome::LocalError && o.sent_ns.is_none()));
    assert_eq!(b[0].error.as_ref().unwrap().category, ErrorCategory::NoRoute);
    let s = r.stats(SB);
    assert_eq!((s.sent, s.finalised, s.local_error), (0, 0, 30));
    assert_eq!(s.no_reply_rate, None, "nothing was sent, so there is no rate to report");
    assert_identities(&s);
    assert!(r.sent.iter().all(|p| p.dest.to_string() == A), "nothing reached the wire for the failing stream");
}

fn libc_ehostunreach() -> i32 {
    // EHOSTUNREACH: 65 on macOS, 113 on Linux.
    if cfg!(target_os = "macos") { 65 } else { 113 }
}

#[tokio::test(start_paused = true)]
async fn icmp_unreachable_is_an_explicit_outcome() {
    let router: IpAddr = "192.0.2.254".parse().unwrap();
    let script: Script =
        Box::new(
            move |dest: IpAddr, _seq, _now| {
                if dest.to_string() == B { vec![FakeReply::Unreachable(Duration::from_millis(3), router)] } else { vec![FakeReply::Echo(Duration::from_millis(12))] }
            },
        );
    let r = run(CONFIG, 4096, script, &[Duration::from_millis(25_300)], |_| {}).await;
    let b = r.observations(SB);
    assert!(b.iter().all(|o| o.outcome == Outcome::Unreachable && o.rtt_us.is_none()));
    assert!(b[0].error.as_ref().unwrap().detail.as_ref().unwrap().contains("192.0.2.254"));
    let s = r.stats(SB);
    assert_eq!((s.unreachable, s.unanswered, s.finalised), (25, 25, 25));
    assert_eq!(s.no_reply_rate, Some(1.0), "unreachable counts as unanswered, separately from timeouts");
    assert_eq!(s.timed_out, 0);
}

#[tokio::test(start_paused = true)]
async fn late_and_duplicate_replies_annotate_without_changing_outcomes() {
    // Stream A: every reply is duplicated. Stream B: replies arrive 2.5 s after
    // the send, past the 2 s deadline.
    let script: Script = Box::new(|dest: IpAddr, _seq, _now| {
        if dest.to_string() == A {
            vec![FakeReply::Echo(Duration::from_millis(12)), FakeReply::Echo(Duration::from_millis(13))]
        } else {
            vec![FakeReply::Echo(Duration::from_millis(2_500))]
        }
    });
    let r = run(CONFIG, 4096, script, &[Duration::from_millis(10_300)], |_| {}).await;
    let a = r.stats(SA);
    assert_eq!((a.replied, a.duplicates), (11, 11));
    assert_eq!(a.max_us, Some(12_000), "the first reply's time stands");
    let b = r.stats(SB);
    assert_eq!(b.replied, 0);
    assert_eq!((b.timed_out, b.late), (8, 8), "timeouts stay timeouts; late replies are annotations");
    let late: Vec<_> = r
        .msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::LateReply(a)) => Some(a.late_rtt_us),
            _ => None,
        })
        .collect();
    assert!(late.iter().all(|&l| l == Some(2_500_000)));
}

#[tokio::test(start_paused = true)]
async fn backpressure_skips_slots_but_never_loses_a_sent_probes_result() {
    let loaded = Loaded::from_layers(Some(CONFIG), None, &[]).unwrap();
    let plan = loaded.plan(&|_| None);
    let clock = Arc::new(ManualClock::new());
    // Room for four messages only, and nobody reading for the first 20 s.
    let (tx, mut rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let (io, sent) = FakeIcmpIo::new(clock.clone(), replies(Some(12), Some(18)));
    let setup = Setup {
        config: Arc::new(loaded.config.clone()),
        plan,
        session: "bp".into(),
        config_rev: 1,
        clock: clock.clone(),
        tx,
        cancel: cancel.clone(),
        tls: None,
        resolver: None,
        dns_port: 53,
        pause: Default::default(),
        net: Some(Arc::new(FakeNet::default())),
        reload: None,
    };
    let handle = tokio::spawn(collector::run(setup, IcmpSetup::Use(io), IcmpSetup::<FakeIcmpIo>::Unavailable("n/a".into())));
    tokio::time::sleep(Duration::from_secs(20)).await;
    let sent_while_blocked = sent.lock().unwrap().len();
    assert!(sent_while_blocked <= 2, "with the processor stalled, probes stop being sent ({sent_while_blocked} went out)");

    cancel.cancel();
    let mut msgs = Vec::new();
    let counters = loop {
        match rx.recv().await {
            Some(m) => msgs.push(m),
            None => break handle.await.unwrap(),
        }
    };
    assert!(counters.backpressure_skips >= 30, "{counters:?}");
    assert_eq!(counters.dropped_records, 0);
    // Every probe that was announced as sent also delivered a final record.
    let announced: Vec<(u32, u64)> = msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Sent { stream, seq, .. } => Some((*stream, *seq)),
            _ => None,
        })
        .collect();
    assert_eq!(announced.len(), sent.lock().unwrap().len());
    let skipped = msgs
        .iter()
        .filter(|m| matches!(m, Msg::Record(Record::Observation(o)) if o.outcome == Outcome::Skipped && o.error.as_ref().unwrap().category == ErrorCategory::Backpressure))
        .count();
    assert_eq!(skipped as u64, counters.backpressure_skips, "each skipped slot is an explicit record");
    for (_, seq) in announced {
        assert!(msgs.iter().any(|m| matches!(m, Msg::Record(Record::Observation(o)) if o.seq == seq && o.outcome.was_sent())));
    }
}

#[tokio::test(start_paused = true)]
async fn unavailable_icmp_is_reported_as_unsupported_and_sends_nothing() {
    let loaded = Loaded::from_layers(Some(CONFIG), None, &[]).unwrap();
    let plan = loaded.plan(&|_| None);
    let clock = Arc::new(ManualClock::new());
    let (tx, mut rx) = mpsc::channel(256);
    let cancel = CancellationToken::new();
    let why = "unprivileged ICMP is disabled on this host for your groups";
    let setup = Setup {
        config: Arc::new(loaded.config.clone()),
        plan,
        session: "cap".into(),
        config_rev: 1,
        clock,
        tx,
        cancel: cancel.clone(),
        tls: None,
        resolver: None,
        dns_port: 53,
        pause: Default::default(),
        net: Some(Arc::new(FakeNet::default())),
        reload: None,
    };
    let handle = tokio::spawn(collector::run(setup, IcmpSetup::<FakeIcmpIo>::Unavailable(why.into()), IcmpSetup::<FakeIcmpIo>::Unavailable(why.into())));
    tokio::time::sleep(Duration::from_secs(10)).await;
    cancel.cancel();
    let mut msgs = Vec::new();
    while let Some(m) = rx.recv().await {
        msgs.push(m);
    }
    handle.await.unwrap();
    let start = msgs
        .iter()
        .find_map(|m| match m {
            Msg::Record(Record::Session(s)) if s.state == "start" => Some(s),
            _ => None,
        })
        .expect("session start record");
    assert!(start.streams.iter().all(|s| s.state == "unsupported" && s.reason.as_deref() == Some(why)));
    assert!(!msgs.iter().any(|m| matches!(m, Msg::Sent { .. } | Msg::Record(Record::Observation(_)))), "no probes and no outcomes: an unsupported stream is not a failing one");
}

/// A session with no ICMP stream opens no ICMP transport: an open one is
/// handed other programs' replies on macOS, to be read and counted for
/// nothing. A reload that brings the first ICMP stream opens it then.
#[tokio::test(start_paused = true)]
async fn an_icmp_transport_is_opened_when_a_stream_first_needs_it_and_not_before() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let without_icmp = format!("{CONFIG}\n[icmp]\nenabled = false\n");
    let loaded = Loaded::from_layers(Some(&without_icmp), None, &[]).unwrap();
    let plan = loaded.plan(&|_| None);
    assert_eq!(plan.collecting().count(), 0, "the streams are there and switched off");
    let clock = Arc::new(ManualClock::new());
    let (tx, mut rx) = mpsc::channel(4096);
    let (reload_tx, reload_rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    let (io, sent) = FakeIcmpIo::new(clock.clone(), replies(Some(12), Some(18)));
    let (opened_v4, opened_v6) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let v4 = {
        let opened = opened_v4.clone();
        IcmpSetup::OnDemand(Box::new(move || {
            opened.fetch_add(1, Ordering::SeqCst);
            Ok(io)
        }))
    };
    let v6 = {
        let opened = opened_v6.clone();
        IcmpSetup::<FakeIcmpIo>::OnDemand(Box::new(move || {
            opened.fetch_add(1, Ordering::SeqCst);
            Err("not under test".into())
        }))
    };
    let setup = Setup {
        config: Arc::new(loaded.config.clone()),
        plan,
        session: "on-demand".into(),
        config_rev: 1,
        clock: clock.clone(),
        tx,
        cancel: cancel.clone(),
        tls: None,
        resolver: None,
        dns_port: 53,
        pause: Default::default(),
        net: Some(Arc::new(FakeNet::default())),
        reload: Some(reload_rx),
    };
    let handle = tokio::spawn(collector::run(setup, v4, v6));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!((opened_v4.load(Ordering::SeqCst), opened_v6.load(Ordering::SeqCst)), (0, 0), "nothing asked for one");
    assert!(sent.lock().unwrap().is_empty());

    // ICMP switched on by a reload: the IPv4 transport is opened, once, and used.
    let with_icmp = Loaded::from_layers(Some(CONFIG), None, &[]).unwrap();
    let plan = with_icmp.plan(&|_| None);
    assert!(reload_tx.try_send(collector::Reload { config: Arc::new(with_icmp.config.clone()), plan }).is_ok());
    tokio::time::sleep(Duration::from_millis(5_300)).await;
    cancel.cancel();
    let mut msgs = Vec::new();
    while let Some(m) = rx.recv().await {
        msgs.push(m);
    }
    handle.await.unwrap();
    assert_eq!((opened_v4.load(Ordering::SeqCst), opened_v6.load(Ordering::SeqCst)), (1, 0), "IPv4 once; an IPv4-only plan never asks for IPv6");
    let answered = |stream: &str| msgs.iter().filter(|m| matches!(m, Msg::Record(Record::Observation(o)) if &*o.stream == stream && o.outcome == Outcome::Success)).count();
    assert!(answered(SA) >= 4 && answered(SB) >= 4, "both streams ran on it: {} and {}", answered(SA), answered(SB));
    assert_eq!(sent.lock().unwrap().len(), msgs.iter().filter(|m| matches!(m, Msg::Sent { .. })).count());
    // And the reload announced them as collecting, not as unsupported.
    let announced = msgs
        .iter()
        .find_map(|m| match m {
            Msg::Record(Record::Config(c)) => Some(c),
            _ => None,
        })
        .expect("the reload's record");
    assert!(announced.streams.iter().all(|s| s.state == "collecting"), "{:?}", announced.streams);
}

// ───────────────────────── network changes ─────────────────────────

fn of_kind<'a>(r: &'a Run, name: &str) -> Vec<&'a pathwatch_core::model::EventRec> {
    r.msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::Event(e)) if e.event == name => Some(e),
            _ => None,
        })
        .collect()
}

fn gaps(r: &Run) -> Vec<&pathwatch_core::model::GapRec> {
    r.msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::Gap(g)) => Some(g),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_network_change_cancels_probes_in_flight_and_is_one_gap_and_one_event() {
    // Both targets answer after 1.5 s, so there is always a probe in flight.
    // At 5.3 s the host moves from en0 to a tunnel with another address.
    let phases = [Duration::from_millis(5_300), Duration::from_millis(5_000)];
    let r = run_net(CONFIG, 4096, replies(Some(1_500), Some(1_500)), &phases, |_, _, net| net.set("198.51.100.7", "utun3", None)).await;

    // One gap, from the last look at the old network to the first at the new.
    let g = gaps(&r);
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!((g[0].cause, g[0].start_ns, g[0].end_ns, g[0].duration_ms), (GapCause::NetworkChange, 4_000_000_000, 6_000_000_000, 2_000));

    // One event for the context at start, one for the change, in words.
    let start = of_kind(&r, "source_context");
    assert_eq!(start.len(), 1);
    assert_eq!(start[0].detail, "source context 1: IPv4 192.0.2.10 on en0, gateway 192.0.2.1; IPv6 no route");
    let changed = of_kind(&r, "source_context_changed");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].at_ns, 6_000_000_000);
    assert!(
        changed[0].detail.starts_with("source context 2: IPv4 192.0.2.10 on en0, gateway 192.0.2.1 → 198.51.100.7 on utun3 (looks like a tunnel), no gateway found."),
        "{}",
        changed[0].detail
    );
    assert!(changed[0].detail.contains("cancelled, not counted as unanswered") && !changed[0].detail.contains("IPv6"), "only what changed is described: {}", changed[0].detail);

    for stream in [SA, SB] {
        let obs = r.observations(stream);
        // Probes in flight at the change were cancelled, not left to time out.
        let cancelled: Vec<_> = obs.iter().filter(|o| o.outcome == Outcome::Cancelled && o.sent_ns.is_some_and(|t| t < 6_000_000_000)).collect();
        assert!(!cancelled.is_empty(), "{stream}: a probe was in flight");
        assert!(cancelled.iter().all(|o| o.error.as_ref().map(|e| e.category) == Some(ErrorCategory::Gap) && o.sent_ns.unwrap() >= 4_000_000_000), "{stream}: {cancelled:?}");
        let s = r.stats(stream);
        assert_eq!((s.timed_out, s.unanswered), (0, 0), "{stream}: nothing is counted as unanswered: {s:?}");
        assert!(s.replied >= 6, "{stream}: probes before and after were answered: {s:?}");
        assert_identities(&s);
        // Every observation says which context it was sent in.
        let context_of = |o: &&pathwatch_core::model::Observation| o.src.as_ref().map(|src| (src.ctx, src.ip.map(|ip| ip.to_string()), src.ifname.clone()));
        let before = obs.iter().find(|o| o.sent_ns.is_some_and(|t| t < 4_000_000_000)).unwrap();
        let after = obs.iter().find(|o| o.sent_ns.is_some_and(|t| t > 6_500_000_000)).unwrap();
        assert_eq!(context_of(before), Some((1, Some("192.0.2.10".into()), Some("en0".into()))), "{stream}");
        assert_eq!(context_of(after), Some((2, Some("198.51.100.7".into()), Some("utun3".into()))), "{stream}");
    }

    // And no incident comes of it.
    let mut incidents = pathwatch_core::incident::IncidentTracker::new([pathwatch_core::model::Protocol::Icmp, pathwatch_core::model::Protocol::Icmp]);
    for m in &r.msgs {
        match m {
            Msg::Record(Record::Observation(o)) => incidents.on_observation(r.engine.stream_index(&o.stream).unwrap(), o),
            Msg::Record(Record::Gap(_)) => incidents.on_gap(),
            _ => {}
        }
    }
    assert!(incidents.events().is_empty(), "{:?}", incidents.events());
}

#[tokio::test(start_paused = true)]
async fn a_network_that_does_not_change_records_its_context_once_and_no_gap() {
    let r = run(CONFIG, 4096, replies(Some(12), Some(18)), &[Duration::from_millis(20_300)], |_| {}).await;
    assert_eq!(of_kind(&r, "source_context").len(), 1);
    assert!(of_kind(&r, "source_context_changed").is_empty() && gaps(&r).is_empty());
    assert!(r.observations(SA).iter().all(|o| o.src.as_ref().is_some_and(|s| s.ctx == 1)));
}

const WITH_GATEWAY: &str = r#"
[general]
families = ["ipv4"]

[[providers.cloudflare.targets]]
id = "a"
address = "192.0.2.1"
protocols = ["icmp"]

[controls.gateway]
enabled = true
"#;

#[tokio::test(start_paused = true)]
async fn the_gateway_control_follows_the_host_to_its_new_gateway_and_says_when_there_is_none() {
    const GW: &str = "gateway/auto/icmp/4";
    // Start behind 10.0.0.1; at 5.3 s move to a network whose gateway is
    // 10.9.9.1; at 11.3 s lose the network altogether.
    let phases = [Duration::from_millis(5_300), Duration::from_millis(6_000), Duration::from_millis(5_000)];
    let r = run_on(
        WITH_GATEWAY,
        4096,
        replies(Some(12), Some(2)),
        &phases,
        |net| net.set("10.0.0.20", "en0", Some("10.0.0.1")),
        |clock, _, net| if clock.mono_ns() < 8_000_000_000 { net.set("10.9.9.20", "en1", Some("10.9.9.1")) } else { net.disconnect() },
    )
    .await;
    assert_eq!(of_kind(&r, "source_context_changed").len(), 2);
    assert_eq!(gaps(&r).iter().filter(|g| g.cause == GapCause::NetworkChange).count(), 2);

    let obs = r.observations(GW);
    let dest = |o: &&pathwatch_core::model::Observation| (o.dest_ip.map(|ip| ip.to_string()), o.epoch);
    let first = obs.iter().find(|o| o.outcome == Outcome::Success).unwrap();
    assert_eq!(dest(first), (Some("10.0.0.1".into()), 1));
    // On the second network it probes that network's gateway, as a new epoch:
    // the two gateways' statistics are never pooled.
    let second = obs.iter().find(|o| o.outcome == Outcome::Success && o.sent_ns.is_some_and(|t| t > 6_500_000_000)).expect("probes to the new gateway");
    assert_eq!(dest(second), (Some("10.9.9.1".into()), 2));
    assert!(
        obs.iter().filter(|o| o.sent_ns.is_some_and(|t| t > 6_500_000_000 && t < 11_000_000_000)).all(|o| o.dest_ip.map(|ip| ip.to_string()).as_deref() == Some("10.9.9.1")),
        "nothing goes to the old gateway"
    );
    // With no network there is no gateway to probe: local errors that say so, and nothing sent.
    let last = obs.last().unwrap();
    assert_eq!(last.outcome, Outcome::LocalError);
    assert!(last.error.as_ref().and_then(|e| e.detail.as_deref()).is_some_and(|d| d.contains("no default gateway found")), "{:?}", last.error);
    assert_eq!(r.sent.iter().filter(|p| p.dest.to_string().starts_with("10.") && p.at_ns > 12_500_000_000).count(), 0);
    let changes = of_kind(&r, "source_context_changed");
    assert!(changes[1].detail.contains("→ no route"), "{}", changes[1].detail);
}

// ───────────────────────── configuration reload ─────────────────────────

fn config_records(r: &Run) -> Vec<&pathwatch_core::model::ConfigRec> {
    r.msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(Record::Config(c)) => Some(c),
            _ => None,
        })
        .collect()
}

/// As `CONFIG`, with stream B pointed at another address and a third stream added.
const RELOADED: &str = r#"
[general]
families = ["ipv4"]

[[providers.cloudflare.targets]]
id = "a"
address = "192.0.2.1"
protocols = ["icmp"]

[[providers.cloudflare.targets]]
id = "c"
address = "192.0.2.4"
protocols = ["icmp"]

[[providers.google.targets]]
id = "b"
address = "192.0.2.3"
protocols = ["icmp"]
"#;

#[tokio::test(start_paused = true)]
async fn a_reload_leaves_unchanged_streams_alone_restarts_changed_ones_and_tags_what_follows() {
    const SC: &str = "cloudflare/c/icmp/4";
    let phases = [Duration::from_millis(5_300), Duration::from_millis(5_000)];
    let r = run_full(CONFIG, 4096, replies(Some(12), Some(18)), &phases, |_| {}, |h| h.reload(RELOADED)).await;
    let at = 5_300_000_000u64;

    // One revision record, saying what it runs and what changed.
    let revs = config_records(&r);
    assert_eq!(revs.len(), 1);
    assert_eq!((revs[0].rev, revs[0].at_ns, revs[0].summary.as_str()), (2, at, "1 started, 0 stopped, 1 restarted, 1 unchanged"));
    assert_eq!(revs[0].streams.iter().map(|s| (&*s.stream, &*s.configured)).collect::<Vec<_>>(), vec![(SA, "192.0.2.1"), (SC, "192.0.2.4"), (SB, "192.0.2.3")]);
    let event = of_kind(&r, "config_reloaded");
    assert_eq!(event.len(), 1);
    assert_eq!(event[0].detail, "configuration revision 2: 1 started, 0 stopped, 1 restarted, 1 unchanged");

    // Every observation carries the revision it was made under.
    for o in r.msgs.iter().filter_map(|m| if let Msg::Record(Record::Observation(o)) = m { Some(o) } else { None }) {
        assert_eq!(o.config_rev, if o.sched_ns < at { 1 } else { 2 }, "{} seq {} at {}", o.stream, o.seq, o.sched_ns);
    }

    // The unchanged stream never noticed: same schedule, same numbering,
    // same epoch, one unbroken history across the reload.
    let a = r.observations(SA);
    assert_eq!(a.iter().map(|o| o.seq).collect::<Vec<_>>(), (0..=10).collect::<Vec<_>>());
    assert!(a.iter().all(|o| o.sched_ns == o.seq * 1_000_000_000 && o.epoch == 1 && o.outcome == Outcome::Success), "{a:?}");
    let sa = r.stats(SA);
    assert_eq!((sa.sent, sa.replied), (11, 11), "its statistics span the reload: {sa:?}");

    // The changed stream stopped and started again: a new destination, a
    // new epoch, sequence numbers that carry on, and nothing more to the old address.
    let b = r.observations(SB);
    let old: Vec<_> = b.iter().filter(|o| o.sched_ns < at).copied().collect();
    let new: Vec<_> = b.iter().filter(|o| o.sched_ns >= at).copied().collect();
    assert_eq!(old.len(), 5, "0.5 s to 4.5 s");
    assert!(old.iter().all(|o| o.dest_ip.map(|ip| ip.to_string()).as_deref() == Some("192.0.2.2") && o.epoch == 1));
    assert!(new.len() >= 4 && new.iter().all(|o| o.dest_ip.map(|ip| ip.to_string()).as_deref() == Some("192.0.2.3") && o.epoch == 2), "{new:?}");
    let seqs: Vec<u64> = b.iter().map(|o| o.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "no sequence number is used twice: {seqs:?}");
    assert_eq!(r.sent.iter().filter(|p| p.dest.to_string() == "192.0.2.2" && p.at_ns > at).count(), 0);
    // Its statistics are the new definition's alone: the two are not pooled.
    let sb = r.stats(SB);
    assert_eq!((sb.sent, sb.replied), (new.len() as u64, new.len() as u64), "{sb:?}");

    // The added stream exists from the reload on, after the streams already known.
    assert_eq!(r.engine.stream_index(SC), Some(2), "appended, so earlier indices keep their meaning");
    let c = r.observations(SC);
    assert!(c.len() >= 4 && c.iter().all(|o| o.sched_ns > at && o.config_rev == 2 && o.outcome == Outcome::Success), "{c:?}");
    assert_eq!(r.stats(SC).sent, c.len() as u64);
    for stream in [SA, SB, SC] {
        assert_identities(&r.stats(stream));
    }
    // A reload is not a gap and not a change of network.
    assert!(gaps(&r).is_empty() && of_kind(&r, "source_context_changed").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_reload_that_removes_a_stream_stops_it_and_keeps_what_it_measured() {
    let without_b = format!("{CONFIG}\n[providers.google]\nenabled = false\n");
    let phases = [Duration::from_millis(5_300), Duration::from_millis(5_000)];
    let r = run_full(CONFIG, 4096, replies(Some(12), Some(18)), &phases, |_| {}, |h| h.reload(&without_b)).await;
    let revs = config_records(&r);
    assert_eq!(revs[0].summary, "0 started, 1 stopped, 0 restarted, 1 unchanged");
    // A provider that is switched off has no streams; the record says so of the provider.
    assert!(revs[0].streams.iter().all(|s| &*s.stream != SB));
    assert_eq!(revs[0].providers.iter().find(|p| &*p.key == "google").map(|p| p.state.as_str()), Some("disabled"));
    assert_eq!(r.sent.iter().filter(|p| p.dest.to_string() == B && p.at_ns > 5_300_000_000).count(), 0, "nothing more is sent to it");
    // What it measured before is still there, exactly.
    let b = r.engine.session_stats(r.engine.stream_index(SB).unwrap(), r.end_ns);
    assert_eq!((b.sent, b.replied, b.timed_out), (5, 5, 0), "{b:?}");
    assert_eq!(r.stats(SA).sent, 11, "and the other stream ran on");

    // Reloading the same configuration changes nothing but the revision.
    let same = run_full(CONFIG, 4096, replies(Some(12), Some(18)), &phases, |_| {}, |h| h.reload(CONFIG)).await;
    assert_eq!(config_records(&same)[0].summary, "0 started, 0 stopped, 0 restarted, 2 unchanged");
    assert_eq!((same.stats(SA).sent, same.stats(SB).sent), (11, 10));
    assert!(same.observations(SB).iter().all(|o| o.epoch == 1), "an unchanged stream does not start a new epoch");
}

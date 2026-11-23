//! Properties that must hold for every scripted scenario, plus what each
//! scenario is there to show.

use std::time::Duration;

use pathwatch_core::engine::{Engine, HistoryLimits};
use pathwatch_core::model::{Line, Msg, Outcome, Record};
use pathwatch_core::script::{self, Scenario};
use pathwatch_core::stats::WindowStats;

const SEED: u64 = 7;

fn lines(msgs: &[Msg]) -> Vec<String> {
    msgs.iter()
        .filter_map(|m| match m {
            Msg::Record(r) => Some(Line::new(r.clone()).to_json()),
            Msg::Sent { .. } => None,
        })
        .collect()
}

fn engine_for(sc: &Scenario, msgs: &[Msg]) -> Engine {
    let mut e = Engine::new(sc.infos(), HistoryLimits::default());
    for m in msgs {
        e.ingest(m);
    }
    e
}

fn stats(sc: &Scenario, e: &Engine, stream: &str, window_s: u64) -> WindowStats {
    let idx = e.stream_index(stream).unwrap_or_else(|| panic!("{}: no stream {stream}", sc.name));
    e.stats(idx, Duration::from_secs(window_s), sc.duration.as_nanos() as u64)
}

const CF: &str = "cloudflare/resolver-v4/icmp/4";
const GO: &str = "google/resolver-v4/icmp/4";
const ND: &str = "nextdns/anycast-v4/icmp/4";
const AR: &str = "arelion/router/icmp/4";

#[test]
fn every_scenario_is_deterministic_and_internally_consistent() {
    let all = script::all();
    assert_eq!(all.len(), 13);
    for sc in &all {
        let msgs = script::generate(sc, SEED);
        assert_eq!(lines(&msgs), lines(&script::generate(sc, SEED)), "{}: same seed, same bytes", sc.name);
        assert_ne!(lines(&msgs), lines(&script::generate(sc, SEED + 1)), "{}: the seed matters", sc.name);

        let e = engine_for(sc, &msgs);
        assert_eq!(e.unknown_stream_msgs, 0, "{}", sc.name);
        for i in 0..e.stream_count() {
            for window in [10u64, 60, 300, 3600] {
                let s = e.stats(i, Duration::from_secs(window), sc.duration.as_nanos() as u64);
                let id = &e.stream_info(i).id;
                assert_eq!(s.sent, s.pending + s.replied + s.timed_out + s.unreachable + s.service_error + s.protocol_error + s.cancelled, "{} {id} {window}s", sc.name);
                assert_eq!(s.slots, s.sent + s.local_error + s.skipped, "{} {id} {window}s", sc.name);
                assert_eq!(s.pending, 0, "{} {id}: a finished session has nothing pending", sc.name);
                assert!(s.n <= s.replied);
            }
        }

        // A probe without a reply never carries a round-trip time — not zero,
        // not the deadline — and every success carries one.
        for m in &msgs {
            if let Msg::Record(Record::Observation(o)) = m {
                match o.outcome {
                    Outcome::Success | Outcome::ServiceError => assert!(o.rtt_us.is_some_and(|r| r > 0), "{}: {o:?}", sc.name),
                    _ => assert_eq!((o.rtt_us, o.recv_ns), (None, None), "{}: {:?} seq {}", sc.name, o.outcome, o.seq),
                }
            }
        }
        for line in lines(&msgs) {
            if line.contains("\"outcome\":\"timeout\"") || line.contains("\"outcome\":\"cancelled\"") || line.contains("\"outcome\":\"local_error\"") {
                assert!(!line.contains("rtt_us"), "{}: {line}", sc.name);
            }
        }
    }
}

#[test]
fn statistics_do_not_depend_on_how_often_they_are_read() {
    // Reading statistics after every message (as a fast renderer would) and
    // reading them once at the end must agree: reads have no side effects.
    let sc = script::by_name("isolated-failure").unwrap();
    let msgs = script::generate(&sc, SEED);
    let end = sc.duration.as_nanos() as u64;
    let mut busy = Engine::new(sc.infos(), HistoryLimits::default());
    for (i, m) in msgs.iter().enumerate() {
        busy.ingest(m);
        if i % 3 == 0 {
            for s in 0..busy.stream_count() {
                let _ = busy.stats(s, Duration::from_secs(60), end);
            }
        }
    }
    let quiet = engine_for(&sc, &msgs);
    for s in 0..quiet.stream_count() {
        for w in [60u64, 300] {
            assert_eq!(busy.stats(s, Duration::from_secs(w), end), quiet.stats(s, Duration::from_secs(w), end));
        }
    }
}

#[test]
fn healthy() {
    let sc = script::by_name("healthy").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    for id in [CF, GO, ND, AR] {
        let s = stats(&sc, &e, id, 300);
        assert_eq!((s.timed_out, s.unanswered, s.local_error, s.skipped), (0, 0, 0, 0), "{id}");
        assert_eq!(s.no_reply_rate, Some(0.0));
        // 180 s of a 300 s window.
        assert!((s.coverage - 0.6).abs() < 0.01, "{id}: {}", s.coverage);
        assert!(s.p50_us.value().is_some() && s.p95_us.value().is_some() && s.p99_us.value().is_some());
    }
    let cf = stats(&sc, &e, CF, 300);
    let p50 = cf.p50_us.value().unwrap();
    assert!((11_000.0..13_000.0).contains(&p50), "{p50}");
}

#[test]
fn isolated_failure_touches_one_stream_only() {
    let sc = script::by_name("isolated-failure").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    let ar = stats(&sc, &e, AR, 300);
    assert_eq!(ar.timed_out, 40);
    assert_eq!(ar.longest_unanswered_run, 40);
    assert_eq!(ar.unanswered_run, 0, "it recovered");
    let rate = ar.no_reply_rate.unwrap();
    assert!((rate - 40.0 / ar.finalised as f64).abs() < 1e-9);
    for id in [CF, GO, ND] {
        assert_eq!(stats(&sc, &e, id, 300).unanswered, 0, "{id}");
    }
}

#[test]
fn correlated_spike_moves_latency_not_loss() {
    let sc = script::by_name("correlated-spike").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    for id in [CF, GO, ND, AR] {
        let s = stats(&sc, &e, id, 300);
        assert_eq!(s.unanswered, 0);
        assert!(f64::from(s.max_us.unwrap()) > s.p50_us.value().unwrap() + 35_000.0, "{id}: the spike is visible in the maximum");
        assert!(s.p50_us.value().unwrap() < 35_000.0, "{id}: and does not move the median");
    }
}

#[test]
fn dns_only_failure_separates_error_responses_from_silence() {
    let sc = script::by_name("dns-only").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    let dns = stats(&sc, &e, "cloudflare/resolver-v4/dns-udp/4", 300);
    assert_eq!((dns.service_error, dns.timed_out), (4, 4));
    assert_eq!(dns.errors, 4);
    assert_eq!(dns.unanswered, 4, "SERVFAIL is an error response, not an unanswered query");
    assert_eq!(dns.n, dns.replied, "error responses do not enter latency statistics");
    for id in [CF, GO, ND, AR] {
        assert_eq!(stats(&sc, &e, id, 300).unanswered, 0);
    }
}

#[test]
fn unsupported_and_unconfigured_streams_have_no_outcomes_at_all() {
    for (name, quiet) in [("missing-permission", vec![CF, GO, ND, AR]), ("needs-config", vec![ND, AR])] {
        let sc = script::by_name(name).unwrap();
        let msgs = script::generate(&sc, SEED);
        let e = engine_for(&sc, &msgs);
        for id in quiet {
            let s = stats(&sc, &e, id, 300);
            assert_eq!((s.slots, s.sent, s.finalised), (0, 0, 0), "{name} {id}");
            assert_eq!(s.no_reply_rate, None, "{name} {id}: no rate, and certainly not 100 %");
        }
    }
}

#[test]
fn address_change_restarts_statistics_in_a_new_epoch() {
    let sc = script::by_name("address-change").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    let i = e.stream_index(ND).unwrap();
    assert_eq!(e.current_epoch(i), 2);
    let s = stats(&sc, &e, ND, 300);
    // Only the 120 s since the change count; the old destination's 24 ms
    // samples are not pooled with the new destination's 41 ms.
    assert_eq!(s.replied, 120);
    assert!(f64::from(s.min_us.unwrap()) > 35_000.0, "{:?}", s.min_us);
}

#[test]
fn collector_overload_is_never_network_loss() {
    let sc = script::by_name("collector-overload").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    for id in [CF, GO, ND, AR] {
        let s = stats(&sc, &e, id, 300);
        assert_eq!(s.skipped, 20, "{id}");
        assert_eq!((s.unanswered, s.no_reply_rate), (0, Some(0.0)), "{id}");
        assert!(s.coverage < 0.6, "{id}: skipped slots reduce coverage");
    }
}

#[test]
fn sleep_resume_is_a_gap_not_an_outage() {
    let sc = script::by_name("sleep-resume").unwrap();
    let msgs = script::generate(&sc, SEED);
    let e = engine_for(&sc, &msgs);
    assert_eq!(e.gaps().len(), 1);
    assert_eq!(e.gaps()[0].duration_ms, 720_000);
    for id in [CF, GO, ND, AR] {
        // No timeouts anywhere: probes in flight at the suspend were cancelled.
        let all = stats(&sc, &e, id, 3600);
        assert_eq!(all.timed_out, 0, "{id}");
        assert!(all.cancelled <= 2, "{id}");
        // The last five minutes of real time contain only post-resume probes.
        let recent = stats(&sc, &e, id, 300);
        assert!((115..=121).contains(&recent.sent), "{id}: {}", recent.sent);
        assert_eq!(recent.no_reply_rate, Some(0.0));
    }
    // 180 s of monotonic time produced ~180 probes per stream — not 900.
    let sent = msgs.iter().filter(|m| matches!(m, Msg::Sent { stream: 0, .. })).count();
    assert_eq!(sent, 180);
    // The RTT-variation pairing does not bridge the gap.
    let cf = stats(&sc, &e, CF, 3600);
    assert_eq!(cf.delta_pairs, cf.n - 2, "one pair is broken by the gap, in addition to the usual n − 1");
}

#[test]
fn late_duplicates_and_wraparound() {
    let sc = script::by_name("late-dup-reorder").unwrap();
    let msgs = script::generate(&sc, SEED);
    let e = engine_for(&sc, &msgs);
    // Sequence numbers run from 65500 across 65535 → 65536 without incident.
    let cf = stats(&sc, &e, CF, 300);
    assert_eq!((cf.replied, cf.unanswered), (180, 0));
    assert_eq!(cf.delta_pairs, 179, "adjacency holds across the 16-bit boundary");
    // Late replies leave their probes as timeouts.
    let go = stats(&sc, &e, GO, 300);
    assert_eq!((go.timed_out, go.late), (20, 20));
    // Duplicates are counted and change nothing else.
    let nd = stats(&sc, &e, ND, 300);
    assert_eq!((nd.duplicates, nd.replied, nd.unanswered), (20, 180, 0));
}

#[test]
fn flapping_and_missing_ipv6() {
    let sc = script::by_name("flapping").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    let ar = stats(&sc, &e, AR, 300);
    assert_eq!((ar.timed_out, ar.longest_unanswered_run), (30, 6));

    let sc = script::by_name("ipv6-missing").unwrap();
    let e = engine_for(&sc, &script::generate(&sc, SEED));
    let v6 = stats(&sc, &e, "cloudflare/resolver-v6/icmp/6", 300);
    assert_eq!((v6.local_error, v6.sent, v6.finalised), (180, 0, 0));
    assert_eq!(v6.no_reply_rate, None);
    assert_eq!(stats(&sc, &e, CF, 300).unanswered, 0, "IPv4 is unaffected and stays visible separately");
}

/// Guards the JSONL schema: these lines are what a version-1 reader expects.
#[test]
fn schema_v1_golden() {
    let sc = script::by_name("late-dup-reorder").unwrap();
    let msgs = script::generate(&sc, SEED);
    let all = lines(&msgs);
    let pick = |needle: &str| all.iter().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no line containing {needle}")).clone();
    let sample = [pick("\"outcome\":\"success\""), pick("\"outcome\":\"timeout\""), pick("\"kind\":\"late_reply\""), pick("\"kind\":\"duplicate\"")].join("\n") + "\n";
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/schema_v1.jsonl");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
        std::fs::write(path, &sample).unwrap();
    }
    let golden = std::fs::read_to_string(path).expect("golden file exists; run with UPDATE_GOLDEN=1 to create it");
    assert_eq!(sample, golden, "the JSONL schema changed; bump SCHEMA_VERSION and regenerate deliberately");
    // Every line round-trips through the typed schema.
    for l in golden.lines() {
        let parsed: Line = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
        assert_eq!(parsed.schema, 1);
        assert_eq!(parsed.to_json(), l);
    }
}

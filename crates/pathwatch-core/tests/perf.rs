//! What a snapshot costs as the history behind it grows. Run on request:
//!
//! ```sh
//! cargo test --release -p pathwatch-core --test perf -- --ignored --nocapture
//! ```
//!
//! The dashboard builds a snapshot ten times a second, so each millisecond
//! here is one per cent of a processor core, for as long as the session
//! runs. Nothing is sent: the history is made up, a day of it in seconds.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use pathwatch_core::engine::{Engine, HistoryLimits};
use pathwatch_core::incident::IncidentTracker;
use pathwatch_core::model::{Family, Msg, Observation, Outcome, Protocol, Record};
use pathwatch_core::script::{self, Scenario};
use pathwatch_core::snapshot::{self, ViewSpec};

const SEC: u64 = 1_000_000_000;

/// Counts the bytes this process has allocated and not yet freed, so that
/// what the history weighs can be read exactly instead of guessed from the
/// resident size.
struct Counting;

static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// SAFETY: every call is passed straight to the system allocator with the
// arguments it was given; the only addition is a counter.
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), std::sync::atomic::Ordering::Relaxed);
        // SAFETY: the caller's contract is the system allocator's own.
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        LIVE.fetch_sub(layout.size(), std::sync::atomic::Ordering::Relaxed);
        // SAFETY: as above.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size, std::sync::atomic::Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), std::sync::atomic::Ordering::Relaxed);
        // SAFETY: as above.
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn live_mb() -> f64 {
    LIVE.load(std::sync::atomic::Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
}

/// What the history of the workload weighs, in allocated bytes, after a
/// given time — and how much of it is the full records.
#[test]
#[ignore = "a measurement; see the file comment"]
fn perf_memory_of_the_history() {
    let sc = workload();
    let mut template: HashMap<String, Observation> = HashMap::new();
    for m in script::generate(&sc, 7) {
        if let Msg::Record(Record::Observation(o)) = m
            && o.outcome == Outcome::Success
        {
            let mut o = *o;
            // As a live record has them: where it was sent from and when.
            o.src = Some(pathwatch_core::model::Source { ctx: 1, ifname: Some("en0".into()), ip: Some("192.0.2.10".parse().unwrap()) });
            template.entry(o.stream.to_string()).or_insert(o);
        }
    }
    let infos = sc.infos();
    println!(
        "PERF | sizes | — | a sample {} bytes; a full observation {} bytes and what is kept of it {} bytes, each before what it points to | —",
        std::mem::size_of::<pathwatch_core::stats::Sample>(),
        std::mem::size_of::<Observation>(),
        std::mem::size_of::<pathwatch_core::engine::Kept>()
    );
    {
        use pathwatch_core::model::*;
        use std::mem::size_of;
        println!(
            "PERF | parts of a record | — | dns {} · conn {} · error {} · source {} · phases {} · icmp {} · backend {} · address {} · flags {} · utc {} | —",
            size_of::<Option<DnsInfo>>(),
            size_of::<Option<ConnInfo>>(),
            size_of::<Option<ErrorInfo>>(),
            size_of::<Option<Source>>(),
            size_of::<Option<Phases>>(),
            size_of::<Option<IcmpInfo>>(),
            size_of::<Backend>(),
            size_of::<Option<std::net::IpAddr>>(),
            size_of::<Vec<Flag>>(),
            size_of::<Option<String>>()
        );
    }
    let mut ok = true;
    for (label, records) in [("with full records for an hour", 3600u64), ("with no full records", 0)] {
        let before = live_mb();
        let mut engine = Engine::new(infos.clone(), HistoryLimits { samples: Duration::from_secs(24 * 3600), records: Duration::from_secs(records), events: 10_000 });
        let mut incidents = IncidentTracker::new(infos.iter().map(|s| s.protocol));
        let mut next = 0u64;
        for (after, history_s) in [("10 minutes", 600u64), ("1 hour", 3600), ("6 hours", 6 * 3600), ("24 hours", 24 * 3600)] {
            while next < history_s {
                second(next, false, &infos, &template, &mut engine, &mut incidents);
                next += 1;
            }
            let mb = live_mb() - before;
            // The whole program has 64 MB; about 13 of them are in use before any history exists.
            let within = records == 0 || mb <= 48.0;
            ok &= within;
            println!(
                "PERF | history of {} streams after {after}, {label} | ≤ 48 MB of the 64 MB | {mb:.1} MB allocated | {}",
                infos.len(),
                if records == 0 {
                    "—"
                } else if within {
                    "within"
                } else {
                    "OVER"
                }
            );
        }
        drop(engine);
    }
    assert!(ok, "the history outgrows its share of the memory budget; see the PERF rows");
}

/// The plan's workload W1 in shape: four providers, both families, ICMP
/// every second and DNS over UDP and HTTPS every five, and the two controls.
fn workload() -> Scenario {
    let mut streams = Vec::new();
    for (i, provider) in ["cloudflare", "google", "nextdns", "arelion"].into_iter().enumerate() {
        for (target, family, address) in [("v4", Family::V4, "192.0.2.1"), ("v6", Family::V6, "2001:db8::1")] {
            for protocol in [Protocol::Icmp, Protocol::DnsUdp, Protocol::Doh] {
                streams.push(script::stream(provider, target, protocol, family, address, 12 + 6 * i as u32, 900, 100 * i as u64));
            }
        }
    }
    streams.push(script::stream(pathwatch_core::localnet::GATEWAY, "auto", Protocol::Icmp, Family::V4, "192.0.2.254", 2, 300, 333));
    streams.push(script::stream(pathwatch_core::localnet::LOCAL_DNS, "auto", Protocol::DnsUdp, Family::V4, "192.0.2.53", 9, 600, 1_666));
    Scenario { name: "perf", shows: "", duration: Duration::from_secs(11), streams, gaps: Vec::new() }
}

/// One second of the workload: every probe due is sent and answered.
fn second(at_s: u64, refused: bool, infos: &[pathwatch_core::engine::StreamInfo], template: &HashMap<String, Observation>, engine: &mut Engine, incidents: &mut IncidentTracker) {
    for (idx, info) in infos.iter().enumerate() {
        let every = info.interval.as_secs().max(1);
        if !at_s.is_multiple_of(every) {
            continue;
        }
        let mut o = template[&*info.id].clone();
        let sent = at_s * SEC + idx as u64 * 1_000_000;
        let rtt = o.rtt_us.unwrap_or(10_000) + ((at_s * 7919 + idx as u64 * 104_729) % 900) as u32;
        (o.seq, o.transaction, o.sched_ns, o.sent_ns, o.recv_ns, o.rtt_us) = (at_s / every, at_s / every, sent, Some(sent), Some(sent + u64::from(rtt) * 1_000), Some(rtt));
        // The loopback workload of the terminal measurements: nothing listens for DNS, so every query is refused.
        if refused && info.protocol != Protocol::Icmp {
            (o.outcome, o.rtt_us, o.recv_ns) = (Outcome::Unreachable, None, None);
            o.error = Some(pathwatch_core::model::ErrorInfo::new(pathwatch_core::model::ErrorCategory::ConnectionRefused));
        }
        // As the collector reports it: the send, then the result.
        engine.ingest(&Msg::Sent { stream: idx as u32, seq: o.seq, sent_ns: sent });
        let msg = Msg::obs(o);
        engine.ingest(&msg);
        if let Msg::Record(Record::Observation(o)) = &msg {
            incidents.on_observation(idx, o);
        }
    }
}

#[test]
#[ignore = "a measurement; see the file comment"]
fn perf_snapshot_cost_against_history() {
    snapshot_cost(false);
}

/// The same with every DNS stream refused throughout: incidents open on all
/// of them at once, so the grouping of incidents is at work in every snapshot.
#[test]
#[ignore = "a measurement; see the file comment"]
fn perf_snapshot_cost_with_every_dns_stream_failing() {
    snapshot_cost(true);
}

fn snapshot_cost(refused: bool) {
    if cfg!(debug_assertions) {
        panic!("this is a measurement of a release build: add --release");
    }
    let sc = workload();
    let (providers, metas) = sc.metas();
    // One answered probe of each stream to copy from.
    let mut template: HashMap<String, Observation> = HashMap::new();
    for m in script::generate(&sc, 7) {
        if let Msg::Record(Record::Observation(o)) = m
            && o.outcome == Outcome::Success
        {
            template.entry(o.stream.to_string()).or_insert(*o);
        }
    }
    let infos = sc.infos();
    assert_eq!(template.len(), infos.len(), "a template for every stream");
    let mut engine = Engine::new(infos.clone(), HistoryLimits { samples: Duration::from_secs(24 * 3600), records: Duration::from_secs(3600), events: 10_000 });
    let mut incidents = IncidentTracker::new(infos.iter().map(|s| s.protocol));

    let live = ViewSpec::default();
    let mut ok = true;
    let mut next = 0u64;
    for (label, history_s) in [("10 minutes", 600u64), ("1 hour", 3600), ("6 hours", 6 * 3600), ("24 hours", 24 * 3600)] {
        while next < history_s {
            second(next, refused, &infos, &template, &mut engine, &mut incidents);
            next += 1;
        }
        // A minute as the dashboard lives it: new results every second, a
        // snapshot every tenth of one. First live, then frozen a minute back,
        // then live with the window at 24 hours.
        let mut minute = |frozen: bool, window_s: u64| {
            let frozen_at = frozen.then_some(next * SEC);
            let spec = ViewSpec { frozen_at_ns: frozen_at, window: Duration::from_secs(window_s), ..live.clone() };
            let mut spent = Duration::ZERO;
            for _ in 0..60 {
                second(next, refused, &infos, &template, &mut engine, &mut incidents);
                for tenth in 0..10u64 {
                    let now = next * SEC + tenth * SEC / 10 + SEC / 20;
                    let start = Instant::now();
                    std::hint::black_box(snapshot::build(&engine, &incidents, &providers, &metas, &spec, now));
                    spent += start.elapsed();
                }
                next += 1;
            }
            spent.as_secs_f64() * 1000.0 / 600.0
        };
        let ms_live = minute(false, 300);
        let ms_frozen = minute(true, 300);
        let ms_day = minute(false, 24 * 3600);
        // The live and frozen five-minute views are what the dashboard shows
        // by default, all day. One millisecond is one per cent of a core.
        let within = ms_live <= 1.0 && ms_frozen <= 1.0;
        ok &= within;
        // What the history itself weighs: this process holds nothing else of any size.
        let resident_mb = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
            .map_or(0.0, |kb| kb / 1024.0);
        println!(
            "PERF | history of {} streams{} after {label} | part of the 64 MB resident budget | {resident_mb:.0} MB resident in this test process | —",
            infos.len(),
            if refused { ", DNS refused" } else { "" }
        );
        println!(
            "PERF | snapshot of {} streams{} after {label} | mean ≤ 1 ms for the 5-minute view | live {ms_live:.2} ms, frozen {ms_frozen:.2} ms; 24-hour window {ms_day:.2} ms | {}",
            infos.len(),
            if refused { ", DNS refused" } else { "" },
            if within { "within" } else { "OVER" }
        );
    }
    assert!(ok, "a snapshot costs more than its share of the CPU budget; see the PERF rows");
}

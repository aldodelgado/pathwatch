//! The plain-text summary: one fixed-width row per stream with the metrics
//! from the metric dictionary, plus everything that was *not* measured and why.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::time::Duration;

use pathwatch_core::config::{Dur, Plan, PlanState};
use pathwatch_core::engine::Engine;
use pathwatch_core::model::{CollectorCounters, Family, Outcome, StreamRec, TrafficRow};
use pathwatch_core::stats::{Gated, WindowStats, nearest_rank};
use pathwatch_core::traffic::BudgetRow;

fn ms(us: f64) -> String {
    format!("{:.1}", us / 1000.0)
}

/// A gated statistic: withheld as `—` below its minimum sample count, marked
/// with `·` while the count is low.
fn gated(g: Gated) -> String {
    match g {
        Gated::Ok(v) => ms(v),
        Gated::LowN(v) => format!("{}·", ms(v)),
        Gated::Insufficient => "—".into(),
    }
}

/// A rate as a percentage once there are enough finalised probes, otherwise
/// the raw fraction.
fn rate(rate: Option<f64>, k: u64, n: u64) -> String {
    match rate {
        Some(r) => format!("{:.1}%", r * 100.0),
        None if n == 0 => "—".into(),
        None => format!("{k}/{n}"),
    }
}

/// For streams whose round trips end at the kernel's receive stamp: how long
/// replies then waited for this process (excluded from the times shown), and
/// what the round trip would read if that wait were included.
fn receive_delay(engine: &Engine, idx: usize) -> Option<String> {
    let pairs: Vec<(u32, u32)> = engine.records(idx).filter(|o| o.outcome == Outcome::Success).filter_map(|o| Some((o.recv_delay_us?, o.rtt_user_us?))).collect();
    if pairs.is_empty() {
        return None;
    }
    let mut delays: Vec<u32> = pairs.iter().map(|p| p.0).collect();
    let mut user: Vec<u32> = pairs.iter().map(|p| p.1).collect();
    delays.sort_unstable();
    user.sort_unstable();
    let q = |v: &[u32], p: u64| f64::from(nearest_rank(v, p, 100).unwrap_or(0)) / 1000.0;
    Some(format!(
        "kernel-stamped on receive; our own receive delay, excluded: median {:.2} · p95 {:.2} · max {:.2} ms — with it the median would read {:.1} ms",
        q(&delays, 50),
        q(&delays, 95),
        q(&delays, 100),
        q(&user, 50),
    ))
}

/// The most frequent reason recent slots on a stream did not succeed, with the
/// address that was actually dialled — so a row of errors explains itself.
fn dominant_error(engine: &Engine, idx: usize) -> Option<String> {
    let mut counts: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    for o in engine.records(idx).filter(|o| o.attempt == 1) {
        let Some(e) = &o.error else { continue };
        if matches!(o.outcome, Outcome::Cancelled | Outcome::Skipped) {
            continue;
        }
        let category = serde_json::to_value(e.category).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        let dest = o.dest_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "no address".into());
        *counts.entry((category, e.detail.clone().unwrap_or_default(), dest)).or_default() += 1;
    }
    let ((category, detail, dest), n) = counts.into_iter().max_by_key(|(_, n)| *n)?;
    let detail = if detail.is_empty() { String::new() } else { format!(": {detail}") };
    Some(format!("{n}× {}{detail} (destination {dest})", category.replace('_', " ")))
}

/// Outbound traffic as measured, beside what the configuration implies.
fn traffic_table(out: &mut String, traffic: &[TrafficRow], budget: &[BudgetRow], elapsed_s: f64, dim: &str, reset: &str) {
    let mut keys: Vec<(String, Family)> = budget.iter().map(|b| (b.kind.as_str().to_string(), b.family)).collect();
    for t in traffic {
        if !keys.contains(&(t.kind.clone(), t.family)) {
            keys.push((t.kind.clone(), t.family));
        }
    }
    if keys.is_empty() || elapsed_s <= 0.0 {
        return;
    }
    let _ = writeln!(out, "traffic sent over {elapsed_s:.1} s, against the configured budget (IP-layer bytes for ICMP and UDP; TCP payload bytes for DoH, DoT and DNS/TCP)");
    let _ = writeln!(out, "{dim}  {:<8} {:<4} {:>7} {:>8} {:>9} {:>9} {:>11} {:>8}   received{reset}", "kind", "fam", "probes", "packets", "bytes", "B/s", "budget B/s", "diff");
    for (kind, family) in keys {
        let t = traffic.iter().find(|t| t.kind == kind && t.family == family);
        let b = budget.iter().find(|b| b.kind.as_str() == kind && b.family == family);
        let (probes, packets, bytes, pin, bin) = t.map_or((0, 0, 0, 0, 0), |t| (t.transactions, t.packets_out, t.bytes_out, t.packets_in, t.bytes_in));
        let rate = bytes as f64 / elapsed_s;
        let datagrams = matches!(kind.as_str(), "icmp" | "dns-udp");
        let (expected, diff) = match b.and_then(|b| b.bytes_out_per_s) {
            Some(e) if e > 0.0 => (format!("{e:.1}"), format!("{:+.1}%", (rate - e) / e * 100.0)),
            _ => ("—".to_string(), "—".to_string()),
        };
        let received = if datagrams { format!("{pin} packets, {bin} B") } else { format!("{bin} B") };
        let _ = writeln!(
            out,
            "  {:<8} {:<4} {:>7} {:>8} {:>9} {:>9.1} {:>11} {:>8}   {}",
            kind,
            format!("v{}", family.as_str()),
            probes,
            if datagrams { packets.to_string() } else { "—".to_string() },
            bytes,
            rate,
            expected,
            diff,
            received
        );
    }
}

#[allow(clippy::too_many_arguments)] // one call site; these are the things a summary is made of
pub fn render(
    engine: &Engine,
    plan: &Plan,
    states: &[StreamRec],
    counters: &CollectorCounters,
    traffic: &[TrafficRow],
    budget: &[BudgetRow],
    window: Duration,
    now_ns: u64,
    color: bool,
) -> String {
    let (dim, bold, reset) = if color { ("\x1b[2m", "\x1b[1m", "\x1b[0m") } else { ("", "", "") };
    let mut out = String::new();
    let _ = writeln!(out, "{bold}window {} · times in ms · `—` not enough samples · `·` low sample count{reset}", Dur(window));
    let _ = writeln!(
        out,
        "{dim}{:<34} {:>7} {:>7} {:>7} {:>7} {:>6} {:>9} {:>8} {:>5} {:>5}  sent/pend/t-o/unr/err/local/skip{reset}",
        "stream", "last", "p50", "p95", "max", "Δ", "no-reply", "err-resp", "n", "cov"
    );
    for (idx, s) in plan.streams.iter().enumerate() {
        // The collector's announcement is authoritative for what actually ran
        // (it knows, for example, that ICMP is unsupported on this host).
        let announced = states.iter().find(|r| r.stream == s.id);
        let (state, reason) = match announced {
            Some(r) => (r.state.as_str(), r.reason.clone()),
            None => (s.state.label(), s.state.reason().map(str::to_string)),
        };
        if state != "collecting" {
            if state != "disabled" {
                let _ = writeln!(out, "{:<34} {} — {} (nothing sent; not counted as loss)", s.id, state.to_uppercase().replace('_', " "), reason.unwrap_or_default());
            }
            continue;
        }
        let w: WindowStats = engine.stats(idx, window, now_ns);
        let last = engine.last_success(idx).map_or_else(|| "—".to_string(), |l| ms(f64::from(l.rtt_us)));
        let _ = writeln!(
            out,
            "{:<34} {:>7} {:>7} {:>7} {:>7} {:>6} {:>9} {:>8} {:>5} {:>4.0}%  {}/{}/{}/{}/{}/{}/{}",
            s.id,
            last,
            gated(w.p50_us),
            gated(w.p95_us),
            w.max_us.map_or_else(|| "—".to_string(), |v| ms(f64::from(v))),
            gated(w.delta_us),
            rate(w.no_reply_rate, w.unanswered, w.finalised),
            rate(w.error_rate, w.errors, w.finalised),
            w.n,
            w.coverage * 100.0,
            w.sent,
            w.pending,
            w.timed_out,
            w.unreachable,
            w.errors,
            w.local_error,
            w.skipped,
        );
        if let Some(reason) = dominant_error(engine, idx) {
            let _ = writeln!(out, "{dim}  ↳ {reason}{reset}");
        }
        if let Some(line) = receive_delay(engine, idx) {
            let _ = writeln!(out, "{dim}  ↳ {line}{reset}");
        }
    }
    for p in &plan.providers {
        if let PlanState::NeedsConfig(reason) = &p.state {
            let _ = writeln!(out, "{bold}{} {}{reset}: NEEDS CONFIG — {reason} (nothing sent; not counted as loss)", p.slot, p.name);
        }
    }
    for g in engine.gaps() {
        let _ = writeln!(out, "observation gap: {:?} for {:.1} s ending {} — no probes existed in it; excluded from every rate", g.cause, g.duration_ms as f64 / 1000.0, g.end_utc);
    }
    let c = counters;
    if c.backpressure_skips + c.rate_limit_skips + c.late_timer_skips + c.dropped_records > 0 {
        let _ = writeln!(
            out,
            "collector (instrument health, not network): {} backpressure skips, {} rate-limit skips, {} late-timer skips, {} dropped records",
            c.backpressure_skips, c.rate_limit_skips, c.late_timer_skips, c.dropped_records
        );
    }
    traffic_table(&mut out, traffic, budget, now_ns as f64 / 1e9, dim, reset);
    let _ = writeln!(
        out,
        "{dim}collector: {} unmatched packets ignored · worst receive-loop lag {:.1} ms, over 2 ms in {} of {} checks{reset}",
        c.unmatched_packets,
        c.max_loop_lag_us as f64 / 1000.0,
        c.loop_lag_over_2ms,
        c.loop_lag_checks
    );
    let _ = writeln!(out, "{dim}no-reply = (timed out + unreachable) ÷ finalised probes; it is not proof of packet loss on your line.{reset}");
    out
}

/// A time that may be withheld, in words.
fn spoken(name: &str, g: Gated, n: u64, needs: u64) -> String {
    match g {
        Gated::Ok(v) => format!("{name} {}", ms(v)),
        Gated::LowN(v) => format!("{name} {} (from few answers)", ms(v)),
        Gated::Insufficient => format!("{name} not shown ({n} of the {needs} it needs)"),
    }
}

/// A share in words: a percentage once there are enough probes, the count always.
fn spoken_share(name: &str, rate: Option<f64>, k: u64, n: u64) -> String {
    match rate {
        Some(r) => format!("{name} {:.1} percent ({k} of {n})", r * 100.0),
        None if n == 0 => format!("{name} unknown (no probe was finalised)"),
        None => format!("{name} {k} of {n}"),
    }
}

/// The summary for reading aloud or line by line: the same numbers as
/// [`render`], as sentences. No table to keep one's place in, no symbol
/// standing in for a word, nothing outside ASCII, and no colour.
#[allow(clippy::too_many_arguments)] // as `render`
pub fn render_plain(
    engine: &Engine,
    plan: &Plan,
    states: &[StreamRec],
    counters: &CollectorCounters,
    traffic: &[TrafficRow],
    budget: &[BudgetRow],
    window: Duration,
    now_ns: u64,
) -> String {
    use pathwatch_core::stats::{MIN_N_CENTRAL, MIN_N_P95, MIN_PAIRS_DELTA};
    let mut out = String::new();
    let elapsed_s = now_ns as f64 / 1e9;
    let _ = writeln!(out, "pathwatch sample: {elapsed_s:.0} seconds, statistics over a window of {}. Times are in milliseconds.", Dur(window));
    for (idx, s) in plan.streams.iter().enumerate() {
        let announced = states.iter().find(|r| r.stream == s.id);
        let (state, reason) = match announced {
            Some(r) => (r.state.as_str(), r.reason.clone()),
            None => (s.state.label(), s.state.reason().map(str::to_string)),
        };
        if state == "disabled" {
            continue;
        }
        let _ = writeln!(out);
        if state != "collecting" {
            let why = reason.map(|r| format!(": {r}")).unwrap_or_default();
            let _ = writeln!(out, "Stream {}: {}{why}. Nothing was sent; this is not counted as loss.", s.id, state.replace('_', " "));
            continue;
        }
        let w: WindowStats = engine.stats(idx, window, now_ns);
        let _ = writeln!(out, "Stream {}, to {}.", s.id, s.configured);
        let _ = writeln!(
            out,
            "  Probes: {} sent, {} answered, {} timed out, {} unreachable, {} error responses, {} pending, {} local errors, {} skipped.",
            w.sent, w.replied, w.timed_out, w.unreachable, w.errors, w.pending, w.local_error, w.skipped
        );
        let _ = writeln!(
            out,
            "  {}. {}. Coverage {:.0} percent of the window.",
            spoken_share("No reply", w.no_reply_rate, w.unanswered, w.finalised),
            spoken_share("Error responses", w.error_rate, w.errors, w.finalised),
            w.coverage * 100.0
        );
        if w.n == 0 {
            let _ = writeln!(out, "  Round-trip time: none, because no probe was answered as expected.");
        } else {
            let last = engine.last_success(idx).map_or_else(|| "none".to_string(), |l| ms(f64::from(l.rtt_us)));
            let _ = writeln!(
                out,
                "  Round-trip time, from {} answer{}: last {last}, {}, {}, maximum {}, {}.",
                w.n,
                if w.n == 1 { "" } else { "s" },
                spoken("median", w.p50_us, w.n, MIN_N_CENTRAL),
                spoken("95th percentile", w.p95_us, w.n, MIN_N_P95),
                w.max_us.map_or_else(|| "none".to_string(), |v| ms(f64::from(v))),
                spoken("variation between consecutive answers", w.delta_us, w.delta_pairs, MIN_PAIRS_DELTA),
            );
        }
        if let Some(reason) = dominant_error(engine, idx) {
            let _ = writeln!(out, "  Most frequent failure: {}.", reason.replacen("× ", " times, ", 1));
        }
        if let Some(line) = receive_delay(engine, idx) {
            let _ = writeln!(out, "  Timing: {}.", line.replace(" · ", ", ").replace(" — ", "; "));
        }
    }
    let _ = writeln!(out);
    for p in &plan.providers {
        if let PlanState::NeedsConfig(reason) = &p.state {
            let _ = writeln!(out, "Provider {} {}: needs configuration: {reason}. Nothing was sent; this is not counted as loss.", p.slot, p.name);
        }
    }
    for g in engine.gaps() {
        let _ = writeln!(
            out,
            "Observation gap: {:?} for {:.1} seconds ending {}. No probes existed in it; it is excluded from every share.",
            g.cause,
            g.duration_ms as f64 / 1000.0,
            g.end_utc
        );
    }
    for t in traffic {
        let b = budget.iter().find(|b| b.kind.as_str() == t.kind && b.family == t.family).and_then(|b| b.bytes_out_per_s).filter(|e| *e > 0.0);
        let rate = if elapsed_s > 0.0 { t.bytes_out as f64 / elapsed_s } else { 0.0 };
        let against = b.map(|e| format!(", against a configured {e:.1}")).unwrap_or_default();
        let _ = writeln!(
            out,
            "Traffic, {} over IPv{}: {} probes, {} bytes sent ({rate:.1} bytes a second{against}), {} bytes received. Bytes are {}.",
            t.kind,
            t.family.as_str(),
            t.transactions,
            t.bytes_out,
            t.bytes_in,
            if t.bytes == "ip-layer" { "counted at the IP layer" } else { "TCP payload" }
        );
    }
    let c = counters;
    let _ = writeln!(
        out,
        "The instrument itself: {} skipped for backlog, {} for the rate limit, {} for late timers; {} records dropped; {} packets for other programs ignored; worst receive-loop lag {:.1}, over 2 in {} of {} checks.",
        c.backpressure_skips,
        c.rate_limit_skips,
        c.late_timer_skips,
        c.dropped_records,
        c.unmatched_packets,
        c.max_loop_lag_us as f64 / 1000.0,
        c.loop_lag_over_2ms,
        c.loop_lag_checks
    );
    let _ = writeln!(out, "No reply is timed out plus unreachable, over finalised probes. It is not proof of packet loss on your line. `pathwatch glossary` explains every term.");
    // Whatever a stream id, an address or an error text held, the result is ASCII.
    out.chars()
        .map(|ch| match ch {
            ch if ch.is_ascii() => ch,
            '—' | '–' => '-',
            '×' => 'x',
            '·' => ',',
            _ => '?',
        })
        .collect()
}

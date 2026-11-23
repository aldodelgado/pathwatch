//! Route inspection: an on-demand ICMP trace.
//!
//! A trace sends echo requests with increasing TTL and records which router
//! answers "time exceeded" at each. It is a separate diagnostic: it runs
//! only when asked, on its own socket, within a stated budget, and nothing
//! it measures feeds the latency series.
//!
//! What a trace can and cannot show is easy to over-read, so the view
//! explains itself: each annotation is generated from the counts and states
//! its basis. None of them names a cause.

use std::net::IpAddr;
use std::time::Duration;

use crate::clock::Clock;
use crate::model::Family;
use crate::probe::icmp::IcmpIo;
use crate::probe::icmp_core::Incoming;
use crate::probe::icmp_packet::{self, ErrorKind, Parsed, Token};

/// The stream number in a trace probe's token. No measurement stream has it.
const TRACE_STREAM: u32 = u32::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub max_hops: u8,
    pub rounds: u32,
    pub round_interval: Duration,
    /// Hard cap on probes in one trace.
    pub max_probes: u32,
    /// Probes per second, at most.
    pub max_rate: u32,
}

impl Budget {
    pub fn from_config(t: &crate::config::TraceCfg) -> Self {
        Self { max_hops: t.max_hops.clamp(1, 64), rounds: t.rounds.max(1), round_interval: t.round_interval.0, max_probes: t.max_probes.max(1), max_rate: t.max_rate.max(1) }
    }

    /// The least time between two probes.
    fn spacing_ns(&self) -> u64 {
        1_000_000_000 / u64::from(self.max_rate.max(1))
    }

    /// The budget in words, for the view shown before a trace starts.
    pub fn describe(&self) -> String {
        format!(
            "ICMP echo, TTL 1 to {}, {} rounds, one every {} — at most {} probes in all and {} a second",
            self.max_hops,
            self.rounds,
            crate::config::Dur(self.round_interval),
            self.max_probes,
            self.max_rate
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub ttl: u8,
    /// Every address that answered at this TTL, with how often, most first.
    pub addrs: Vec<(IpAddr, u32)>,
    pub sent: u32,
    pub replies: u32,
    pub last_us: Option<u32>,
    pub best_us: Option<u32>,
    pub worst_us: Option<u32>,
    sum_us: u64,
    /// Every round trip counted, in order of arrival.
    rtts_us: Vec<u32>,
    pub first_seen_ns: Option<u64>,
    pub last_seen_ns: Option<u64>,
    /// The destination itself answered at this TTL.
    pub is_destination: bool,
    /// What this row's numbers do and do not mean, if anything needs saying.
    pub notes: Vec<&'static str>,
}

impl Hop {
    fn new(ttl: u8) -> Self {
        Self {
            ttl,
            addrs: Vec::new(),
            sent: 0,
            replies: 0,
            last_us: None,
            best_us: None,
            worst_us: None,
            sum_us: 0,
            rtts_us: Vec::new(),
            first_seen_ns: None,
            last_seen_ns: None,
            is_destination: false,
            notes: Vec::new(),
        }
    }

    pub fn avg_us(&self) -> Option<u32> {
        (self.replies > 0).then(|| (self.sum_us / u64::from(self.replies)) as u32)
    }

    /// The middle round trip: what this hop usually takes, whatever one
    /// slow answer did to the average.
    pub fn median_us(&self) -> Option<u32> {
        let mut sorted = self.rtts_us.clone();
        sorted.sort_unstable();
        let mid = sorted.len() / 2;
        match sorted.len() {
            0 => None,
            n if n % 2 == 1 => Some(sorted[mid]),
            _ => Some(((u64::from(sorted[mid - 1]) + u64::from(sorted[mid])) / 2) as u32),
        }
    }

    /// The share of this hop's probes that drew no reply, of those whose
    /// time to answer has passed.
    pub fn no_reply_share(&self, settled: u32) -> Option<f64> {
        (settled > 0).then(|| f64::from(settled.saturating_sub(self.replies)) / f64::from(settled))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    /// Every round was sent.
    Finished,
    /// The probe cap was reached first.
    Capped,
    /// Stopped on request.
    Stopped,
    /// The transport refused: nothing can be sent.
    Failed,
}

pub const NOTE_SILENT: &str = "does not send ICMP Time Exceeded — not evidence of loss";
pub const NOTE_RATE_LIMITED: &str = "consistent with ICMP rate limiting at this hop, not forwarding loss";
pub const NOTE_PERSISTS: &str = "no-reply begins at or before this hop and persists";
pub const NOTE_ECMP: &str = "several addresses: per-flow load balancing (ECMP) or a route change";
pub const NOTE_SLOW_HOP: &str = "hop RTT reflects that router's own reply handling and return path";

/// What the Route view shows. A plain value: drawing it reads nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceView {
    pub dest: IpAddr,
    pub state: State,
    pub round: u32,
    pub budget: Budget,
    pub probes_sent: u32,
    pub hops: Vec<Hop>,
    /// Set when the trace could not send, with the reason.
    pub error: Option<String>,
    /// Reverse-DNS names found so far. Untrusted text, already sanitised.
    pub names: Vec<(IpAddr, String)>,
}

#[derive(Debug, Clone, Copy)]
struct Probe {
    ttl: u8,
    sent_ns: u64,
    answered: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Send one probe with this TTL and sequence number, now.
    Send {
        ttl: u8,
        seq: u16,
    },
    /// Nothing to do before this time.
    Wait(u64),
    Done,
}

/// The trace as a state machine: told the time, it says what to do next.
/// It never exceeds its budget, whatever it is told.
pub struct Trace {
    dest: IpAddr,
    budget: Budget,
    state: State,
    round: u32,
    round_start_ns: u64,
    next_ttl: u8,
    /// The lowest TTL at which the destination itself answered.
    dest_ttl: Option<u8>,
    last_send_ns: Option<u64>,
    probes: Vec<Probe>,
    hops: Vec<Hop>,
    error: Option<String>,
    /// How long after a probe a reply still counts.
    reply_window_ns: u64,
    /// What an answer must carry to be this trace's: the echo identifier,
    /// where the kernel leaves it alone, and the nonce in the payload.
    expect_id: Option<u16>,
    nonce: Option<u32>,
}

impl Trace {
    pub fn new(dest: IpAddr, budget: Budget, now_ns: u64) -> Self {
        let reply_window_ns = budget.round_interval.as_nanos().clamp(500_000_000, 3_000_000_000) as u64;
        Self {
            dest,
            budget,
            state: State::Running,
            round: 1,
            round_start_ns: now_ns,
            next_ttl: 1,
            dest_ttl: None,
            last_send_ns: None,
            probes: Vec::new(),
            hops: Vec::new(),
            error: None,
            reply_window_ns,
            expect_id: None,
            nonce: None,
        }
    }

    /// Tells the trace how to recognise its own probes among everything a
    /// shared ICMP socket may deliver.
    pub fn expect(&mut self, id: Option<u16>, nonce: u32) {
        (self.expect_id, self.nonce) = (id, Some(nonce));
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn stop(&mut self) {
        if self.state == State::Running {
            self.state = State::Stopped;
        }
    }

    pub fn fail(&mut self, why: String) {
        self.error = Some(why);
        self.state = State::Failed;
    }

    /// The highest TTL worth probing: the destination's, once it is known.
    fn limit(&self) -> u8 {
        self.dest_ttl.unwrap_or(self.budget.max_hops).min(self.budget.max_hops)
    }

    pub fn poll(&mut self, now_ns: u64) -> Step {
        if self.state != State::Running {
            return Step::Done;
        }
        // The end of a round: start the next on the round grid, or finish.
        if self.next_ttl > self.limit() {
            if self.round >= self.budget.rounds {
                // Give the last probes their time to be answered.
                let settle = self.last_send_ns.map_or(now_ns, |t| t + self.reply_window_ns);
                if now_ns < settle {
                    return Step::Wait(settle);
                }
                self.state = State::Finished;
                return Step::Done;
            }
            let next_round = self.round_start_ns + self.budget.round_interval.as_nanos() as u64;
            if now_ns < next_round {
                return Step::Wait(next_round);
            }
            (self.round, self.round_start_ns, self.next_ttl) = (self.round + 1, next_round.max(now_ns), 1);
        }
        if self.probes.len() as u32 >= self.budget.max_probes {
            self.state = State::Capped;
            return Step::Done;
        }
        // Never faster than the rate cap, whatever else is true.
        if let Some(last) = self.last_send_ns
            && now_ns < last + self.budget.spacing_ns()
        {
            return Step::Wait(last + self.budget.spacing_ns());
        }
        Step::Send { ttl: self.next_ttl, seq: self.probes.len() as u16 }
    }

    /// Records that the probe [`Self::poll`] asked for has left.
    pub fn sent(&mut self, ttl: u8, now_ns: u64) {
        self.probes.push(Probe { ttl, sent_ns: now_ns, answered: false });
        self.last_send_ns = Some(now_ns);
        while self.hops.len() < usize::from(ttl) {
            self.hops.push(Hop::new(self.hops.len() as u8 + 1));
        }
        self.hops[usize::from(ttl) - 1].sent += 1;
        self.next_ttl = ttl + 1;
    }

    /// Takes something the socket received. Anything that is not an answer
    /// to one of this trace's probes is ignored.
    pub fn incoming(&mut self, inc: &Incoming) {
        let (seq16, from, is_dest) = match inc.parsed {
            Parsed::EchoReply { id, seq16, token, .. }
                if inc.src == self.dest
                    && self.expect_id.is_none_or(|want| want == id)
                    && token.is_none_or(|t| t.stream == TRACE_STREAM && self.nonce.is_none_or(|n| n == t.nonce)) =>
            {
                (seq16, inc.src, true)
            }
            Parsed::Error { kind: ErrorKind::TimeExceeded, quoted, .. }
                if quoted.dest.is_none_or(|d| d == self.dest)
                    && self.expect_id.is_none_or(|want| quoted.id.is_none_or(|id| id == want))
                    && quoted.token.is_none_or(|t| t.stream == TRACE_STREAM) =>
            {
                match quoted.seq16 {
                    Some(seq16) => (seq16, inc.offender.unwrap_or(inc.src), false),
                    None => return,
                }
            }
            _ => return,
        };
        let Some(probe) = self.probes.get_mut(usize::from(seq16)) else { return };
        // Two clocks are involved — ours at the send, the kernel's at the
        // arrival — so on a very short path the difference can come out a
        // hair below zero. That is a round trip of nothing, not a stranger.
        let elapsed = inc.recv_ns.saturating_sub(probe.sent_ns);
        if probe.answered || elapsed > self.reply_window_ns {
            return; // a duplicate, or too late to count
        }
        probe.answered = true;
        let rtt = (elapsed / 1_000).min(u64::from(u32::MAX)) as u32;
        let hop = &mut self.hops[usize::from(probe.ttl) - 1];
        hop.replies += 1;
        hop.sum_us += u64::from(rtt);
        hop.rtts_us.push(rtt);
        hop.last_us = Some(rtt);
        hop.best_us = Some(hop.best_us.map_or(rtt, |b| b.min(rtt)));
        hop.worst_us = Some(hop.worst_us.map_or(rtt, |w| w.max(rtt)));
        hop.first_seen_ns.get_or_insert(inc.recv_ns.max(probe.sent_ns));
        hop.last_seen_ns = Some(inc.recv_ns.max(probe.sent_ns));
        match hop.addrs.iter_mut().find(|(a, _)| *a == from) {
            Some((_, n)) => *n += 1,
            None => hop.addrs.push((from, 1)),
        }
        hop.addrs.sort_by_key(|a| std::cmp::Reverse(a.1));
        if is_dest {
            hop.is_destination = true;
            let ttl = probe.ttl;
            if self.dest_ttl.is_none_or(|d| ttl < d) {
                self.dest_ttl = Some(ttl);
            }
        }
    }

    /// For each hop, how many of its probes have had their time to answer.
    fn settled(&self, now_ns: u64) -> Vec<u32> {
        let mut settled = vec![0u32; self.hops.len()];
        for p in &self.probes {
            if p.answered || now_ns.saturating_sub(p.sent_ns) >= self.reply_window_ns {
                settled[usize::from(p.ttl) - 1] += 1;
            }
        }
        settled
    }

    pub fn view(&self, now_ns: u64) -> TraceView {
        // Only hops up to the destination are the path; a probe that
        // overshot while the destination was being found is not a hop.
        let shown = self.dest_ttl.map_or(self.hops.len(), |d| usize::from(d).min(self.hops.len()));
        let mut hops: Vec<Hop> = self.hops[..shown].to_vec();
        let settled = &self.settled(now_ns)[..shown];
        annotate(&mut hops, settled, self.dest_ttl.is_some());
        TraceView {
            dest: self.dest,
            state: self.state,
            round: self.round,
            budget: self.budget.clone(),
            probes_sent: self.probes.len() as u32,
            hops,
            error: self.error.clone(),
            names: Vec::new(),
        }
    }
}

/// Writes each hop's notes from the counts. Needs a few probes per hop
/// before it says anything: the first round alone proves nothing.
fn annotate(hops: &mut [Hop], settled: &[u32], destination_reached: bool) {
    const MIN: u32 = 3;
    let share: Vec<Option<f64>> = hops.iter().zip(settled).map(|(h, s)| if *s >= MIN { h.no_reply_share(*s) } else { None }).collect();
    let n = hops.len();
    // What a hop and the destination usually take: the median, and only
    // once each has a few answers to take it from.
    let usual = |h: &Hop| if h.replies >= MIN { h.median_us() } else { None };
    let dest_usual = hops.last().filter(|h| h.is_destination).and_then(usual);
    // Where no-reply starts and never gets better: the first hop whose
    // share is above zero and is not beaten by any later hop, through to
    // the destination (or to the last hop probed, if it was never reached).
    let persists_from = (0..n).find(|&i| {
        let Some(here) = share[i] else { return false };
        let end_counts = destination_reached || i + 1 < n;
        here > 0.0 && end_counts && (i + 1..n).all(|j| share[j].is_some_and(|later| later >= here - 1e-9)) && share[n - 1].is_some_and(|last| last > 0.0)
    });
    for i in 0..n {
        let Some(here) = share[i] else {
            if hops[i].addrs.len() > 1 {
                hops[i].notes.push(NOTE_ECMP);
            }
            continue;
        };
        let later_better = (i + 1..n).any(|j| share[j].is_some_and(|later| later < here - 1e-9));
        if here >= 1.0 - 1e-9 && later_better {
            hops[i].notes.push(NOTE_SILENT);
        } else if here > 0.0 && later_better {
            hops[i].notes.push(NOTE_RATE_LIMITED);
        } else if persists_from == Some(i) {
            hops[i].notes.push(NOTE_PERSISTS);
        }
        if hops[i].addrs.len() > 1 {
            hops[i].notes.push(NOTE_ECMP);
        }
        // Usually slower than the destination, by a margin that is not
        // noise. Medians, not averages: one slow answer in ten is that
        // answer's story, not the hop's.
        if let (Some(here), Some(dest), false) = (usual(&hops[i]), dest_usual, hops[i].is_destination)
            && here > dest + dest / 2
            && here > dest + 2_000
        {
            hops[i].notes.push(NOTE_SLOW_HOP);
        }
    }
}

/// Runs one trace to its end on `io`, publishing the view as it changes.
/// `stop` ends it early. The transport is the trace's own: nothing here
/// touches a measurement socket.
pub async fn run<I: IcmpIo>(
    mut io: I,
    dest: IpAddr,
    budget: Budget,
    clock: &dyn Clock,
    stop: tokio_util::sync::CancellationToken,
    publish: &(dyn Fn(TraceView) + Sync),
) -> TraceView {
    let family = Family::of(&dest);
    let mut trace = Trace::new(dest, budget, clock.mono_ns());
    let mut nonce = [0u8; 4];
    let _ = getrandom::fill(&mut nonce);
    let nonce = u32::from_ne_bytes(nonce);
    trace.expect(io.check_id().then(|| io.id()), nonce);
    loop {
        match trace.poll(clock.mono_ns()) {
            Step::Done => break,
            Step::Send { ttl, seq } => {
                let packet = icmp_packet::build_echo(family, io.id(), Token { nonce, stream: TRACE_STREAM, seq: u64::from(seq) }, 32);
                // The clock is read before the send, as for every probe: an
                // answer can be stamped by the kernel before `send` returns.
                let sent_ns = clock.mono_ns();
                if let Err(e) = io.set_ttl(ttl).and_then(|()| io.send(dest, &packet)) {
                    trace.fail(format!("cannot send a probe with TTL {ttl}: {e}"));
                    break;
                }
                trace.sent(ttl, sent_ns);
            }
            Step::Wait(until_ns) => {
                tokio::select! {
                    _ = stop.cancelled() => trace.stop(),
                    _ = tokio::time::sleep_until(clock.instant_at(until_ns)) => {}
                    got = io.recv() => match got {
                        Ok(items) => items.iter().for_each(|inc| trace.incoming(inc)),
                        Err(e) => trace.fail(format!("cannot receive: {e}")),
                    },
                }
            }
        }
        publish(trace.view(clock.mono_ns()));
    }
    let view = trace.view(clock.mono_ns());
    publish(view.clone());
    view
}

// ───────────────────────────── the service ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Trace the route to this address. Ignored while a trace is running:
    /// there is one at a time.
    Start(IpAddr),
    /// Stop the running trace, if there is one.
    Stop,
}

/// Looks up the name of an address. May block; it is never called on the
/// path of a probe.
pub type NameFn = std::sync::Arc<dyn Fn(IpAddr) -> Option<String> + Send + Sync>;

/// What the dashboard holds: the latest view, or none before the first trace.
pub type Shown = tokio::sync::watch::Sender<Option<std::sync::Arc<TraceView>>>;

/// Serves trace requests until the request channel closes: one trace at a
/// time, each on a transport of its own from `open`, each within `budget`.
/// Names of the addresses found are looked up on other threads and added to
/// the view as they arrive.
pub async fn serve<I: IcmpIo>(
    mut requests: tokio::sync::mpsc::UnboundedReceiver<Request>,
    open: impl Fn(Family) -> std::io::Result<I>,
    budget: Budget,
    clock: std::sync::Arc<dyn Clock>,
    names: Option<NameFn>,
    shown: Shown,
) {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    while let Some(request) = requests.recv().await {
        let Request::Start(dest) = request else { continue };
        let io = match open(Family::of(&dest)) {
            Ok(io) => io,
            Err(e) => {
                // Said in the view, where it was asked for.
                let mut failed = Trace::new(dest, budget.clone(), clock.mono_ns());
                failed.fail(format!("cannot open a socket for the trace: {e}"));
                shown.send_replace(Some(Arc::new(failed.view(clock.mono_ns()))));
                continue;
            }
        };
        let stop = tokio_util::sync::CancellationToken::new();
        let found: Arc<Mutex<BTreeMap<IpAddr, Option<String>>>> = Arc::default();
        let found_elsewhere = Arc::clone(&found);
        let publish = |mut view: TraceView| {
            let mut found = found.lock().unwrap();
            // A hop seen for the first time: ask for its name, elsewhere.
            if let Some(lookup) = &names {
                for addr in view.hops.iter().flat_map(|h| h.addrs.iter().map(|a| a.0)) {
                    if found.contains_key(&addr) {
                        continue;
                    }
                    found.insert(addr, None);
                    let (lookup, found, shown) = (lookup.clone(), Arc::clone(&found_elsewhere), shown.clone());
                    tokio::task::spawn_blocking(move || {
                        // Untrusted text: length-limited and stripped of control characters.
                        let Some(name) = lookup(addr).map(|n| crate::sanitize::clean(&n, 80)).filter(|n| !n.is_empty()) else { return };
                        found.lock().unwrap().insert(addr, Some(name.clone()));
                        shown.send_modify(|current| {
                            if let Some(view) = current {
                                let mut updated = (**view).clone();
                                if !updated.names.iter().any(|(a, _)| *a == addr) {
                                    updated.names.push((addr, name));
                                    *view = Arc::new(updated);
                                }
                            }
                        });
                    });
                }
            }
            view.names = found.iter().filter_map(|(a, n)| n.clone().map(|n| (*a, n))).collect();
            shown.send_replace(Some(Arc::new(view)));
        };
        let running = run(io, dest, budget.clone(), clock.as_ref(), stop.clone(), &publish);
        tokio::pin!(running);
        loop {
            tokio::select! {
                _ = &mut running => break,
                next = requests.recv() => match next {
                    Some(Request::Stop) => stop.cancel(),
                    // One trace at a time: a second request waits for nothing and starts nothing.
                    Some(Request::Start(_)) => {}
                    None => stop.cancel(),
                },
            }
        }
    }
}

//! The observation processor: bounded history per stream, pending-probe
//! tracking, the gap timeline, and window statistics.
//!
//! `ingest` is the only way state changes and `stats` is a pure read, so the
//! same messages always produce the same statistics regardless of when or how
//! often anything is rendered.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::model::{ErrorInfo, EventRec, Family, Flag, GapRec, Msg, Observation, Outcome, Phases, Protocol, Record, Source};
use crate::stats::{self, Sample, WindowStats};

#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub id: Arc<str>,
    pub provider: Arc<str>,
    pub target: Arc<str>,
    pub protocol: Protocol,
    pub family: Family,
    pub interval: Duration,
    pub deadline: Duration,
}

impl From<&crate::model::StreamRec> for StreamInfo {
    fn from(s: &crate::model::StreamRec) -> Self {
        Self {
            id: s.stream.clone(),
            provider: s.provider.clone(),
            target: s.target.clone(),
            protocol: s.protocol,
            family: s.family,
            interval: Duration::from_millis(s.interval_ms.max(1)),
            deadline: Duration::from_millis(s.deadline_ms),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HistoryLimits {
    /// Compact samples kept per stream.
    pub samples: Duration,
    /// Full observation records kept per stream.
    pub records: Duration,
    pub events: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self { samples: Duration::from_secs(24 * 3600), records: Duration::from_secs(3600), events: 10_000 }
    }
}

/// What is kept in memory of a finalised observation, for the views that
/// list records: the Raw tab, the DNS tab's tally, the source in the header,
/// the summary's commonest failure.
///
/// A full observation is some 700 bytes, most of it things no view reads
/// back — which session and stream it belongs to, the backend's name, the
/// connection's details — and an hour of them for every stream was half of
/// what the program held. This keeps what is read, a quarter of the size,
/// and shares between records what is the same in all of them. A recording
/// on disk is unaffected: it is written from the full observation.
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub seq: u64,
    pub attempt: u8,
    pub outcome: Outcome,
    /// When it was sent, or the time of its slot if nothing was sent.
    pub at_ns: u64,
    pub sent_utc: Option<Box<str>>,
    pub dest_ip: Option<std::net::IpAddr>,
    pub src: Option<Arc<Source>>,
    pub rtt_us: Option<u32>,
    pub rtt_user_us: Option<u32>,
    pub recv_delay_us: Option<u32>,
    pub sched_lag_us: Option<u64>,
    pub flags: Box<[Flag]>,
    pub error: Option<Box<ErrorInfo>>,
    pub phase: Option<Box<Phases>>,
    pub dns: Option<Box<DnsKept>>,
}

/// The DNS side of a kept record.
#[derive(Debug, Clone, PartialEq)]
pub struct DnsKept {
    pub query: Arc<DnsQuery>,
    pub rcode: Option<Box<str>>,
    pub answers: Option<u16>,
}

/// What was asked and what was expected: the same for every record of one
/// query scenario, and held once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsQuery {
    pub qname: Box<str>,
    pub qtype: Box<str>,
    pub expect: Box<str>,
}

impl Kept {
    pub fn of(o: &Observation) -> Self {
        Self {
            seq: o.seq,
            attempt: o.attempt,
            outcome: o.outcome,
            at_ns: o.at_ns(),
            sent_utc: o.sent_utc.as_deref().map(Box::from),
            dest_ip: o.dest_ip,
            src: o.src.clone().map(Arc::new),
            rtt_us: o.rtt_us,
            rtt_user_us: o.rtt_user_us,
            recv_delay_us: o.recv_delay_us,
            sched_lag_us: o.sched_lag_us,
            flags: o.flags.as_slice().into(),
            error: o.error.clone().map(Box::new),
            phase: o.phase.map(Box::new),
            dns: o.dns.as_ref().map(|d| {
                Box::new(DnsKept {
                    query: Arc::new(DnsQuery { qname: d.qname.as_str().into(), qtype: d.qtype.as_str().into(), expect: d.expect.as_str().into() }),
                    rcode: d.rcode.as_deref().map(Box::from),
                    answers: d.answers,
                })
            }),
        }
    }

    pub fn at_ns(&self) -> u64 {
        self.at_ns
    }

    pub fn has_flag(&self, f: Flag) -> bool {
        self.flags.contains(&f)
    }
}

/// How many distinct queries a stream's records share between them before
/// a new one is simply kept by the record that has it.
const SHARED_QUERIES: usize = 64;

/// How far from its place in send-time order a sample can be. Samples are
/// kept in slot order; a probe is sent within its own interval of its slot,
/// so only neighbours can be out of order. Ranges found by bisection are
/// widened by this much each side, and every sample in them is then tested.
const SLACK: usize = 8;

/// How many of the earliest finalised probes of an epoch are remembered:
/// enough to answer "have there been at least n by this time?" for any n
/// the statuses ask about.
pub const EARLIEST_KEPT: usize = 32;

/// What is known of a stream's current destination epoch without looking
/// through its history: kept up to date as results arrive.
#[derive(Debug, Default)]
struct EpochFacts {
    /// Send time of the earliest slot of any outcome.
    first_slot_ns: Option<u64>,
    /// Send time of the earliest probe answered as expected.
    first_success_ns: Option<u64>,
    /// Send times of the earliest finalised probes, at most `EARLIEST_KEPT`,
    /// with the latest of them on top.
    earliest_finalised: std::collections::BinaryHeap<u64>,
}

/// The `kind` under which since-start statistics are remembered; any other
/// kind is a window's length in nanoseconds.
const SINCE_START: u64 = u64::MAX;

/// How many windows' statistics are remembered per stream.
const REMEMBERED: usize = 8;

/// Which samples a remembered set of statistics was computed from. Samples
/// never change once they are in, are only ever added in slot order and
/// only ever dropped from the front, so the first one's slot, how many
/// there are, and which of those at the window's two ends are inside it,
/// say exactly which they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowKey {
    kind: u64,
    first_seq: u64,
    len: usize,
    head: u64,
    tail: u64,
    epoch: u32,
    late: usize,
    duplicates: usize,
    gaps: usize,
    interval_ns: u64,
}

struct StreamState {
    info: StreamInfo,
    facts: EpochFacts,
    memo: std::sync::Mutex<Vec<(WindowKey, WindowStats)>>,
    samples: VecDeque<Sample>,
    samples_cap: usize,
    records: VecDeque<Kept>,
    records_cap: usize,
    /// The queries this stream's records have asked, each held once.
    queries: Vec<Arc<DnsQuery>>,
    /// seq → send time of probes awaiting a reply or their deadline.
    pending: BTreeMap<u64, u64>,
    /// Send times of probes that later received a late / duplicate reply.
    late: VecDeque<u64>,
    duplicates: VecDeque<u64>,
    epoch: u32,
    last_success: Option<LastSuccess>,
}

impl StreamState {
    fn new(info: StreamInfo, limits: &HistoryLimits) -> Self {
        let per = |d: Duration| (d.as_nanos() / info.interval.as_nanos().max(1)) as usize + 8;
        let (samples_cap, records_cap) = (per(limits.samples), per(limits.records));
        Self {
            facts: EpochFacts::default(),
            memo: std::sync::Mutex::new(Vec::new()),
            samples: VecDeque::with_capacity(samples_cap.min(1 << 20)),
            samples_cap,
            records: VecDeque::new(),
            records_cap,
            queries: Vec::new(),
            pending: BTreeMap::new(),
            late: VecDeque::new(),
            duplicates: VecDeque::new(),
            epoch: 0,
            last_success: None,
            info,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastSuccess {
    pub seq: u64,
    pub rtt_us: u32,
    pub recv_ns: u64,
}

pub struct Engine {
    streams: Vec<StreamState>,
    by_id: HashMap<Arc<str>, usize>,
    gaps: Vec<GapRec>,
    events: VecDeque<EventRec>,
    limits: HistoryLimits,
    /// Messages naming a stream this engine was not told about.
    pub unknown_stream_msgs: u64,
}

impl Engine {
    pub fn new(streams: Vec<StreamInfo>, limits: HistoryLimits) -> Self {
        let by_id = streams.iter().enumerate().map(|(i, s)| (s.id.clone(), i)).collect();
        let streams = streams.into_iter().map(|info| StreamState::new(info, &limits)).collect();
        Self { streams, by_id, gaps: Vec::new(), events: VecDeque::new(), limits, unknown_stream_msgs: 0 }
    }

    /// Takes a new configuration revision's streams. One the engine does not
    /// know is added at the end, in the order given — the same order the
    /// collector numbers them in. One it knows keeps its index and all of
    /// its history, and takes the new definition (interval, deadline,
    /// target). Nothing is ever removed: a stream that is gone from the
    /// configuration simply receives no more observations.
    pub fn apply_streams(&mut self, streams: impl IntoIterator<Item = StreamInfo>) {
        for info in streams {
            match self.by_id.get(&info.id) {
                Some(&idx) => {
                    self.streams[idx].info = info;
                }
                None => {
                    self.by_id.insert(info.id.clone(), self.streams.len());
                    self.streams.push(StreamState::new(info, &self.limits));
                }
            }
        }
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn stream_info(&self, idx: usize) -> &StreamInfo {
        &self.streams[idx].info
    }

    pub fn stream_index(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    pub fn gaps(&self) -> &[GapRec] {
        &self.gaps
    }

    pub fn events(&self) -> impl Iterator<Item = &EventRec> {
        self.events.iter()
    }

    pub fn records(&self, idx: usize) -> impl DoubleEndedIterator<Item = &Kept> {
        self.streams[idx].records.iter()
    }

    pub fn last_success(&self, idx: usize) -> Option<LastSuccess> {
        self.streams[idx].last_success
    }

    pub fn current_epoch(&self, idx: usize) -> u32 {
        self.streams[idx].epoch
    }

    pub fn ingest(&mut self, msg: &Msg) {
        match msg {
            Msg::Sent { stream, seq, sent_ns } => match self.streams.get_mut(*stream as usize) {
                Some(st) => {
                    st.pending.insert(*seq, *sent_ns);
                }
                None => self.unknown_stream_msgs += 1,
            },
            Msg::Record(Record::Observation(o)) => self.ingest_observation(o),
            Msg::Record(Record::LateReply(a)) => {
                if let Some(&i) = self.by_id.get(&a.stream) {
                    let at = self.streams[i].sample_time(a.seq).unwrap_or(a.at_ns);
                    self.streams[i].note(at, false);
                } else {
                    self.unknown_stream_msgs += 1;
                }
            }
            Msg::Record(Record::Duplicate(a)) => {
                if let Some(&i) = self.by_id.get(&a.stream) {
                    let at = self.streams[i].sample_time(a.seq).unwrap_or(a.at_ns);
                    self.streams[i].note(at, true);
                } else {
                    self.unknown_stream_msgs += 1;
                }
            }
            Msg::Record(Record::Gap(g)) => self.gaps.push(g.clone()),
            Msg::Record(Record::Event(e)) => {
                if self.events.len() >= self.limits.events {
                    self.events.pop_front();
                }
                self.events.push_back(e.clone());
            }
            Msg::Record(Record::Config(c)) => self.apply_streams(c.streams.iter().map(StreamInfo::from)),
            Msg::Record(Record::Segment(_) | Record::Session(_) | Record::Resolution(_) | Record::Connection(_)) => {}
        }
    }

    fn ingest_observation(&mut self, o: &Observation) {
        let Some(&i) = self.by_id.get(&o.stream) else {
            self.unknown_stream_msgs += 1;
            return;
        };
        let st = &mut self.streams[i];
        if st.records.len() >= st.records_cap {
            st.records.pop_front();
        }
        let mut kept = Kept::of(o);
        // What is the same as in the record before it is held once: where
        // it was sent from, and what was asked.
        if let (Some(new), Some(previous)) = (&kept.src, st.records.back().and_then(|k| k.src.as_ref()))
            && **new == **previous
        {
            kept.src = Some(previous.clone());
        }
        if let Some(dns) = &mut kept.dns {
            match st.queries.iter().find(|q| ***q == *dns.query) {
                Some(shared) => dns.query = shared.clone(),
                None if st.queries.len() < SHARED_QUERIES => st.queries.push(dns.query.clone()),
                None => {}
            }
        }
        st.records.push_back(kept);

        // First-attempt truth: retries and fallbacks are kept as records but
        // only the first attempt of a slot enters the statistics.
        if o.attempt > 1 {
            return;
        }
        st.pending.remove(&o.seq);
        // A new destination starts its own count of everything.
        if o.epoch > st.epoch {
            st.epoch = o.epoch;
            st.facts = EpochFacts::default();
        }
        if o.outcome == Outcome::Success
            && let (Some(rtt), Some(recv)) = (o.rtt_us, o.recv_ns)
            && st.last_success.is_none_or(|l| o.seq >= l.seq)
        {
            st.last_success = Some(LastSuccess { seq: o.seq, rtt_us: rtt, recv_ns: recv });
        }
        let sample = Sample { seq: o.seq, at_ns: o.at_ns(), outcome: o.outcome, rtt_us: o.rtt_us, epoch: o.epoch, latency_eligible: !o.has_flag(Flag::AfterReconnect) };
        // Finalisation order is not send order (a timeout finalises after later
        // successes), so insert by seq; the distance from the back is small.
        let pos = st.samples.iter().rposition(|s| s.seq <= sample.seq).map_or(0, |p| p + 1);
        if pos > 0 && st.samples[pos - 1].seq == sample.seq {
            return; // a slot is finalised once
        }
        st.samples.insert(pos, sample);
        // Counted once per slot, like the sample itself.
        if o.epoch == st.epoch {
            let at = o.at_ns();
            let facts = &mut st.facts;
            facts.first_slot_ns = Some(facts.first_slot_ns.map_or(at, |t| t.min(at)));
            if o.outcome == Outcome::Success {
                facts.first_success_ns = Some(facts.first_success_ns.map_or(at, |t| t.min(at)));
            }
            if matches!(o.outcome, Outcome::Success | Outcome::Timeout | Outcome::Unreachable | Outcome::ServiceError | Outcome::ProtocolError) {
                if facts.earliest_finalised.len() < EARLIEST_KEPT {
                    facts.earliest_finalised.push(at);
                } else if facts.earliest_finalised.peek().is_some_and(|latest| at < *latest) {
                    facts.earliest_finalised.pop();
                    facts.earliest_finalised.push(at);
                }
            }
        }
        if st.samples.len() > st.samples_cap {
            st.samples.pop_front();
        }
    }

    /// Maps a monotonic time onto the session timeline, which also counts real
    /// time that passed while the monotonic clock stood still (host suspend).
    pub fn timeline_ns(&self, mono_ns: u64) -> u64 {
        mono_ns + self.gaps.iter().filter(|g| g.end_ns <= mono_ns).map(GapRec::hidden_ns).sum::<u64>()
    }

    fn gap_between(&self, a_ns: u64, b_ns: u64) -> bool {
        self.gaps.iter().any(|g| g.start_ns >= a_ns && g.start_ns <= b_ns)
    }

    /// Statistics for `window` ending at `now_ns` (monotonic), by send time,
    /// for the stream's current destination epoch.
    pub fn stats(&self, idx: usize, window: Duration, now_ns: u64) -> WindowStats {
        let window_ns = window.as_nanos() as u64;
        let now_tl = self.timeline_ns(now_ns);
        self.stats_between(idx, now_tl.saturating_sub(window_ns), now_tl, window_ns, window_ns)
    }

    /// Statistics over everything retained for the stream ("since start").
    pub fn session_stats(&self, idx: usize, now_ns: u64) -> WindowStats {
        let now_tl = self.timeline_ns(now_ns);
        // A window that reaches back past the first sample, whenever it is asked for.
        self.stats_between(idx, 0, now_tl, now_tl.max(1) + 1, SINCE_START)
    }

    /// Statistics for the samples sent within `[from_tl, to_tl)` on the
    /// session timeline.
    ///
    /// The samples are found by position, and the result is remembered
    /// under `kind` for as long as the same samples are in the window: a
    /// dashboard asks ten times a second and a new sample comes once a
    /// second, and a day of history must cost no more to look at than a
    /// minute. What changes without a sample changing — how many probes
    /// are still waiting, the length of a since-start window — is brought
    /// up to date on the remembered result.
    fn stats_between(&self, idx: usize, from_tl: u64, to_tl: u64, window_ns: u64, kind: u64) -> WindowStats {
        let st = &self.streams[idx];
        let inside = |at_ns: u64| {
            let tl = self.timeline_ns(at_ns);
            tl >= from_tl && tl < to_tl
        };
        let range = self.span(st, from_tl, to_tl);
        // Which samples are in the window. Away from its two ends every
        // sample in the range is; at the ends, each one is tested.
        let edge = 2 * SLACK;
        let mark = |positions: std::ops::Range<usize>| positions.fold(0u64, |mask, i| (mask << 1) | u64::from(inside(st.samples[i].at_ns)));
        let (head, tail) = if range.len() <= 2 * edge { (mark(range.clone()), 0) } else { (mark(range.start..range.start + edge), mark(range.end - edge..range.end)) };
        let interval_ns = st.info.interval.as_nanos() as u64;
        let key = WindowKey {
            kind,
            first_seq: st.samples.get(range.start).map_or(u64::MAX, |s| s.seq),
            len: range.len(),
            head,
            tail,
            epoch: st.epoch,
            late: st.late.iter().filter(|&&at| inside(at)).count(),
            duplicates: st.duplicates.iter().filter(|&&at| inside(at)).count(),
            gaps: self.gaps.len(),
            interval_ns,
        };
        let pending = st.pending.values().filter(|&&at| inside(at)).count() as u64;
        let mut memo = st.memo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, known)) = memo.iter().find(|(k, _)| *k == key) {
            let mut stats = known.clone();
            stats.set_pending(pending, window_ns, interval_ns);
            return stats;
        }
        let samples: Vec<Sample> = st.samples.range(range).filter(|s| s.epoch == st.epoch && inside(s.at_ns)).copied().collect();
        let stats = stats::compute(&samples, pending, key.late as u64, key.duplicates as u64, window_ns, interval_ns, |a, b| self.gap_between(a, b));
        // A handful of windows are ever in use; the oldest result makes room.
        if memo.len() >= REMEMBERED {
            memo.remove(0);
        }
        memo.push((key, stats.clone()));
        stats
    }

    /// The positions of the samples sent within `[from_tl, to_tl)` on the
    /// session timeline, widened by `SLACK`.
    fn span(&self, st: &StreamState, from_tl: u64, to_tl: u64) -> std::ops::Range<usize> {
        let lo = st.samples.partition_point(|s| self.timeline_ns(s.at_ns) < from_tl).saturating_sub(SLACK);
        let hi = (st.samples.partition_point(|s| self.timeline_ns(s.at_ns) < to_tl) + SLACK).min(st.samples.len());
        lo..hi.max(lo)
    }

    /// Whether the stream had any slot at all, in its current epoch, before `now_ns`.
    pub fn had_slot_before(&self, idx: usize, now_ns: u64) -> bool {
        let st = &self.streams[idx];
        st.facts.first_slot_ns.is_some_and(|t| t < now_ns) || st.pending.values().any(|&at| at < now_ns)
    }

    /// Whether any probe of the current epoch sent before `now_ns` was answered as expected.
    pub fn answered_before(&self, idx: usize, now_ns: u64) -> bool {
        self.streams[idx].facts.first_success_ns.is_some_and(|t| t < now_ns)
    }

    /// How many probes of the current epoch sent before `now_ns` have been
    /// finalised, counted no further than [`EARLIEST_KEPT`].
    pub fn finalised_before(&self, idx: usize, now_ns: u64) -> u64 {
        self.streams[idx].facts.earliest_finalised.iter().filter(|&&at| at < now_ns).count() as u64
    }
}

impl StreamState {
    fn sample_time(&self, seq: u64) -> Option<u64> {
        self.samples.iter().rev().find(|s| s.seq == seq).map(|s| s.at_ns)
    }

    /// Records a late or a duplicate reply against the time its probe was
    /// sent. Kept no longer than the samples themselves are.
    fn note(&mut self, at_ns: u64, duplicate: bool) {
        let cap = self.samples_cap;
        let list = if duplicate { &mut self.duplicates } else { &mut self.late };
        list.push_back(at_ns);
        if list.len() > cap {
            list.pop_front();
        }
    }
}

impl Engine {
    /// The compact per-slot history of a stream, in send order.
    pub fn samples(&self, idx: usize) -> impl DoubleEndedIterator<Item = &Sample> {
        self.streams[idx].samples.iter()
    }

    /// The samples sent within `[from_tl, to_tl)` on the session timeline,
    /// with a few neighbours either side: found by position. The caller
    /// tests each one, as it would have tested the whole history.
    pub fn samples_near(&self, idx: usize, from_tl: u64, to_tl: u64) -> impl Iterator<Item = &Sample> {
        let st = &self.streams[idx];
        st.samples.range(self.span(st, from_tl, to_tl))
    }

    /// The latest sample sent at or before `now_ns`.
    pub fn latest_sample(&self, idx: usize, now_ns: u64) -> Option<&Sample> {
        let st = &self.streams[idx];
        let hi = (st.samples.partition_point(|s| s.at_ns <= now_ns) + SLACK).min(st.samples.len());
        st.samples.range(..hi).rev().find(|s| s.at_ns <= now_ns)
    }

    /// How many samples were sent after `after_ns`.
    pub fn samples_since(&self, idx: usize, after_ns: u64) -> usize {
        let st = &self.streams[idx];
        let lo = st.samples.partition_point(|s| s.at_ns <= after_ns).saturating_sub(SLACK);
        st.samples.range(lo..).filter(|s| s.at_ns > after_ns).count()
    }

    /// The most recently finalised record of a stream.
    pub fn latest_record(&self, idx: usize) -> Option<&Kept> {
        self.streams[idx].records.back()
    }

    /// Whether an observation gap lies between two monotonic times.
    pub fn has_gap_between(&self, a_ns: u64, b_ns: u64) -> bool {
        self.gap_between(a_ns, b_ns)
    }
}

const _: () = assert!(EARLIEST_KEPT as u64 >= stats::MIN_N_RATE, "the statuses ask whether MIN_N_RATE probes have been finalised");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script;

    impl Engine {
        /// Window statistics the plain way: every sample of the history is
        /// looked at. What `stats` must always agree with.
        fn stats_by_scan(&self, idx: usize, window: Duration, now_ns: u64) -> WindowStats {
            let st = &self.streams[idx];
            let window_ns = window.as_nanos() as u64;
            let now_tl = self.timeline_ns(now_ns);
            let from_tl = now_tl.saturating_sub(window_ns);
            let inside = |at_ns: u64| {
                let tl = self.timeline_ns(at_ns);
                tl >= from_tl && tl < now_tl
            };
            let samples: Vec<Sample> = st.samples.iter().filter(|s| s.epoch == st.epoch && inside(s.at_ns)).copied().collect();
            let pending = st.pending.values().filter(|&&at| inside(at)).count() as u64;
            let late = st.late.iter().filter(|&&at| inside(at)).count() as u64;
            let dup = st.duplicates.iter().filter(|&&at| inside(at)).count() as u64;
            stats::compute(&samples, pending, late, dup, window_ns, st.info.interval.as_nanos() as u64, |a, b| self.gap_between(a, b))
        }

        fn session_by_scan(&self, idx: usize, now_ns: u64) -> WindowStats {
            self.stats_by_scan(idx, Duration::from_nanos(self.timeline_ns(now_ns).max(1) + 1), now_ns)
        }
    }

    /// Every scripted scenario — gaps, a host suspend, late and duplicate
    /// replies, a sequence wraparound, a change of destination, skipped
    /// slots — played part of the way and all of the way, and asked at many
    /// moments, live and in the past, over short and long windows.
    #[test]
    fn looking_things_up_by_position_gives_what_walking_the_whole_history_gives() {
        let mut compared = 0u64;
        for sc in script::all() {
            let msgs = script::generate_timed(&sc, 7);
            let end = sc.duration.as_nanos() as u64;
            for stop_at in [end / 3, end] {
                let mut engine = Engine::new(sc.infos(), HistoryLimits::default());
                for (t, m) in &msgs {
                    if *t <= stop_at {
                        engine.ingest(m);
                    }
                }
                for idx in 0..engine.stream_count() {
                    // Moments on, between and beyond the samples; the past ones are a frozen view's.
                    let moments = (0..=40).map(|k| stop_at / 40 * k + 137_000_000).chain([0, 1, stop_at, stop_at + 1, stop_at + 7_000_000_000]);
                    for now in moments {
                        for window_s in [1, 10, 60, 300, 3_600, 86_400] {
                            let window = Duration::from_secs(window_s);
                            assert_eq!(engine.stats(idx, window, now), engine.stats_by_scan(idx, window, now), "{}: stream {idx}, {window_s} s window at {now}", sc.name);
                            compared += 1;
                        }
                        let session = engine.session_by_scan(idx, now);
                        // Asked twice: the second answer is the remembered one, brought up to date.
                        assert_eq!(engine.session_stats(idx, now), session, "{}: stream {idx}, since start at {now}", sc.name);
                        assert_eq!(engine.session_stats(idx, now), session, "{}: stream {idx}, since start at {now}, remembered", sc.name);
                        assert_eq!(engine.had_slot_before(idx, now), session.slots > 0, "{}: stream {idx} at {now}", sc.name);
                        assert_eq!(engine.answered_before(idx, now), session.replied > 0, "{}: stream {idx} at {now}", sc.name);
                        assert_eq!(engine.finalised_before(idx, now), session.finalised.min(EARLIEST_KEPT as u64), "{}: stream {idx} at {now}", sc.name);
                        let st = &engine.streams[idx];
                        assert_eq!(engine.latest_sample(idx, now), st.samples.iter().rfind(|s| s.at_ns <= now), "{}: stream {idx} at {now}", sc.name);
                        assert_eq!(engine.samples_since(idx, now), st.samples.iter().filter(|s| s.at_ns > now).count(), "{}: stream {idx} at {now}", sc.name);
                    }
                }
            }
        }
        assert!(compared > 30_000, "{compared} comparisons");
    }

    /// What is kept of each observation is everything the views read, at a
    /// fraction of the size, with what repeats from record to record held once.
    #[test]
    fn a_kept_record_carries_what_the_views_read_and_shares_what_repeats() {
        assert!(std::mem::size_of::<Kept>() <= 160, "a kept record has grown to {} bytes", std::mem::size_of::<Kept>());
        let sc = script::by_name("dns-only").unwrap();
        let mut engine = Engine::new(sc.infos(), HistoryLimits::default());
        let mut full: Vec<Observation> = Vec::new();
        for m in script::generate(&sc, 7) {
            if let Msg::Record(Record::Observation(o)) = &m {
                let mut o = (**o).clone();
                o.src = Some(Source { ctx: 1, ifname: Some("en0".into()), ip: Some("192.0.2.10".parse().unwrap()) });
                full.push(o.clone());
                engine.ingest(&Msg::obs(o));
            } else {
                engine.ingest(&m);
            }
        }
        let mut compared = 0;
        for idx in 0..engine.stream_count() {
            let id = engine.stream_info(idx).id.clone();
            let mine: Vec<&Observation> = full.iter().filter(|o| o.stream == id).collect();
            let kept: Vec<&Kept> = engine.records(idx).collect();
            assert_eq!(kept.len(), mine.len(), "{id}: every observation has a record");
            for (k, o) in kept.iter().zip(&mine) {
                assert_eq!((k.seq, k.attempt, k.outcome, k.at_ns(), k.dest_ip), (o.seq, o.attempt, o.outcome, o.at_ns(), o.dest_ip));
                assert_eq!((k.rtt_us, k.rtt_user_us, k.recv_delay_us, k.sched_lag_us), (o.rtt_us, o.rtt_user_us, o.recv_delay_us, o.sched_lag_us));
                assert_eq!(k.sent_utc.as_deref(), o.sent_utc.as_deref());
                assert_eq!((&*k.flags, k.error.as_deref(), k.phase.as_deref()), (o.flags.as_slice(), o.error.as_ref(), o.phase.as_ref()));
                assert_eq!(k.src.as_deref(), o.src.as_ref());
                match (&k.dns, &o.dns) {
                    (Some(k), Some(o)) => {
                        assert_eq!((&*k.query.qname, &*k.query.qtype, &*k.query.expect), (o.qname.as_str(), o.qtype.as_str(), o.expect.as_str()));
                        assert_eq!((k.rcode.as_deref(), k.answers), (o.rcode.as_deref(), o.answers));
                    }
                    (None, None) => {}
                    other => panic!("{id}: DNS details kept for one and not the other: {other:?}"),
                }
                compared += 1;
            }
            // One source and a few queries behind all of a stream's records.
            let sources: std::collections::HashSet<*const Source> = kept.iter().filter_map(|k| k.src.as_ref().map(Arc::as_ptr)).collect();
            assert!(sources.len() <= 1, "{id}: {} copies of the same source", sources.len());
            let queries: std::collections::HashSet<*const DnsQuery> = kept.iter().filter_map(|k| k.dns.as_ref().map(|d| Arc::as_ptr(&d.query))).collect();
            let asked: std::collections::HashSet<(&str, &str)> = mine.iter().filter_map(|o| o.dns.as_ref().map(|d| (d.qname.as_str(), d.qtype.as_str()))).collect();
            assert_eq!(queries.len(), asked.len(), "{id}: each query is held once");
        }
        assert!(compared > 100, "{compared} records compared");
    }

    /// What is remembered is not served once anything it rests on has
    /// changed: asked after every single message, as a dashboard asks, at
    /// the moment of the change and again a little later with nothing new.
    #[test]
    fn remembered_statistics_follow_every_change() {
        for name in ["late-dup-reorder", "sleep-resume", "address-change", "collector-overload"] {
            let sc = script::by_name(name).unwrap();
            let mut engine = Engine::new(sc.infos(), HistoryLimits::default());
            let mut checked = 0;
            for (t, m) in script::generate_timed(&sc, 7) {
                engine.ingest(&m);
                for idx in 0..engine.stream_count() {
                    for now in [t, t + 40_000_000, t + 900_000_000] {
                        assert_eq!(engine.session_stats(idx, now), engine.session_by_scan(idx, now), "{name}: stream {idx}, since start at {now}");
                        for window_s in [60, 300] {
                            let window = Duration::from_secs(window_s);
                            assert_eq!(engine.stats(idx, window, now), engine.stats_by_scan(idx, window, now), "{name}: stream {idx}, {window_s} s at {now}");
                        }
                        checked += 1;
                    }
                }
            }
            assert!(checked > 1_000, "{name}: {checked}");
        }
    }
}

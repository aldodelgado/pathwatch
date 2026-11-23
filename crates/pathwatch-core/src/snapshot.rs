//! Immutable view models for the dashboard.
//!
//! A [`Snapshot`] is a pure function of the processor's state, the view the
//! user has selected, and a point in time. The renderer only draws it; it
//! cannot reach the network or change a measurement, and drawing more or less
//! often cannot change a number.

use std::sync::Arc;
use std::time::Duration;

use crate::engine::{Engine, Kept};
use crate::incident::{IncidentKind, IncidentTracker, Rule};
use crate::model::{Family, Outcome, Protocol};
use crate::stats::WindowStats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Icmp,
    Dns,
    /// DNS over HTTPS.
    Encrypted,
    /// DNS over TLS.
    Dot,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Icmp, Mode::Dns, Mode::Encrypted, Mode::Dot];

    pub fn protocol(self) -> Protocol {
        match self {
            Mode::Icmp => Protocol::Icmp,
            Mode::Dns => Protocol::DnsUdp,
            Mode::Encrypted => Protocol::Doh,
            Mode::Dot => Protocol::Dot,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Icmp => "ICMP echo",
            Mode::Dns => "DNS over UDP",
            Mode::Encrypted => "DNS over HTTPS",
            Mode::Dot => "DNS over TLS",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Mode::Icmp => "ICMP",
            Mode::Dns => "DNS/UDP",
            Mode::Encrypted => "DoH",
            Mode::Dot => "DoT",
        }
    }

    /// What the latency series of this mode measures.
    pub fn metric(self) -> &'static str {
        match self {
            Mode::Icmp => "ICMP echo RTT",
            Mode::Dns => "DNS/UDP transaction time",
            Mode::Encrypted => "DoH request time, reused connection",
            Mode::Dot => "DoT query time, reused connection",
        }
    }

    pub fn step(self, forward: bool) -> Self {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + if forward { 1 } else { Self::ALL.len() - 1 }) % Self::ALL.len()]
    }
}

pub const WINDOWS: [Duration; 6] =
    [Duration::from_secs(60), Duration::from_secs(300), Duration::from_secs(900), Duration::from_secs(3600), Duration::from_secs(6 * 3600), Duration::from_secs(24 * 3600)];

/// What the user is looking at. Everything a snapshot depends on besides data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSpec {
    pub mode: Mode,
    pub family: Family,
    pub window: Duration,
    /// When set, the view is frozen at this session time. Collection continues.
    pub frozen_at_ns: Option<u64>,
    /// Time columns to downsample into: the width of the chart's plot area.
    pub columns: u16,
}

impl Default for ViewSpec {
    fn default() -> Self {
        Self { mode: Mode::Icmp, family: Family::V4, window: Duration::from_secs(300), frozen_at_ns: None, columns: 100 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    Collecting,
    NeedsConfig(String),
    Unsupported(String),
    Disabled,
}

#[derive(Debug, Clone)]
pub struct StreamMeta {
    /// Index of the stream in the processor.
    pub idx: usize,
    pub provider: Arc<str>,
    pub protocol: Protocol,
    pub family: Family,
    pub fresh: bool,
    pub configured: Arc<str>,
    pub state: StreamState,
}

#[derive(Debug, Clone)]
pub struct ProviderMeta {
    pub key: Arc<str>,
    pub name: String,
    pub slot: u8,
    /// Set when the provider as a whole has nothing configured.
    pub needs_config: Option<String>,
    /// The provider is switched off in the configuration.
    pub disabled: bool,
}

/// The health of a card. Carried by a symbol and a word in the interface,
/// never by colour alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    WarmingUp,
    Lossy,
    /// The recent median is well above the preceding baseline.
    Slow,
    Errors,
    NoReply,
    LocalErr,
    Stale,
    NeedsConfig(String),
    Unsupported(String),
    Disabled,
}

impl Status {
    pub fn word(&self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::WarmingUp => "WARMING UP",
            Status::Lossy => "LOSSY",
            Status::Slow => "SLOW",
            Status::Errors => "ERRORS",
            Status::NoReply => "NO REPLY",
            Status::LocalErr => "LOCAL ERR",
            Status::Stale => "STALE",
            Status::NeedsConfig(_) => "NEEDS CONFIG",
            Status::Unsupported(_) => "UNSUPPORTED",
            Status::Disabled => "DISABLED",
        }
    }

    /// True when nothing is being sent for this card.
    pub fn is_inactive(&self) -> bool {
        matches!(self, Status::NeedsConfig(_) | Status::Unsupported(_) | Status::Disabled)
    }
}

/// One time column of a latency series. `n == 0` means the column has no
/// round-trip time at all — it is drawn as a hole, never interpolated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Column {
    pub n: u32,
    pub min_us: u32,
    pub med_us: u32,
    pub max_us: u32,
}

/// What happened in one time column, for the outcome timeline. When several
/// things happened the worst is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// No slot fell in this column (before the session, or not collecting).
    Empty,
    Replied,
    SomeUnanswered,
    NoneAnswered,
    ErrorResponse,
    LocalError,
    Skipped,
    /// The instrument was not observing. Not an outage.
    Gap,
}

#[derive(Debug, Clone)]
pub struct Card {
    pub slot: u8,
    pub key: Arc<str>,
    pub name: String,
    pub status: Status,
    /// The stream shown, if this provider has one for the mode and family.
    pub stream: Option<Arc<str>>,
    /// The address actually probed, or the target as configured.
    pub target: String,
    pub last_us: Option<u32>,
    pub last_age_ms: Option<u64>,
    pub stats: Option<WindowStats>,
    pub columns: Vec<Column>,
    pub cells: Vec<Cell>,
    /// Status of the same provider's stream in the other address family, so a
    /// failure there stays visible.
    pub other_family: Option<Status>,
    /// Round-trip times in the window, in fixed, labelled bins.
    pub histogram: Vec<Bin>,
    /// The same statistics over everything retained ("since start").
    pub session: Option<WindowStats>,
    /// The most recent observations, newest first.
    pub raw: Vec<RawRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bin {
    pub lo_us: u32,
    /// The bin's upper edge; for an open bin, the largest value in it.
    pub hi_us: u32,
    pub count: u32,
    /// The last bin when it holds everything above the binned range.
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    pub utc: String,
    pub seq: u64,
    pub outcome: Outcome,
    pub rtt_us: Option<u32>,
    pub recv_delay_us: Option<u32>,
    pub sched_lag_us: Option<u64>,
    /// Flags and, for anything but a success, the reason.
    pub note: String,
}

/// What one configured query has returned on one DNS stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryRow {
    pub qname: String,
    pub qtype: String,
    pub expect: String,
    pub sent: u32,
    /// Result label (an RCODE, or `timeout`, `unreachable`, …) and its count.
    pub results: Vec<(String, u32)>,
}

#[derive(Debug, Clone)]
pub struct DnsView {
    pub provider: Arc<str>,
    pub label: String,
    pub queries: Vec<QueryRow>,
    /// The latest transaction, in words.
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    pub at_ns: u64,
    pub utc: String,
    /// `▲` opened, `●` recovered, `╌` gap, `◆` change, `!` local, `·` note.
    pub kind: char,
    pub stream: String,
    pub text: String,
    /// The providers this event is about. Empty for an event about the
    /// whole session (a gap, the collector, the recorder).
    pub providers: Vec<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub view: ViewSpec,
    /// The session time the view shows: now, or the freeze point.
    pub as_of_ns: u64,
    /// The latest session time known, for the session clock and freeze notice.
    pub live_ns: u64,
    pub cards: Vec<Card>,
    /// Top of the common latency scale.
    pub y_max_us: u32,
    pub events: Vec<EventLine>,
    /// Observations finalised since the view was frozen.
    pub new_since_freeze: u64,
    /// Every stream of every provider, across protocols and families, for the
    /// details view.
    pub streams: Vec<StreamRow>,
    /// Per-query results of every DNS stream.
    pub dns: Vec<DnsView>,
    /// The optional controls for the address family in view. Never cards.
    pub controls: Vec<ControlRow>,
    /// Where the latest probe of the family in view was sent from, in words:
    /// `via 192.0.2.10 on en0`. It follows the host when its network changes.
    pub source: Option<String>,
}

/// A control: the default gateway, or the system's resolver. Secondary to
/// the four providers, and there to answer one question during an incident:
/// was the local segment still fine?
#[derive(Debug, Clone)]
pub struct ControlRow {
    pub label: &'static str,
    pub target: String,
    pub status: Status,
    pub last_us: Option<u32>,
    pub stats: WindowStats,
    /// What a reader must know to interpret this row, if anything.
    pub note: Option<&'static str>,
    /// The same in a word, for where there is no room for the sentence.
    pub tag: Option<&'static str>,
}

fn title_case(key: &str) -> String {
    let mut c = key.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

/// The cards and streams a session shows, from the records that describe
/// it: the session's opening record, or a configuration reload. Used the
/// same way by a live session after a reload and by a replay, so both show
/// what was recorded and nothing else.
///
/// `providers` are those the configuration listed; a provider that only
/// appears on a stream is given the next free slot. A control is never a
/// provider. `index_of` gives a stream's index in the engine.
pub fn metas_from(streams: &[crate::model::StreamRec], providers: &[crate::model::ProviderRec], index_of: &dyn Fn(&str) -> Option<usize>) -> (Vec<ProviderMeta>, Vec<StreamMeta>) {
    use crate::localnet::is_control;
    let metas: Vec<StreamMeta> = streams
        .iter()
        .filter_map(|s| {
            let reason = || s.reason.clone().unwrap_or_else(|| "no reason recorded".into());
            Some(StreamMeta {
                idx: index_of(&s.stream)?,
                provider: s.provider.clone(),
                protocol: s.protocol,
                family: s.family,
                fresh: s.stream.ends_with("/fresh"),
                configured: s.configured.clone(),
                state: match s.state.as_str() {
                    "collecting" => StreamState::Collecting,
                    "needs_config" => StreamState::NeedsConfig(reason()),
                    "unsupported" => StreamState::Unsupported(reason()),
                    _ => StreamState::Disabled,
                },
            })
        })
        .collect();
    let mut listed: Vec<crate::model::ProviderRec> = providers.iter().filter(|p| !is_control(&p.key)).cloned().collect();
    for s in streams.iter().filter(|s| !is_control(&s.provider)) {
        if !listed.iter().any(|p| p.key == s.provider) {
            let slot = (1..=u8::MAX).find(|n| !listed.iter().any(|p| p.slot == *n)).unwrap_or(u8::MAX);
            listed.push(crate::model::ProviderRec { key: s.provider.clone(), name: title_case(&s.provider), slot, state: String::new(), reason: None });
        }
    }
    listed.sort_by_key(|p| p.slot);
    let cards = listed
        .into_iter()
        .map(|p| {
            let mine: Vec<&StreamMeta> = metas.iter().filter(|s| s.provider == p.key).collect();
            let unconfigured = mine.iter().find_map(|s| match &s.state {
                StreamState::NeedsConfig(r) => Some(r.clone()),
                _ => None,
            });
            // What the record says about the provider, where it says; else
            // what its streams imply.
            let needs_config = match p.state.as_str() {
                "needs_config" => Some(p.reason.clone().unwrap_or_else(|| "no target configured".to_string())),
                "collecting" | "disabled" => None,
                _ if mine.is_empty() => Some("no target configured".to_string()),
                _ if mine.iter().all(|s| matches!(s.state, StreamState::NeedsConfig(_))) => unconfigured,
                _ => None,
            };
            ProviderMeta { disabled: p.state == "disabled", key: p.key, name: p.name, slot: p.slot, needs_config }
        })
        .collect();
    (cards, metas)
}

fn source_in_words(engine: &Engine, streams: &[StreamMeta], view: &ViewSpec, as_of_ns: u64) -> Option<String> {
    streams
        .iter()
        .filter(|m| m.family == view.family && !crate::localnet::is_control(&m.provider))
        .filter_map(|m| engine.records(m.idx).rev().find(|o| o.at_ns() <= as_of_ns && o.src.as_ref().is_some_and(|s| s.ip.is_some())))
        .max_by_key(|o| o.at_ns())
        .and_then(|o| o.src.clone())
        .and_then(|s| Some(format!("via {}{}", s.ip?, s.ifname.as_ref().map_or_else(String::new, |n| format!(" on {n}")))))
}

fn control_rows(engine: &Engine, incidents: &IncidentTracker, streams: &[StreamMeta], view: &ViewSpec, as_of_ns: u64) -> Vec<ControlRow> {
    use crate::localnet::{GATEWAY, is_control};
    streams
        .iter()
        .filter(|m| is_control(&m.provider) && m.family == view.family)
        .map(|m| {
            let gateway = &*m.provider == GATEWAY;
            let stats = engine.stats(m.idx, view.window, as_of_ns);
            let dest = engine.latest_record(m.idx).and_then(|o| o.dest_ip).or_else(|| m.configured.parse().ok());
            let never_answered = gateway && m.state == StreamState::Collecting && !engine.answered_before(m.idx, as_of_ns) && engine.finalised_before(m.idx, as_of_ns) >= 10;
            // Where the system's resolver is decides what its row can say.
            let resolver = (!gateway).then_some(dest).flatten().map(crate::localnet::ResolverKind::of);
            let (note, tag) = match resolver {
                _ if never_answered => (Some("no echo reply so far; some gateways never answer, so this may say nothing"), Some("silent")),
                Some(kind) => (kind.note(), kind.tag()),
                None => (None, None),
            };
            ControlRow {
                label: match resolver {
                    _ if gateway => "Gateway",
                    // "Local" would be untrue of a resolver across the internet.
                    Some(crate::localnet::ResolverKind::Remote) => "System DNS",
                    _ => "Local DNS",
                },
                target: dest.map_or_else(|| m.configured.to_string(), |ip| ip.to_string()),
                status: derive_status(engine, incidents, m, &stats, as_of_ns),
                last_us: engine.last_success(m.idx).filter(|l| l.recv_ns <= as_of_ns).map(|l| l.rtt_us),
                stats,
                note,
                tag,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct StreamRow {
    pub provider: Arc<str>,
    pub label: String,
    pub target: String,
    pub status: Status,
    pub last_us: Option<u32>,
    pub stats: WindowStats,
}

fn stream_label(id: &str) -> String {
    // "cloudflare/resolver-v4/icmp/4" → "cloudflare icmp v4"
    let parts: Vec<&str> = id.split('/').collect();
    match parts.as_slice() {
        [provider, _, proto, family, rest @ ..] => format!("{provider} {proto} v{family}{}", if rest.is_empty() { "" } else { " fresh" }),
        _ => id.to_string(),
    }
}

fn derive_status(engine: &Engine, incidents: &IncidentTracker, meta: &StreamMeta, window: &WindowStats, as_of_ns: u64) -> Status {
    match &meta.state {
        StreamState::NeedsConfig(r) => return Status::NeedsConfig(r.clone()),
        StreamState::Unsupported(r) => return Status::Unsupported(r.clone()),
        StreamState::Disabled => return Status::Disabled,
        StreamState::Collecting => {}
    }
    let info = engine.stream_info(meta.idx);
    let recent = engine.stats(meta.idx, Duration::from_secs(60), as_of_ns);
    // Incident state is read as of the displayed time, so a frozen view
    // shows what was true then, not what is true now.
    let open = |rule: Rule| incidents.is_open_at(meta.idx, rule, as_of_ns);
    if open(Rule::NoReplyRun) {
        return Status::NoReply;
    }
    // Sends are failing on this host: nothing reached the network.
    if open(Rule::LocalError) || (recent.sent == 0 && recent.local_error > 0) {
        return Status::LocalErr;
    }
    if !engine.had_slot_before(meta.idx, as_of_ns) {
        return Status::WarmingUp;
    }
    // Collection should be producing results and is not.
    let quiet_for = engine.latest_sample(meta.idx, as_of_ns).map(|s| as_of_ns.saturating_sub(s.at_ns));
    let stale_after = (info.interval * 3).max(Duration::from_secs(5)) + info.deadline;
    if quiet_for.is_some_and(|q| q > stale_after.as_nanos() as u64) {
        return Status::Stale;
    }
    // Each of these has hysteresis in the incident tracker, so a stream at
    // the edge of a threshold does not flicker between words. An open
    // incident is known trouble: it outranks "too young to judge".
    if open(Rule::ErrorResponses) {
        return Status::Errors;
    }
    if open(Rule::ElevatedNoReply) {
        return Status::Lossy;
    }
    if open(Rule::LatencyShift) {
        return Status::Slow;
    }
    if engine.finalised_before(meta.idx, as_of_ns) < crate::stats::MIN_N_RATE {
        return Status::WarmingUp;
    }
    let _ = window;
    Status::Ok
}

fn nice_ceiling(us: u32) -> u32 {
    // 1, 2, 4, 8 × 10^k milliseconds, so the axis has round quarter marks.
    let ms = (us as f64 / 1000.0).max(1.0);
    let pow = 10f64.powf(ms.log10().floor());
    let step = [1.0, 2.0, 4.0, 8.0, 10.0].into_iter().find(|s| ms <= s * pow).unwrap_or(10.0);
    (step * pow * 1000.0) as u32
}

/// The top of the common latency scale, from each time column's
/// (median, maximum) round trip.
///
/// The scale is set by the bulk of the data so that one slow reply cannot
/// press everything else into the bottom row: it covers the highest round
/// trip of 95 % of the columns. Values up to twice that are simply included.
/// Anything higher is off the scale — the renderer marks those columns and
/// states the true maximum; nothing is hidden or altered. A change that
/// lasts (more than a twentieth of the window) is data, and sets the scale.
pub fn scale_top(points: &[(u32, u32)]) -> u32 {
    const FLOOR_US: u32 = 8_000;
    let mut maxima: Vec<u32> = points.iter().map(|p| p.1).collect();
    maxima.sort_unstable();
    let bulk = crate::stats::nearest_rank(&maxima, 95, 100).unwrap_or(0);
    let top = nice_ceiling(bulk.max(FLOOR_US));
    // Whatever lies within twice that is shown, whether or not something
    // far beyond it is also present.
    match maxima.iter().rev().find(|v| **v <= top.saturating_mul(2)) {
        Some(near) if *near > top => nice_ceiling(*near),
        _ => top,
    }
}

/// Builds the dashboard's view of the data at `now_ns`.
pub fn build(engine: &Engine, incidents: &IncidentTracker, providers: &[ProviderMeta], streams: &[StreamMeta], view: &ViewSpec, now_ns: u64) -> Snapshot {
    let as_of_ns = view.frozen_at_ns.unwrap_or(now_ns).min(now_ns);
    let window_ns = view.window.as_nanos() as u64;
    let cols = usize::from(view.columns.max(1));
    // The window always ends at "now" on the right. When the session is
    // younger than the window its start lies before time zero, so the
    // arithmetic is signed: early columns are simply empty.
    let to_tl = engine.timeline_ns(as_of_ns) as i128;
    let from_tl = to_tl - window_ns as i128;
    let span = (window_ns as i128).max(1);
    // Which of `n` equal time columns across the window a moment falls in.
    let column_of = |at_ns: u64, n: usize| -> Option<usize> {
        let tl = engine.timeline_ns(at_ns) as i128;
        (tl >= from_tl && tl < to_tl).then(|| (((tl - from_tl) * n as i128 / span) as usize).min(n - 1))
    };

    let primary = |provider: &str, family: Family| streams.iter().find(|s| &*s.provider == provider && s.protocol == view.mode.protocol() && s.family == family && !s.fresh);

    let mut cards = Vec::with_capacity(providers.len());
    // The (median, maximum) of every time column that has data, for the scale.
    let mut scale_points: Vec<(u32, u32)> = Vec::new();
    for p in providers {
        let other = match view.family {
            Family::V4 => Family::V6,
            Family::V6 => Family::V4,
        };
        let other_family = primary(&p.key, other).map(|m| {
            let w = engine.stats(m.idx, view.window, as_of_ns);
            derive_status(engine, incidents, m, &w, as_of_ns)
        });
        let mut card = Card {
            slot: p.slot,
            key: p.key.clone(),
            name: p.name.clone(),
            status: Status::Disabled,
            stream: None,
            target: String::new(),
            last_us: None,
            last_age_ms: None,
            stats: None,
            columns: vec![Column::default(); cols],
            cells: vec![Cell::Empty; cols],
            other_family,
            histogram: Vec::new(),
            session: None,
            raw: Vec::new(),
        };
        let Some(meta) = primary(&p.key, view.family) else {
            card.status = match &p.needs_config {
                _ if p.disabled => Status::Disabled,
                Some(reason) => Status::NeedsConfig(reason.clone()),
                None => Status::Unsupported(format!("no {} target for IPv{}", view.mode.short(), view.family.as_str())),
            };
            cards.push(card);
            continue;
        };
        let info = engine.stream_info(meta.idx);
        let stats = engine.stats(meta.idx, view.window, as_of_ns);
        card.stream = Some(info.id.clone());
        card.status = derive_status(engine, incidents, meta, &stats, as_of_ns);
        card.target = engine.latest_record(meta.idx).and_then(|o| o.dest_ip).map_or_else(|| meta.configured.to_string(), |ip| ip.to_string());
        if let Some(last) = engine.last_success(meta.idx).filter(|l| l.recv_ns <= as_of_ns) {
            card.last_us = Some(last.rtt_us);
            card.last_age_ms = Some(as_of_ns.saturating_sub(last.recv_ns) / 1_000_000);
        }

        // Downsample into columns. Each column keeps the spread of its round
        // trips (so a single spike survives) and a tally of outcomes.
        #[derive(Default, Clone)]
        struct Tally {
            rtts: Vec<u32>,
            replied: u32,
            unanswered: u32,
            errors: u32,
            local: u32,
            skipped: u32,
        }
        // A column never covers less than one probe interval: when the plot
        // is wider than the window has slots, each slot spans several cells
        // (a step, not a dotted line), so a run of unanswered probes reads as
        // a solid bar and a run of replies as a continuous line.
        let data_cols = cols.min(((window_ns / (info.interval.as_nanos() as u64).max(1)) as usize).max(1));
        let mut tallies = vec![Tally::default(); data_cols];
        // Only the window's own samples: found by position, however long the history.
        let (near_from, near_to) = (from_tl.max(0) as u64, to_tl.max(0) as u64);
        for s in engine.samples_near(meta.idx, near_from, near_to).filter(|s| s.at_ns <= as_of_ns) {
            let Some(c) = column_of(s.at_ns, data_cols) else { continue };
            let t = &mut tallies[c];
            match s.outcome {
                Outcome::Success => {
                    t.replied += 1;
                    if let (Some(rtt), true) = (s.rtt_us, s.latency_eligible) {
                        t.rtts.push(rtt);
                    }
                }
                Outcome::Timeout | Outcome::Unreachable => t.unanswered += 1,
                Outcome::ServiceError | Outcome::ProtocolError => t.errors += 1,
                Outcome::LocalError => t.local += 1,
                Outcome::Skipped => t.skipped += 1,
                Outcome::Cancelled => {}
            }
        }
        let mut data_columns = vec![Column::default(); data_cols];
        let mut data_cells = vec![Cell::Empty; data_cols];
        for (i, t) in tallies.iter_mut().enumerate() {
            if !t.rtts.is_empty() {
                t.rtts.sort_unstable();
                let n = t.rtts.len();
                data_columns[i] = Column { n: n as u32, min_us: t.rtts[0], med_us: t.rtts[(n - 1) / 2], max_us: t.rtts[n - 1] };
                scale_points.push((t.rtts[(n - 1) / 2], t.rtts[n - 1]));
            }
            data_cells[i] = if t.unanswered > 0 && t.replied + t.errors == 0 {
                Cell::NoneAnswered
            } else if t.unanswered > 0 {
                Cell::SomeUnanswered
            } else if t.errors > 0 {
                Cell::ErrorResponse
            } else if t.local > 0 {
                Cell::LocalError
            } else if t.skipped > 0 {
                Cell::Skipped
            } else if t.replied > 0 {
                Cell::Replied
            } else {
                Cell::Empty
            };
        }
        for i in 0..cols {
            card.columns[i] = data_columns[i * data_cols / cols];
            card.cells[i] = data_cells[i * data_cols / cols];
        }
        // Columns in which nothing was sent because the instrument was not
        // observing are gaps — exact to the gap record, not to column edges.
        for g in engine.gaps() {
            let gap_start = engine.timeline_ns(g.start_ns.saturating_sub(1)) as i128;
            let start_tl = gap_start.max(from_tl);
            let end_tl = (gap_start + i128::from(g.duration_ms) * 1_000_000).min(to_tl);
            if end_tl <= start_tl {
                continue;
            }
            let first = ((start_tl - from_tl) * cols as i128 / span) as usize;
            let last = (((end_tl - from_tl) * cols as i128 / span) as usize).min(cols - 1);
            for cell in &mut card.cells[first.min(cols - 1)..=last] {
                if *cell == Cell::Empty {
                    *cell = Cell::Gap;
                }
            }
        }
        let epoch = engine.current_epoch(meta.idx);
        let rtts: Vec<u32> = engine
            .samples_near(meta.idx, near_from, near_to)
            .filter(|s| s.at_ns <= as_of_ns && s.epoch == epoch && s.outcome == Outcome::Success && s.latency_eligible && column_of(s.at_ns, 1).is_some())
            .filter_map(|s| s.rtt_us)
            .collect();
        card.histogram = histogram(&rtts);
        card.session = Some(engine.session_stats(meta.idx, as_of_ns));
        card.raw = engine.records(meta.idx).rev().filter(|o| o.at_ns() <= as_of_ns).take(200).map(raw_row).collect();
        card.stats = Some(stats);
        cards.push(card);
    }

    let mut events: Vec<EventLine> = Vec::new();
    // Incidents that opened together on comparable streams are one event,
    // which also says which comparable streams were not affected.
    let groups = crate::correlate::groups(engine, incidents, streams, as_of_ns);
    let grouped: std::collections::HashSet<usize> = groups.iter().flat_map(|g| g.events.iter().copied()).collect();
    for g in &groups {
        // A group concerns every provider it names, affected or not.
        let mut providers: Vec<Arc<str>> = g.affected.iter().chain(&g.unaffected).map(|&i| engine.stream_info(i).provider.clone()).collect();
        providers.sort();
        providers.dedup();
        events.push(EventLine { at_ns: g.at_ns, utc: g.utc.clone().unwrap_or_default(), kind: '▲', stream: g.label.clone(), text: g.text.clone(), providers });
    }
    for (i, e) in incidents.events().iter().enumerate() {
        if grouped.contains(&i) {
            continue;
        }
        events.push(EventLine {
            at_ns: e.at_ns,
            utc: e.utc.clone().or_else(|| incidents.utc_at(e.at_ns)).unwrap_or_default(),
            kind: match e.kind {
                IncidentKind::Opened => '▲',
                IncidentKind::Closed | IncidentKind::FlappingEnded => '●',
                IncidentKind::Flapping => '◆',
            },
            stream: stream_label(&engine.stream_info(e.stream).id),
            text: e.text.clone(),
            providers: vec![engine.stream_info(e.stream).provider.clone()],
        });
    }
    for g in engine.gaps() {
        events.push(EventLine {
            at_ns: g.end_ns,
            utc: g.end_utc.clone(),
            kind: '╌',
            stream: "all streams".into(),
            text: format!("observation gap of {} ({:?}) — nothing was sent in it; excluded from every rate", human_duration(g.duration_ms), g.cause).to_lowercase(),
            providers: Vec::new(),
        });
    }
    for e in engine.events() {
        let kind = match e.event.as_str() {
            "address_changed" | "address_pinned" => '◆',
            "bootstrap_failed" | "icmp_socket_error" | "tls_unavailable" | "recording_stopped" => '!',
            _ => '·',
        };
        // A collector event names its stream or target as `provider/…`.
        let providers = e.stream.as_deref().and_then(|s| s.split('/').next()).map(|p| vec![Arc::from(p)]).unwrap_or_default();
        events.push(EventLine { at_ns: e.at_ns, utc: e.utc.clone(), kind, stream: e.stream.as_deref().unwrap_or("collector").to_string(), text: e.detail.clone(), providers });
    }
    events.retain(|e| e.at_ns <= as_of_ns);
    events.sort_by_key(|e| std::cmp::Reverse(e.at_ns));
    events.truncate(200);

    let new_since_freeze = match view.frozen_at_ns {
        Some(f) => streams.iter().map(|m| engine.samples_since(m.idx, f) as u64).sum(),
        None => 0,
    };
    let rows = streams
        .iter()
        .map(|m| {
            let info = engine.stream_info(m.idx);
            let stats = engine.stats(m.idx, view.window, as_of_ns);
            StreamRow {
                provider: m.provider.clone(),
                label: format!("{} v{}{}", info.protocol.as_str(), info.family.as_str(), if m.fresh { " fresh" } else { "" }),
                target: engine.latest_record(m.idx).and_then(|o| o.dest_ip).map_or_else(|| m.configured.to_string(), |ip| ip.to_string()),
                status: derive_status(engine, incidents, m, &stats, as_of_ns),
                last_us: engine.last_success(m.idx).filter(|l| l.recv_ns <= as_of_ns).map(|l| l.rtt_us),
                stats,
            }
        })
        .collect();
    Snapshot {
        view: view.clone(),
        as_of_ns,
        live_ns: now_ns,
        cards,
        y_max_us: scale_top(&scale_points),
        events,
        new_since_freeze,
        streams: rows,
        dns: dns_views(engine, streams, as_of_ns),
        controls: control_rows(engine, incidents, streams, view, as_of_ns),
        source: source_in_words(engine, streams, view, as_of_ns),
    }
}

/// Bins round-trip times with round, labelled edges: a 1–2–5 step chosen so
/// there are at most twelve rows.
///
/// The bins cover the bulk of the data — up to the 95th percentile, or the
/// whole range when the tail reaches no more than twice as far — so that one
/// slow reply cannot stretch the bins until everything else lands in one.
/// Whatever lies above the binned range is counted in a single open last
/// bin, whose `hi_us` is the true maximum. Every sample is in exactly one bin.
pub fn histogram(rtts: &[u32]) -> Vec<Bin> {
    let mut sorted = rtts.to_vec();
    sorted.sort_unstable();
    let (Some(&lo), Some(&hi)) = (sorted.first(), sorted.last()) else { return Vec::new() };
    let bulk = crate::stats::nearest_rank(&sorted, 95, 100).unwrap_or(hi);
    let (cover, rows) = if hi - lo <= (bulk - lo).saturating_mul(2) { (hi, 12) } else { (bulk, 11) };
    let mut step = 100u32; // 0.1 ms
    loop {
        let start = lo / step * step;
        let bins = (cover - start) / step + 1;
        if bins <= rows {
            let mut out: Vec<Bin> = (0..bins).map(|i| Bin { lo_us: start + i * step, hi_us: start + (i + 1) * step, count: 0, open: false }).collect();
            let mut above = 0;
            for &r in &sorted {
                match out.get_mut(((r - start) / step) as usize) {
                    Some(bin) => bin.count += 1,
                    None => above += 1,
                }
            }
            if above > 0 {
                out.push(Bin { lo_us: start + bins * step, hi_us: hi, count: above, open: true });
            }
            return out;
        }
        // 100 → 200 → 500 → 1000 → …
        let lead = step / 10u32.pow(step.ilog10());
        step = if lead == 2 { step / 2 * 5 } else { step * 2 };
    }
}

fn raw_row(o: &Kept) -> RawRow {
    let mut notes: Vec<String> = o.flags.iter().map(|f| format!("{f:?}").to_lowercase()).collect();
    if let Some(e) = &o.error {
        let category = format!("{:?}", e.category);
        notes.push(match &e.detail {
            Some(d) => format!("{category}: {d}"),
            None => category,
        });
    }
    if let (Some(d), Outcome::Success) = (&o.dns, o.outcome) {
        notes.push(format!("{} {} → {}", d.query.qname, d.query.qtype, d.rcode.as_deref().unwrap_or("?")));
    }
    // A probe that made a connection of its own: the parts its time is
    // made of, as far as it got. A query on a connection already open has
    // no such parts and gets no note.
    if let Some(p) = o.phase.as_deref().filter(|p| p.connect_us.is_some()) {
        let parts: Vec<String> = [("connect", p.connect_us), ("tls", p.tls_us), ("query", p.request_us)]
            .into_iter()
            .filter_map(|(name, v)| v.map(|us| format!("{name} {:.1}", f64::from(us) / 1000.0)))
            .collect();
        notes.push(format!("{} ms", parts.join(" + ")));
    }
    RawRow {
        utc: o.sent_utc.as_deref().unwrap_or_default().to_string(),
        seq: o.seq,
        outcome: o.outcome,
        rtt_us: o.rtt_us,
        recv_delay_us: o.recv_delay_us,
        sched_lag_us: o.sched_lag_us,
        note: notes.join(" · "),
    }
}

pub fn outcome_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Success => "success",
        Outcome::Timeout => "timeout",
        Outcome::Unreachable => "unreachable",
        Outcome::ServiceError => "error response",
        Outcome::ProtocolError => "invalid response",
        Outcome::LocalError => "local error",
        Outcome::Skipped => "skipped",
        Outcome::Cancelled => "cancelled",
    }
}

/// An error category as it is written in the log: `no_route`, `certificate`.
fn category_word(c: crate::model::ErrorCategory) -> String {
    serde_json::to_value(c).ok().and_then(|v| v.as_str().map(|s| s.replace('_', " "))).unwrap_or_else(|| format!("{c:?}").to_lowercase())
}

fn dns_views(engine: &Engine, streams: &[StreamMeta], as_of_ns: u64) -> Vec<DnsView> {
    let mut out = Vec::new();
    for m in streams.iter().filter(|m| m.protocol.is_dns() && m.state == StreamState::Collecting) {
        let info = engine.stream_info(m.idx);
        let mut queries: Vec<QueryRow> = Vec::new();
        // The newest transaction is written out once, after the tally: this
        // runs over an hour of records for every snapshot, so nothing is
        // allocated per record that the tally does not keep.
        let mut newest = None;
        for o in engine.records(m.idx).filter(|o| o.attempt == 1 && o.at_ns() <= as_of_ns) {
            let Some(d) = &o.dns else { continue };
            // The result as the resolver gave it where there is one, otherwise what happened.
            // Without a response, the reason where one is recorded.
            let label: std::borrow::Cow<'_, str> = match (&d.rcode, o.outcome, &o.error) {
                (Some(rc), Outcome::Success | Outcome::ServiceError, _) => rc.as_ref().into(),
                (_, other @ (Outcome::ProtocolError | Outcome::LocalError), Some(e)) => format!("{} ({})", outcome_word(other), category_word(e.category)).into(),
                (_, other, _) => outcome_word(other).into(),
            };
            let asked = &d.query;
            let row = match queries.iter_mut().find(|q| *q.qname == *asked.qname && *q.qtype == *asked.qtype) {
                Some(q) => q,
                None => {
                    queries.push(QueryRow { qname: asked.qname.to_string(), qtype: asked.qtype.to_string(), expect: asked.expect.to_string(), sent: 0, results: Vec::new() });
                    queries.last_mut().expect("just pushed")
                }
            };
            row.sent += 1;
            match row.results.iter_mut().find(|(l, _)| *l == label) {
                Some((_, n)) => *n += 1,
                None => row.results.push((label.to_string(), 1)),
            }
            newest = Some((o, d, label));
        }
        let last = newest.map(|(o, d, label)| {
            let time = o.rtt_us.map_or_else(String::new, |r| format!(", {:.1} ms", f64::from(r) / 1000.0));
            let answers = d.answers.map_or_else(String::new, |a| format!(", {a} answer(s)"));
            let when = o.sent_utc.as_deref().and_then(|u| u.get(11..23)).map_or_else(String::new, |t| format!("{t}  "));
            format!("{when}{} {} → {label}{answers}{time}", d.query.qname, d.query.qtype)
        });
        out.push(DnsView {
            provider: m.provider.clone(),
            label: format!("{} v{}{}", info.protocol.as_str(), info.family.as_str(), if m.fresh { " fresh" } else { "" }),
            queries,
            last,
        });
    }
    out
}

pub fn human_duration(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::HistoryLimits;
    use crate::model::{Msg, Record};
    use crate::script;

    /// Plays a scenario up to `until_s` and builds the snapshot at that moment.
    fn at(name: &str, until_s: u64, view: ViewSpec) -> Snapshot {
        play(&script::by_name(name).unwrap(), until_s, view)
    }

    fn play(sc: &script::Scenario, until_s: u64, view: ViewSpec) -> Snapshot {
        let (providers, streams) = sc.metas();
        let mut engine = Engine::new(sc.infos(), HistoryLimits::default());
        let mut incidents = IncidentTracker::new(sc.streams.iter().map(|s| s.info.protocol));
        let now = until_s * 1_000_000_000;
        for (t, m) in script::generate_timed(sc, 7) {
            if t > now {
                break;
            }
            engine.ingest(&m);
            match &m {
                Msg::Record(Record::Observation(o)) => incidents.on_observation(engine.stream_index(&o.stream).unwrap(), o),
                Msg::Record(Record::Gap(_)) => incidents.on_gap(),
                _ => {}
            }
        }
        build(&engine, &incidents, &providers, &streams, &view, now)
    }

    fn view(columns: u16, window_s: u64) -> ViewSpec {
        ViewSpec { columns, window: Duration::from_secs(window_s), ..ViewSpec::default() }
    }

    #[test]
    fn a_probe_that_made_its_own_connection_lists_what_its_time_is_made_of() {
        use crate::model::Phases;
        let sc = script::by_name("healthy").unwrap();
        let mut o = script::generate(&sc, 7)
            .into_iter()
            .find_map(|m| match m {
                Msg::Record(Record::Observation(o)) if o.outcome == Outcome::Success => Some((*o).clone()),
                _ => None,
            })
            .expect("an answered probe");
        // A query on a connection that was already open has no parts to list.
        o.phase = Some(Phases { ttfb_us: Some(21_000), request_us: Some(22_300), ..Phases::default() });
        assert!(!raw_row(&Kept::of(&o)).note.contains("query"), "{}", raw_row(&Kept::of(&o)).note);
        // DNS over TCP: connect, then the query.
        o.phase = Some(Phases { connect_us: Some(20_100), request_us: Some(22_300), ..Phases::default() });
        assert!(raw_row(&Kept::of(&o)).note.ends_with("connect 20.1 + query 22.3 ms"), "{}", raw_row(&Kept::of(&o)).note);
        // A fresh encrypted connection has a handshake between them.
        o.phase = Some(Phases { connect_us: Some(20_100), tls_us: Some(24_000), ttfb_us: Some(21_000), request_us: Some(22_300) });
        assert!(raw_row(&Kept::of(&o)).note.ends_with("connect 20.1 + tls 24.0 + query 22.3 ms"), "{}", raw_row(&Kept::of(&o)).note);
        // Connected and never answered: as far as it got.
        o.outcome = Outcome::Timeout;
        o.phase = Some(Phases { connect_us: Some(20_100), ..Phases::default() });
        assert!(raw_row(&Kept::of(&o)).note.ends_with("connect 20.1 ms"), "{}", raw_row(&Kept::of(&o)).note);
    }

    /// Regressions from the first live run, where a stream whose every
    /// transaction failed still read WARMING UP and events had no time.
    #[test]
    fn known_trouble_outranks_warming_up_and_every_event_is_dated() {
        use crate::model::ErrorCategory;
        use script::{Behaviour, Rule as Scripted};
        let whole = |what| Scripted { from: Duration::ZERO, to: Duration::from_secs(600), what };

        // Every DNS transaction is an error response from the first one on.
        let mut sc = script::by_name("dns-only").unwrap();
        for s in sc.streams.iter_mut().filter(|s| s.info.protocol.is_dns()) {
            s.rules = vec![whole(Behaviour::ServiceError(ErrorCategory::DnsRcode, "SERVFAIL"))];
        }
        let s = play(&sc, 30, ViewSpec { mode: Mode::Dns, ..view(60, 60) });
        let stats = s.cards[0].stats.as_ref().unwrap();
        assert!(stats.finalised < crate::stats::MIN_N_RATE && stats.errors == stats.finalised, "young, and nothing but errors: {stats:?}");
        assert_eq!(s.cards[0].status, Status::Errors);

        // One probe leaves, then the host refuses every send: one timeout and
        // a run of local errors, as on a network with no IPv6 route.
        let mut sc = script::by_name("ipv6-missing").unwrap();
        let v6 = sc.streams.iter_mut().rev().find(|s| s.info.family == Family::V6).expect("an IPv6 stream");
        v6.rules = vec![
            Scripted { from: Duration::ZERO, to: Duration::from_millis(900), what: Behaviour::Silent },
            Scripted { from: Duration::from_millis(900), to: Duration::from_secs(600), what: Behaviour::LocalError(ErrorCategory::NoRoute) },
        ];
        let provider = v6.info.provider.clone();
        let s = play(&sc, 20, ViewSpec { family: Family::V6, ..view(60, 60) });
        let card = s.cards.iter().find(|c| c.key == provider).unwrap();
        let stats = card.stats.as_ref().unwrap();
        assert!(stats.sent >= 1 && stats.local_error > 5, "{stats:?}");
        assert_eq!(card.status, Status::LocalErr);
        assert!(!s.events.is_empty());
        for e in &s.events {
            assert!(e.utc.len() >= 19 && e.utc.as_bytes()[10] == b'T', "an event about a probe that was never sent still has a time: {e:?}");
        }
    }

    #[test]
    fn healthy_cards_are_ok_with_a_common_scale() {
        let s = at("healthy", 120, view(60, 60));
        let words: Vec<&str> = s.cards.iter().map(|c| c.status.word()).collect();
        assert_eq!(words, vec!["OK", "OK", "OK", "OK"]);
        assert_eq!(s.cards.iter().map(|c| c.slot).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert_eq!(s.y_max_us, 40_000, "one scale for all four series, rounded up");
        assert!(s.cards.iter().all(|c| c.cells.iter().all(|x| *x == Cell::Replied)));
        assert!(s.cards[0].columns.iter().all(|c| c.n == 1 && c.min_us == c.max_us));
        assert_eq!(s.cards[0].target, "1.1.1.1");
        assert!(s.cards[0].last_age_ms.unwrap() < 1_000);
    }

    #[test]
    fn a_failing_target_shows_holes_not_zeros_and_a_distinct_status() {
        // Arelion is silent from 60 s to 100 s; look at 90 s over a one-minute window.
        let s = at("isolated-failure", 90, view(60, 60));
        let ar = &s.cards[3];
        assert_eq!(ar.status, Status::NoReply);
        // Columns 0..30 cover 30–60 s (replied); the rest are unanswered.
        assert!(ar.columns[..29].iter().all(|c| c.n == 1));
        assert!(ar.columns[31..58].iter().all(|c| c.n == 0), "no latency is drawn where there was no reply");
        assert!(ar.cells[31..58].iter().all(|c| *c == Cell::NoneAnswered));
        assert_eq!(ar.cells[10], Cell::Replied);
        // The others are untouched.
        assert!(s.cards[..3].iter().all(|c| c.status == Status::Ok));
        // The event states counts, not a cause.
        assert!(s.events.iter().any(|e| e.kind == '▲' && e.stream == "arelion icmp v4" && e.text.contains("5 consecutive")));
        // After recovery the status clears and a closing event appears.
        let later = at("isolated-failure", 170, view(60, 60));
        assert_eq!(later.cards[3].status, Status::Lossy, "the run is over, but the rate rule is held open until the minute has been clean for 30 s");
        assert!(later.events.iter().any(|e| e.kind == '●' && e.text.contains("replies resumed")));
    }

    #[test]
    fn unconfigured_and_unsupported_cards_say_so_and_show_nothing() {
        let s = at("needs-config", 120, view(40, 60));
        assert!(matches!(s.cards[2].status, Status::NeedsConfig(_)));
        assert!(matches!(s.cards[3].status, Status::NeedsConfig(_)));
        assert!(s.cards[2].stats.is_none() || s.cards[2].stats.as_ref().unwrap().slots == 0);
        assert!(s.cards[2].cells.iter().all(|c| *c == Cell::Empty), "never drawn as loss");
        let s = at("missing-permission", 120, view(40, 60));
        assert!(s.cards.iter().all(|c| matches!(c.status, Status::Unsupported(_))));
        // DNS runs on the same host: switching mode shows it.
        let dns = at("missing-permission", 120, ViewSpec { mode: Mode::Dns, ..view(40, 60) });
        assert_eq!(dns.cards[0].status, Status::Ok);
        assert!(matches!(dns.cards[1].status, Status::Unsupported(_)), "no DNS target for that provider in this scenario");
    }

    #[test]
    fn the_scale_follows_the_bulk_of_the_data_not_one_slow_reply() {
        let steady = |n: usize, us: u32| vec![(us, us + 1_000); n];
        // Nothing unusual: the scale is the round number above the data.
        assert_eq!(scale_top(&steady(100, 6_000)), 8_000);
        assert_eq!(scale_top(&[]), 8_000, "an empty chart still has an axis");
        // The first live recording: three minutes at 6–8 ms and single echoes of 118 and 69 ms.
        let mut live = steady(200, 6_500);
        live[40].1 = 117_900;
        live[90].1 = 69_400;
        assert_eq!(scale_top(&live), 8_000, "two slow replies in two hundred columns do not set the scale");
        // A peak within twice the scale is simply shown.
        let mut near = steady(200, 6_500);
        near[10].1 = 15_000;
        assert_eq!(scale_top(&near), 20_000);
        // A sustained change is the data, not an outlier: the scale follows it.
        let mut shifted = steady(180, 6_500);
        shifted.extend(steady(20, 52_000));
        assert_eq!(scale_top(&shifted), 80_000, "a tenth of the window at 52 ms is on the scale");
        // Moderately higher values are shown even when a far outlier is
        // present: only the outlier is left off (seen in a live session,
        // where 9–16 ms echoes were marked off scale beside a 59 ms one).
        let mut both = steady(200, 6_500);
        (both[10].1, both[20].1, both[30].1) = (9_000, 15_000, 59_400);
        assert_eq!(scale_top(&both), 20_000);
        // With one probe per column a slow reply is that column's median too;
        // it is still one column in two hundred.
        let mut lone = steady(200, 6_500);
        lone[5] = (117_900, 117_900);
        assert_eq!(scale_top(&lone), 8_000);
    }

    #[test]
    fn an_isolated_spike_is_left_off_the_scale_and_stays_in_the_data() {
        use script::{Behaviour, Rule as Scripted};
        let mut sc = script::by_name("healthy").unwrap();
        sc.streams[0].rules.push(Scripted { from: Duration::from_secs(100), to: Duration::from_millis(100_900), what: Behaviour::Slower(150_000) });
        let s = play(&sc, 120, view(60, 60));
        assert_eq!(s.y_max_us, 40_000, "the same scale as without the spike");
        let peak = s.cards[0].columns.iter().map(|c| c.max_us).max().unwrap();
        assert!(peak > 150_000, "the column still holds the real value: {peak}");
        assert_eq!(s.cards[0].columns.iter().filter(|c| c.max_us > s.y_max_us).count(), 1, "exactly one column is off the scale");
        // A sustained rise is on the scale.
        let rise = at("correlated-spike", 100, view(60, 60));
        assert!(rise.cards.iter().all(|c| c.columns.iter().all(|col| col.max_us <= rise.y_max_us)), "scale {}", rise.y_max_us);
    }

    #[test]
    fn a_single_spike_survives_downsampling() {
        // 180 one-second samples into 30 columns: six samples per column.
        let s = at("correlated-spike", 180, view(30, 180));
        let cf = &s.cards[0];
        let spike = cf.columns.iter().map(|c| c.max_us).max().unwrap();
        assert!(spike > 45_000, "the 40 ms spike is still visible: {spike}");
        assert!(cf.columns.iter().all(|c| c.n == 6));
        assert!(cf.columns.iter().all(|c| c.min_us <= c.med_us && c.med_us <= c.max_us));
    }

    #[test]
    fn a_suspend_is_drawn_as_a_gap_on_the_real_timeline() {
        // Suspend of 12 minutes at 60.1 s; look at 120 s of monotonic time over a 15-minute window.
        let s = at("sleep-resume", 120, view(90, 900));
        let cf = &s.cards[0];
        let gaps = cf.cells.iter().filter(|c| **c == Cell::Gap).count();
        // 720 of 900 seconds are gap: 72 of 90 columns, give or take the edges.
        assert!((70..=73).contains(&gaps), "{gaps}");
        assert!(cf.cells.iter().all(|c| !matches!(c, Cell::NoneAnswered | Cell::SomeUnanswered)), "sleep is never shown as unanswered probes");
        assert_eq!(cf.status, Status::Ok);
        assert!(s.events.iter().any(|e| e.kind == '╌' && e.text.contains("12:00")));
    }

    #[test]
    fn freezing_pins_the_view_while_data_keeps_arriving() {
        let frozen = ViewSpec { frozen_at_ns: Some(50_000_000_000), ..view(60, 60) };
        // The failure starts at 60 s; a view frozen at 50 s must not show it at 90 s.
        let s = at("isolated-failure", 90, frozen);
        assert_eq!(s.as_of_ns, 50_000_000_000);
        assert_eq!(s.cards[3].status, Status::Ok);
        assert!(s.cards[3].cells.iter().all(|c| !matches!(c, Cell::NoneAnswered)));
        assert!(s.events.is_empty());
        assert!(s.new_since_freeze > 100, "collection continued: {}", s.new_since_freeze);
        assert_eq!(s.live_ns, 90_000_000_000);
    }

    #[test]
    fn snapshots_are_a_pure_function_of_data_view_and_time() {
        let a = at("flapping", 150, view(80, 300));
        let b = at("flapping", 150, view(80, 300));
        assert_eq!(a.cards.len(), b.cards.len());
        for (x, y) in a.cards.iter().zip(&b.cards) {
            assert_eq!((x.status.clone(), &x.columns, &x.cells, &x.stats), (y.status.clone(), &y.columns, &y.cells, &y.stats));
        }
        assert_eq!(a.events, b.events);
    }

    #[test]
    fn the_other_family_stays_visible_on_the_card() {
        let s = at("ipv6-missing", 120, view(40, 60));
        assert_eq!(s.cards[0].status, Status::Ok);
        assert_eq!(s.cards[0].other_family, Some(Status::LocalErr), "IPv6 is failing locally while IPv4 is shown");
        let v6 = at("ipv6-missing", 120, ViewSpec { family: Family::V6, ..view(40, 60) });
        assert_eq!(v6.cards[0].status, Status::LocalErr);
        assert!(v6.cards[0].cells.contains(&Cell::LocalError));
    }

    #[test]
    fn histogram_has_round_edges_and_counts_everything() {
        let rtts: Vec<u32> = (0..500).map(|i| 10_900 + (i * 37) % 9_000).collect();
        let bins = histogram(&rtts);
        assert!(bins.len() <= 12 && bins.len() >= 5, "{}", bins.len());
        assert_eq!(bins.iter().map(|b| b.count).sum::<u32>(), 500);
        let step = bins[0].hi_us - bins[0].lo_us;
        assert_eq!(step, 1_000, "a 9 ms spread uses 1 ms bins");
        assert!(bins.iter().all(|b| b.lo_us % step == 0 && b.hi_us - b.lo_us == step));
        assert!(bins[0].lo_us <= 10_900 && bins.last().unwrap().hi_us > 19_800);
        assert_eq!(histogram(&[]), Vec::new());
        assert_eq!(histogram(&[12_345]), vec![Bin { lo_us: 12_300, hi_us: 12_400, count: 1, open: false }]);
        assert!(bins.iter().all(|b| !b.open), "an even spread has no tail to set aside");
    }

    #[test]
    fn a_long_tail_gets_one_open_bin_instead_of_stretching_the_others() {
        // The shape of the first live recording: 175 echoes at 5.9–7.8 ms,
        // a few at 10–30 ms, and single ones at 69 and 118 ms.
        let mut rtts: Vec<u32> = (0..168).map(|i| 5_900 + (i * 11) % 1_900).collect();
        rtts.extend([12_000, 14_500, 17_000, 22_000, 27_000, 69_400, 117_900]);
        let bins = histogram(&rtts);
        assert!(bins.len() <= 12, "{}", bins.len());
        assert_eq!(bins.iter().map(|b| b.count).sum::<u32>(), rtts.len() as u32, "every sample is in exactly one bin");
        let (open, regular) = bins.split_last().unwrap();
        assert!(open.open && regular.iter().all(|b| !b.open), "one open bin, and it is the last");
        assert_eq!((open.count, open.hi_us), (7, 117_900), "the tail is counted and its true maximum kept: {open:?}");
        assert_eq!(open.lo_us, regular.last().unwrap().hi_us, "it starts where the bins end");
        let step = regular[0].hi_us - regular[0].lo_us;
        assert_eq!(step, 200, "the bulk is resolved in 0.2 ms bins, not 10 ms ones");
        assert!(regular.iter().filter(|b| b.count > 0).count() >= 8, "and is spread across them: {regular:?}");

        // A tail that reaches no more than twice as far as the bulk is simply binned.
        // (Here the bulk spans 0.66 ms and the tail reaches 1.2 ms from the minimum.)
        let mut near: Vec<u32> = (0..100).map(|i| 10_000 + i * 7).collect();
        near.push(11_200);
        assert!(histogram(&near).iter().all(|b| !b.open));
        // Identical values and one outlier: one bin and the open bin.
        let mut flat = vec![5_000; 50];
        flat.push(90_000);
        let bins = histogram(&flat);
        assert_eq!(bins.iter().map(|b| (b.count, b.open)).collect::<Vec<_>>(), vec![(50, false), (1, true)]);
    }

    #[test]
    fn details_data_is_present_for_the_focused_stream() {
        let s = at("isolated-failure", 90, view(60, 60));
        let ar = &s.cards[3];
        // 30 answered probes in the window, all in the histogram; none invented for the outage.
        assert_eq!(ar.histogram.iter().map(|b| b.count).sum::<u32>(), 30);
        assert_eq!(ar.raw[0].outcome, Outcome::Timeout, "newest first; the latest finalised probe timed out");
        assert_eq!(ar.raw[0].rtt_us, None);
        assert!(ar.raw.iter().any(|r| r.outcome == Outcome::Success && r.rtt_us.is_some()));
        assert!(ar.session.as_ref().unwrap().replied >= 60);
    }

    #[test]
    fn dns_results_are_grouped_by_query_and_keep_error_responses_apart() {
        let s = at("dns-only", 120, ViewSpec { mode: Mode::Dns, ..view(60, 300) });
        let v = s.dns.iter().find(|d| &*d.provider == "cloudflare").expect("a DNS view");
        assert_eq!(v.label, "dns-udp v4");
        assert_eq!(v.queries.len(), 3, "the three default queries: {:?}", v.queries);
        let total: u32 = v.queries.iter().map(|q| q.sent).sum();
        assert_eq!(total, 24, "one transaction every 5 s for 120 s");
        let labels: std::collections::BTreeSet<&str> = v.queries.iter().flat_map(|q| q.results.iter().map(|(l, _)| l.as_str())).collect();
        assert_eq!(labels.into_iter().collect::<Vec<_>>(), vec!["NOERROR", "SERVFAIL", "timeout"]);
        assert!(v.last.as_ref().unwrap().contains("NOERROR"));
    }
}

//! Identity and record types.
//!
//! The serialised field names in this module are the JSONL schema, version
//! [`SCHEMA_VERSION`]. See `docs/design/architecture.md` §6.

use std::net::IpAddr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Protocol {
    #[serde(rename = "icmp")]
    Icmp,
    #[serde(rename = "dns-udp")]
    DnsUdp,
    #[serde(rename = "dns-tcp")]
    DnsTcp,
    #[serde(rename = "dot")]
    Dot,
    #[serde(rename = "doh")]
    Doh,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Icmp => "icmp",
            Protocol::DnsUdp => "dns-udp",
            Protocol::DnsTcp => "dns-tcp",
            Protocol::Dot => "dot",
            Protocol::Doh => "doh",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "icmp" => Protocol::Icmp,
            "dns-udp" => Protocol::DnsUdp,
            "dns-tcp" => Protocol::DnsTcp,
            "dot" => Protocol::Dot,
            "doh" => Protocol::Doh,
            _ => return None,
        })
    }

    pub fn is_dns(self) -> bool {
        !matches!(self, Protocol::Icmp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Family {
    #[serde(rename = "4")]
    V4,
    #[serde(rename = "6")]
    V6,
}

impl Family {
    pub fn of(ip: &IpAddr) -> Self {
        if ip.is_ipv4() { Family::V4 } else { Family::V6 }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Family::V4 => "4",
            Family::V6 => "6",
        }
    }
}

/// How one scheduled slot ended. Only the first five are network outcomes;
/// see `docs/design/design.md` §4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Timeout,
    Unreachable,
    ServiceError,
    ProtocolError,
    LocalError,
    Skipped,
    Cancelled,
}

impl Outcome {
    /// The probe left this host (the send system call succeeded).
    pub fn was_sent(self) -> bool {
        !matches!(self, Outcome::LocalError | Outcome::Skipped)
    }

    /// Counts in network denominators.
    pub fn is_finalised_network(self) -> bool {
        matches!(self, Outcome::Success | Outcome::Timeout | Outcome::Unreachable | Outcome::ServiceError | Outcome::ProtocolError)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    TimingSuspect,
    AfterReconnect,
    Truncated,
    Fresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    // unreachable
    IcmpUnreachable,
    IcmpTimeExceeded,
    ConnectionRefused,
    // service errors
    DnsRcode,
    DnsUnexpected,
    DnsBlocked,
    HttpStatus,
    TlsAlert,
    // protocol errors
    Malformed,
    Mismatch,
    Certificate,
    Protocol,
    // local errors
    NoRoute,
    NoAddress,
    Permission,
    Bootstrap,
    Connect,
    Io,
    // skipped
    InFlight,
    RateLimit,
    Backpressure,
    LateTimer,
    // cancelled
    Shutdown,
    Gap,
    Reconfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub category: ErrorCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ErrorInfo {
    pub fn new(category: ErrorCategory) -> Self {
        Self { category, code: None, detail: None }
    }

    pub fn code(mut self, code: i64) -> Self {
        self.code = Some(code);
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(crate::sanitize::clean(&detail.into(), 200));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub ctx: u32,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "if")]
    pub ifname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Phases {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_us: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_us: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_us: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_us: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcmpInfo {
    pub id: u16,
    pub seq16: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_ttl: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub icmp_type: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<u8>,
    pub payload: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsInfo {
    pub qname: String,
    pub qtype: String,
    pub id: u16,
    pub expect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tc: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ttl: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ede: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnInfo {
    pub id: u64,
    pub reused: bool,
    pub age_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

/// Round trips end at this process's read of the reply.
pub const TS_USERSPACE: &str = "userspace-monotonic";
/// Round trips end at the kernel's receive stamp.
pub const TS_KERNEL_RX: &str = "kernel-rx";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backend {
    pub name: Arc<str>,
    pub version: Arc<str>,
    pub ts_source: Arc<str>,
}

impl Backend {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), version: env!("CARGO_PKG_VERSION").into(), ts_source: TS_USERSPACE.into() }
    }
}

/// One finalised slot. `rtt_us` is absent when there is no round-trip time;
/// it is never zero-filled and never set to the deadline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub session: Arc<str>,
    pub config_rev: u32,
    pub provider: Arc<str>,
    pub target: Arc<str>,
    pub stream: Arc<str>,
    pub seq: u64,
    pub transaction: u64,
    pub attempt: u8,
    pub protocol: Protocol,
    pub family: Family,
    /// The target as configured, with secrets already redacted.
    pub configured: Arc<str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_ip: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<u16>,
    pub epoch: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<Source>,
    /// Monotonic nanoseconds since session start; comparable only within a session.
    pub sched_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recv_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_utc: Option<String>,
    pub deadline_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sched_lag_us: Option<u64>,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    /// The round-trip time. Which clock ended it is named by
    /// `backend.ts_source`: with `kernel-rx` it ends when the kernel received
    /// the reply; with `userspace-monotonic` it ends when this process read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u32>,
    /// With `kernel-rx` only: the round trip as it would read if ended at this
    /// process's read, i.e. `rtt_us` plus `recv_delay_us`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_user_us: Option<u32>,
    /// How long the reply waited between the kernel receiving it and this
    /// process reading it: the instrument's own receive delay. Excluded from
    /// `rtt_us` when `ts_source` is `kernel-rx`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recv_delay_us: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phases>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icmp: Option<IcmpInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<DnsInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conn: Option<ConnInfo>,
    pub backend: Backend,
}

impl Observation {
    /// When the slot happened on the session's monotonic clock: the send time
    /// if it was sent, otherwise its scheduled time.
    pub fn at_ns(&self) -> u64 {
        self.sent_ns.unwrap_or(self.sched_ns)
    }

    pub fn has_flag(&self, f: Flag) -> bool {
        self.flags.contains(&f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GapCause {
    Suspend,
    Stopped,
    ProcessStopped,
    NetworkChange,
    CollectorStall,
}

/// An interval in which the instrument was not observing. Not an outage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapRec {
    pub session: Arc<str>,
    pub cause: GapCause,
    /// Monotonic bounds. For a host suspend these are nearly equal, because the
    /// monotonic clock does not advance while the host sleeps.
    pub start_ns: u64,
    pub end_ns: u64,
    /// Elapsed real time, including any suspended time.
    pub duration_ms: u64,
    pub end_utc: String,
}

impl GapRec {
    /// Real time that passed without the monotonic clock advancing.
    pub fn hidden_ns(&self) -> u64 {
        (self.duration_ms * 1_000_000).saturating_sub(self.end_ns.saturating_sub(self.start_ns))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRec {
    pub session: Arc<str>,
    pub at_ns: u64,
    pub utc: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<Arc<str>>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRec {
    pub session: Arc<str>,
    pub at_ns: u64,
    pub provider: Arc<str>,
    pub target: Arc<str>,
    /// Hostname as configured, redacted.
    pub name: Arc<str>,
    pub resolver: String,
    pub duration_us: u64,
    pub addresses: Vec<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRec {
    pub session: Arc<str>,
    pub at_ns: u64,
    pub stream: Arc<str>,
    pub conn: u64,
    /// `opened`, `failed`, or `closed`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Phases>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A reply that arrived after its probe was finalised as a timeout, or a
/// second reply to a probe that already succeeded. Neither changes the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationRec {
    pub session: Arc<str>,
    pub stream: Arc<str>,
    pub seq: u64,
    pub at_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_rtt_us: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRec {
    pub session: Arc<str>,
    /// `start` or `end`.
    pub state: String,
    pub utc: String,
    pub app: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<StreamRec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counters: Option<CollectorCounters>,
    /// On `end`: what the collector sent and received, by kind and family.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traffic: Vec<TrafficRow>,
}

/// Traffic totals for one probe kind and address family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficRow {
    pub kind: String,
    pub family: Family,
    /// Probe transactions that started on the wire.
    pub transactions: u64,
    /// Datagrams; zero for TCP-based kinds, where only bytes are known.
    pub packets_out: u64,
    pub bytes_out: u64,
    pub packets_in: u64,
    pub bytes_in: u64,
    /// `ip-layer` (payload plus transport and IP headers) or `tcp-payload`.
    pub bytes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRec {
    pub stream: Arc<str>,
    pub provider: Arc<str>,
    pub target: Arc<str>,
    pub protocol: Protocol,
    pub family: Family,
    pub configured: Arc<str>,
    pub interval_ms: u64,
    pub deadline_ms: u64,
    /// `collecting`, `needs_config`, `unsupported`, or `disabled`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Instrument-health counters. None of these enters a network rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CollectorCounters {
    pub unmatched_packets: u64,
    pub backpressure_skips: u64,
    pub rate_limit_skips: u64,
    pub late_timer_skips: u64,
    /// Records that could not be delivered to the processor at all.
    pub dropped_records: u64,
    pub max_loop_lag_us: u64,
    /// How many times the receive loop's lateness was checked, and how many
    /// of those found it more than 2 ms late. Absent from older recordings.
    #[serde(default)]
    pub loop_lag_checks: u64,
    #[serde(default)]
    pub loop_lag_over_2ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// The first line of every recording segment.
    Segment(SegmentRec),
    Session(SessionRec),
    /// A configuration reload that was applied.
    Config(ConfigRec),
    Observation(Box<Observation>),
    LateReply(AnnotationRec),
    Duplicate(AnnotationRec),
    Gap(GapRec),
    Event(EventRec),
    Resolution(ResolutionRec),
    Connection(ConnectionRec),
}

/// A provider as the dashboard shows it, so a recording can be replayed
/// without the configuration that made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRec {
    pub key: Arc<str>,
    pub name: String,
    pub slot: u8,
    /// `collecting`, `needs_config`, or `disabled`, as configured. Empty in
    /// a record that does not say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ProviderRec {
    pub fn of(p: &crate::config::ProviderPlan) -> Self {
        Self { key: p.key.clone(), name: p.name.clone(), slot: p.slot, state: p.state.label().to_string(), reason: p.state.reason().map(str::to_string) }
    }
}

/// A configuration revision taking effect during a session: everything the
/// session runs from here on. The session's opening record plays this part
/// for revision 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigRec {
    pub session: Arc<str>,
    pub rev: u32,
    pub at_ns: u64,
    pub utc: String,
    /// Every stream of the new revision, running or not.
    pub streams: Vec<StreamRec>,
    pub providers: Vec<ProviderRec>,
    /// What changed, in counts: started, stopped, restarted, unchanged.
    pub summary: String,
}

/// What a recording segment says about itself: every segment can be read on
/// its own, whatever has been rotated away before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentRec {
    pub session: Arc<str>,
    /// 1 for the first segment of a session.
    pub segment: u32,
    pub utc: String,
    pub app: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    /// Whether profile ids were removed as the records were written.
    pub redacted: bool,
    #[serde(default)]
    pub providers: Vec<ProviderRec>,
    /// The effective configuration as TOML, secrets masked.
    #[serde(default)]
    pub config: String,
}

/// One JSONL line: the schema version followed by the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub schema: u32,
    #[serde(flatten)]
    pub record: Record,
}

impl Line {
    pub fn new(record: Record) -> Self {
        Self { schema: SCHEMA_VERSION, record }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("records are always serialisable")
    }
}

/// Messages from a collector (live or scripted) to the processor.
// Records dominate the traffic and observations inside them are already boxed,
// so boxing the whole variant would only add an allocation per message.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Msg {
    /// A probe left this host and is awaiting a reply or its deadline.
    Sent { stream: u32, seq: u64, sent_ns: u64 },
    /// Anything that is also written to a recording.
    Record(Record),
}

impl Msg {
    pub fn obs(o: Observation) -> Self {
        Msg::Record(Record::Observation(Box::new(o)))
    }
}

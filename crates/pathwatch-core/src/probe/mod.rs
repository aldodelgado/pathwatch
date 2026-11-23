//! Probe adapters and the plumbing they share.

pub mod dns;
pub mod doh;
pub mod icmp;
pub mod icmp_core;
pub mod icmp_packet;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::{self, OwnedPermit, error::TrySendError};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::clock::{Clock, utc_string};
use crate::config::StreamPlan;
use crate::model::{Backend, CollectorCounters, ErrorCategory, ErrorInfo, EventRec, Msg, Observation, Outcome, Record, Source};
use crate::schedule::TokenBucket;

/// Static facts about one stream, shared by everything that builds its records.
#[derive(Debug)]
pub struct StreamCtx {
    pub idx: u32,
    pub session: Arc<str>,
    /// The configuration revision in force, shared by every stream: a
    /// record is tagged with the revision at the moment it is made.
    pub rev: Arc<std::sync::atomic::AtomicU32>,
    pub plan: StreamPlan,
    pub backend: Backend,
}

impl StreamCtx {
    /// A record for slot `seq` with nothing sent yet. Callers fill in what happened.
    pub fn observation(&self, seq: u64, sched_ns: u64) -> Observation {
        Observation {
            session: self.session.clone(),
            config_rev: self.rev.load(std::sync::atomic::Ordering::Relaxed),
            provider: self.plan.provider.clone(),
            target: self.plan.target.clone(),
            stream: self.plan.id.clone(),
            seq,
            transaction: seq,
            attempt: 1,
            protocol: self.plan.protocol,
            family: self.plan.family,
            configured: self.plan.configured.clone(),
            dest_ip: None,
            dest_port: None,
            epoch: 0,
            src: None,
            sched_ns,
            sent_ns: None,
            recv_ns: None,
            sent_utc: None,
            deadline_ms: self.plan.deadline.as_millis() as u32,
            sched_lag_us: None,
            outcome: Outcome::Skipped,
            error: None,
            rtt_us: None,
            rtt_user_us: None,
            recv_delay_us: None,
            flags: Vec::new(),
            phase: None,
            icmp: None,
            dns: None,
            conn: None,
            backend: self.backend.clone(),
        }
    }

    /// A slot that was never attempted.
    pub fn skipped(&self, seq: u64, sched_ns: u64, why: ErrorCategory) -> Observation {
        let mut o = self.observation(seq, sched_ns);
        o.error = Some(ErrorInfo::new(why));
        o
    }
}

/// Instrument-health counters, shared across collector tasks.
#[derive(Debug, Default)]
pub struct Counters {
    pub unmatched_packets: AtomicU64,
    pub backpressure_skips: AtomicU64,
    pub rate_limit_skips: AtomicU64,
    pub late_timer_skips: AtomicU64,
    pub dropped_records: AtomicU64,
    pub max_loop_lag_us: AtomicU64,
    pub last_loop_lag_us: AtomicU64,
    pub loop_lag_checks: AtomicU64,
    pub loop_lag_over_2ms: AtomicU64,
    pub traffic: crate::traffic::Traffic,
}

impl Counters {
    pub fn snapshot(&self) -> CollectorCounters {
        CollectorCounters {
            unmatched_packets: self.unmatched_packets.load(Ordering::Relaxed),
            backpressure_skips: self.backpressure_skips.load(Ordering::Relaxed),
            rate_limit_skips: self.rate_limit_skips.load(Ordering::Relaxed),
            late_timer_skips: self.late_timer_skips.load(Ordering::Relaxed),
            dropped_records: self.dropped_records.load(Ordering::Relaxed),
            max_loop_lag_us: self.max_loop_lag_us.load(Ordering::Relaxed),
            loop_lag_checks: self.loop_lag_checks.load(Ordering::Relaxed),
            loop_lag_over_2ms: self.loop_lag_over_2ms.load(Ordering::Relaxed),
        }
    }

    /// True when the receive loop was recently scheduled late enough that
    /// user-space timestamps taken now deserve a warning flag.
    pub fn timing_suspect(&self) -> bool {
        self.last_loop_lag_us.load(Ordering::Relaxed) > LOOP_LAG_SUSPECT_US
    }
}

pub const LOOP_LAG_SUSPECT_US: u64 = 5_000;

/// Capacity reserved for one probe's two messages before the probe is sent, so
/// a probe that reaches the wire can always report its result.
pub struct Reservation {
    sent: OwnedPermit<Msg>,
    fin: OwnedPermit<Msg>,
}

impl Reservation {
    /// Announces the send and returns the permit for the final record.
    pub fn sent(self, stream: u32, seq: u64, sent_ns: u64) -> OwnedPermit<Msg> {
        self.sent.send(Msg::Sent { stream, seq, sent_ns });
        self.fin
    }

    /// For a slot that ended before anything was sent.
    pub fn finish_unsent(self, o: Observation) {
        self.fin.send(Msg::obs(o));
    }
}

/// The collector's side of the bounded channel to the processor.
pub struct Outbox {
    tx: mpsc::Sender<Msg>,
    overflow: Mutex<VecDeque<Msg>>,
    counters: Arc<Counters>,
}

const OVERFLOW_CAP: usize = 4096;

impl Outbox {
    pub fn new(tx: mpsc::Sender<Msg>, counters: Arc<Counters>) -> Self {
        Self { tx, overflow: Mutex::new(VecDeque::new()), counters }
    }

    /// Reserves room for a probe's messages. `None` means the processor is not
    /// keeping up: the slot must be skipped, not sent.
    pub fn reserve(&self) -> Option<Reservation> {
        self.flush();
        let sent = self.tx.clone().try_reserve_owned().ok()?;
        let fin = self.tx.clone().try_reserve_owned().ok()?;
        Some(Reservation { sent, fin })
    }

    /// Sends a record that is not tied to a reservation (events, gaps, skipped
    /// slots, annotations). If the channel is full it waits in a bounded local
    /// queue; if that is full too it is dropped and counted.
    pub fn emit(&self, msg: Msg) {
        self.flush();
        if let Err(TrySendError::Full(msg)) = self.tx.try_send(msg) {
            let mut q = self.overflow.lock().unwrap();
            if q.len() < OVERFLOW_CAP {
                q.push_back(msg);
            } else {
                self.counters.dropped_records.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn record(&self, r: Record) {
        self.emit(Msg::Record(r));
    }

    fn flush(&self) {
        let mut q = self.overflow.lock().unwrap();
        while let Some(msg) = q.pop_front() {
            if let Err(TrySendError::Full(msg) | TrySendError::Closed(msg)) = self.tx.try_send(msg) {
                q.push_front(msg);
                break;
            }
        }
    }

    /// Waits until everything queued locally has been handed over (shutdown).
    pub async fn drain(&self) {
        loop {
            self.flush();
            if self.overflow.lock().unwrap().is_empty() || self.tx.is_closed() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

/// Marks the most recent observation gap, so tasks can tell whether one
/// overlapped work they had in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GapMark {
    pub generation: u64,
    pub end_ns: u64,
}

/// Everything collector tasks share.
#[derive(Clone)]
pub struct Shared {
    pub clock: Arc<dyn Clock>,
    pub outbox: Arc<Outbox>,
    pub counters: Arc<Counters>,
    pub bucket: Arc<Mutex<TokenBucket>>,
    pub gap: watch::Receiver<GapMark>,
    /// True while the user has stopped collection.
    pub pause: watch::Receiver<bool>,
    /// What this host's network looks like, and the id of the current
    /// source context: it goes up by one each time the network changes.
    pub net: Arc<dyn crate::netwatch::NetProbe>,
    pub context: watch::Receiver<u32>,
    pub cancel: CancellationToken,
    pub session: Arc<str>,
}

impl Shared {
    pub fn event(&self, name: &str, stream: Option<Arc<str>>, detail: impl Into<String>) {
        self.outbox.record(Record::Event(EventRec {
            session: self.session.clone(),
            at_ns: self.clock.mono_ns(),
            utc: utc_string(self.clock.wall()),
            event: name.to_string(),
            stream,
            detail: crate::sanitize::clean(&detail.into(), 300),
        }));
    }

    pub async fn sleep_until_ns(&self, mono_ns: u64) {
        tokio::time::sleep_until(self.clock.instant_at(mono_ns)).await;
    }
}

/// Classifies a failed send or connect that never reached the network.
pub fn local_error(e: &io::Error) -> ErrorInfo {
    let category = match e.raw_os_error() {
        Some(libc::ENETUNREACH | libc::EHOSTUNREACH | libc::ENETDOWN | libc::EHOSTDOWN) => ErrorCategory::NoRoute,
        Some(libc::EADDRNOTAVAIL | libc::EAFNOSUPPORT) => ErrorCategory::NoAddress,
        Some(libc::EPERM | libc::EACCES) => ErrorCategory::Permission,
        _ => ErrorCategory::Io,
    };
    let mut info = ErrorInfo::new(category).detail(e.to_string());
    if let Some(code) = e.raw_os_error() {
        info = info.code(code.into());
    }
    info
}

/// The source context for a destination: which local address the OS will use.
pub fn source_for(dest: std::net::IpAddr) -> Option<Source> {
    crate::platform::icmp_sock::local_source_for(dest).map(|ip| Source { ctx: 1, ifname: None, ip: Some(ip) })
}

pub fn us(ns: u64) -> u32 {
    (ns / 1_000).min(u64::from(u32::MAX)) as u32
}

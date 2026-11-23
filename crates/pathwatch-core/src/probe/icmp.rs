//! The ICMP echo engine: one actor per address family owning the socket, the
//! pending table, and the deadline queue.
//!
//! Stream tasks never wait on this actor — they hand over a request and move
//! on — so a silent target cannot delay another stream's sends.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::{self, OwnedPermit};

use super::icmp_core::{CoreEvent, IcmpCore, Incoming, Key};
use super::icmp_packet::{self, ErrorKind, Parsed, Token};
use super::{Reservation, Shared, StreamCtx, local_error, us};
use crate::clock::{Clock, utc_string};
use crate::model::{AnnotationRec, ErrorCategory, ErrorInfo, Family, Flag, IcmpInfo, Msg, Observation, Outcome, Record, Source, TS_KERNEL_RX};
use crate::platform::icmp_sock::{self, IcmpSocket};
use crate::traffic::{Kind, ip_header};

/// The socket as the actor sees it. The real implementation is
/// [`RealIcmpIo`]; tests substitute a scripted one.
pub trait IcmpIo: Send + 'static {
    /// The identifier written into echo requests.
    fn id(&self) -> u16;
    /// Whether replies must carry that identifier (false where the kernel rewrites it).
    fn check_id(&self) -> bool;
    fn send(&mut self, dest: IpAddr, packet: &[u8]) -> io::Result<()>;
    /// Sets the TTL of packets sent from now on. Only a route trace asks,
    /// and only of a transport of its own.
    fn set_ttl(&mut self, _ttl: u8) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "this transport cannot set a TTL"))
    }
    /// Waits for traffic and returns everything currently readable, each item
    /// stamped immediately after the system call that produced it. Must be
    /// cancel-safe.
    fn recv(&mut self) -> impl Future<Output = io::Result<Vec<Incoming>>> + Send;
}

pub struct IcmpRequest {
    pub ctx: Arc<StreamCtx>,
    pub seq: u64,
    pub sched_ns: u64,
    pub dest: IpAddr,
    pub epoch: u32,
    pub src: Option<Source>,
    pub reservation: Reservation,
}

pub enum IcmpCmd {
    Send(Box<IcmpRequest>),
    /// Finalise everything pending as cancelled (an observation gap began).
    CancelPending(ErrorCategory),
}

struct Meta {
    ctx: Arc<StreamCtx>,
    sched_ns: u64,
    dest: IpAddr,
    epoch: u32,
    src: Option<Source>,
    sent_utc: String,
    fin: OwnedPermit<Msg>,
}

pub async fn run_actor<I: IcmpIo>(mut io: I, family: Family, payload_len: usize, late_window_ns: u64, shared: Shared, mut rx: mpsc::Receiver<IcmpCmd>) {
    let mut nonce_bytes = [0u8; 4];
    getrandom::fill(&mut nonce_bytes).expect("the OS random source is available");
    let nonce = u32::from_ne_bytes(nonce_bytes);
    let mut core = IcmpCore::new(nonce, io.check_id().then(|| io.id()), late_window_ns);
    let mut meta: HashMap<Key, Meta> = HashMap::new();
    // Stream index → id, kept after a probe's own bookkeeping is released so
    // late and duplicate replies can still be attributed.
    let mut names: HashMap<u32, Arc<str>> = HashMap::new();
    let id = io.id();

    loop {
        let deadline = core.next_deadline();
        tokio::select! {
            biased;
            _ = shared.cancel.cancelled() => break,
            items = io.recv() => match items {
                Ok(items) => {
                    for inc in items {
                        for ev in core.on_incoming(inc) {
                            handle(ev, &mut meta, &names, &shared, id, payload_len);
                        }
                    }
                }
                Err(e) => {
                    shared.event("icmp_socket_error", None, format!("IPv{} socket: {e}", family.as_str()));
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            },
            cmd = rx.recv() => match cmd {
                None => break,
                Some(IcmpCmd::Send(req)) => {
                    names.entry(req.ctx.idx).or_insert_with(|| req.ctx.plan.id.clone());
                    send(&mut io, &mut core, &mut meta, &shared, family, payload_len, *req);
                }
                Some(IcmpCmd::CancelPending(why)) => cancel_all(&mut core, &mut meta, id, payload_len, why),
            },
            _ = shared.sleep_until_ns(deadline.unwrap_or(0)), if deadline.is_some() => {
                for ev in core.expire(shared.clock.mono_ns()) {
                    handle(ev, &mut meta, &names, &shared, id, payload_len);
                }
            }
        }
    }
    cancel_all(&mut core, &mut meta, id, payload_len, ErrorCategory::Shutdown);
    shared.counters.unmatched_packets.fetch_add(core.unmatched, Ordering::Relaxed);
}

fn send<I: IcmpIo>(io: &mut I, core: &mut IcmpCore, meta: &mut HashMap<Key, Meta>, shared: &Shared, family: Family, payload_len: usize, req: IcmpRequest) {
    let key = Key { stream: req.ctx.idx, seq: req.seq };
    let packet = icmp_packet::build_echo(family, io.id(), Token { nonce: core.nonce(), stream: key.stream, seq: key.seq }, payload_len);
    let sent_utc = utc_string(shared.clock.wall());
    // The send timestamp is taken immediately before the system call.
    let sent_ns = shared.clock.mono_ns();
    match io.send(req.dest, &packet) {
        Ok(()) => {
            let t = &shared.counters.traffic;
            t.transaction(Kind::Icmp, family);
            t.sent(Kind::Icmp, family, 1, packet.len() as u64 + ip_header(family));
            core.register(key, req.dest, sent_ns, sent_ns + req.ctx.plan.deadline.as_nanos() as u64);
            let fin = req.reservation.sent(key.stream, key.seq, sent_ns);
            meta.insert(key, Meta { ctx: req.ctx, sched_ns: req.sched_ns, dest: req.dest, epoch: req.epoch, src: req.src, sent_utc, fin });
        }
        Err(e) => {
            // Nothing left this host: a local error, never network loss.
            let mut o = req.ctx.observation(req.seq, req.sched_ns);
            o.dest_ip = Some(req.dest);
            o.epoch = req.epoch;
            o.src = req.src;
            o.outcome = Outcome::LocalError;
            o.error = Some(local_error(&e));
            req.reservation.finish_unsent(o);
        }
    }
}

fn base(m: &Meta, key: Key, sent_ns: u64, id: u16, payload_len: usize) -> Observation {
    let mut o = m.ctx.observation(key.seq, m.sched_ns);
    o.dest_ip = Some(m.dest);
    o.epoch = m.epoch;
    o.src = m.src.clone();
    o.sent_ns = Some(sent_ns);
    o.sent_utc = Some(m.sent_utc.clone());
    o.sched_lag_us = Some(u64::from(us(sent_ns.saturating_sub(m.sched_ns))));
    o.icmp = Some(IcmpInfo { id, seq16: key.seq as u16, reply_ttl: None, icmp_type: None, code: None, payload: payload_len as u16 });
    o
}

fn handle(ev: CoreEvent, meta: &mut HashMap<Key, Meta>, names: &HashMap<u32, Arc<str>>, shared: &Shared, id: u16, payload_len: usize) {
    match ev {
        CoreEvent::Reply { key, sent_ns, recv_ns, ttl, recv_delay_ns } => {
            let Some(m) = meta.remove(&key) else { return };
            let mut o = base(&m, key, sent_ns, id, payload_len);
            // An echo reply carries our payload back, so it is the size we sent.
            let family = m.ctx.plan.family;
            shared.counters.traffic.received(Kind::Icmp, family, 1, crate::traffic::ICMP_HEADER + payload_len as u64 + ip_header(family));
            o.outcome = Outcome::Success;
            match recv_delay_ns {
                // `recv_ns` is the kernel's arrival time, so the round trip
                // excludes the wait for this process to be scheduled. The
                // user-space figure and the wait itself are kept alongside.
                Some(delay) if recv_ns > sent_ns => {
                    o.recv_ns = Some(recv_ns);
                    o.rtt_us = Some(us(recv_ns - sent_ns));
                    o.rtt_user_us = Some(us(recv_ns + delay - sent_ns));
                    o.recv_delay_us = Some(us(delay));
                    o.backend.ts_source = TS_KERNEL_RX.into();
                }
                // No kernel stamp, or one that would precede the send: the
                // round trip ends at our read.
                _ => {
                    let read_ns = recv_ns + recv_delay_ns.unwrap_or(0);
                    o.recv_ns = Some(read_ns);
                    o.rtt_us = Some(us(read_ns.saturating_sub(sent_ns)));
                }
            }
            if let Some(i) = o.icmp.as_mut() {
                i.reply_ttl = ttl;
            }
            if shared.counters.timing_suspect() {
                o.flags.push(Flag::TimingSuspect);
            }
            m.fin.send(Msg::obs(o));
        }
        CoreEvent::Unreachable { key, sent_ns, recv_ns, kind, icmp_type, code, from } => {
            let Some(m) = meta.remove(&key) else { return };
            let mut o = base(&m, key, sent_ns, id, payload_len);
            o.outcome = Outcome::Unreachable;
            o.recv_ns = Some(recv_ns);
            let category = if kind == ErrorKind::TimeExceeded { ErrorCategory::IcmpTimeExceeded } else { ErrorCategory::IcmpUnreachable };
            o.error = Some(ErrorInfo::new(category).code(i64::from(code)).detail(format!("ICMP type {icmp_type} code {code} from {from}")));
            if let Some(i) = o.icmp.as_mut() {
                i.icmp_type = Some(icmp_type);
                i.code = Some(code);
            }
            m.fin.send(Msg::obs(o));
        }
        CoreEvent::Timeout { key, sent_ns } => {
            let Some(m) = meta.remove(&key) else { return };
            let mut o = base(&m, key, sent_ns, id, payload_len);
            // No round-trip time exists for a timeout; none is recorded.
            o.outcome = Outcome::Timeout;
            m.fin.send(Msg::obs(o));
        }
        CoreEvent::Late { key, late_rtt_ns, recv_ns } => {
            if let Some(stream) = names.get(&key.stream).cloned() {
                shared.outbox.record(Record::LateReply(AnnotationRec {
                    session: shared.session.clone(),
                    stream,
                    seq: key.seq,
                    at_ns: recv_ns,
                    late_rtt_us: Some(us(late_rtt_ns)),
                }));
            }
        }
        CoreEvent::Duplicate { key, recv_ns } => {
            if let Some(stream) = names.get(&key.stream).cloned() {
                shared.outbox.record(Record::Duplicate(AnnotationRec { session: shared.session.clone(), stream, seq: key.seq, at_ns: recv_ns, late_rtt_us: None }));
            }
        }
        CoreEvent::Unmatched => {}
    }
}

fn cancel_all(core: &mut IcmpCore, meta: &mut HashMap<Key, Meta>, id: u16, payload_len: usize, why: ErrorCategory) {
    for (key, sent_ns) in core.drain_pending() {
        if let Some(m) = meta.remove(&key) {
            let mut o = base(&m, key, sent_ns, id, payload_len);
            o.outcome = Outcome::Cancelled;
            o.error = Some(ErrorInfo::new(why));
            m.fin.send(Msg::obs(o));
        }
    }
}

/// The real socket.
pub struct RealIcmpIo {
    afd: AsyncFd<IcmpSocket>,
    family: Family,
    id: u16,
    clock: Arc<dyn Clock>,
    buf: Box<[u8; 2048]>,
}

impl RealIcmpIo {
    /// Must be called inside a Tokio runtime.
    pub fn open(family: Family, clock: Arc<dyn Clock>) -> io::Result<Self> {
        let sock = IcmpSocket::open(family)?;
        let id = sock.kernel_id().unwrap_or_else(|| {
            let mut b = [0u8; 2];
            getrandom::fill(&mut b).expect("the OS random source is available");
            u16::from_ne_bytes(b) | 1
        });
        let interest = if icmp_sock::HAS_ERROR_QUEUE { Interest::READABLE | Interest::ERROR } else { Interest::READABLE };
        // SAFETY: `IcmpSocket` owns its descriptor for its whole lifetime and
        // `as_raw_fd` always returns that same descriptor; it is closed only
        // when the `AsyncFd` (and with it the socket) is dropped.
        let afd = unsafe { AsyncFd::register_with_interest(sock, interest) }.map_err(io::Error::from)?;
        Ok(Self { afd, family, id, clock, buf: Box::new([0; 2048]) })
    }

    fn drain(&mut self, items: &mut Vec<Incoming>) {
        let sock = self.afd.get_ref();
        loop {
            match sock.recv(&mut self.buf[..]) {
                Ok(d) => {
                    // Receive timestamp. Our clock is read immediately after the
                    // system call returns; where the kernel stamped the arrival,
                    // the measured wait is taken off to give the arrival time.
                    let recv_ns = self.clock.mono_ns().saturating_sub(d.recv_delay_ns.unwrap_or(0));
                    let Some(src) = d.src else { continue };
                    let data = &self.buf[..d.len];
                    let parsed = match self.family {
                        Family::V4 => icmp_packet::parse_v4(data, icmp_sock::V4_INCLUDES_IP_HEADER).map(|(p, ttl)| (p, ttl.or(d.ttl))),
                        Family::V6 => icmp_packet::parse_v6(data).map(|p| (p, d.ttl)),
                    };
                    if let Some((parsed, ttl)) = parsed {
                        items.push(Incoming { recv_ns, src, parsed, ttl, offender: None, recv_delay_ns: d.recv_delay_ns });
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if icmp_sock::HAS_ERROR_QUEUE {
            loop {
                match sock.recv_error(&mut self.buf[..]) {
                    Ok(q) => {
                        let recv_ns = self.clock.mono_ns();
                        let kind = match (self.family, q.from_icmp, q.icmp_type) {
                            (Family::V4, true, 3) | (Family::V6, true, 1) => ErrorKind::Unreachable,
                            (Family::V4, true, 11) | (Family::V6, true, 3) => ErrorKind::TimeExceeded,
                            _ => ErrorKind::Other,
                        };
                        let quoted = icmp_packet::quoted_echo(&self.buf[..q.len], q.dest);
                        let Some(src) = q.offender.or(q.dest) else { continue };
                        items.push(Incoming {
                            recv_ns,
                            src,
                            parsed: Parsed::Error { kind, icmp_type: q.icmp_type, code: q.code, quoted },
                            ttl: None,
                            offender: q.offender,
                            recv_delay_ns: None,
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
    }
}

impl IcmpIo for RealIcmpIo {
    fn id(&self) -> u16 {
        self.id
    }

    fn check_id(&self) -> bool {
        !icmp_sock::KERNEL_REWRITES_ID
    }

    fn send(&mut self, dest: IpAddr, packet: &[u8]) -> io::Result<()> {
        self.afd.get_ref().send_to(dest, packet).map(drop)
    }

    fn set_ttl(&mut self, ttl: u8) -> io::Result<()> {
        self.afd.get_ref().set_ttl(ttl)
    }

    async fn recv(&mut self) -> io::Result<Vec<Incoming>> {
        let interest = if icmp_sock::HAS_ERROR_QUEUE { Interest::READABLE | Interest::ERROR } else { Interest::READABLE };
        loop {
            // No await between readiness and the drain, so dropping this future
            // (when another branch of the caller's select wins) loses nothing.
            self.afd.ready(interest).await?.clear_ready();
            let mut items = Vec::new();
            self.drain(&mut items);
            if !items.is_empty() {
                return Ok(items);
            }
        }
    }
}

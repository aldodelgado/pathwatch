//! Test support: a scripted ICMP transport driven by the (pausable) Tokio
//! clock, so collector behaviour can be exercised deterministically with no
//! network. Not used by the production binary.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::clock::Clock;
use crate::probe::icmp::IcmpIo;
use crate::probe::icmp_core::Incoming;
use crate::probe::icmp_packet::{ErrorKind, ICMP_HEADER, Parsed, Quoted, Token};

/// What the scripted network does with one echo request.
#[derive(Debug, Clone)]
pub enum FakeReply {
    /// An echo reply after this long.
    Echo(Duration),
    /// A destination-unreachable from this router after this long.
    Unreachable(Duration, IpAddr),
    /// The local stack refuses the send with this OS error.
    SendError(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentProbe {
    pub at_ns: u64,
    pub dest: IpAddr,
    pub stream: u32,
    pub seq: u64,
}

/// Decides the replies for a probe from its destination, stream-local
/// sequence, and send time. Returning an empty list means silence.
pub type Script = Box<dyn FnMut(IpAddr, u64, u64) -> Vec<FakeReply> + Send>;

pub struct FakeIcmpIo {
    clock: Arc<dyn Clock>,
    script: Script,
    queue: BinaryHeap<Reverse<(u64, u64)>>,
    items: std::collections::HashMap<u64, Incoming>,
    order: u64,
    sent: Arc<Mutex<Vec<SentProbe>>>,
}

impl FakeIcmpIo {
    pub fn new(clock: Arc<dyn Clock>, script: Script) -> (Self, Arc<Mutex<Vec<SentProbe>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Self { clock, script, queue: BinaryHeap::new(), items: Default::default(), order: 0, sent: sent.clone() }, sent)
    }

    fn push(&mut self, due_ns: u64, inc: Incoming) {
        self.order += 1;
        self.items.insert(self.order, inc);
        self.queue.push(Reverse((due_ns, self.order)));
    }
}

fn read_token(packet: &[u8]) -> Option<Token> {
    let b = packet.get(ICMP_HEADER..ICMP_HEADER + 20)?;
    Some(Token { nonce: u32::from_be_bytes(b[4..8].try_into().ok()?), stream: u32::from_be_bytes(b[8..12].try_into().ok()?), seq: u64::from_be_bytes(b[12..20].try_into().ok()?) })
}

impl IcmpIo for FakeIcmpIo {
    fn id(&self) -> u16 {
        0x4242
    }

    fn check_id(&self) -> bool {
        true
    }

    fn send(&mut self, dest: IpAddr, packet: &[u8]) -> io::Result<()> {
        let token = read_token(packet).ok_or_else(|| io::Error::other("probe without a token"))?;
        let now = self.clock.mono_ns();
        let replies = (self.script)(dest, token.seq, now);
        if let Some(FakeReply::SendError(errno)) = replies.iter().find(|r| matches!(r, FakeReply::SendError(_))) {
            return Err(io::Error::from_raw_os_error(*errno));
        }
        self.sent.lock().unwrap().push(SentProbe { at_ns: now, dest, stream: token.stream, seq: token.seq });
        let payload_len = (packet.len() - ICMP_HEADER) as u16;
        for r in replies {
            match r {
                FakeReply::Echo(after) => {
                    let due = now + after.as_nanos() as u64;
                    let parsed = Parsed::EchoReply { id: self.id(), seq16: token.seq as u16, token: Some(token), payload_len };
                    self.push(due, Incoming { recv_ns: due, src: dest, parsed, ttl: Some(57), offender: None, recv_delay_ns: None });
                }
                FakeReply::Unreachable(after, router) => {
                    let due = now + after.as_nanos() as u64;
                    let quoted = Quoted { dest: Some(dest), id: Some(self.id()), seq16: Some(token.seq as u16), token: None };
                    let parsed = Parsed::Error { kind: ErrorKind::Unreachable, icmp_type: 3, code: 1, quoted };
                    self.push(due, Incoming { recv_ns: due, src: router, parsed, ttl: None, offender: None, recv_delay_ns: None });
                }
                FakeReply::SendError(_) => {}
            }
        }
        Ok(())
    }

    async fn recv(&mut self) -> io::Result<Vec<Incoming>> {
        loop {
            let Some(&Reverse((due, _))) = self.queue.peek() else {
                // Nothing in flight. The caller re-polls after every send.
                return std::future::pending().await;
            };
            tokio::time::sleep_until(self.clock.instant_at(due)).await;
            let now = self.clock.mono_ns();
            let mut out = Vec::new();
            while let Some(&Reverse((d, order))) = self.queue.peek() {
                if d > now {
                    break;
                }
                self.queue.pop();
                if let Some(inc) = self.items.remove(&order) {
                    out.push(inc);
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
    }
}

/// A host network that a test sets and changes at will. Answers at once, so
/// it is asked on the collector's own thread and stays deterministic under a
/// paused clock.
pub struct FakeNet {
    state: std::sync::Mutex<(Option<crate::netwatch::SourceAddr>, crate::localnet::LocalNet)>,
}

impl Default for FakeNet {
    fn default() -> Self {
        let net = Self { state: std::sync::Mutex::new((None, crate::localnet::LocalNet::unknown("not set in this test"))) };
        net.set("192.0.2.10", "en0", Some("192.0.2.1"));
        net
    }
}

impl FakeNet {
    /// The IPv4 source address and interface, and the IPv4 gateway. IPv6 has no route.
    pub fn set(&self, ip: &str, ifname: &str, gateway: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        state.0 = Some(crate::netwatch::SourceAddr { ip: ip.parse().expect("an address"), ifname: Some(ifname.into()) });
        state.1.gateway_v4 = gateway.map(|g| g.parse().expect("an address")).ok_or_else(|| "the routing table has no IPv4 default route".to_string());
    }

    pub fn set_resolvers(&self, list: &[&str]) {
        self.state.lock().unwrap().1.resolvers = Ok(list.iter().map(|r| r.parse().expect("an address")).collect());
    }

    /// No network at all.
    pub fn disconnect(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = None;
        state.1.gateway_v4 = Err("the routing table has no IPv4 default route".into());
    }
}

impl crate::netwatch::NetProbe for FakeNet {
    fn source_for(&self, dest: std::net::IpAddr) -> Option<crate::netwatch::SourceAddr> {
        if dest.is_ipv4() { self.state.lock().unwrap().0.clone() } else { None }
    }

    fn local(&self) -> crate::localnet::LocalNet {
        self.state.lock().unwrap().1.clone()
    }

    fn inline(&self) -> bool {
        true
    }
}

/// How one router on a scripted path behaves.
pub struct PathHop {
    /// The addresses that answer at this TTL, taken in turn.
    pub addrs: Vec<IpAddr>,
    /// Whether the n-th probe (from 0) to this hop is answered.
    pub answers: Box<dyn Fn(u32) -> bool + Send>,
    pub rtt: Duration,
    /// The n-th probe (from 0) for which this is true is answered after
    /// `slow_rtt` instead.
    pub slow: Box<dyn Fn(u32) -> bool + Send>,
    pub slow_rtt: Duration,
}

impl PathHop {
    pub fn replying(addr: &str, rtt_ms: u64) -> Self {
        Self {
            addrs: vec![addr.parse().expect("an address")],
            answers: Box::new(|_| true),
            rtt: Duration::from_millis(rtt_ms),
            slow: Box::new(|_| false),
            slow_rtt: Duration::ZERO,
        }
    }

    pub fn silent() -> Self {
        Self { addrs: vec!["0.0.0.0".parse().expect("an address")], answers: Box::new(|_| false), rtt: Duration::ZERO, slow: Box::new(|_| false), slow_rtt: Duration::ZERO }
    }

    pub fn answering(mut self, f: impl Fn(u32) -> bool + Send + 'static) -> Self {
        self.answers = Box::new(f);
        self
    }

    /// Answers the probes `f` picks after `rtt_ms` instead of the usual time.
    pub fn slow_when(mut self, f: impl Fn(u32) -> bool + Send + 'static, rtt_ms: u64) -> Self {
        (self.slow, self.slow_rtt) = (Box::new(f), Duration::from_millis(rtt_ms));
        self
    }
}

/// A scripted path for route tracing: a list of routers, then the
/// destination. Every probe sent is recorded with its TTL and time, so a
/// test can count them against a budget.
pub struct FakePath {
    clock: Arc<dyn Clock>,
    hops: Vec<PathHop>,
    dest: PathHop,
    ttl: u8,
    seen: std::collections::HashMap<u8, u32>,
    queue: BinaryHeap<Reverse<(u64, u64)>>,
    items: std::collections::HashMap<u64, Incoming>,
    order: u64,
    sent: SentLog,
}

/// Every probe a scripted path was sent: when, and with which TTL.
pub type SentLog = Arc<Mutex<Vec<(u64, u8)>>>;

impl FakePath {
    /// `dest` says how the destination itself answers, once a probe's TTL is
    /// enough to reach it.
    pub fn new(clock: Arc<dyn Clock>, hops: Vec<PathHop>, dest: PathHop) -> (Self, SentLog) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (Self { clock, hops, dest, ttl: 64, seen: Default::default(), queue: BinaryHeap::new(), items: Default::default(), order: 0, sent: sent.clone() }, sent)
    }
}

impl IcmpIo for FakePath {
    fn id(&self) -> u16 {
        0x7ace
    }

    fn check_id(&self) -> bool {
        true
    }

    fn set_ttl(&mut self, ttl: u8) -> io::Result<()> {
        self.ttl = ttl;
        Ok(())
    }

    fn send(&mut self, dest: IpAddr, packet: &[u8]) -> io::Result<()> {
        let now = self.clock.mono_ns();
        let (ttl, id) = (self.ttl, self.id());
        self.sent.lock().unwrap().push((now, ttl));
        let seq16 = u16::from_be_bytes([packet[6], packet[7]]);
        let nth = self.seen.entry(ttl).or_default();
        let n = *nth;
        *nth += 1;
        let (hop, reached) = match self.hops.get(usize::from(ttl) - 1) {
            Some(hop) => (hop, false),
            None => (&self.dest, true),
        };
        if !(hop.answers)(n) {
            return Ok(());
        }
        let due = now + if (hop.slow)(n) { hop.slow_rtt } else { hop.rtt }.as_nanos() as u64;
        let inc = if reached {
            let parsed = Parsed::EchoReply { id, seq16, token: read_token(packet), payload_len: (packet.len() - ICMP_HEADER) as u16 };
            Incoming { recv_ns: due, src: dest, parsed, ttl: Some(50), offender: None, recv_delay_ns: None }
        } else {
            // A router quotes the start of the probe: enough for the
            // identifier and sequence number, not for the payload.
            let quoted = Quoted { dest: Some(dest), id: Some(id), seq16: Some(seq16), token: None };
            let parsed = Parsed::Error { kind: ErrorKind::TimeExceeded, icmp_type: 11, code: 0, quoted };
            Incoming { recv_ns: due, src: hop.addrs[n as usize % hop.addrs.len()], parsed, ttl: None, offender: None, recv_delay_ns: None }
        };
        self.order += 1;
        self.items.insert(self.order, inc);
        self.queue.push(Reverse((due, self.order)));
        Ok(())
    }

    async fn recv(&mut self) -> io::Result<Vec<Incoming>> {
        loop {
            let Some(&Reverse((due, _))) = self.queue.peek() else {
                return std::future::pending().await;
            };
            tokio::time::sleep_until(self.clock.instant_at(due)).await;
            let now = self.clock.mono_ns();
            let mut out = Vec::new();
            while let Some(&Reverse((d, order))) = self.queue.peek() {
                if d > now {
                    break;
                }
                self.queue.pop();
                if let Some(inc) = self.items.remove(&order) {
                    out.push(inc);
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
    }
}

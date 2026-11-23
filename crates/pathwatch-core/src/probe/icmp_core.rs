//! ICMP reply matching as a pure state machine: no sockets, no clocks, no
//! async. The caller supplies every timestamp, which makes deadline edges,
//! late replies, duplicates, and reordering exactly testable.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::net::IpAddr;

use super::icmp_packet::{ErrorKind, Parsed, Quoted, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub stream: u32,
    pub seq: u64,
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    dest: IpAddr,
    sent_ns: u64,
    deadline_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finalised {
    TimedOut,
    /// Timed out and already annotated with one late reply.
    TimedOutLateSeen,
    Answered,
}

#[derive(Debug, Clone, Copy)]
struct Recent {
    dest: IpAddr,
    sent_ns: u64,
    state: Finalised,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Incoming {
    pub recv_ns: u64,
    pub src: IpAddr,
    pub parsed: Parsed,
    pub ttl: Option<u8>,
    /// For errors read from a socket error queue: the router that sent it.
    pub offender: Option<IpAddr>,
    /// Kernel receipt → our read, where the kernel stamps arrivals.
    pub recv_delay_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreEvent {
    Reply {
        key: Key,
        sent_ns: u64,
        recv_ns: u64,
        ttl: Option<u8>,
        recv_delay_ns: Option<u64>,
    },
    Unreachable {
        key: Key,
        sent_ns: u64,
        recv_ns: u64,
        kind: ErrorKind,
        icmp_type: u8,
        code: u8,
        from: IpAddr,
    },
    Timeout {
        key: Key,
        sent_ns: u64,
    },
    /// A reply to a probe already finalised as a timeout. The outcome stands.
    Late {
        key: Key,
        late_rtt_ns: u64,
        recv_ns: u64,
    },
    /// A further reply to a probe that was already answered.
    Duplicate {
        key: Key,
        recv_ns: u64,
    },
    /// Not ours, or no longer attributable.
    Unmatched,
}

pub struct IcmpCore {
    nonce: u32,
    /// Our echo identifier, when the kernel preserves it (macOS). `None` when
    /// the kernel rewrites it and demultiplexes replies itself (Linux).
    check_id: Option<u16>,
    late_window_ns: u64,
    pending: HashMap<Key, Pending>,
    deadlines: BinaryHeap<Reverse<(u64, Key)>>,
    recent: HashMap<Key, Recent>,
    recent_order: VecDeque<(u64, Key)>,
    pub unmatched: u64,
}

impl IcmpCore {
    pub fn new(nonce: u32, check_id: Option<u16>, late_window_ns: u64) -> Self {
        Self { nonce, check_id, late_window_ns, pending: HashMap::new(), deadlines: BinaryHeap::new(), recent: HashMap::new(), recent_order: VecDeque::new(), unmatched: 0 }
    }

    pub fn nonce(&self) -> u32 {
        self.nonce
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Records a probe that has just been sent.
    pub fn register(&mut self, key: Key, dest: IpAddr, sent_ns: u64, deadline_ns: u64) {
        self.pending.insert(key, Pending { dest, sent_ns, deadline_ns });
        self.deadlines.push(Reverse((deadline_ns, key)));
    }

    /// Removes a probe whose send failed after registration.
    pub fn forget(&mut self, key: Key) {
        self.pending.remove(&key);
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.deadlines.peek().map(|Reverse((t, _))| *t)
    }

    /// Finalises every probe whose deadline is at or before `now_ns`.
    pub fn expire(&mut self, now_ns: u64) -> Vec<CoreEvent> {
        let mut out = Vec::new();
        while let Some(Reverse((deadline, key))) = self.deadlines.peek().copied() {
            if deadline > now_ns {
                break;
            }
            self.deadlines.pop();
            // Entries for probes that were answered stay in the heap until now.
            if let Some(p) = self.pending.get(&key).copied()
                && p.deadline_ns == deadline
            {
                self.pending.remove(&key);
                self.remember(key, p, Finalised::TimedOut, deadline);
                out.push(CoreEvent::Timeout { key, sent_ns: p.sent_ns });
            }
        }
        self.forget_old(now_ns);
        out
    }

    /// Drops everything pending, returning the keys (for cancellation).
    pub fn drain_pending(&mut self) -> Vec<(Key, u64)> {
        self.deadlines.clear();
        let mut v: Vec<(Key, u64)> = self.pending.drain().map(|(k, p)| (k, p.sent_ns)).collect();
        v.sort_unstable();
        v
    }

    /// Handles one packet read from the socket.
    pub fn on_incoming(&mut self, inc: Incoming) -> Vec<CoreEvent> {
        // A reply at or after its deadline is late even if the timer has not
        // fired yet: the deadline is part of the metric's definition.
        let mut out = self.expire_through(inc.recv_ns);
        let ev = match inc.parsed {
            Parsed::EchoReply { id, seq16, token, .. } => self.on_reply(inc, id, seq16, token),
            Parsed::Error { kind, icmp_type, code, quoted } => self.on_error(inc, kind, icmp_type, code, quoted),
            Parsed::Other { .. } => CoreEvent::Unmatched,
        };
        if ev == CoreEvent::Unmatched {
            self.unmatched += 1;
        }
        out.push(ev);
        out
    }

    fn expire_through(&mut self, recv_ns: u64) -> Vec<CoreEvent> {
        self.expire(recv_ns)
    }

    fn on_reply(&mut self, inc: Incoming, id: u16, seq16: u16, token: Option<Token>) -> CoreEvent {
        let Some(token) = token else { return CoreEvent::Unmatched };
        if token.nonce != self.nonce || seq16 != token.seq as u16 || self.check_id.is_some_and(|ours| ours != id) {
            return CoreEvent::Unmatched;
        }
        let key = Key { stream: token.stream, seq: token.seq };
        if let Some(p) = self.pending.get(&key).copied() {
            if p.dest != inc.src {
                return CoreEvent::Unmatched;
            }
            self.pending.remove(&key);
            self.remember(key, p, Finalised::Answered, inc.recv_ns);
            return CoreEvent::Reply { key, sent_ns: p.sent_ns, recv_ns: inc.recv_ns, ttl: inc.ttl, recv_delay_ns: inc.recv_delay_ns };
        }
        match self.recent.get_mut(&key) {
            Some(r) if r.dest != inc.src => CoreEvent::Unmatched,
            Some(r) if r.state == Finalised::TimedOut => {
                r.state = Finalised::TimedOutLateSeen;
                CoreEvent::Late { key, late_rtt_ns: inc.recv_ns.saturating_sub(r.sent_ns), recv_ns: inc.recv_ns }
            }
            Some(_) => CoreEvent::Duplicate { key, recv_ns: inc.recv_ns },
            None => CoreEvent::Unmatched,
        }
    }

    fn on_error(&mut self, inc: Incoming, kind: ErrorKind, icmp_type: u8, code: u8, q: Quoted) -> CoreEvent {
        if let (Some(ours), Some(theirs)) = (self.check_id, q.id)
            && ours != theirs
        {
            return CoreEvent::Unmatched;
        }
        let key = match q.token {
            Some(t) if t.nonce == self.nonce => Some(Key { stream: t.stream, seq: t.seq }),
            Some(_) => None,
            // Only the echo header was quoted: attribute by original destination
            // and 16-bit sequence among probes still pending.
            None => match (q.dest, q.seq16) {
                (Some(dest), Some(seq16)) => self.pending.iter().filter(|(k, p)| p.dest == dest && k.seq as u16 == seq16).map(|(k, _)| *k).min(),
                _ => None,
            },
        };
        let Some(key) = key else { return CoreEvent::Unmatched };
        let Some(p) = self.pending.get(&key).copied() else { return CoreEvent::Unmatched };
        if q.dest.is_some_and(|d| d != p.dest) {
            return CoreEvent::Unmatched;
        }
        self.pending.remove(&key);
        self.remember(key, p, Finalised::Answered, inc.recv_ns);
        CoreEvent::Unreachable { key, sent_ns: p.sent_ns, recv_ns: inc.recv_ns, kind, icmp_type, code, from: inc.offender.unwrap_or(inc.src) }
    }

    fn remember(&mut self, key: Key, p: Pending, state: Finalised, at_ns: u64) {
        self.recent.insert(key, Recent { dest: p.dest, sent_ns: p.sent_ns, state });
        self.recent_order.push_back((at_ns, key));
    }

    fn forget_old(&mut self, now_ns: u64) {
        while let Some(&(at, key)) = self.recent_order.front() {
            if now_ns.saturating_sub(at) <= self.late_window_ns {
                break;
            }
            self.recent_order.pop_front();
            self.recent.remove(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Family;
    use crate::probe::icmp_packet::{build_echo, parse_v4};

    const NONCE: u32 = 0x5eed_0001;
    const MS: u64 = 1_000_000;
    const US: u64 = 1_000;
    const DEADLINE: u64 = 2_000 * MS;

    fn dest() -> IpAddr {
        "198.51.100.7".parse().unwrap()
    }

    fn core() -> IcmpCore {
        IcmpCore::new(NONCE, None, 30_000 * MS)
    }

    fn key(seq: u64) -> Key {
        Key { stream: 1, seq }
    }

    fn send(c: &mut IcmpCore, seq: u64, at: u64) {
        c.register(key(seq), dest(), at, at + DEADLINE);
    }

    fn reply(seq: u64, recv_ns: u64) -> Incoming {
        reply_from(seq, recv_ns, dest(), NONCE)
    }

    fn reply_from(seq: u64, recv_ns: u64, src: IpAddr, nonce: u32) -> Incoming {
        let mut pkt = build_echo(Family::V4, 77, Token { nonce, stream: 1, seq }, 56);
        pkt[0] = 0;
        let (parsed, ttl) = parse_v4(&pkt, false).unwrap();
        Incoming { recv_ns, src, parsed, ttl, offender: None, recv_delay_ns: None }
    }

    #[test]
    fn deadline_edges() {
        // 1 µs before the deadline: success.
        let mut c = core();
        send(&mut c, 0, 0);
        assert_eq!(c.on_incoming(reply(0, DEADLINE - US)), vec![CoreEvent::Reply { key: key(0), sent_ns: 0, recv_ns: DEADLINE - US, ttl: None, recv_delay_ns: None }]);

        // Exactly at the deadline: timeout, and the reply is late.
        let mut c = core();
        send(&mut c, 0, 0);
        assert_eq!(
            c.on_incoming(reply(0, DEADLINE)),
            vec![CoreEvent::Timeout { key: key(0), sent_ns: 0 }, CoreEvent::Late { key: key(0), late_rtt_ns: DEADLINE, recv_ns: DEADLINE }]
        );

        // 1 µs after, with the timer having fired first: same result.
        let mut c = core();
        send(&mut c, 0, 0);
        assert_eq!(c.expire(DEADLINE), vec![CoreEvent::Timeout { key: key(0), sent_ns: 0 }]);
        assert_eq!(c.on_incoming(reply(0, DEADLINE + US)), vec![CoreEvent::Late { key: key(0), late_rtt_ns: DEADLINE + US, recv_ns: DEADLINE + US }]);
    }

    #[test]
    fn timeout_carries_no_round_trip_time() {
        let mut c = core();
        send(&mut c, 0, 0);
        assert_eq!(c.expire(DEADLINE - 1), vec![]);
        assert_eq!(c.expire(DEADLINE), vec![CoreEvent::Timeout { key: key(0), sent_ns: 0 }]);
        assert_eq!(c.pending_len(), 0);
    }

    #[test]
    fn duplicates_keep_the_first_reply() {
        let mut c = core();
        send(&mut c, 0, 0);
        assert!(matches!(c.on_incoming(reply(0, 12 * MS))[0], CoreEvent::Reply { recv_ns, .. } if recv_ns == 12 * MS));
        assert_eq!(c.on_incoming(reply(0, 13 * MS)), vec![CoreEvent::Duplicate { key: key(0), recv_ns: 13 * MS }]);
        // A second late reply to a timed-out probe is a duplicate, not a second "late".
        send(&mut c, 1, 0);
        c.expire(DEADLINE);
        assert!(matches!(c.on_incoming(reply(1, DEADLINE + MS))[0], CoreEvent::Late { .. }));
        assert!(matches!(c.on_incoming(reply(1, DEADLINE + 2 * MS))[0], CoreEvent::Duplicate { .. }));
    }

    #[test]
    fn out_of_order_replies_match_by_probe_not_arrival() {
        let mut c = core();
        for seq in 0..3 {
            send(&mut c, seq, seq * 1_000 * MS);
        }
        let got: Vec<_> = [2u64, 0, 1].iter().flat_map(|&seq| c.on_incoming(reply(seq, 2_500 * MS + seq))).collect();
        // seq 0's deadline (2000 ms) has passed by 2500 ms: it times out and its reply is late.
        assert_eq!(
            got,
            vec![
                CoreEvent::Timeout { key: key(0), sent_ns: 0 },
                CoreEvent::Reply { key: key(2), sent_ns: 2_000 * MS, recv_ns: 2_500 * MS + 2, ttl: None, recv_delay_ns: None },
                CoreEvent::Late { key: key(0), late_rtt_ns: 2_500 * MS, recv_ns: 2_500 * MS },
                CoreEvent::Reply { key: key(1), sent_ns: 1_000 * MS, recv_ns: 2_500 * MS + 1, ttl: None, recv_delay_ns: None },
            ]
        );
    }

    #[test]
    fn sequence_wraparound_matches_on_the_64_bit_sequence() {
        let mut c = core();
        // seq 65535 and 65536 put 0xffff and 0x0000 on the wire; seq 0 is long gone.
        send(&mut c, 65_535, 0);
        send(&mut c, 65_536, 1_000 * MS);
        assert!(matches!(c.on_incoming(reply(65_536, 1_010 * MS))[0], CoreEvent::Reply { key, .. } if key == key_of(65_536)));
        assert!(matches!(c.on_incoming(reply(65_535, 1_011 * MS))[0], CoreEvent::Reply { key, .. } if key == key_of(65_535)));
        // A stale reply for seq 0 (same 16-bit sequence as 65536) is not ours any more.
        assert_eq!(c.on_incoming(reply(0, 1_012 * MS)), vec![CoreEvent::Unmatched]);
        fn key_of(seq: u64) -> Key {
            Key { stream: 1, seq }
        }
    }

    #[test]
    fn foreign_packets_are_counted_not_matched() {
        let mut c = core();
        send(&mut c, 0, 0);
        assert_eq!(c.on_incoming(reply_from(0, MS, dest(), NONCE + 1)), vec![CoreEvent::Unmatched], "another session's nonce");
        assert_eq!(c.on_incoming(reply_from(0, MS, "203.0.113.9".parse().unwrap(), NONCE)), vec![CoreEvent::Unmatched], "wrong source address");
        assert_eq!(c.unmatched, 2);
        assert_eq!(c.pending_len(), 1, "the probe is still waiting");
    }

    #[test]
    fn identifier_is_checked_only_where_the_kernel_preserves_it() {
        let mut preserved = IcmpCore::new(NONCE, Some(99), 30_000 * MS);
        preserved.register(key(0), dest(), 0, DEADLINE);
        assert_eq!(preserved.on_incoming(reply(0, MS)), vec![CoreEvent::Unmatched], "reply carries id 77, ours is 99");
        let mut rewritten = core();
        send(&mut rewritten, 0, 0);
        assert!(matches!(rewritten.on_incoming(reply(0, MS))[0], CoreEvent::Reply { .. }));
    }

    #[test]
    fn unreachable_is_attributed_by_destination_and_sequence() {
        let mut c = core();
        send(&mut c, 5, 0);
        let router: IpAddr = "192.0.2.1".parse().unwrap();
        let quoted = Quoted { dest: Some(dest()), id: None, seq16: Some(5), token: None };
        let inc = Incoming {
            recv_ns: 3 * MS,
            src: router,
            parsed: Parsed::Error { kind: ErrorKind::Unreachable, icmp_type: 3, code: 1, quoted },
            ttl: None,
            offender: None,
            recv_delay_ns: None,
        };
        assert_eq!(
            c.on_incoming(inc),
            vec![CoreEvent::Unreachable { key: key(5), sent_ns: 0, recv_ns: 3 * MS, kind: ErrorKind::Unreachable, icmp_type: 3, code: 1, from: router }]
        );
        // The same error again is no longer attributable.
        assert_eq!(c.on_incoming(inc), vec![CoreEvent::Unmatched]);
        // An error quoting a different destination is not ours.
        send(&mut c, 6, 0);
        let other = Incoming { parsed: Parsed::Error { kind: ErrorKind::Unreachable, icmp_type: 3, code: 1, quoted: Quoted { dest: Some(router), ..quoted } }, ..inc };
        assert_eq!(c.on_incoming(other), vec![CoreEvent::Unmatched]);
    }

    #[test]
    fn late_window_bounds_memory() {
        let mut c = IcmpCore::new(NONCE, None, 30_000 * MS);
        for seq in 0..1_000u64 {
            send(&mut c, seq, seq * 1_000 * MS);
            c.expire(seq * 1_000 * MS + DEADLINE);
        }
        assert!(c.recent.len() <= 32, "only timeouts from the last 30 s are remembered, got {}", c.recent.len());
        // Beyond the window a reply is unmatched rather than late.
        assert_eq!(c.on_incoming(reply(0, 1_100_000 * MS)), vec![CoreEvent::Unmatched]);
    }

    #[test]
    fn drain_pending_returns_everything_for_cancellation() {
        let mut c = core();
        send(&mut c, 0, 0);
        send(&mut c, 1, 1_000 * MS);
        assert_eq!(c.drain_pending(), vec![(key(0), 0), (key(1), 1_000 * MS)]);
        assert_eq!(c.next_deadline(), None);
        assert_eq!(c.expire(u64::MAX / 2), vec![]);
    }
}

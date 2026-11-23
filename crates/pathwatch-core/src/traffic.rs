//! Traffic accounting: what the collector actually put on the wire, and what
//! the configuration says it should. See `planning/01-design-spec.md` §13.
//!
//! ICMP and UDP are counted as IP-layer bytes (payload plus transport and IP
//! headers, no link framing), which is exact. TCP-based transports are counted
//! as TCP payload bytes — everything written to and read from the socket,
//! including TLS records — because segmentation and acknowledgements are not
//! visible from user space.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::config::{Config, Plan};
use crate::model::{Family, Protocol, TrafficRow};
use crate::probe::dns::{DnsParams, QuerySpec, build_query};

pub const UDP_HEADER: u64 = 8;
pub const ICMP_HEADER: u64 = 8;

pub const fn ip_header(family: Family) -> u64 {
    match family {
        Family::V4 => 20,
        Family::V6 => 40,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Icmp = 0,
    DnsUdp = 1,
    DnsTcp = 2,
    Doh = 3,
    Dot = 4,
}

impl Kind {
    const ALL: [Kind; 5] = [Kind::Icmp, Kind::DnsUdp, Kind::DnsTcp, Kind::Doh, Kind::Dot];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Icmp => "icmp",
            Kind::DnsUdp => "dns-udp",
            Kind::DnsTcp => "dns-tcp",
            Kind::Doh => "doh",
            Kind::Dot => "dot",
        }
    }

    /// What the byte counts for this kind mean.
    pub fn bytes_are(self) -> &'static str {
        match self {
            Kind::Icmp | Kind::DnsUdp => "ip-layer",
            Kind::DnsTcp | Kind::Doh | Kind::Dot => "tcp-payload",
        }
    }
}

#[derive(Debug, Default)]
struct Cell {
    transactions: AtomicU64,
    packets_out: AtomicU64,
    bytes_out: AtomicU64,
    packets_in: AtomicU64,
    bytes_in: AtomicU64,
}

/// Counters for everything sent and received, by kind and address family.
#[derive(Debug, Default)]
pub struct Traffic {
    cells: [[Cell; 2]; 5],
}

impl Traffic {
    fn cell(&self, kind: Kind, family: Family) -> &Cell {
        &self.cells[kind as usize][if family == Family::V4 { 0 } else { 1 }]
    }

    /// One probe transaction started on the wire.
    pub fn transaction(&self, kind: Kind, family: Family) {
        self.cell(kind, family).transactions.fetch_add(1, Ordering::Relaxed);
    }

    pub fn sent(&self, kind: Kind, family: Family, packets: u64, bytes: u64) {
        let c = self.cell(kind, family);
        c.packets_out.fetch_add(packets, Ordering::Relaxed);
        c.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn received(&self, kind: Kind, family: Family, packets: u64, bytes: u64) {
        let c = self.cell(kind, family);
        c.packets_in.fetch_add(packets, Ordering::Relaxed);
        c.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Every kind and family that saw any traffic.
    pub fn snapshot(&self) -> Vec<TrafficRow> {
        let mut rows = Vec::new();
        for kind in Kind::ALL {
            for family in [Family::V4, Family::V6] {
                let c = self.cell(kind, family);
                let row = TrafficRow {
                    kind: kind.as_str().into(),
                    family,
                    transactions: c.transactions.load(Ordering::Relaxed),
                    packets_out: c.packets_out.load(Ordering::Relaxed),
                    bytes_out: c.bytes_out.load(Ordering::Relaxed),
                    packets_in: c.packets_in.load(Ordering::Relaxed),
                    bytes_in: c.bytes_in.load(Ordering::Relaxed),
                    bytes: kind.bytes_are().into(),
                };
                if row.transactions + row.bytes_out + row.bytes_in > 0 {
                    rows.push(row);
                }
            }
        }
        rows
    }
}

/// A stream wrapper that counts every byte written and read, for TCP-based
/// transports. It sits under TLS, so it sees what the socket carries.
/// (The wrapped stream stays reachable, for checks that do not read from it.)
pub struct Counted<T> {
    inner: T,
    traffic: Arc<crate::probe::Counters>,
    kind: Kind,
    family: Family,
}

impl<T> Counted<T> {
    pub fn new(inner: T, traffic: Arc<crate::probe::Counters>, kind: Kind, family: Family) -> Self {
        Self { inner, traffic, kind, family }
    }

    pub fn get_ref(&self) -> &T {
        &self.inner
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &r {
            let n = (buf.filled().len() - before) as u64;
            self.traffic.traffic.received(self.kind, self.family, 0, n);
        }
        r
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Counted<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.traffic.traffic.sent(self.kind, self.family, 0, *n as u64);
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The steady-state outbound traffic a plan implies.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetRow {
    pub kind: Kind,
    pub family: Family,
    pub transactions_per_s: f64,
    /// Outbound bytes per second. Exact for ICMP and DNS over UDP; `None`
    /// where it cannot be computed from the configuration (TCP-based transports).
    pub bytes_out_per_s: Option<f64>,
}

/// Computes the expected outbound traffic of every collecting stream, assuming
/// each slot is sent. Streams that cannot send (no route, unsupported) will
/// measure below this, which is the point of comparing.
pub fn budget(config: &Config, plan: &Plan) -> Vec<BudgetRow> {
    let params = DnsParams { edns_udp_size: config.dns.edns_udp_size, recursion_desired: config.dns.recursion_desired, tcp_fallback: config.dns.tcp_fallback };
    // Queries rotate evenly, so the mean query size is the expected size.
    let sizes: Vec<u64> = config.dns.query.iter().filter_map(|q| QuerySpec::from_cfg(q).ok()).filter_map(|s| build_query(&s, 0, params).ok()).map(|b| b.len() as u64).collect();
    let mean_query = if sizes.is_empty() { 0.0 } else { sizes.iter().sum::<u64>() as f64 / sizes.len() as f64 };

    let mut rows: Vec<BudgetRow> = Vec::new();
    for s in plan.collecting() {
        let kind = match s.protocol {
            Protocol::Icmp => Kind::Icmp,
            Protocol::DnsUdp => Kind::DnsUdp,
            Protocol::DnsTcp => Kind::DnsTcp,
            Protocol::Doh => Kind::Doh,
            Protocol::Dot => Kind::Dot,
        };
        let rate = 1.0 / s.interval.as_secs_f64();
        let per_probe = match kind {
            Kind::Icmp => Some((ICMP_HEADER + u64::from(config.icmp.payload_bytes) + ip_header(s.family)) as f64),
            Kind::DnsUdp => Some(mean_query + (UDP_HEADER + ip_header(s.family)) as f64),
            Kind::DnsTcp | Kind::Doh | Kind::Dot => None,
        };
        match rows.iter_mut().find(|r| r.kind == kind && r.family == s.family) {
            Some(r) => {
                r.transactions_per_s += rate;
                r.bytes_out_per_s = match (r.bytes_out_per_s, per_probe) {
                    (Some(a), Some(b)) => Some(a + rate * b),
                    _ => None,
                };
            }
            None => rows.push(BudgetRow { kind, family: s.family, transactions_per_s: rate, bytes_out_per_s: per_probe.map(|b| rate * b) }),
        }
    }
    rows.sort_by_key(|r| (r.kind as usize, r.family));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Loaded;

    #[test]
    fn default_budget_matches_the_documented_packet_sizes() {
        let l = Loaded::from_layers(None, None, &[]).unwrap();
        let rows = budget(&l.config, &l.plan(&|_| None));
        let get = |k: Kind, f: Family| rows.iter().find(|r| r.kind == k && r.family == f).unwrap();
        // Two ICMP streams per family at 1/s: 84-byte IPv4 and 104-byte IPv6 packets.
        assert_eq!(get(Kind::Icmp, Family::V4).bytes_out_per_s, Some(168.0));
        assert_eq!(get(Kind::Icmp, Family::V6).bytes_out_per_s, Some(208.0));
        // Queries are 28, 40, and 42 bytes (root NS, example.com A, wikipedia.org AAAA).
        let dns4 = get(Kind::DnsUdp, Family::V4);
        assert!((dns4.transactions_per_s - 0.4).abs() < 1e-9);
        let per_query = dns4.bytes_out_per_s.unwrap() / dns4.transactions_per_s;
        assert!((per_query - (110.0 / 3.0 + 28.0)).abs() < 1e-6, "{per_query}");
        // DoH has a known rate but no computable size.
        let doh = get(Kind::Doh, Family::V4);
        assert!((doh.transactions_per_s - (0.4 + 2.0 / 300.0)).abs() < 1e-9);
        assert_eq!(doh.bytes_out_per_s, None);
    }

    #[test]
    fn counters_report_only_what_saw_traffic() {
        let t = Traffic::default();
        assert!(t.snapshot().is_empty());
        t.transaction(Kind::Icmp, Family::V6);
        t.sent(Kind::Icmp, Family::V6, 1, 104);
        t.received(Kind::Icmp, Family::V6, 1, 104);
        let rows = t.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].kind.as_str(), rows[0].family, rows[0].packets_out, rows[0].bytes_out, rows[0].bytes.as_str()), ("icmp", Family::V6, 1, 104, "ip-layer"));
    }
}

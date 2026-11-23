//! ICMP echo encoding and decoding for both address families.
//!
//! Every echo request carries a token in its payload — magic, session nonce,
//! stream index, and the full 64-bit sequence — so replies are matched on
//! payload, not on the 16-bit ICMP sequence that wraps every 18 hours at 1 Hz.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::model::Family;

pub const MAGIC: [u8; 4] = *b"pwch";
pub const ICMP_HEADER: usize = 8;
pub const TOKEN_LEN: usize = 20;

const V4_ECHO_REQUEST: u8 = 8;
const V4_ECHO_REPLY: u8 = 0;
const V4_UNREACHABLE: u8 = 3;
const V4_TIME_EXCEEDED: u8 = 11;
const V4_PARAM_PROBLEM: u8 = 12;
const V6_UNREACHABLE: u8 = 1;
const V6_PACKET_TOO_BIG: u8 = 2;
const V6_TIME_EXCEEDED: u8 = 3;
const V6_PARAM_PROBLEM: u8 = 4;
const V6_ECHO_REQUEST: u8 = 128;
const V6_ECHO_REPLY: u8 = 129;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub nonce: u32,
    pub stream: u32,
    pub seq: u64,
}

impl Token {
    fn write(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&MAGIC);
        out[4..8].copy_from_slice(&self.nonce.to_be_bytes());
        out[8..12].copy_from_slice(&self.stream.to_be_bytes());
        out[12..20].copy_from_slice(&self.seq.to_be_bytes());
    }

    fn read(b: &[u8]) -> Option<Self> {
        if b.len() < TOKEN_LEN || b[0..4] != MAGIC {
            return None;
        }
        Some(Self {
            nonce: u32::from_be_bytes(b[4..8].try_into().ok()?),
            stream: u32::from_be_bytes(b[8..12].try_into().ok()?),
            seq: u64::from_be_bytes(b[12..20].try_into().ok()?),
        })
    }
}

/// Builds an echo request with `payload_len` bytes of ICMP payload (at least
/// [`TOKEN_LEN`]). The IPv4 checksum is filled in; for IPv6 the kernel computes
/// it over the pseudo-header.
pub fn build_echo(family: Family, id: u16, token: Token, payload_len: usize) -> Vec<u8> {
    let payload_len = payload_len.max(TOKEN_LEN);
    let mut p = vec![0u8; ICMP_HEADER + payload_len];
    p[0] = match family {
        Family::V4 => V4_ECHO_REQUEST,
        Family::V6 => V6_ECHO_REQUEST,
    };
    p[4..6].copy_from_slice(&id.to_be_bytes());
    p[6..8].copy_from_slice(&(token.seq as u16).to_be_bytes());
    token.write(&mut p[ICMP_HEADER..ICMP_HEADER + TOKEN_LEN]);
    for (i, b) in p[ICMP_HEADER + TOKEN_LEN..].iter_mut().enumerate() {
        *b = i as u8;
    }
    if family == Family::V4 {
        let sum = checksum(&p);
        p[2..4].copy_from_slice(&sum.to_be_bytes());
    }
    p
}

/// RFC 1071 Internet checksum.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let (pairs, rest) = data.as_chunks::<2>();
    for c in pairs {
        sum += u32::from(u16::from_be_bytes(*c));
    }
    if let [last] = rest {
        sum += u32::from(*last) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Unreachable,
    TimeExceeded,
    Other,
}

/// What an ICMP error says about the packet that caused it. Routers quote at
/// least the original IP header plus 8 bytes, which covers our echo header
/// (identifier and 16-bit sequence) but not necessarily the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quoted {
    pub dest: Option<IpAddr>,
    pub id: Option<u16>,
    pub seq16: Option<u16>,
    pub token: Option<Token>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parsed {
    EchoReply { id: u16, seq16: u16, token: Option<Token>, payload_len: u16 },
    Error { kind: ErrorKind, icmp_type: u8, code: u8, quoted: Quoted },
    Other { icmp_type: u8 },
}

/// Parses a datagram read from an ICMPv4 socket. `with_ip_header` is true on
/// systems that deliver the IP header (macOS datagram ICMP sockets). Returns
/// the parsed message and, when the header was present, the packet's TTL.
pub fn parse_v4(buf: &[u8], with_ip_header: bool) -> Option<(Parsed, Option<u8>)> {
    let (icmp, ttl) = if with_ip_header {
        let ihl = ipv4_header_len(buf)?;
        (&buf[ihl..], Some(buf[8]))
    } else {
        (buf, None)
    };
    if icmp.len() < ICMP_HEADER {
        return None;
    }
    let (ty, code) = (icmp[0], icmp[1]);
    let parsed = match ty {
        V4_ECHO_REPLY => echo_reply(icmp),
        V4_UNREACHABLE | V4_TIME_EXCEEDED | V4_PARAM_PROBLEM => Parsed::Error {
            kind: match ty {
                V4_UNREACHABLE => ErrorKind::Unreachable,
                V4_TIME_EXCEEDED => ErrorKind::TimeExceeded,
                _ => ErrorKind::Other,
            },
            icmp_type: ty,
            code,
            quoted: quoted_v4(&icmp[ICMP_HEADER..]),
        },
        other => Parsed::Other { icmp_type: other },
    };
    Some((parsed, ttl))
}

/// Parses a datagram read from an ICMPv6 socket (never includes the IPv6 header).
pub fn parse_v6(icmp: &[u8]) -> Option<Parsed> {
    if icmp.len() < ICMP_HEADER {
        return None;
    }
    let (ty, code) = (icmp[0], icmp[1]);
    Some(match ty {
        V6_ECHO_REPLY => echo_reply(icmp),
        V6_UNREACHABLE | V6_PACKET_TOO_BIG | V6_TIME_EXCEEDED | V6_PARAM_PROBLEM => Parsed::Error {
            kind: match ty {
                V6_UNREACHABLE => ErrorKind::Unreachable,
                V6_TIME_EXCEEDED => ErrorKind::TimeExceeded,
                _ => ErrorKind::Other,
            },
            icmp_type: ty,
            code,
            quoted: quoted_v6(&icmp[ICMP_HEADER..]),
        },
        other => Parsed::Other { icmp_type: other },
    })
}

/// Interprets the start of an echo request we sent, as returned in a socket
/// error queue (Linux) where the kernel hands back the offending packet's
/// ICMP header onward.
pub fn quoted_echo(echo: &[u8], dest: Option<IpAddr>) -> Quoted {
    if echo.len() < ICMP_HEADER || !matches!(echo[0], V4_ECHO_REQUEST | V6_ECHO_REQUEST) {
        return Quoted { dest, id: None, seq16: None, token: None };
    }
    Quoted { dest, id: Some(u16::from_be_bytes([echo[4], echo[5]])), seq16: Some(u16::from_be_bytes([echo[6], echo[7]])), token: Token::read(&echo[ICMP_HEADER..]) }
}

fn echo_reply(icmp: &[u8]) -> Parsed {
    Parsed::EchoReply {
        id: u16::from_be_bytes([icmp[4], icmp[5]]),
        seq16: u16::from_be_bytes([icmp[6], icmp[7]]),
        token: Token::read(&icmp[ICMP_HEADER..]),
        payload_len: (icmp.len() - ICMP_HEADER) as u16,
    }
}

fn ipv4_header_len(buf: &[u8]) -> Option<usize> {
    let first = *buf.first()?;
    if first >> 4 != 4 {
        return None;
    }
    let ihl = usize::from(first & 0x0f) * 4;
    (ihl >= 20 && buf.len() >= ihl).then_some(ihl)
}

fn quoted_v4(inner: &[u8]) -> Quoted {
    let none = Quoted { dest: None, id: None, seq16: None, token: None };
    let Some(ihl) = ipv4_header_len(inner) else { return none };
    if inner[9] != 1 {
        return none; // the offending packet was not ICMP
    }
    let dest = IpAddr::V4(Ipv4Addr::new(inner[16], inner[17], inner[18], inner[19]));
    quoted_echo(&inner[ihl..], Some(dest))
}

fn quoted_v6(inner: &[u8]) -> Quoted {
    let none = Quoted { dest: None, id: None, seq16: None, token: None };
    if inner.len() < 40 || inner[0] >> 4 != 6 || inner[6] != 58 {
        return none; // not IPv6, or the next header is not ICMPv6
    }
    let mut d = [0u8; 16];
    d.copy_from_slice(&inner[24..40]);
    quoted_echo(&inner[40..], Some(IpAddr::V6(Ipv6Addr::from(d))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: Token = Token { nonce: 0xdead_beef, stream: 3, seq: 70_000 };

    fn reply_from(mut request: Vec<u8>, family: Family) -> Vec<u8> {
        request[0] = if family == Family::V4 { V4_ECHO_REPLY } else { V6_ECHO_REPLY };
        request
    }

    #[test]
    fn echo_has_ping_sized_message_and_valid_checksum() {
        let p = build_echo(Family::V4, 0x1234, TOKEN, 56);
        assert_eq!(p.len(), 64, "8-byte header + 56-byte payload, the same as ping");
        assert_eq!(checksum(&p), 0, "a packet with a correct checksum sums to zero");
        assert_eq!(u16::from_be_bytes([p[6], p[7]]), (70_000u64 % 65_536) as u16);
    }

    #[test]
    fn reply_round_trips_the_token_past_sequence_wraparound() {
        let reply = reply_from(build_echo(Family::V4, 7, TOKEN, 56), Family::V4);
        let (parsed, ttl) = parse_v4(&reply, false).unwrap();
        assert_eq!(ttl, None);
        assert_eq!(parsed, Parsed::EchoReply { id: 7, seq16: 4464, token: Some(TOKEN), payload_len: 56 });

        let reply6 = reply_from(build_echo(Family::V6, 7, TOKEN, 56), Family::V6);
        assert_eq!(parse_v6(&reply6).unwrap(), Parsed::EchoReply { id: 7, seq16: 4464, token: Some(TOKEN), payload_len: 56 });
    }

    fn ipv4_header(proto: u8, ttl: u8, src: [u8; 4], dst: [u8; 4]) -> Vec<u8> {
        let mut h = vec![0u8; 20];
        h[0] = 0x45;
        h[8] = ttl;
        h[9] = proto;
        h[12..16].copy_from_slice(&src);
        h[16..20].copy_from_slice(&dst);
        h
    }

    #[test]
    fn parses_reply_with_ip_header_and_reads_ttl() {
        let mut pkt = ipv4_header(1, 57, [1, 1, 1, 1], [192, 0, 2, 10]);
        pkt.extend(reply_from(build_echo(Family::V4, 9, TOKEN, 56), Family::V4));
        let (parsed, ttl) = parse_v4(&pkt, true).unwrap();
        assert_eq!(ttl, Some(57));
        assert!(matches!(parsed, Parsed::EchoReply { id: 9, token: Some(TOKEN), .. }));
    }

    #[test]
    fn unreachable_quotes_only_the_echo_header() {
        // Router quotes the original IP header + 8 bytes: id and seq16, no token.
        let request = build_echo(Family::V4, 9, TOKEN, 56);
        let mut icmp = vec![V4_UNREACHABLE, 1, 0, 0, 0, 0, 0, 0];
        icmp.extend(ipv4_header(1, 1, [192, 0, 2, 10], [198, 51, 100, 7]));
        icmp.extend(&request[..8]);
        let (parsed, _) = parse_v4(&icmp, false).unwrap();
        assert_eq!(
            parsed,
            Parsed::Error {
                kind: ErrorKind::Unreachable,
                icmp_type: 3,
                code: 1,
                quoted: Quoted { dest: Some("198.51.100.7".parse().unwrap()), id: Some(9), seq16: Some(4464), token: None },
            }
        );
    }

    #[test]
    fn time_exceeded_v6_with_full_quote_recovers_the_token() {
        let request = build_echo(Family::V6, 9, TOKEN, 56);
        let mut icmp = vec![V6_TIME_EXCEEDED, 0, 0, 0, 0, 0, 0, 0];
        let mut ip6 = vec![0u8; 40];
        ip6[0] = 0x60;
        ip6[6] = 58;
        ip6[24..40].copy_from_slice(&"2001:db8::7".parse::<Ipv6Addr>().unwrap().octets());
        icmp.extend(ip6);
        icmp.extend(&request);
        match parse_v6(&icmp).unwrap() {
            Parsed::Error { kind: ErrorKind::TimeExceeded, quoted, .. } => {
                assert_eq!(quoted.dest, Some("2001:db8::7".parse().unwrap()));
                assert_eq!(quoted.token, Some(TOKEN));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn foreign_and_short_packets_do_not_parse_as_ours() {
        assert_eq!(parse_v4(&[0, 0, 0], false), None);
        let (p, _) = parse_v4(&[0, 0, 0, 0, 0, 1, 0, 2, b'a', b'b'], false).unwrap();
        assert!(matches!(p, Parsed::EchoReply { token: None, .. }), "someone else's reply carries no token");
        assert_eq!(parse_v4(&[8, 0, 0, 0, 0, 0, 0, 0], false).unwrap().0, Parsed::Other { icmp_type: 8 });
    }
}

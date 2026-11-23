//! Explicit DNS transactions over UDP and TCP.
//!
//! These are real DNS exchanges this application builds, sends to a chosen
//! address, and parses. The OS resolver is never timed. `hickory-proto` is
//! used only to encode and decode messages; every socket and every timestamp
//! is ours. See `planning/01-design-spec.md` §6.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::time::Duration;

use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::opt::{EdnsCode, EdnsOption};
use hickory_proto::rr::{Name, RData, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, UdpSocket};

use super::{Counters, local_error, us};
use crate::clock::{Clock, utc_string};
use crate::config::QueryCfg;
use crate::model::{DnsInfo, ErrorCategory, ErrorInfo, Family, Outcome, Phases};
use crate::traffic::{Kind, UDP_HEADER, ip_header};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// `NOERROR`; answers optional.
    NoError,
    /// `NOERROR` with at least one record of the requested type.
    Answer,
    NxDomain,
    /// The resolver's filtering policy is expected to block this name.
    Blocked,
    Any,
}

impl Expect {
    pub fn as_str(self) -> &'static str {
        match self {
            Expect::NoError => "noerror",
            Expect::Answer => "answer",
            Expect::NxDomain => "nxdomain",
            Expect::Blocked => "blocked",
            Expect::Any => "any",
        }
    }
}

/// One configured query scenario.
#[derive(Debug, Clone)]
pub struct QuerySpec {
    pub name: Name,
    pub qtype: RecordType,
    pub expect: Expect,
    pub name_str: String,
    pub type_str: String,
}

impl QuerySpec {
    pub fn from_cfg(q: &QueryCfg) -> Result<Self, String> {
        let name = Name::from_ascii(&q.name).map_err(|e| format!("invalid query name {:?}: {e}", q.name))?;
        let qtype: RecordType = q.qtype.parse().map_err(|_| format!("unsupported record type {:?}", q.qtype))?;
        let expect = match q.expect.as_str() {
            "noerror" => Expect::NoError,
            "answer" => Expect::Answer,
            "nxdomain" => Expect::NxDomain,
            "blocked" => Expect::Blocked,
            "any" => Expect::Any,
            other => return Err(format!("unknown expectation {other:?}")),
        };
        Ok(Self { name, qtype, expect, name_str: q.name.clone(), type_str: q.qtype.clone() })
    }

    pub fn info(&self, id: u16) -> DnsInfo {
        DnsInfo {
            qname: self.name_str.clone(),
            qtype: self.type_str.clone(),
            id,
            expect: self.expect.as_str().to_string(),
            rcode: None,
            tc: None,
            answers: None,
            size: None,
            min_ttl: None,
            ede: Vec::new(),
            matched: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DnsParams {
    pub edns_udp_size: u16,
    pub recursion_desired: bool,
    pub tcp_fallback: bool,
}

pub fn random_id() -> u16 {
    let mut b = [0u8; 2];
    getrandom::fill(&mut b).expect("the OS random source is available");
    u16::from_ne_bytes(b)
}

/// Builds a query: one question, RD as configured, EDNS(0) with the configured
/// UDP payload size, DO clear, no Client Subnet, no cookie.
pub fn build_query(spec: &QuerySpec, id: u16, p: DnsParams) -> Result<Vec<u8>, String> {
    let mut m = Message::new(id, MessageType::Query, OpCode::Query);
    m.metadata.recursion_desired = p.recursion_desired;
    m.add_query(Query::query(spec.name.clone(), spec.qtype));
    let mut edns = Edns::new();
    edns.set_max_payload(p.edns_udp_size);
    edns.set_version(0);
    m.set_edns(edns);
    m.to_vec().map_err(|e| format!("cannot encode query: {e}"))
}

/// A well-formed response that matches our query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub rcode: ResponseCode,
    pub tc: bool,
    pub answers: u16,
    pub size: u32,
    pub min_ttl: Option<u32>,
    pub ede: Vec<u16>,
    pub has_requested_type: bool,
    /// An answer of `0.0.0.0` / `::`, or an Extended DNS Error of Blocked,
    /// Censored, or Filtered.
    pub looks_blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    Malformed(String),
    WrongId,
    NotAResponse,
    WrongQuestion,
}

impl Invalid {
    pub fn error(&self) -> ErrorInfo {
        match self {
            Invalid::Malformed(d) => ErrorInfo::new(ErrorCategory::Malformed).detail(d.clone()),
            Invalid::WrongId => ErrorInfo::new(ErrorCategory::Mismatch).detail("response id does not match the query"),
            Invalid::NotAResponse => ErrorInfo::new(ErrorCategory::Mismatch).detail("message is not a response"),
            Invalid::WrongQuestion => ErrorInfo::new(ErrorCategory::Mismatch).detail("response question does not match the query"),
        }
    }
}

pub const EDE_OPTION: u16 = 15;
const EDE_BLOCKED: [u16; 3] = [15, 16, 17];
pub const MAX_DNS_MESSAGE: usize = 65_535;

/// Validates a response against the query it should answer.
pub fn parse_response(bytes: &[u8], id: u16, spec: &QuerySpec) -> Result<Response, Invalid> {
    let m = Message::from_vec(bytes).map_err(|e| Invalid::Malformed(e.to_string()))?;
    if m.metadata.message_type != MessageType::Response {
        return Err(Invalid::NotAResponse);
    }
    if m.metadata.id != id {
        return Err(Invalid::WrongId);
    }
    if m.queries.len() != 1 || m.queries[0].name != spec.name || m.queries[0].query_type != spec.qtype {
        return Err(Invalid::WrongQuestion);
    }
    let mut ede = Vec::new();
    if let Some(edns) = &m.edns {
        for (code, opt) in edns.options().as_ref() {
            if u16::from(*code) == EDE_OPTION
                && let EdnsOption::Unknown(_, data) = opt
                && data.len() >= 2
            {
                ede.push(u16::from_be_bytes([data[0], data[1]]));
            }
        }
        let _ = EdnsCode::Unknown(EDE_OPTION);
    }
    let unspecified = m.answers.iter().any(|r| match &r.data {
        RData::A(a) => a.0 == Ipv4Addr::UNSPECIFIED,
        RData::AAAA(a) => a.0 == Ipv6Addr::UNSPECIFIED,
        _ => false,
    });
    Ok(Response {
        rcode: m.metadata.response_code,
        tc: m.metadata.truncation,
        answers: m.answers.len() as u16,
        size: bytes.len() as u32,
        min_ttl: m.answers.iter().map(|r| r.ttl).min(),
        has_requested_type: m.answers.iter().any(|r| r.record_type() == spec.qtype),
        looks_blocked: unspecified || ede.iter().any(|c| EDE_BLOCKED.contains(c)),
        ede,
    })
}

pub fn rcode_name(rc: ResponseCode) -> String {
    match rc {
        ResponseCode::NoError => "NOERROR".into(),
        ResponseCode::FormErr => "FORMERR".into(),
        ResponseCode::ServFail => "SERVFAIL".into(),
        ResponseCode::NXDomain => "NXDOMAIN".into(),
        ResponseCode::NotImp => "NOTIMP".into(),
        ResponseCode::Refused => "REFUSED".into(),
        other => format!("RCODE{}", u16::from(other)),
    }
}

/// Decides the outcome of a valid response against the scenario's expectation.
/// An error RCODE, an empty answer, or a blocked answer is an *error response*
/// — never a timeout, and never loss.
pub fn classify(r: &Response, expect: Expect) -> (Outcome, Option<ErrorInfo>) {
    let met = match expect {
        Expect::Any => true,
        Expect::Blocked => r.looks_blocked,
        Expect::NxDomain => r.rcode == ResponseCode::NXDomain,
        Expect::NoError => r.rcode == ResponseCode::NoError && !r.looks_blocked,
        Expect::Answer => r.rcode == ResponseCode::NoError && r.has_requested_type && !r.looks_blocked,
    };
    if met {
        return (Outcome::Success, None);
    }
    let error = if r.looks_blocked {
        ErrorInfo::new(ErrorCategory::DnsBlocked).detail("answer looks policy-blocked and the scenario does not expect that")
    } else if r.rcode != ResponseCode::NoError {
        ErrorInfo::new(ErrorCategory::DnsRcode).code(i64::from(u16::from(r.rcode))).detail(rcode_name(r.rcode))
    } else {
        ErrorInfo::new(ErrorCategory::DnsUnexpected).detail(format!("NOERROR with {} answer(s); the scenario expects {}", r.answers, expect.as_str()))
    };
    (Outcome::ServiceError, Some(error))
}

fn fill_info(info: &mut DnsInfo, r: &Response) {
    info.rcode = Some(rcode_name(r.rcode));
    info.tc = Some(r.tc);
    info.answers = Some(r.answers);
    info.size = Some(r.size);
    info.min_ttl = r.min_ttl;
    info.ede = r.ede.clone();
}

/// The result of one attempt on one transport.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub sent_ns: Option<u64>,
    pub sent_utc: Option<String>,
    pub recv_ns: Option<u64>,
    pub outcome: Outcome,
    pub error: Option<ErrorInfo>,
    /// Present whenever a valid response arrived, including error responses.
    pub rtt_us: Option<u32>,
    pub info: DnsInfo,
    /// The response had TC set: a valid UDP exchange that needs a TCP retry.
    pub truncated: bool,
    pub phase: Option<Phases>,
}

impl Exchange {
    pub fn unsent(info: DnsInfo, error: ErrorInfo) -> Self {
        Self { sent_ns: None, sent_utc: None, recv_ns: None, outcome: Outcome::LocalError, error: Some(error), rtt_us: None, info, truncated: false, phase: None }
    }

    /// Applies a validated response.
    pub fn finish(&mut self, r: &Response, expect: Expect, recv_ns: u64, query_sent_ns: u64) {
        fill_info(&mut self.info, r);
        self.recv_ns = Some(recv_ns);
        self.rtt_us = Some(us(recv_ns.saturating_sub(query_sent_ns)));
        if r.tc {
            self.truncated = true;
            self.outcome = Outcome::Success;
            return;
        }
        let (outcome, error) = classify(r, expect);
        self.info.matched = Some(outcome == Outcome::Success);
        self.outcome = outcome;
        self.error = error;
    }
}

fn unspecified(dest: &SocketAddr) -> SocketAddr {
    match dest.ip() {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    }
}

/// One DNS transaction over UDP: a fresh connected socket (so the source port
/// is new and an ICMP port-unreachable becomes an explicit outcome), one
/// datagram out, and a wait for a matching response until the deadline.
pub async fn udp_exchange(clock: &dyn Clock, dest: SocketAddr, spec: &QuerySpec, p: DnsParams, deadline: Duration, counters: &Counters) -> Exchange {
    let id = random_id();
    let info = spec.info(id);
    let query = match build_query(spec, id, p) {
        Ok(q) => q,
        Err(e) => return Exchange::unsent(info, ErrorInfo::new(ErrorCategory::Io).detail(e)),
    };
    let sock = match UdpSocket::bind(unspecified(&dest)).await {
        Ok(s) => s,
        Err(e) => return Exchange::unsent(info, local_error(&e)),
    };
    if let Err(e) = sock.connect(dest).await {
        return Exchange::unsent(info, local_error(&e));
    }
    let sent_utc = utc_string(clock.wall());
    let sent_ns = clock.mono_ns();
    if let Err(e) = sock.send(&query).await {
        return Exchange::unsent(info, local_error(&e));
    }
    let family = Family::of(&dest.ip());
    counters.traffic.transaction(Kind::DnsUdp, family);
    counters.traffic.sent(Kind::DnsUdp, family, 1, query.len() as u64 + UDP_HEADER + ip_header(family));
    let mut ex =
        Exchange { sent_ns: Some(sent_ns), sent_utc: Some(sent_utc), recv_ns: None, outcome: Outcome::Timeout, error: None, rtt_us: None, info, truncated: false, phase: None };
    let until = clock.instant_at(sent_ns + deadline.as_nanos() as u64);
    let mut buf = vec![0u8; 4096.max(usize::from(p.edns_udp_size))];
    let mut invalid: Option<Invalid> = None;
    loop {
        match tokio::time::timeout_at(until, sock.recv(&mut buf)).await {
            Err(_elapsed) => {
                // Bytes came back but none was a valid answer to our question:
                // a protocol error, not silence.
                if let Some(inv) = invalid {
                    ex.outcome = Outcome::ProtocolError;
                    ex.error = Some(inv.error());
                }
                return ex;
            }
            Ok(Err(e)) => {
                let refused = e.kind() == std::io::ErrorKind::ConnectionRefused;
                let unreachable = matches!(e.raw_os_error(), Some(libc::EHOSTUNREACH | libc::ENETUNREACH));
                if refused || unreachable {
                    ex.outcome = Outcome::Unreachable;
                    ex.recv_ns = Some(clock.mono_ns());
                    let cat = if refused { ErrorCategory::ConnectionRefused } else { ErrorCategory::IcmpUnreachable };
                    ex.error = Some(ErrorInfo::new(cat).detail(e.to_string()));
                } else {
                    ex.outcome = Outcome::LocalError;
                    ex.error = Some(local_error(&e));
                }
                return ex;
            }
            Ok(Ok(n)) => {
                let recv_ns = clock.mono_ns();
                counters.traffic.received(Kind::DnsUdp, family, 1, n as u64 + UDP_HEADER + ip_header(family));
                match parse_response(&buf[..n], id, spec) {
                    Ok(r) => {
                        ex.finish(&r, spec.expect, recv_ns, sent_ns);
                        return ex;
                    }
                    Err(inv) => {
                        // Keep waiting: the real answer may still arrive.
                        counters.unmatched_packets.fetch_add(1, Ordering::Relaxed);
                        invalid = Some(inv);
                    }
                }
            }
        }
    }
}

/// One DNS transaction over a new TCP connection (two-byte length framing),
/// closed afterwards. Connection setup is timed separately from the query:
/// the round-trip time returned is the query's alone, and `sent_ns` is when
/// the connection was started, so a caller that wants the whole transaction
/// has both ends of it.
pub async fn tcp_exchange(clock: &dyn Clock, dest: SocketAddr, spec: &QuerySpec, p: DnsParams, deadline: Duration, counters: &Counters) -> Exchange {
    let id = random_id();
    let info = spec.info(id);
    let query = match build_query(spec, id, p) {
        Ok(q) => q,
        Err(e) => return Exchange::unsent(info, ErrorInfo::new(ErrorCategory::Io).detail(e)),
    };
    let sock = match if dest.is_ipv4() { TcpSocket::new_v4() } else { TcpSocket::new_v6() } {
        Ok(s) => s,
        Err(e) => return Exchange::unsent(info, local_error(&e)),
    };
    let family = Family::of(&dest.ip());
    let sent_utc = utc_string(clock.wall());
    let start_ns = clock.mono_ns();
    let until = clock.instant_at(start_ns + deadline.as_nanos() as u64);
    let mut ex =
        Exchange { sent_ns: Some(start_ns), sent_utc: Some(sent_utc), recv_ns: None, outcome: Outcome::Timeout, error: None, rtt_us: None, info, truncated: false, phase: None };

    let mut stream = match tokio::time::timeout_at(until, sock.connect(dest)).await {
        // Unanswered, and said to be so at this stage: no connection phase
        // is recorded, which is how it differs from a query left unanswered.
        Err(_) => {
            ex.error = Some(ErrorInfo::new(ErrorCategory::Connect).detail("TCP connect did not complete before the deadline"));
            return ex;
        }
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            ex.outcome = Outcome::Unreachable;
            ex.error = Some(ErrorInfo::new(ErrorCategory::ConnectionRefused).detail(e.to_string()));
            return ex;
        }
        Ok(Err(e)) => {
            ex.sent_ns = None;
            ex.outcome = Outcome::LocalError;
            ex.error = Some(local_error(&e));
            return ex;
        }
        Ok(Ok(s)) => s,
    };
    let connected_ns = clock.mono_ns();
    let mut phase = Phases { connect_us: Some(us(connected_ns - start_ns)), ..Phases::default() };
    ex.phase = Some(phase);

    let mut framed = Vec::with_capacity(query.len() + 2);
    framed.extend_from_slice(&(query.len() as u16).to_be_bytes());
    framed.extend_from_slice(&query);
    let io = async {
        let q_ns = clock.mono_ns();
        stream.write_all(&framed).await?;
        counters.traffic.transaction(Kind::DnsTcp, family);
        counters.traffic.sent(Kind::DnsTcp, family, 0, framed.len() as u64);
        let mut len = [0u8; 2];
        stream.read_exact(&mut len).await?;
        let mut body = vec![0u8; usize::from(u16::from_be_bytes(len))];
        stream.read_exact(&mut body).await?;
        counters.traffic.received(Kind::DnsTcp, family, 0, 2 + body.len() as u64);
        Ok::<_, std::io::Error>((q_ns, clock.mono_ns(), body))
    };
    match tokio::time::timeout_at(until, io).await {
        Err(_) => {}
        Ok(Err(e)) => {
            ex.outcome = Outcome::ProtocolError;
            ex.error = Some(ErrorInfo::new(ErrorCategory::Protocol).detail(format!("connection ended before a complete response: {e}")));
        }
        Ok(Ok((q_ns, recv_ns, body))) => match parse_response(&body, id, spec) {
            Ok(r) => {
                ex.finish(&r, spec.expect, recv_ns, q_ns);
                if ex.truncated {
                    // Marked truncated on the transport that has no size
                    // limit to truncate for. There is nothing to fall back
                    // to, so the answer is judged as it stands.
                    let (outcome, error) = classify(&r, spec.expect);
                    ex.info.matched = Some(outcome == Outcome::Success);
                    ex.outcome = outcome;
                    ex.error = error;
                }
                phase.request_us = ex.rtt_us;
                ex.phase = Some(phase);
            }
            Err(inv) => {
                ex.recv_ns = Some(recv_ns);
                ex.outcome = Outcome::ProtocolError;
                ex.error = Some(inv.error());
            }
        },
    }
    ex
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::rr::rdata::{A, AAAA};
    use hickory_proto::rr::{Record, rdata::opt::OPT};

    fn spec(name: &str, qtype: &str, expect: &str) -> QuerySpec {
        QuerySpec::from_cfg(&QueryCfg { name: name.into(), qtype: qtype.into(), expect: expect.into() }).unwrap()
    }

    const P: DnsParams = DnsParams { edns_udp_size: 1232, recursion_desired: true, tcp_fallback: true };

    fn response(spec: &QuerySpec, id: u16, rcode: ResponseCode, answers: Vec<RData>) -> Message {
        let mut m = Message::new(id, MessageType::Response, OpCode::Query);
        m.metadata.response_code = rcode;
        m.metadata.recursion_available = true;
        m.add_query(Query::query(spec.name.clone(), spec.qtype));
        for a in answers {
            m.add_answer(Record::from_rdata(spec.name.clone(), 300, a));
        }
        m
    }

    fn parse(m: &Message, id: u16, s: &QuerySpec) -> Result<Response, Invalid> {
        parse_response(&m.to_vec().unwrap(), id, s)
    }

    #[test]
    fn query_has_the_documented_shape() {
        let s = spec("example.com.", "A", "answer");
        let bytes = build_query(&s, 0x9c41, P).unwrap();
        let m = Message::from_vec(&bytes).unwrap();
        assert_eq!(m.metadata.id, 0x9c41);
        assert!(m.metadata.recursion_desired && !m.metadata.checking_disabled && !m.metadata.authentic_data);
        assert_eq!(m.queries.len(), 1);
        let edns = m.edns.as_ref().expect("EDNS(0) present");
        assert_eq!(edns.max_payload(), 1232);
        assert!(!edns.flags().dnssec_ok, "DO is clear");
        assert!(edns.options().as_ref().is_empty(), "no Client Subnet, cookie, or padding");
        assert!(bytes.len() < 60, "a small query: {} bytes", bytes.len());
        // The root is a valid name.
        assert!(build_query(&spec(".", "NS", "noerror"), 1, P).is_ok());
    }

    #[test]
    fn every_rcode_class_is_an_error_response_not_a_timeout() {
        let s = spec("example.com.", "A", "answer");
        let a = RData::A(A("93.184.215.14".parse().unwrap()));

        let ok = parse(&response(&s, 7, ResponseCode::NoError, vec![a.clone()]), 7, &s).unwrap();
        assert_eq!(classify(&ok, s.expect), (Outcome::Success, None));

        for (rc, name) in [(ResponseCode::ServFail, "SERVFAIL"), (ResponseCode::Refused, "REFUSED"), (ResponseCode::NXDomain, "NXDOMAIN")] {
            let r = parse(&response(&s, 7, rc, vec![]), 7, &s).unwrap();
            let (outcome, err) = classify(&r, s.expect);
            assert_eq!(outcome, Outcome::ServiceError, "{name}");
            let err = err.unwrap();
            assert_eq!(err.category, ErrorCategory::DnsRcode);
            assert_eq!(err.detail.as_deref(), Some(name));
        }

        // NOERROR with no data when an answer was expected.
        let empty = parse(&response(&s, 7, ResponseCode::NoError, vec![]), 7, &s).unwrap();
        let (outcome, err) = classify(&empty, s.expect);
        assert_eq!((outcome, err.unwrap().category), (Outcome::ServiceError, ErrorCategory::DnsUnexpected));
        // ...but acceptable when the scenario only asks for NOERROR.
        assert_eq!(classify(&empty, Expect::NoError).0, Outcome::Success);
        // NXDOMAIN is success when that is what the scenario expects.
        let nx = parse(&response(&s, 7, ResponseCode::NXDomain, vec![]), 7, &s).unwrap();
        assert_eq!(classify(&nx, Expect::NxDomain).0, Outcome::Success);
    }

    #[test]
    fn blocked_answers_depend_on_the_scenario() {
        let s = spec("ads.example.", "A", "answer");
        let blocked = parse(&response(&s, 1, ResponseCode::NoError, vec![RData::A(A(Ipv4Addr::UNSPECIFIED))]), 1, &s).unwrap();
        assert!(blocked.looks_blocked);
        let (outcome, err) = classify(&blocked, Expect::Answer);
        assert_eq!((outcome, err.unwrap().category), (Outcome::ServiceError, ErrorCategory::DnsBlocked));
        assert_eq!(classify(&blocked, Expect::Blocked).0, Outcome::Success);

        let s6 = spec("ads.example.", "AAAA", "answer");
        let v6 = parse(&response(&s6, 1, ResponseCode::NoError, vec![RData::AAAA(AAAA(Ipv6Addr::UNSPECIFIED))]), 1, &s6).unwrap();
        assert!(v6.looks_blocked);

        // An Extended DNS Error of "Filtered" (17) also marks a response as blocked.
        let mut m = response(&s, 1, ResponseCode::NXDomain, vec![]);
        let mut edns = Edns::new();
        *edns.options_mut() = OPT::new(vec![(EdnsCode::Unknown(15), EdnsOption::Unknown(15, vec![0, 17, b'x']))]);
        m.set_edns(edns);
        let ede = parse(&m, 1, &s).unwrap();
        assert_eq!(ede.ede, vec![17]);
        assert!(ede.looks_blocked);
        assert_eq!(classify(&ede, Expect::Answer).1.unwrap().category, ErrorCategory::DnsBlocked);
    }

    #[test]
    fn mismatched_and_malformed_responses_are_rejected() {
        let s = spec("example.com.", "A", "answer");
        let good = response(&s, 7, ResponseCode::NoError, vec![]);
        assert_eq!(parse(&good, 8, &s), Err(Invalid::WrongId));
        let other = spec("example.org.", "A", "answer");
        assert_eq!(parse(&response(&other, 7, ResponseCode::NoError, vec![]), 7, &s), Err(Invalid::WrongQuestion));
        let aaaa = spec("example.com.", "AAAA", "answer");
        assert_eq!(parse(&response(&aaaa, 7, ResponseCode::NoError, vec![]), 7, &s), Err(Invalid::WrongQuestion));
        let query = build_query(&s, 7, P).unwrap();
        assert_eq!(parse_response(&query, 7, &s), Err(Invalid::NotAResponse));
        assert!(matches!(parse_response(&[1, 2, 3], 7, &s), Err(Invalid::Malformed(_))));
    }

    #[test]
    fn truncated_response_is_a_valid_exchange_flagged_for_fallback() {
        let s = spec("example.com.", "A", "answer");
        let mut m = response(&s, 7, ResponseCode::NoError, vec![]);
        m.metadata.truncation = true;
        let r = parse(&m, 7, &s).unwrap();
        let mut ex =
            Exchange { sent_ns: Some(0), sent_utc: None, recv_ns: None, outcome: Outcome::Timeout, error: None, rtt_us: None, info: s.info(7), truncated: false, phase: None };
        ex.finish(&r, s.expect, 21_300_000, 0);
        assert!(ex.truncated);
        assert_eq!(ex.outcome, Outcome::Success, "TC is not judged against the expectation");
        assert_eq!(ex.rtt_us, Some(21_300));
        assert_eq!(ex.info.matched, None);
    }
}

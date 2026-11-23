//! DNS over HTTPS (RFC 8484) and DNS over TLS (RFC 7858), with connection
//! phases timed separately. The two share everything up to the end of the
//! TLS handshake; after it one speaks HTTP/2 and the other sends DNS
//! messages with a two-byte length in front.
//!
//! We dial the chosen IP ourselves, hand the socket to `tokio-rustls` with the
//! configured hostname as server name (so SNI and certificate verification
//! stay correct), then drive HTTP/2 through hyper's connection-level API. That
//! is what makes TCP connect, TLS handshake, and request times separable, and
//! reuse exact. Certificate verification is never disabled.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::client::conn::http2::SendRequest;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::net::TcpSocket;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

use super::dns::{DnsParams, Exchange, QuerySpec, build_query, parse_response, random_id};
use super::{Counters, local_error, us};
use crate::clock::{Clock, utc_string};
use crate::model::{ErrorCategory, ErrorInfo, Family, Outcome, Phases};
use crate::traffic::{Counted, Kind};

pub const DNS_MESSAGE: &str = "application/dns-message";
const MAX_BODY: usize = 64 * 1024;

/// What is spoken once TLS is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// HTTP/2 requests carrying DNS messages (DoH).
    Https,
    /// DNS messages straight on the TLS stream (DoT).
    Tls,
}

impl Transport {
    fn kind(self) -> Kind {
        match self {
            Transport::Https => Kind::Doh,
            Transport::Tls => Kind::Dot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DohTarget {
    /// TLS server name and, for DoH, the HTTP authority.
    pub host: String,
    pub port: u16,
    /// The URL path, for DoH; unused for DoT.
    pub path: String,
    pub transport: Transport,
}

/// A client configuration that verifies against the operating system's trust
/// store and offers only HTTP/2.
pub fn default_tls() -> Result<Arc<ClientConfig>, String> {
    use rustls_platform_verifier::ConfigVerifierExt;
    let mut cfg = ClientConfig::with_platform_verifier().map_err(|e| format!("cannot initialise TLS verification: {e}"))?;
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::new(cfg))
}

/// The same verification, offering DNS over TLS instead of HTTP/2.
pub fn for_dot(cfg: &ClientConfig) -> Arc<ClientConfig> {
    let mut c = cfg.clone();
    c.alpn_protocols = vec![b"dot".to_vec()];
    Arc::new(c)
}

/// The same configuration with session resumption off, for fresh-connection
/// probes that must measure a full handshake.
pub fn without_resumption(cfg: &ClientConfig) -> Arc<ClientConfig> {
    let mut c = cfg.clone();
    c.resumption = rustls::client::Resumption::disabled();
    Arc::new(c)
}

type TlsStream = tokio_rustls::client::TlsStream<Counted<tokio::net::TcpStream>>;

enum Wire {
    H2 { sender: SendRequest<Full<Bytes>>, driver: JoinHandle<()> },
    Dot(Box<TlsStream>),
}

/// An established encrypted connection to one address.
pub struct DohConn {
    pub id: u64,
    pub peer: IpAddr,
    pub opened_ns: u64,
    pub alpn: Option<String>,
    pub tls: Option<String>,
    pub resumed: Option<bool>,
    pub requests: u64,
    wire: Wire,
}

impl DohConn {
    pub fn is_closed(&self) -> bool {
        match &self.wire {
            Wire::H2 { sender, driver } => sender.is_closed() || driver.is_finished(),
            // Between queries nothing should arrive on a DoT connection. If
            // the socket has anything to read — the server's close, or its
            // end of stream — the connection is finished. Looking does not
            // disturb a healthy one: there is nothing there to take.
            Wire::Dot(stream) => !matches!(stream.get_ref().0.get_ref().try_read(&mut [0u8; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        }
    }
}

impl Drop for DohConn {
    fn drop(&mut self) {
        if let Wire::H2 { driver, .. } = &self.wire {
            driver.abort();
        }
    }
}

/// Why a connection could not be established, with whatever phases completed.
#[derive(Debug, Clone)]
pub struct ConnectFailure {
    pub outcome: Outcome,
    pub error: ErrorInfo,
    pub phase: Phases,
    /// Whether anything left this host (false for purely local failures).
    pub sent: bool,
}

fn tls_error(e: &std::io::Error) -> (Outcome, ErrorInfo) {
    match e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
        Some(rustls::Error::InvalidCertificate(why)) => {
            (Outcome::ProtocolError, ErrorInfo::new(ErrorCategory::Certificate).detail(format!("certificate verification failed: {why:?}")))
        }
        Some(rustls::Error::AlertReceived(alert)) => (Outcome::ServiceError, ErrorInfo::new(ErrorCategory::TlsAlert).detail(format!("TLS alert: {alert:?}"))),
        Some(other) => (Outcome::ProtocolError, ErrorInfo::new(ErrorCategory::Protocol).detail(format!("TLS: {other}"))),
        None => (Outcome::ProtocolError, ErrorInfo::new(ErrorCategory::Protocol).detail(format!("TLS: {e}"))),
    }
}

/// Opens a connection: TCP connect, TLS handshake, HTTP/2 preface. Each phase
/// is timed in user space around the call that performs it.
pub async fn connect(
    clock: &dyn Clock,
    tls: &Arc<ClientConfig>,
    target: &DohTarget,
    ip: IpAddr,
    conn_id: u64,
    until_ns: u64,
    counters: &Arc<Counters>,
) -> Result<(DohConn, Phases), ConnectFailure> {
    let until = clock.instant_at(until_ns);
    let mut phase = Phases::default();
    let fail = |outcome, error, phase, sent| ConnectFailure { outcome, error, phase, sent };

    let server_name = ServerName::try_from(target.host.clone())
        .map_err(|e| fail(Outcome::LocalError, ErrorInfo::new(ErrorCategory::Io).detail(format!("invalid server name: {e}")), phase, false))?;
    let sock = if ip.is_ipv4() { TcpSocket::new_v4() } else { TcpSocket::new_v6() }.map_err(|e| fail(Outcome::LocalError, local_error(&e), phase, false))?;

    let t0 = clock.mono_ns();
    let tcp = match tokio::time::timeout_at(until, sock.connect(SocketAddr::new(ip, target.port))).await {
        Err(_) => return Err(fail(Outcome::Timeout, ErrorInfo::new(ErrorCategory::Connect).detail("TCP connect did not complete before the deadline"), phase, true)),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Err(fail(Outcome::Unreachable, ErrorInfo::new(ErrorCategory::ConnectionRefused).detail(e.to_string()), phase, true));
        }
        Ok(Err(e)) => return Err(fail(Outcome::LocalError, local_error(&e), phase, false)),
        Ok(Ok(s)) => s,
    };
    let _ = tcp.set_nodelay(true);
    // Count every byte the socket carries, TLS included.
    let family = Family::of(&ip);
    let tcp = Counted::new(tcp, counters.clone(), target.transport.kind(), family);
    let t1 = clock.mono_ns();
    phase.connect_us = Some(us(t1 - t0));

    // Note: certificate verification runs inside this call, so its cost is
    // part of the handshake time and cannot be separated out.
    let stream = match tokio::time::timeout_at(until, TlsConnector::from(tls.clone()).connect(server_name, tcp)).await {
        Err(_) => return Err(fail(Outcome::Timeout, ErrorInfo::new(ErrorCategory::Connect).detail("TLS handshake did not complete before the deadline"), phase, true)),
        Ok(Err(e)) => {
            let (outcome, error) = tls_error(&e);
            return Err(fail(outcome, error, phase, true));
        }
        Ok(Ok(s)) => s,
    };
    let t2 = clock.mono_ns();
    phase.tls_us = Some(us(t2 - t1));

    let (alpn, version, resumed) = {
        let (_, session) = stream.get_ref();
        (
            session.alpn_protocol().map(|a| String::from_utf8_lossy(a).into_owned()),
            session.protocol_version().map(|v| format!("{v:?}")),
            session.handshake_kind().map(|k| matches!(k, rustls::HandshakeKind::Resumed)),
        )
    };
    if target.transport == Transport::Tls {
        // A DoT server may select `dot` or nothing at all; anything else is
        // a different service on this port.
        if !matches!(alpn.as_deref(), None | Some("dot")) {
            return Err(fail(Outcome::ProtocolError, ErrorInfo::new(ErrorCategory::Protocol).detail(format!("server negotiated {alpn:?}, not DNS over TLS")), phase, true));
        }
        return Ok((DohConn { id: conn_id, peer: ip, opened_ns: t2, alpn, tls: version, resumed, requests: 0, wire: Wire::Dot(Box::new(stream)) }, phase));
    }
    if alpn.as_deref() != Some("h2") {
        return Err(fail(Outcome::ProtocolError, ErrorInfo::new(ErrorCategory::Protocol).detail(format!("server did not negotiate HTTP/2 (ALPN {alpn:?})")), phase, true));
    }

    let (sender, connection) = match tokio::time::timeout_at(until, hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))).await {
        Err(_) => return Err(fail(Outcome::Timeout, ErrorInfo::new(ErrorCategory::Connect).detail("HTTP/2 setup did not complete before the deadline"), phase, true)),
        Ok(Err(e)) => return Err(fail(Outcome::ProtocolError, ErrorInfo::new(ErrorCategory::Protocol).detail(format!("HTTP/2: {e}")), phase, true)),
        Ok(Ok(pair)) => pair,
    };
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((DohConn { id: conn_id, peer: ip, opened_ns: t2, alpn, tls: version, resumed, requests: 0, wire: Wire::H2 { sender, driver } }, phase))
}

/// The HTTP status of the last exchange, alongside the DNS result.
pub struct DohExchange {
    pub exchange: Exchange,
    pub http_status: Option<u16>,
    /// The connection can no longer be used and must be replaced.
    pub broken: bool,
}

/// One query on an established connection: POST with `application/dns-message`
/// and DNS ID 0, timed from the request start to the complete response body.
pub async fn request(clock: &dyn Clock, conn: &mut DohConn, target: &DohTarget, spec: &QuerySpec, p: DnsParams, until_ns: u64, counters: &Counters) -> DohExchange {
    if matches!(conn.wire, Wire::Dot(_)) {
        return dot_request(clock, conn, spec, p, until_ns, counters).await;
    }
    let Wire::H2 { sender, .. } = &mut conn.wire else { unreachable!("checked above") };
    let info = spec.info(0);
    let fail_local = |info, e: ErrorInfo| DohExchange { exchange: Exchange::unsent(info, e), http_status: None, broken: true };
    let query = match build_query(spec, 0, p) {
        Ok(q) => q,
        Err(e) => return fail_local(info, ErrorInfo::new(ErrorCategory::Io).detail(e)),
    };
    let authority = if target.port == 443 { target.host.clone() } else { format!("{}:{}", target.host, target.port) };
    let req = match http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("https://{authority}{}", target.path))
        .header(http::header::CONTENT_TYPE, DNS_MESSAGE)
        .header(http::header::ACCEPT, DNS_MESSAGE)
        .body(Full::new(Bytes::from(query)))
    {
        Ok(r) => r,
        Err(e) => return fail_local(info, ErrorInfo::new(ErrorCategory::Io).detail(format!("cannot build request: {e}"))),
    };
    let until = clock.instant_at(until_ns);
    if tokio::time::timeout_at(until, sender.ready()).await.is_err() || sender.is_closed() {
        return fail_local(info, ErrorInfo::new(ErrorCategory::Connect).detail("connection closed before the request could be sent"));
    }

    let sent_utc = utc_string(clock.wall());
    let sent_ns = clock.mono_ns();
    conn.requests += 1;
    counters.traffic.transaction(Kind::Doh, Family::of(&conn.peer));
    let mut ex =
        Exchange { sent_ns: Some(sent_ns), sent_utc: Some(sent_utc), recv_ns: None, outcome: Outcome::Timeout, error: None, rtt_us: None, info, truncated: false, phase: None };
    let mut phase = Phases::default();
    let mut status = None;
    let mut broken = false;

    let work = async {
        let resp = sender.send_request(req).await?;
        let ttfb_ns = clock.mono_ns();
        let code = resp.status();
        let body = Limited::new(resp.into_body(), MAX_BODY).collect().await.map_err(|e| format!("response body: {e}"));
        Ok::<_, hyper::Error>((ttfb_ns, code, body, clock.mono_ns()))
    };
    match tokio::time::timeout_at(until, work).await {
        Err(_) => broken = true, // a stream may be stuck; do not reuse the connection
        Ok(Err(e)) => {
            broken = true;
            ex.outcome = Outcome::ProtocolError;
            ex.error = Some(ErrorInfo::new(ErrorCategory::Protocol).detail(format!("HTTP/2: {e}")));
        }
        Ok(Ok((ttfb_ns, code, body, done_ns))) => {
            status = Some(code.as_u16());
            phase.ttfb_us = Some(us(ttfb_ns - sent_ns));
            phase.request_us = Some(us(done_ns - sent_ns));
            ex.recv_ns = Some(done_ns);
            match body {
                Err(e) => {
                    ex.outcome = Outcome::ProtocolError;
                    ex.error = Some(ErrorInfo::new(ErrorCategory::Protocol).detail(e));
                }
                Ok(_) if code != http::StatusCode::OK => {
                    // Per RFC 8484 a DNS response of any RCODE arrives with 200,
                    // so another status is the HTTP service reporting an error.
                    ex.outcome = Outcome::ServiceError;
                    ex.rtt_us = phase.request_us;
                    ex.error = Some(ErrorInfo::new(ErrorCategory::HttpStatus).code(i64::from(code.as_u16())).detail(format!("HTTP {}", code.as_u16())));
                }
                Ok(collected) => match parse_response(&collected.to_bytes(), 0, spec) {
                    Ok(r) => ex.finish(&r, spec.expect, done_ns, sent_ns),
                    Err(inv) => {
                        ex.outcome = Outcome::ProtocolError;
                        ex.error = Some(inv.error());
                    }
                },
            }
        }
    }
    ex.phase = Some(phase);
    DohExchange { exchange: ex, http_status: status, broken }
}

/// One query on an established DoT connection: the DNS message with its
/// two-byte length, then the response the same way. A random DNS ID, as on
/// UDP. Timed from the write to the last byte of the response.
async fn dot_request(clock: &dyn Clock, conn: &mut DohConn, spec: &QuerySpec, p: DnsParams, until_ns: u64, counters: &Counters) -> DohExchange {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let id = random_id();
    let info = spec.info(id);
    let fail_local = |info, e: ErrorInfo| DohExchange { exchange: Exchange::unsent(info, e), http_status: None, broken: true };
    let query = match build_query(spec, id, p) {
        Ok(q) => q,
        Err(e) => return fail_local(info, ErrorInfo::new(ErrorCategory::Io).detail(e)),
    };
    let Ok(len) = u16::try_from(query.len()) else { return fail_local(info, ErrorInfo::new(ErrorCategory::Io).detail("query too long for DNS over TLS")) };
    // Length and message in one write, so they leave in one TLS record.
    let mut frame = Vec::with_capacity(query.len() + 2);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&query);
    let peer = conn.peer;
    let Wire::Dot(stream) = &mut conn.wire else { unreachable!("called for DoT connections only") };

    let until = clock.instant_at(until_ns);
    let sent_utc = utc_string(clock.wall());
    let sent_ns = clock.mono_ns();
    conn.requests += 1;
    counters.traffic.transaction(Kind::Dot, Family::of(&peer));
    let mut ex =
        Exchange { sent_ns: Some(sent_ns), sent_utc: Some(sent_utc), recv_ns: None, outcome: Outcome::Timeout, error: None, rtt_us: None, info, truncated: false, phase: None };
    let mut phase = Phases::default();
    let mut broken = false;

    let work = async {
        stream.write_all(&frame).await?;
        stream.flush().await?;
        let mut len = [0u8; 2];
        stream.read_exact(&mut len).await?;
        let first_ns = clock.mono_ns();
        let mut body = vec![0u8; usize::from(u16::from_be_bytes(len))];
        stream.read_exact(&mut body).await?;
        Ok::<_, std::io::Error>((first_ns, body, clock.mono_ns()))
    };
    match tokio::time::timeout_at(until, work).await {
        // The response may still arrive later and would be read as the
        // answer to the next query: this connection cannot be used again.
        Err(_) => broken = true,
        Ok(Err(e)) => {
            broken = true;
            ex.outcome = Outcome::ProtocolError;
            ex.error = Some(ErrorInfo::new(ErrorCategory::Protocol).detail(format!("connection ended before a complete response: {e}")));
        }
        Ok(Ok((first_ns, body, done_ns))) => {
            phase.ttfb_us = Some(us(first_ns - sent_ns));
            phase.request_us = Some(us(done_ns - sent_ns));
            ex.recv_ns = Some(done_ns);
            match parse_response(&body, id, spec) {
                Ok(r) => ex.finish(&r, spec.expect, done_ns, sent_ns),
                Err(inv) => {
                    // Not the answer to what was asked: the stream is out of step.
                    broken = true;
                    ex.outcome = Outcome::ProtocolError;
                    ex.error = Some(inv.error());
                }
            }
        }
    }
    ex.phase = Some(phase);
    DohExchange { exchange: ex, http_status: None, broken }
}

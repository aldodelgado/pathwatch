//! Integration tests on real sockets, confined to this host: loopback ICMP and
//! local fixture servers for DNS over UDP/TCP and DNS over HTTPS. Nothing here
//! touches the Internet, and none of it is a statement about any provider.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{RData, Record};
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use pathwatch_core::clock::{Clock, SystemClock};
use pathwatch_core::collector::{self, IcmpSetup, Setup};
use pathwatch_core::config::Loaded;
use pathwatch_core::discovery::ResolveFn;
use pathwatch_core::model::{ErrorCategory, Family, Flag, Msg, Observation, Outcome, Protocol, Record as Rec};
use pathwatch_core::probe::icmp::RealIcmpIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ───────────────────────── the ICMP socket ─────────────────────────

/// On macOS every ICMP socket is handed a copy of every echo reply arriving
/// at the host, and the default receive buffer holds a few dozen (43 of
/// these 60, when measured). More than that waiting — ours here, but other
/// programs' pings and traces count just the same — used to leave no room
/// for our own answers, and each became a timeout the network never caused.
#[test]
fn a_burst_of_replies_left_unread_is_all_still_there_to_read() {
    use pathwatch_core::platform::icmp_sock::{IcmpSocket, V4_INCLUDES_IP_HEADER};
    use pathwatch_core::probe::icmp_packet::{self, Parsed, Token};
    const BURST: u64 = 60;
    // Where the host does not permit unprivileged ICMP there is no buffer to
    // fill. `PATHWATCH_REQUIRE_ICMP` says the host is known to permit it.
    let sock = match IcmpSocket::open(Family::V4) {
        Ok(sock) => sock,
        Err(e) => {
            let why = pathwatch_core::capability::explain_icmp_open_error(Family::V4, &e);
            assert!(std::env::var_os("PATHWATCH_REQUIRE_ICMP").is_none_or(|v| v.is_empty()), "PATHWATCH_REQUIRE_ICMP is set and {why}");
            eprintln!("not run: {why}");
            return;
        }
    };
    let nonce = 0x6275_7273 ^ std::process::id();
    for seq in 0..BURST {
        let packet = icmp_packet::build_echo(Family::V4, 0x7077, Token { nonce, stream: 9, seq }, 56);
        sock.send_to(IpAddr::V4(Ipv4Addr::LOCALHOST), &packet).expect("send to loopback");
    }
    // Nothing is read until every answer has had time to arrive.
    std::thread::sleep(Duration::from_millis(300));
    let mut answered = std::collections::BTreeSet::new();
    let mut buf = [0u8; 2048];
    loop {
        match sock.recv(&mut buf) {
            Ok(d) => {
                if let Some((Parsed::EchoReply { token: Some(t), .. }, _)) = icmp_packet::parse_v4(&buf[..d.len], V4_INCLUDES_IP_HEADER)
                    && t.nonce == nonce
                {
                    answered.insert(t.seq);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("reading the ICMP socket: {e}"),
        }
    }
    assert_eq!(answered.len() as u64, BURST, "answers still readable after the burst, with a receive buffer of {:?} bytes", sock.recv_buffer());
    assert!(sock.recv_buffer().unwrap() >= 64 * 1024, "{:?}", sock.recv_buffer());
}

// ───────────────────────────── fixtures ─────────────────────────────

/// How the fixture resolver answers, decided by the first label of the name.
fn answer(query: &[u8], over_udp: bool) -> Option<Vec<u8>> {
    let q = Message::from_vec(query).ok()?;
    let question = q.queries.first()?.clone();
    let label = question.name.to_string();
    let label = label.split('.').next().unwrap_or("").to_string();
    let mut r = Message::new(q.metadata.id, MessageType::Response, OpCode::Query);
    r.metadata.recursion_desired = q.metadata.recursion_desired;
    r.metadata.recursion_available = true;
    r.add_query(question.clone());
    let a = |ip: Ipv4Addr| Record::from_rdata(question.name.clone(), 60, RData::A(A(ip)));
    match label.as_str() {
        "ok" => {
            r.add_answer(a(Ipv4Addr::new(192, 0, 2, 53)));
        }
        "servfail" => r.metadata.response_code = ResponseCode::ServFail,
        "refused" => r.metadata.response_code = ResponseCode::Refused,
        "nx" => r.metadata.response_code = ResponseCode::NXDomain,
        "empty" => {}
        "blocked" => {
            r.add_answer(a(Ipv4Addr::UNSPECIFIED));
        }
        "tc" if over_udp => r.metadata.truncation = true,
        "tc" => {
            r.add_answer(a(Ipv4Addr::new(192, 0, 2, 54)));
        }
        // Truncated on every transport, with nothing in it.
        "alwaystc" => r.metadata.truncation = true,
        // Over TCP the fixture keeps a "silent" connection open and closes a "hangup" one.
        "silent" | "hangup" => return None,
        "wrongid" => r.metadata.id = q.metadata.id.wrapping_add(1),
        "garbage" => return Some(vec![0xde, 0xad, 0xbe]),
        _ => r.metadata.response_code = ResponseCode::Refused,
    }
    r.to_vec().ok()
}

/// A resolver fixture on 127.0.0.1 serving UDP and TCP on the same port.
async fn dns_fixture() -> u16 {
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = udp.local_addr().unwrap().port();
    let tcp = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((n, peer)) = udp.recv_from(&mut buf).await {
            if let Some(resp) = answer(&buf[..n], true) {
                let _ = udp.send_to(&resp, peer).await;
            }
        }
    });
    tokio::spawn(async move {
        while let Ok((mut s, _)) = tcp.accept().await {
            tokio::spawn(async move {
                let mut len = [0u8; 2];
                if s.read_exact(&mut len).await.is_err() {
                    return;
                }
                let mut body = vec![0u8; usize::from(u16::from_be_bytes(len))];
                if s.read_exact(&mut body).await.is_err() {
                    return;
                }
                match answer(&body, false) {
                    Some(resp) => {
                        let _ = s.write_all(&(resp.len() as u16).to_be_bytes()).await;
                        let _ = s.write_all(&resp).await;
                    }
                    // Connected and never answered, for longer than any deadline here.
                    None if Message::from_vec(&body).is_ok_and(|q| q.queries.first().is_some_and(|x| x.name.to_string().starts_with("silent."))) => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    // Closed without an answer.
                    None => {}
                }
            });
        }
    });
    port
}

/// A DoH fixture: HTTP/2 over TLS on 127.0.0.1 with a self-signed certificate
/// for `doh.test`. Returns its port and a client configuration that trusts it.
async fn doh_fixture() -> (u16, Arc<rustls::ClientConfig>) {
    let ck = rcgen::generate_simple_self_signed(vec!["doh.test".to_string()]).unwrap();
    let cert = ck.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(ck.signing_key.serialize_der()).unwrap();
    let mut server = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert.clone()], key).unwrap();
    server.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    let mut client = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    client.alpn_protocols = vec![b"h2".to_vec()];

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else { return };
                let service = service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                    let ok_type = req.headers().get(http::header::CONTENT_TYPE).is_some_and(|v| v == "application/dns-message");
                    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
                    let resp = match answer(&body, false) {
                        _ if !ok_type => hyper::Response::builder().status(415).body(Full::new(Bytes::new())),
                        // The fixture's "silent" name becomes an HTTP error over DoH.
                        None => hyper::Response::builder().status(500).body(Full::new(Bytes::new())),
                        Some(bytes) => hyper::Response::builder().status(200).header(http::header::CONTENT_TYPE, "application/dns-message").body(Full::new(Bytes::from(bytes))),
                    };
                    Ok::<_, std::convert::Infallible>(resp.unwrap())
                });
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(tls), service).await;
            });
        }
    });
    (port, Arc::new(client))
}

fn resolver_to_loopback() -> ResolveFn {
    Arc::new(|_host: String| Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) }))
}

// ───────────────────────────── harness ─────────────────────────────

struct Options {
    dns_port: u16,
    tls: Option<Arc<rustls::ClientConfig>>,
    resolver: Option<ResolveFn>,
    icmp: bool,
    /// The host network as the collector sees it; `None` is the real one.
    net: Option<Arc<pathwatch_core::testing::FakeNet>>,
}

async fn run(config: &str, opts: Options, wall: Duration) -> Vec<Msg> {
    let loaded = Loaded::from_layers(Some(config), None, &[]).unwrap_or_else(|e| panic!("test configuration: {e:?}"));
    let plan = loaded.plan(&|_| None);
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let (tx, mut rx) = mpsc::channel(8192);
    let cancel = CancellationToken::new();
    let setup = Setup {
        config: Arc::new(loaded.config.clone()),
        plan,
        session: "real".into(),
        config_rev: 1,
        clock: clock.clone(),
        tx,
        cancel: cancel.clone(),
        tls: opts.tls,
        resolver: opts.resolver,
        dns_port: opts.dns_port,
        pause: Default::default(),
        net: opts.net.clone().map(|n| n as Arc<dyn pathwatch_core::netwatch::NetProbe>),
        reload: None,
    };
    let (v4, v6) = if opts.icmp {
        (collector::open_icmp(Family::V4, clock.clone()), collector::open_icmp(Family::V6, clock))
    } else {
        (IcmpSetup::<RealIcmpIo>::Unavailable("not under test".into()), IcmpSetup::<RealIcmpIo>::Unavailable("not under test".into()))
    };
    let handle = tokio::spawn(collector::run(setup, v4, v6));
    tokio::time::sleep(wall).await;
    cancel.cancel();
    let mut msgs = Vec::new();
    while let Some(m) = rx.recv().await {
        msgs.push(m);
    }
    handle.await.unwrap();
    msgs
}

fn observations(msgs: &[Msg]) -> Vec<&Observation> {
    msgs.iter()
        .filter_map(|m| match m {
            Msg::Record(Rec::Observation(o)) => Some(&**o),
            _ => None,
        })
        .collect()
}

/// The traffic totals the collector reports when the session ends.
fn traffic(msgs: &[Msg]) -> Vec<pathwatch_core::model::TrafficRow> {
    msgs.iter()
        .find_map(|m| match m {
            Msg::Record(Rec::Session(s)) if s.state == "end" => Some(s.traffic.clone()),
            _ => None,
        })
        .expect("session end record")
}

fn by_name<'a>(obs: &[&'a Observation], name: &str, attempt: u8) -> &'a Observation {
    obs.iter()
        .find(|o| o.attempt == attempt && o.dns.as_ref().is_some_and(|d| d.qname.starts_with(name)) && o.outcome != Outcome::Cancelled)
        .copied()
        .unwrap_or_else(|| panic!("no finished observation for {name} attempt {attempt}"))
}

fn category(o: &Observation) -> ErrorCategory {
    o.error.as_ref().unwrap_or_else(|| panic!("{:?} has no error", o.dns)).category
}

const DNS_QUERIES: &str = r#"
[general]
families = ["ipv4"]
[icmp]
enabled = false
[dns.doh]
enabled = false
[dns]
interval = "300ms"
deadline = "200ms"
query = [
  { name = "ok.test.", type = "A", expect = "answer" },
  { name = "servfail.test.", type = "A", expect = "answer" },
  { name = "refused.test.", type = "A", expect = "answer" },
  { name = "nx.test.", type = "A", expect = "answer" },
  { name = "empty.test.", type = "A", expect = "answer" },
  { name = "blocked.test.", type = "A", expect = "answer" },
  { name = "tc.test.", type = "A", expect = "answer" },
  { name = "silent.test.", type = "A", expect = "answer" },
  { name = "wrongid.test.", type = "A", expect = "answer" },
  { name = "garbage.test.", type = "A", expect = "answer" },
]
[[providers.cloudflare.targets]]
id = "fixture"
address = "127.0.0.1"
protocols = ["dns-udp"]
[providers.google]
enabled = false
"#;

// ───────────────────────────── tests ─────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn dns_udp_classifies_every_kind_of_answer() {
    let port = dns_fixture().await;
    let msgs = run(DNS_QUERIES, Options { dns_port: port, tls: None, resolver: None, icmp: false, net: None }, Duration::from_millis(3_400)).await;
    let obs = observations(&msgs);

    let ok = by_name(&obs, "ok", 1);
    assert_eq!(ok.outcome, Outcome::Success);
    assert_eq!(ok.dest_port, Some(port));
    let d = ok.dns.as_ref().unwrap();
    assert_eq!((d.rcode.as_deref(), d.answers, d.matched, d.min_ttl), (Some("NOERROR"), Some(1), Some(true), Some(60)));
    let rtt = ok.rtt_us.expect("a success has a round-trip time");
    assert!(rtt > 0 && rtt < 100_000, "loopback DNS should take well under 100 ms, got {rtt} µs");

    // Error responses: an answer arrived, so none of these is a timeout.
    for (name, cat, rcode) in [
        ("servfail", ErrorCategory::DnsRcode, "SERVFAIL"),
        ("refused", ErrorCategory::DnsRcode, "REFUSED"),
        ("nx", ErrorCategory::DnsRcode, "NXDOMAIN"),
        ("empty", ErrorCategory::DnsUnexpected, "NOERROR"),
        ("blocked", ErrorCategory::DnsBlocked, "NOERROR"),
    ] {
        let o = by_name(&obs, name, 1);
        assert_eq!(o.outcome, Outcome::ServiceError, "{name}");
        assert_eq!(category(o), cat, "{name}");
        assert_eq!(o.dns.as_ref().unwrap().rcode.as_deref(), Some(rcode), "{name}");
        assert!(o.rtt_us.is_some(), "{name}: the response time of an error response is kept");
    }

    // Silence is the only timeout, and it has no round-trip time.
    let silent = by_name(&obs, "silent", 1);
    assert_eq!((silent.outcome, silent.rtt_us, silent.recv_ns), (Outcome::Timeout, None, None));

    // Bytes came back but were not a valid answer to our question.
    assert_eq!((by_name(&obs, "wrongid", 1).outcome, category(by_name(&obs, "wrongid", 1))), (Outcome::ProtocolError, ErrorCategory::Mismatch));
    assert_eq!((by_name(&obs, "garbage", 1).outcome, category(by_name(&obs, "garbage", 1))), (Outcome::ProtocolError, ErrorCategory::Malformed));

    // Truncation: the UDP attempt is a valid exchange; the TCP fallback is a
    // second, linked attempt with connection setup timed separately.
    let udp = by_name(&obs, "tc", 1);
    assert_eq!(udp.outcome, Outcome::Success);
    assert!(udp.has_flag(Flag::Truncated));
    assert_eq!(udp.protocol, Protocol::DnsUdp);
    let tcp = by_name(&obs, "tc", 2);
    assert_eq!((tcp.outcome, tcp.protocol, tcp.transaction), (Outcome::Success, Protocol::DnsTcp, udp.transaction));
    assert_eq!(tcp.dns.as_ref().unwrap().answers, Some(1));
    let phase = tcp.phase.expect("TCP attempt records phases");
    assert!(phase.connect_us.is_some() && phase.request_us.is_some());

    // Traffic accounting: every query that left is counted at its exact size.
    let rows = traffic(&msgs);
    let udp_row = rows.iter().find(|r| r.kind == "dns-udp" && r.family == Family::V4).expect("a DNS/UDP traffic row");
    let params = pathwatch_core::probe::dns::DnsParams { edns_udp_size: 1232, recursion_desired: true, tcp_fallback: true };
    let sent_queries: Vec<&&Observation> = obs.iter().filter(|o| o.attempt == 1 && o.sent_ns.is_some()).collect();
    let expected_bytes: u64 = sent_queries
        .iter()
        .map(|o| {
            let cfg = pathwatch_core::config::QueryCfg { name: o.dns.as_ref().unwrap().qname.clone(), qtype: "A".into(), expect: "answer".into() };
            let spec = pathwatch_core::probe::dns::QuerySpec::from_cfg(&cfg).unwrap();
            pathwatch_core::probe::dns::build_query(&spec, 0, params).unwrap().len() as u64 + 28
        })
        .sum();
    assert_eq!((udp_row.transactions, udp_row.packets_out, udp_row.bytes_out), (sent_queries.len() as u64, sent_queries.len() as u64, expected_bytes));
    assert!(udp_row.packets_in >= 8, "every answered query is counted inbound: {}", udp_row.packets_in);
    let tcp_row = rows.iter().find(|r| r.kind == "dns-tcp").expect("the TCP fallback is accounted separately");
    assert_eq!((tcp_row.transactions, tcp_row.packets_out, tcp_row.bytes.as_str()), (1, 0, "tcp-payload"));
    assert!(tcp_row.bytes_out > 30 && tcp_row.bytes_in > 30);

    // Every slot produced exactly one first-attempt record.
    let first_attempts = obs.iter().filter(|o| o.attempt == 1).count();
    let slots: std::collections::BTreeSet<u64> = obs.iter().map(|o| o.seq).collect();
    assert_eq!(first_attempts, slots.len());
}

const DNS_TCP: &str = r#"
[general]
families = ["ipv4"]
[icmp]
enabled = false
[dns.doh]
enabled = false
[dns]
interval = "300ms"
deadline = "200ms"
query = [
  { name = "ok.test.", type = "A", expect = "answer" },
  { name = "servfail.test.", type = "A", expect = "answer" },
  { name = "blocked.test.", type = "A", expect = "answer" },
  { name = "silent.test.", type = "A", expect = "answer" },
  { name = "hangup.test.", type = "A", expect = "answer" },
  { name = "alwaystc.test.", type = "A", expect = "answer" },
  { name = "wrongid.test.", type = "A", expect = "answer" },
  { name = "garbage.test.", type = "A", expect = "answer" },
]
[dns.tcp]
enabled = true
interval = "300ms"
[[providers.cloudflare.targets]]
id = "fixture"
address = "127.0.0.1"
protocols = ["dns-tcp"]
[providers.google]
enabled = false
"#;

/// DNS over TCP as a stream of its own: a connection per probe, the two
/// parts timed, and the total reported.
#[tokio::test(flavor = "current_thread")]
async fn dns_tcp_opens_a_connection_per_probe_and_reports_connect_and_query_together() {
    let port = dns_fixture().await;
    let msgs = run(DNS_TCP, Options { dns_port: port, tls: None, resolver: None, icmp: false, net: None }, Duration::from_millis(2_900)).await;
    let obs = observations(&msgs);
    assert!(obs.len() >= 8, "one probe for each of the eight queries: {}", obs.len());
    for o in &obs {
        assert_eq!((&*o.stream, o.protocol, o.attempt, o.dest_port), ("cloudflare/fixture/dns-tcp/4", Protocol::DnsTcp, 1, Some(port)));
        assert!(!o.has_flag(Flag::Fresh) && o.conn.is_none(), "not a TLS cohort: {:?}", o.flags);
    }
    let parts = |o: &Observation| {
        let p = o.phase.expect("phases are recorded");
        (p.connect_us.expect("connect time"), p.request_us.expect("query time"))
    };

    // An answer: the headline is the whole transaction, and both parts are kept.
    let ok = by_name(&obs, "ok", 1);
    assert_eq!(ok.outcome, Outcome::Success);
    let d = ok.dns.as_ref().unwrap();
    assert_eq!((d.rcode.as_deref(), d.answers, d.matched), (Some("NOERROR"), Some(1), Some(true)));
    let (connect, query) = parts(ok);
    let total = ok.rtt_us.unwrap();
    assert!(total >= connect + query, "total {total} covers connect {connect} + query {query}");
    assert!(total < 100_000 && total - (connect + query) < 20_000, "and little else on loopback: {total} vs {connect} + {query}");
    assert_eq!(u64::from(total), (ok.recv_ns.unwrap() - ok.sent_ns.unwrap()) / 1_000, "measured from the start of the connection");

    // Error responses are answers: classified, and timed the same way.
    for (name, cat) in [("servfail", ErrorCategory::DnsRcode), ("blocked", ErrorCategory::DnsBlocked)] {
        let o = by_name(&obs, name, 1);
        assert_eq!((o.outcome, category(o)), (Outcome::ServiceError, cat), "{name}");
        let (connect, query) = parts(o);
        assert!(o.rtt_us.unwrap() >= connect + query, "{name}");
    }

    // Connected, asked, and never answered: the only timeout. The connect
    // time is kept, which is what tells it from a connection never made.
    let silent = by_name(&obs, "silent", 1);
    assert_eq!((silent.outcome, silent.rtt_us, silent.recv_ns, silent.error.is_none()), (Outcome::Timeout, None, None, true));
    let p = silent.phase.expect("the connection was made");
    assert!(p.connect_us.is_some() && p.request_us.is_none());

    // The server closed the connection instead of answering: not silence.
    let hangup = by_name(&obs, "hangup", 1);
    assert_eq!((hangup.outcome, category(hangup), hangup.rtt_us), (Outcome::ProtocolError, ErrorCategory::Protocol, None));

    // Marked truncated over TCP: there is nowhere to fall back to, so the
    // empty answer is judged as what it is.
    let tc = by_name(&obs, "alwaystc", 1);
    assert!(tc.has_flag(Flag::Truncated));
    assert_eq!((tc.outcome, category(tc), tc.dns.as_ref().unwrap().matched), (Outcome::ServiceError, ErrorCategory::DnsUnexpected, Some(false)));
    assert!(!obs.iter().any(|o| o.attempt != 1), "and no second attempt is made");

    // Bytes that are not an answer to the question.
    assert_eq!((by_name(&obs, "wrongid", 1).outcome, category(by_name(&obs, "wrongid", 1))), (Outcome::ProtocolError, ErrorCategory::Mismatch));
    assert_eq!((by_name(&obs, "garbage", 1).outcome, category(by_name(&obs, "garbage", 1))), (Outcome::ProtocolError, ErrorCategory::Malformed));

    // One record per slot, and every query written is in the traffic count
    // under its own kind, as TCP payload.
    let slots: std::collections::BTreeSet<u64> = obs.iter().map(|o| o.seq).collect();
    assert_eq!(slots.len(), obs.len());
    let rows = traffic(&msgs);
    assert!(rows.iter().all(|r| r.kind == "dns-tcp"), "nothing went over UDP: {rows:?}");
    let row = rows.iter().find(|r| r.family == Family::V4).expect("a DNS/TCP traffic row");
    let written = obs.iter().filter(|o| o.phase.is_some_and(|p| p.connect_us.is_some())).count() as u64;
    assert_eq!((row.transactions, row.packets_out, row.bytes.as_str()), (written, 0, "tcp-payload"));
    assert!(row.bytes_out >= written * 30 && row.bytes_in > 30, "{row:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_refused_tcp_connection_is_unreachable_not_a_timeout() {
    // Bind and drop to find a port with nothing listening.
    let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let msgs = run(DNS_TCP, Options { dns_port: port, tls: None, resolver: None, icmp: false, net: None }, Duration::from_millis(700)).await;
    let obs = observations(&msgs);
    let finished: Vec<_> = obs.iter().filter(|o| o.outcome != Outcome::Cancelled).collect();
    assert!(!finished.is_empty());
    for o in finished {
        assert_eq!((o.outcome, category(o)), (Outcome::Unreachable, ErrorCategory::ConnectionRefused));
        // Something left this host and was answered with a refusal: it
        // was attempted, it has no round-trip time, and no query was written.
        assert!(o.sent_ns.is_some() && o.rtt_us.is_none() && o.phase.is_none(), "{o:?}");
    }
    assert!(traffic(&msgs).is_empty(), "no query was written, so none is counted");
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_port_is_unreachable_not_a_timeout() {
    // Bind and drop to find a port with nothing listening.
    let port = UdpSocket::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let msgs = run(DNS_QUERIES, Options { dns_port: port, tls: None, resolver: None, icmp: false, net: None }, Duration::from_millis(700)).await;
    let obs = observations(&msgs);
    let finished: Vec<_> = obs.iter().filter(|o| o.outcome != Outcome::Cancelled).collect();
    assert!(!finished.is_empty());
    for o in finished {
        assert_eq!(o.outcome, Outcome::Unreachable, "{:?}", o.error);
        assert_eq!(category(o), ErrorCategory::ConnectionRefused);
        assert_eq!(o.rtt_us, None);
    }
}

const ICMP_LOOPBACK: &str = r#"
[icmp]
interval = "200ms"
[[providers.cloudflare.targets]]
id = "lo4"
address = "127.0.0.1"
protocols = ["icmp"]
[[providers.google.targets]]
id = "lo6"
address = "::1"
protocols = ["icmp"]
"#;

#[tokio::test(flavor = "current_thread")]
async fn loopback_icmp_runs_unprivileged_on_both_families() {
    let msgs = run(ICMP_LOOPBACK, Options { dns_port: 53, tls: None, resolver: None, icmp: true, net: None }, Duration::from_millis(2_300)).await;
    let start = msgs
        .iter()
        .find_map(|m| match m {
            Msg::Record(Rec::Session(s)) if s.state == "start" => Some(s),
            _ => None,
        })
        .unwrap();
    let obs = observations(&msgs);
    for (stream, family) in [("cloudflare/lo4/icmp/4", Family::V4), ("google/lo6/icmp/6", Family::V6)] {
        let state = start.streams.iter().find(|s| &*s.stream == stream).unwrap();
        if state.state == "unsupported" {
            // A host that forbids unprivileged ICMP must say so and send nothing.
            eprintln!("skipping {stream}: {}", state.reason.as_deref().unwrap_or(""));
            assert!(obs.iter().all(|o| &*o.stream != stream));
            continue;
        }
        let mine: Vec<_> = obs.iter().filter(|o| &*o.stream == stream).collect();
        let sent = mine.iter().filter(|o| o.outcome.was_sent()).count();
        let local = mine.iter().filter(|o| o.outcome == Outcome::LocalError).count();
        if sent == 0 && local > 0 {
            eprintln!("skipping {stream}: no usable loopback address for this family here");
            continue;
        }
        let ok: Vec<_> = mine.iter().filter(|o| o.outcome == Outcome::Success).collect();
        assert!(ok.len() >= 5, "{stream}: {} of {} echo requests answered", ok.len(), sent);
        for o in &ok {
            let rtt = o.rtt_us.unwrap();
            assert!(rtt < 50_000, "{stream}: loopback RTT {rtt} µs");
            assert_eq!(o.family, family);
            let icmp = o.icmp.unwrap();
            assert_eq!(icmp.payload, 56);
            assert!(icmp.reply_ttl.is_some(), "{stream}: reply TTL is captured");
            // The round trip ends at the kernel's receive stamp; the wait for
            // this process and the user-space figure are recorded beside it.
            assert_eq!(&*o.backend.ts_source, "kernel-rx", "{stream}");
            let delay = o.recv_delay_us.unwrap_or_else(|| panic!("{stream}: kernel receive stamp is captured"));
            let user = o.rtt_user_us.unwrap_or_else(|| panic!("{stream}: user-space round trip is kept"));
            assert!(user >= rtt && user.abs_diff(rtt + delay) <= 1, "{stream}: user {user} µs = kernel {rtt} µs + delay {delay} µs");
            assert!(o.recv_ns.unwrap() > o.sent_ns.unwrap());
            assert!(o.sent_ns.unwrap() <= o.recv_ns.unwrap());
        }
        assert!(mine.iter().all(|o| o.outcome != Outcome::Timeout || o.rtt_us.is_none()));

        // Traffic accounting is exact for ICMP: one 64-byte message per probe
        // plus the IP header, out and back.
        let row = traffic(&msgs).into_iter().find(|r| r.kind == "icmp" && r.family == family).expect("an ICMP traffic row");
        let packet = if family == Family::V4 { 84 } else { 104 };
        assert_eq!(row.packets_out, sent as u64, "{stream}");
        assert_eq!((row.transactions, row.bytes_out), (row.packets_out, row.packets_out * packet), "{stream}");
        assert_eq!((row.packets_in, row.bytes_in), (ok.len() as u64, ok.len() as u64 * packet), "{stream}");
        assert_eq!(row.bytes, "ip-layer");
    }
}

fn doh_config(port: u16, host: &str) -> String {
    format!(
        r#"
[general]
families = ["ipv4"]
[icmp]
enabled = false
[dns.udp]
enabled = false
[dns]
interval = "300ms"
deadline = "250ms"
query = [
  {{ name = "ok.test.", type = "A", expect = "answer" }},
  {{ name = "servfail.test.", type = "A", expect = "answer" }},
  {{ name = "silent.test.", type = "A", expect = "answer" }},
  {{ name = "ok.test.", type = "A", expect = "answer" }},
]
[[providers.cloudflare.targets]]
id = "doh"
url = "https://{host}:{port}/dns-query"
protocols = ["doh"]
[providers.google]
enabled = false
"#
    )
}

#[tokio::test(flavor = "current_thread")]
async fn doh_times_phases_separately_and_tracks_connection_reuse() {
    let (port, tls) = doh_fixture().await;
    let msgs =
        run(&doh_config(port, "doh.test"), Options { dns_port: 53, tls: Some(tls), resolver: Some(resolver_to_loopback()), icmp: false, net: None }, Duration::from_millis(2_300))
            .await;
    let obs = observations(&msgs);
    let reused: Vec<_> = obs.iter().filter(|o| &*o.stream == "cloudflare/doh/doh/4" && o.outcome != Outcome::Cancelled).copied().collect();
    assert!(reused.len() >= 6, "{} observations", reused.len());

    // The destination is the address we dialled; the name was only the TLS server name.
    assert!(reused.iter().all(|o| o.dest_ip == Some(IpAddr::V4(Ipv4Addr::LOCALHOST)) && o.dest_port == Some(port)));
    assert!(reused.iter().all(|o| o.dns.as_ref().unwrap().id == 0), "DoH uses DNS ID 0");

    // First request on the connection: tagged, and excluded from reused statistics.
    let first = reused[0];
    assert_eq!(first.outcome, Outcome::Success);
    assert!(first.has_flag(Flag::AfterReconnect));
    let c = first.conn.as_ref().unwrap();
    assert_eq!((c.id, c.reused, c.alpn.as_deref(), c.http_status), (1, false, Some("h2"), Some(200)));
    assert!(c.tls.as_deref().is_some_and(|v| v.contains("TLSv1_3")), "{:?}", c.tls);

    // Later requests ride the same connection.
    let later = reused.iter().find(|o| o.seq >= 3 && o.outcome == Outcome::Success).unwrap();
    let c = later.conn.as_ref().unwrap();
    assert!(c.reused && c.id == 1 && !later.has_flag(Flag::AfterReconnect));
    let p = later.phase.unwrap();
    assert!(p.connect_us.is_none() && p.tls_us.is_none(), "no setup phases on a reused connection");
    assert!(p.ttfb_us.unwrap() <= p.request_us.unwrap());
    assert_eq!(later.rtt_us, p.request_us);

    // An error RCODE over DoH is an error response with HTTP 200.
    let servfail = reused.iter().find(|o| o.dns.as_ref().unwrap().qname.starts_with("servfail")).unwrap();
    assert_eq!((servfail.outcome, category(servfail), servfail.conn.as_ref().unwrap().http_status), (Outcome::ServiceError, ErrorCategory::DnsRcode, Some(200)));
    // An HTTP error status is a service error carrying the status.
    let http500 = reused.iter().find(|o| o.dns.as_ref().unwrap().qname.starts_with("silent")).unwrap();
    assert_eq!((http500.outcome, category(http500)), (Outcome::ServiceError, ErrorCategory::HttpStatus));
    assert_eq!(http500.error.as_ref().unwrap().code, Some(500));

    // The fresh cohort is a separate stream with every phase and a full handshake.
    let fresh = obs.iter().find(|o| &*o.stream == "cloudflare/doh/doh/4/fresh").expect("one fresh-connection probe at t=0");
    assert_eq!(fresh.outcome, Outcome::Success);
    assert!(fresh.has_flag(Flag::Fresh));
    let p = fresh.phase.unwrap();
    let (connect, tls_us, request) = (p.connect_us.unwrap(), p.tls_us.unwrap(), p.request_us.unwrap());
    assert!(fresh.rtt_us.unwrap() >= connect + tls_us + request, "the fresh total covers connect + handshake + query");
    assert_eq!(fresh.conn.as_ref().unwrap().resumed, Some(false));

    // Traffic accounting for DoH counts what the socket carried, TLS included.
    let row = traffic(&msgs).into_iter().find(|r| r.kind == "doh" && r.family == Family::V4).expect("a DoH traffic row");
    let requests = obs.iter().filter(|o| o.conn.is_some()).count() as u64;
    assert_eq!((row.transactions, row.packets_out, row.bytes.as_str()), (requests, 0, "tcp-payload"));
    assert!(row.bytes_out > 500 && row.bytes_in > 500, "two TLS handshakes and several requests: {} out, {} in", row.bytes_out, row.bytes_in);

    // Connection lifecycle is recorded with its phase timings.
    let opened = msgs
        .iter()
        .find_map(|m| match m {
            Msg::Record(Rec::Connection(c)) if c.state == "opened" && &*c.stream == "cloudflare/doh/doh/4" => Some(c),
            _ => None,
        })
        .unwrap();
    assert!(opened.phase.unwrap().connect_us.is_some() && opened.phase.unwrap().tls_us.is_some());
    // Bootstrap resolution is recorded, off the timing path.
    assert!(msgs.iter().any(|m| matches!(m, Msg::Record(Rec::Resolution(r)) if &*r.name == "doh.test" && r.addresses == vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])));
}

#[tokio::test(flavor = "current_thread")]
async fn doh_rejects_a_certificate_for_another_name() {
    let (port, tls) = doh_fixture().await;
    // Same server, but we ask for a name its certificate does not cover.
    let msgs =
        run(&doh_config(port, "wrong.test"), Options { dns_port: 53, tls: Some(tls), resolver: Some(resolver_to_loopback()), icmp: false, net: None }, Duration::from_millis(900))
            .await;
    let obs = observations(&msgs);
    let finished: Vec<_> = obs.iter().filter(|o| o.outcome != Outcome::Cancelled).collect();
    assert!(!finished.is_empty());
    for o in finished {
        assert_eq!(o.outcome, Outcome::ProtocolError, "{:?}", o.error);
        assert_eq!(category(o), ErrorCategory::Certificate);
        assert!(o.rtt_us.is_none());
        assert!(o.phase.unwrap().connect_us.is_some(), "TCP connected before verification failed");
        let dns = o.dns.as_ref().expect("the failure names the query its slot was for");
        assert!(!dns.qname.is_empty() && dns.rcode.is_none(), "{dns:?}");
    }
    let _ = SocketAddr::from(([127, 0, 0, 1], port));
}

#[tokio::test(flavor = "current_thread")]
async fn static_addresses_bypass_the_resolver_and_keep_the_server_name() {
    let (port, tls) = doh_fixture().await;
    // A resolver that would send the stream somewhere useless — as a filtering
    // resolver does when it answers with a block-page address. It must not be asked.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let hijacking: ResolveFn = Arc::new(move |_host: String| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]) })
    });
    let config = doh_config(port, "doh.test").replace("protocols = [\"doh\"]", "addresses = [\"127.0.0.1\"]\nprotocols = [\"doh\"]");
    let msgs = run(&config, Options { dns_port: 53, tls: Some(tls), resolver: Some(hijacking), icmp: false, net: None }, Duration::from_millis(1_300)).await;
    let obs = observations(&msgs);
    let ok: Vec<_> = obs.iter().filter(|o| &*o.stream == "cloudflare/doh/doh/4" && o.outcome == Outcome::Success).collect();
    assert!(ok.len() >= 2, "{} successes; first error: {:?}", ok.len(), obs.iter().find_map(|o| o.error.clone()));
    assert!(ok.iter().all(|o| o.dest_ip == Some(IpAddr::V4(Ipv4Addr::LOCALHOST)) && o.epoch == 1));
    // The certificate was verified for the hostname, although we dialled an address.
    assert_eq!(ok[0].conn.as_ref().unwrap().alpn.as_deref(), Some("h2"));
    assert!(ok[0].configured.ends_with("via 127.0.0.1"), "{}", ok[0].configured);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "the hostname was never resolved");
    assert!(!msgs.iter().any(|m| matches!(m, Msg::Record(Rec::Resolution(_)))));
}

#[tokio::test(flavor = "current_thread")]
async fn a_network_change_replaces_the_doh_connection_and_marks_the_first_request_on_the_new_one() {
    let (port, tls) = doh_fixture().await;
    // The fixture stays on loopback throughout; only what the collector is
    // told about the host's network changes, 1.3 s in. It looks every 2 s.
    let net = Arc::new(pathwatch_core::testing::FakeNet::default());
    let flip = net.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        flip.set("198.51.100.7", "en1", Some("198.51.100.1"));
    });
    let opts = Options { dns_port: 53, tls: Some(tls), resolver: Some(resolver_to_loopback()), icmp: false, net: Some(net) };
    let msgs = run(&doh_config(port, "doh.test"), opts, Duration::from_millis(3_400)).await;

    let connections: Vec<(u64, &str, Option<&str>)> = msgs
        .iter()
        .filter_map(|m| match m {
            Msg::Record(pathwatch_core::model::Record::Connection(c)) if &*c.stream == "cloudflare/doh/doh/4" => Some((c.conn, c.state.as_str(), c.reason.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(connections[..3], [(1, "opened", None), (1, "closed", Some("the network changed")), (2, "opened", None)], "{connections:?}");

    let obs = observations(&msgs);
    let stream: Vec<_> = obs.iter().filter(|o| &*o.stream == "cloudflare/doh/doh/4" && o.outcome == Outcome::Success).copied().collect();
    let on = |id: u64| stream.iter().filter(|o| o.conn.as_ref().is_some_and(|c| c.id == id)).copied().collect::<Vec<_>>();
    let (old, new) = (on(1), on(2));
    // (Only some of the fixture's queries succeed; the others are its error cases.)
    assert!(old.len() >= 2 && new.len() >= 2, "{} then {}", old.len(), new.len());
    // The first request on the new connection carries the cost of setting it
    // up, and is marked so that it stays out of reused-connection statistics.
    assert!(new[0].has_flag(Flag::AfterReconnect) && !new[0].conn.as_ref().unwrap().reused);
    assert!(new.iter().skip(1).all(|o| !o.has_flag(Flag::AfterReconnect)));
    assert!(old.iter().all(|o| o.src.as_ref().is_some_and(|s| s.ctx == 1)) && new.iter().all(|o| o.src.as_ref().is_some_and(|s| s.ctx == 2)));
    assert_eq!(msgs.iter().filter(|m| matches!(m, Msg::Record(pathwatch_core::model::Record::Gap(g)) if g.cause == pathwatch_core::model::GapCause::NetworkChange)).count(), 1);
}

// ───────────────────────── DNS over TLS ─────────────────────────

/// A DNS-over-TLS server on loopback with a certificate for `dot.test`. It
/// answers with the same fixture as the others, says nothing to the silent
/// name, and hangs up on a connection after its third answer, as a real
/// server does to an idle one.
async fn dot_fixture() -> (u16, Arc<rustls::ClientConfig>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ck = rcgen::generate_simple_self_signed(vec!["dot.test".to_string()]).unwrap();
    let cert = ck.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(ck.signing_key.serialize_der()).unwrap();
    let mut server = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert.clone()], key).unwrap();
    server.alpn_protocols = vec![b"dot".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).unwrap();
    // The collector is handed a configuration that offers HTTP/2, as its
    // default does: offering `dot` instead is the collector's own job.
    let mut client = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    client.alpn_protocols = vec![b"h2".to_vec()];

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                for _ in 0..3 {
                    let mut len = [0u8; 2];
                    if tls.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut query = vec![0u8; usize::from(u16::from_be_bytes(len))];
                    if tls.read_exact(&mut query).await.is_err() {
                        return;
                    }
                    let Some(bytes) = answer(&query, false) else { continue };
                    let mut frame = (bytes.len() as u16).to_be_bytes().to_vec();
                    frame.extend_from_slice(&bytes);
                    if tls.write_all(&frame).await.is_err() {
                        return;
                    }
                }
                let _ = tls.shutdown().await;
            });
        }
    });
    (port, Arc::new(client))
}

fn dot_config(port: u16, host: &str) -> String {
    format!(
        r#"
[general]
families = ["ipv4"]
[icmp]
enabled = false
[dns.udp]
enabled = false
[dns]
interval = "300ms"
deadline = "250ms"
query = [
  {{ name = "ok.test.", type = "A", expect = "answer" }},
  {{ name = "servfail.test.", type = "A", expect = "answer" }},
  {{ name = "ok.test.", type = "A", expect = "answer" }},
  {{ name = "ok.test.", type = "A", expect = "answer" }},
  {{ name = "silent.test.", type = "A", expect = "answer" }},
  {{ name = "ok.test.", type = "A", expect = "answer" }},
]
[[providers.cloudflare.targets]]
id = "dot"
hostname = "{host}"
port = {port}
protocols = ["dot"]
[providers.google]
enabled = false
"#
    )
}

#[tokio::test(flavor = "current_thread")]
async fn dot_queries_ride_one_tls_connection_and_a_closed_one_is_replaced() {
    const REUSED: &str = "cloudflare/dot/dot/4";
    let (port, tls) = dot_fixture().await;
    let msgs =
        run(&dot_config(port, "dot.test"), Options { dns_port: 53, tls: Some(tls), resolver: Some(resolver_to_loopback()), icmp: false, net: None }, Duration::from_millis(2_600))
            .await;
    let obs = observations(&msgs);
    let reused: Vec<_> = obs.iter().filter(|o| &*o.stream == REUSED && o.outcome != Outcome::Cancelled).copied().collect();
    assert!(reused.len() >= 7, "{} observations", reused.len());
    assert!(reused.iter().all(|o| o.protocol == Protocol::Dot && o.dest_ip == Some(IpAddr::V4(Ipv4Addr::LOCALHOST)) && o.dest_port == Some(port)));

    // The first query: a new connection, DNS over TLS negotiated, TLS 1.3, and a real DNS ID.
    let first = reused[0];
    assert_eq!(first.outcome, Outcome::Success);
    assert!(first.has_flag(Flag::AfterReconnect));
    let c = first.conn.as_ref().unwrap();
    assert_eq!((c.id, c.reused, c.alpn.as_deref(), c.http_status), (1, false, Some("dot"), None));
    assert!(c.tls.as_deref().is_some_and(|v| v.contains("TLSv1_3")), "{:?}", c.tls);
    let ids: std::collections::BTreeSet<u16> = reused.iter().map(|o| o.dns.as_ref().unwrap().id).collect();
    assert!(ids.len() >= reused.len() - 1 && !ids.contains(&0) || ids.len() >= 5, "a random ID per query, unlike DoH: {ids:?}");

    // An error RCODE is an error response, on the same connection.
    let servfail = reused[1];
    assert_eq!((servfail.outcome, category(servfail), servfail.conn.as_ref().unwrap().id), (Outcome::ServiceError, ErrorCategory::DnsRcode, 1));
    let third = reused[2];
    let c = third.conn.as_ref().unwrap();
    assert!(third.outcome == Outcome::Success && c.reused && c.id == 1 && !third.has_flag(Flag::AfterReconnect));
    let p = third.phase.unwrap();
    assert!(p.connect_us.is_none() && p.tls_us.is_none() && p.ttfb_us.unwrap() <= p.request_us.unwrap());
    assert_eq!(third.rtt_us, p.request_us);

    // The server hung up after three answers. That is seen before the next
    // query is sent, so it goes out on a new connection — not into a dead one.
    let fourth = reused[3];
    assert_eq!(fourth.outcome, Outcome::Success, "{:?}", fourth.error);
    assert!(fourth.conn.as_ref().unwrap().id == 2 && fourth.has_flag(Flag::AfterReconnect));
    let closed = msgs.iter().find_map(|m| match m {
        Msg::Record(Rec::Connection(c)) if c.state == "closed" && c.conn == 1 && &*c.stream == REUSED => c.reason.clone(),
        _ => None,
    });
    assert_eq!(closed.as_deref(), Some("closed by peer or error"));

    // No answer at all is a timeout, and the connection is not trusted again:
    // a late answer would be read as the reply to the next query.
    let silent = reused[4];
    assert_eq!(silent.outcome, Outcome::Timeout);
    assert!(silent.rtt_us.is_none());
    let after = reused[5];
    assert!(after.outcome == Outcome::Success && after.conn.as_ref().unwrap().id == 3 && after.has_flag(Flag::AfterReconnect), "{:?}", after.conn);

    // The fresh cohort: its own stream, every phase, a full handshake.
    let fresh = obs.iter().find(|o| &*o.stream == "cloudflare/dot/dot/4/fresh").expect("one fresh-connection probe at t=0");
    assert_eq!(fresh.outcome, Outcome::Success);
    assert!(fresh.has_flag(Flag::Fresh));
    let p = fresh.phase.unwrap();
    assert!(fresh.rtt_us.unwrap() >= p.connect_us.unwrap() + p.tls_us.unwrap() + p.request_us.unwrap());
    assert_eq!(fresh.conn.as_ref().unwrap().resumed, Some(false));

    // Its traffic is counted under its own kind.
    let row = traffic(&msgs).into_iter().find(|r| r.kind == "dot" && r.family == Family::V4).expect("a DoT traffic row");
    assert!(row.transactions >= 7 && row.bytes_out > 500 && row.bytes_in > 500 && row.bytes == "tcp-payload", "{row:?}");
    assert!(traffic(&msgs).iter().all(|r| r.kind != "doh"), "and nothing under DoH");
}

#[tokio::test(flavor = "current_thread")]
async fn dot_rejects_a_certificate_for_another_name() {
    let (port, tls) = dot_fixture().await;
    let msgs =
        run(&dot_config(port, "wrong.test"), Options { dns_port: 53, tls: Some(tls), resolver: Some(resolver_to_loopback()), icmp: false, net: None }, Duration::from_millis(900))
            .await;
    let obs = observations(&msgs);
    let finished: Vec<_> = obs.iter().filter(|o| o.outcome != Outcome::Cancelled).collect();
    assert!(!finished.is_empty());
    for o in finished {
        assert_eq!((o.outcome, category(o)), (Outcome::ProtocolError, ErrorCategory::Certificate), "{:?}", o.error);
        assert!(o.rtt_us.is_none() && o.dns.as_ref().is_some_and(|d| !d.qname.is_empty()));
    }
}

/// Leaving must not wait for an answer that is not coming. A query in
/// flight when the session ends is recorded as cancelled, at once.
#[tokio::test(flavor = "current_thread")]
async fn ending_a_session_does_not_wait_out_a_query_in_flight() {
    let port = dns_fixture().await;
    // Every query goes unanswered, with five seconds to wait for each.
    let config = DNS_QUERIES.replace("deadline = \"200ms\"", "deadline = \"5s\"").replace("interval = \"300ms\"", "interval = \"6s\"");
    let config = {
        let (head, tail) = config.split_once("query = [").expect("the query list");
        let rest = tail.split_once("]\n").expect("the end of the query list").1;
        format!("{head}query = [\n  {{ name = \"silent.test.\", type = \"A\", expect = \"answer\" }},\n]\n{rest}")
    };
    let started = std::time::Instant::now();
    let msgs = run(&config, Options { dns_port: port, tls: None, resolver: None, icmp: false, net: None }, Duration::from_millis(600)).await;
    let took = started.elapsed();
    assert!(took < Duration::from_millis(1_500), "the session took {took:?} to end with a query in flight");
    let obs = observations(&msgs);
    assert_eq!(obs.len(), 1, "one query was sent: {obs:?}");
    assert_eq!((obs[0].outcome, category(obs[0])), (Outcome::Cancelled, ErrorCategory::Shutdown), "in flight at the end: cancelled, not timed out");
    assert!(obs[0].sent_ns.is_some() && obs[0].rtt_us.is_none());
}

/// The same over TCP, where the connection is made and then left unanswered.
#[tokio::test(flavor = "current_thread")]
async fn ending_a_session_does_not_wait_out_a_tcp_query_either() {
    let port = dns_fixture().await;
    let config = DNS_TCP.replace("deadline = \"200ms\"", "deadline = \"5s\"").replace("interval = \"300ms\"", "interval = \"6s\"");
    let config = {
        let (head, tail) = config.split_once("query = [").expect("the query list");
        let rest = tail.split_once("]\n").expect("the end of the query list").1;
        format!("{head}query = [\n  {{ name = \"silent.test.\", type = \"A\", expect = \"answer\" }},\n]\n{rest}")
    };
    let started = std::time::Instant::now();
    let msgs = run(&config, Options { dns_port: port, tls: None, resolver: None, icmp: false, net: None }, Duration::from_millis(600)).await;
    let took = started.elapsed();
    assert!(took < Duration::from_millis(1_500), "the session took {took:?} to end with a query in flight");
    let obs = observations(&msgs);
    assert_eq!(obs.len(), 1, "one query was sent: {obs:?}");
    assert_eq!((obs[0].outcome, category(obs[0]), obs[0].protocol), (Outcome::Cancelled, ErrorCategory::Shutdown, Protocol::DnsTcp));
}

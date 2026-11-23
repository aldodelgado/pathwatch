//! Route tracing against a scripted path: the budget is never exceeded, a
//! trace stops when asked, and each annotation appears where the design
//! says and nowhere else. Paused clock; nothing is sent anywhere.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use pathwatch_core::clock::ManualClock;
use pathwatch_core::probe::icmp_core::Incoming;
use pathwatch_core::probe::icmp_packet::{Parsed, Token};
use pathwatch_core::testing::{FakePath, PathHop};
use pathwatch_core::trace::{self, Budget, NOTE_ECMP, NOTE_PERSISTS, NOTE_RATE_LIMITED, NOTE_SILENT, NOTE_SLOW_HOP, State, Step, Trace, TraceView};
use tokio_util::sync::CancellationToken;

const DEST: &str = "203.0.113.9";

fn budget() -> Budget {
    Budget { max_hops: 30, rounds: 10, round_interval: Duration::from_secs(1), max_probes: 300, max_rate: 30 }
}

fn dest() -> PathHop {
    PathHop::replying(DEST, 10)
}

/// Routers `10.0.0.1`, `10.0.0.2`, … answering in 1, 2, … ms.
fn routers(n: u8) -> Vec<PathHop> {
    (1..=n).map(|i| PathHop::replying(&format!("10.0.0.{i}"), u64::from(i))).collect()
}

async fn trace_with(hops: Vec<PathHop>, dest: PathHop, budget: Budget, stop_after: Option<Duration>) -> (TraceView, Vec<(u64, u8)>) {
    let clock = Arc::new(ManualClock::new());
    let (io, sent) = FakePath::new(clock.clone(), hops, dest);
    let stop = CancellationToken::new();
    if let Some(after) = stop_after {
        let stop = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            stop.cancel();
        });
    }
    let view = trace::run(io, DEST.parse().unwrap(), budget, clock.as_ref(), stop, &|_| {}).await;
    let sent = sent.lock().unwrap().clone();
    (view, sent)
}

async fn trace_over(hops: Vec<PathHop>, dest: PathHop) -> TraceView {
    trace_with(hops, dest, budget(), None).await.0
}

fn notes(view: &TraceView) -> Vec<(u8, Vec<&'static str>)> {
    view.hops.iter().filter(|h| !h.notes.is_empty()).map(|h| (h.ttl, h.notes.clone())).collect()
}

fn assert_within(sent: &[(u64, u8)], b: &Budget) {
    assert!(sent.len() as u32 <= b.max_probes, "{} probes for a cap of {}", sent.len(), b.max_probes);
    let spacing = 1_000_000_000 / u64::from(b.max_rate);
    assert!(sent.windows(2).all(|w| w[1].0 - w[0].0 >= spacing), "two probes closer together than the rate cap allows");
    // And counted the other way: no second holds more than the cap.
    for (i, (start, _)) in sent.iter().enumerate() {
        let in_second = sent[i..].iter().take_while(|(t, _)| t - start < 1_000_000_000).count() as u32;
        assert!(in_second <= b.max_rate, "{in_second} probes within one second");
    }
    assert!(sent.iter().all(|(_, ttl)| (1..=b.max_hops).contains(ttl)));
}

#[tokio::test(start_paused = true)]
async fn a_clean_path_is_found_once_and_then_probed_only_as_far_as_the_destination() {
    let (view, sent) = trace_with(routers(5), dest(), budget(), None).await;
    assert_eq!(view.state, State::Finished);
    assert_eq!(view.hops.len(), 6, "five routers and the destination");
    assert_eq!(view.hops.iter().map(|h| h.addrs[0].0.to_string()).collect::<Vec<_>>(), ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4", "10.0.0.5", DEST]);
    assert!(view.hops[5].is_destination && view.hops[..5].iter().all(|h| !h.is_destination));
    for h in &view.hops {
        assert_eq!((h.sent, h.replies), (10, 10), "hop {}", h.ttl);
        assert!(h.best_us <= h.avg_us() && h.avg_us() <= h.worst_us && h.last_us.is_some());
        assert!(h.first_seen_ns < h.last_seen_ns);
    }
    assert_eq!(view.hops[2].avg_us(), Some(3_000));
    assert_eq!(notes(&view), Vec::<(u8, Vec<&str>)>::new(), "a clean path needs no explaining");
    // The first round overshoots the destination by at most a probe or two;
    // every later round stops at it.
    assert!((60..=62).contains(&sent.len()), "{} probes", sent.len());
    assert!(sent.iter().filter(|(t, _)| *t >= 1_000_000_000).all(|(_, ttl)| *ttl <= 6));
    assert_eq!(view.probes_sent as usize, sent.len());
    assert_within(&sent, &budget());
}

#[tokio::test(start_paused = true)]
async fn a_trace_never_exceeds_its_probe_or_rate_cap() {
    // A destination that never answers: every round runs to the hop limit.
    let unreachable = || PathHop::silent();
    let (view, sent) = trace_with(routers(3), unreachable(), budget(), None).await;
    assert_eq!(sent.len(), 300, "30 hops for 10 rounds is exactly the cap");
    assert_within(&sent, &budget());
    assert!(matches!(view.state, State::Finished | State::Capped));
    assert_eq!(view.hops.len(), 30);

    // A tighter cap ends the trace early, at the cap and not one probe past it.
    let tight = Budget { max_probes: 100, ..budget() };
    let (view, sent) = trace_with(routers(3), unreachable(), tight.clone(), None).await;
    assert_eq!((sent.len(), view.state, view.probes_sent), (100, State::Capped, 100));
    assert_within(&sent, &tight);

    // A lower rate spreads a round out; it does not drop the cap.
    let slow = Budget { max_rate: 5, rounds: 3, ..budget() };
    let (_, sent) = trace_with(routers(5), dest(), slow.clone(), None).await;
    assert_within(&sent, &slow);
    assert!(sent.windows(2).all(|w| w[1].0 - w[0].0 >= 200_000_000));

    // Whatever it is told about the time, the state machine itself refuses
    // to send faster than the cap or beyond it.
    let mut t = Trace::new(DEST.parse().unwrap(), Budget { max_probes: 5, ..budget() }, 0);
    let mut sends = 0;
    for _ in 0..1_000 {
        match t.poll(0) {
            Step::Send { ttl, .. } => {
                t.sent(ttl, 0);
                sends += 1;
            }
            Step::Wait(_) | Step::Done => break,
        }
    }
    assert_eq!(sends, 1, "asked again at the same instant, it waits");
}

#[tokio::test(start_paused = true)]
async fn a_trace_stops_within_a_round_of_being_asked() {
    let (view, sent) = trace_with(routers(5), dest(), budget(), Some(Duration::from_millis(3_500))).await;
    assert_eq!(view.state, State::Stopped);
    let last = sent.iter().map(|(t, _)| *t).max().unwrap();
    assert!(last <= 3_500_000_000, "nothing is sent after the stop: last probe at {last}");
    assert!(view.round <= 4 && view.hops.iter().all(|h| h.sent <= 4), "round {}", view.round);
    // What was measured up to then is kept.
    assert!(view.hops.len() == 6 && view.hops.iter().all(|h| h.replies >= 3));
}

#[tokio::test(start_paused = true)]
async fn a_hop_that_never_answers_is_not_called_loss() {
    let mut hops = routers(4);
    hops[1] = PathHop::silent();
    let view = trace_over(hops, dest()).await;
    assert_eq!(notes(&view), vec![(2, vec![NOTE_SILENT])]);
    assert_eq!((view.hops[1].replies, view.hops[1].addrs.len()), (0, 0));
    assert!(view.hops[4].is_destination, "the hops after it are still counted from the right TTL");
}

#[tokio::test(start_paused = true)]
async fn a_hop_that_answers_only_some_probes_while_later_hops_answer_all_is_rate_limiting() {
    let mut hops = routers(4);
    hops[1] = PathHop::replying("10.0.0.2", 2).answering(|n| n % 2 == 0);
    let view = trace_over(hops, dest()).await;
    assert_eq!(notes(&view), vec![(2, vec![NOTE_RATE_LIMITED])]);
    assert_eq!((view.hops[1].sent, view.hops[1].replies), (10, 5));
}

#[tokio::test(start_paused = true)]
async fn no_reply_that_starts_at_a_hop_and_lasts_to_the_destination_is_marked_where_it_starts() {
    // From the third hop on, one probe in three goes unanswered — through to the destination.
    let lossy = |addr: &str, ms| PathHop::replying(addr, ms).answering(|n| n % 3 != 2);
    let hops = vec![PathHop::replying("10.0.0.1", 1), PathHop::replying("10.0.0.2", 2), lossy("10.0.0.3", 3), lossy("10.0.0.4", 4)];
    let view = trace_over(hops, lossy(DEST, 10)).await;
    assert_eq!(notes(&view), vec![(3, vec![NOTE_PERSISTS])], "once, where it begins; not on every hop after");
    assert!(view.hops[4].is_destination && view.hops[4].replies < view.hops[4].sent);

    // The same share at one hop only, with the destination answering everything, is not that.
    let mut hops = routers(4);
    hops[2] = lossy("10.0.0.3", 3);
    let view = trace_over(hops, dest()).await;
    assert_eq!(notes(&view), vec![(3, vec![NOTE_RATE_LIMITED])]);
}

#[tokio::test(start_paused = true)]
async fn two_addresses_at_one_ttl_and_a_slow_middle_hop_are_explained() {
    let mut hops = routers(4);
    hops[1].addrs.push("10.0.9.2".parse().unwrap());
    hops[2].rtt = Duration::from_millis(80);
    let view = trace_over(hops, dest()).await;
    assert_eq!(notes(&view), vec![(2, vec![NOTE_ECMP]), (3, vec![NOTE_SLOW_HOP])]);
    let seen: Vec<(String, u32)> = view.hops[1].addrs.iter().map(|(a, n)| (a.to_string(), *n)).collect();
    assert_eq!(seen, [("10.0.0.2".to_string(), 5), ("10.0.9.2".to_string(), 5)], "every address seen, with its count");
}

#[tokio::test(start_paused = true)]
async fn one_slow_answer_does_not_make_a_hop_slow() {
    // As seen on a real path: a hop answering in 6 ms nine times and 87 ms
    // once, in front of a destination at 7 ms. Its average is double the
    // destination's; what it usually takes is not.
    let mut hops = routers(4);
    hops[2] = PathHop::replying("10.0.0.3", 6).slow_when(|n| n == 4, 87);
    let view = trace_over(hops, PathHop::replying(DEST, 7)).await;
    let hop = &view.hops[2];
    assert_eq!((hop.replies, hop.median_us(), hop.worst_us), (10, Some(6_000), Some(87_000)));
    assert!(hop.avg_us().unwrap() > 14_000, "the average alone would have set the note off: {:?}", hop.avg_us());
    assert_eq!(notes(&view), Vec::<(u8, Vec<&str>)>::new());

    // Slow more often than not is the hop's story, and is said.
    let mut hops = routers(4);
    hops[2] = PathHop::replying("10.0.0.3", 6).slow_when(|n| n % 3 != 0, 87);
    let view = trace_over(hops, PathHop::replying(DEST, 7)).await;
    assert_eq!(notes(&view), vec![(3, vec![NOTE_SLOW_HOP])]);

    // With an even number of answers the median is the mean of the middle two.
    let mut hops = routers(4);
    hops[2] = PathHop::replying("10.0.0.3", 6).slow_when(|n| n >= 5, 8);
    let view = trace_over(hops, PathHop::replying(DEST, 7)).await;
    assert_eq!(view.hops[2].median_us(), Some(7_000));
}

#[tokio::test(start_paused = true)]
async fn one_round_is_not_enough_to_say_anything() {
    let mut hops = routers(4);
    hops[1] = PathHop::silent();
    let (view, _) = trace_with(hops, dest(), Budget { rounds: 1, ..budget() }, None).await;
    assert_eq!(view.state, State::Finished);
    assert_eq!(notes(&view), Vec::<(u8, Vec<&str>)>::new());
}

#[test]
fn no_annotation_claims_a_cause() {
    for note in [NOTE_SILENT, NOTE_RATE_LIMITED, NOTE_PERSISTS, NOTE_ECMP, NOTE_SLOW_HOP] {
        let lower = note.to_lowercase();
        for word in ["outage", "is down", "isp", "fault", "caused", "because", "congest", "proves", "confirmed", "broken"] {
            assert!(!lower.contains(word), "{word:?} in {note:?}");
        }
    }
    // The ones that mention loss do so to say what the numbers are not.
    assert!(NOTE_SILENT.contains("not evidence of loss") && NOTE_RATE_LIMITED.contains("not forwarding loss"));
}

#[test]
fn only_this_traces_own_answers_are_counted() {
    let dest: IpAddr = DEST.parse().unwrap();
    let mut t = Trace::new(dest, budget(), 0);
    t.expect(Some(0x1111), 0xabcd);
    let Step::Send { ttl, seq } = t.poll(0) else { panic!("the first step is a probe") };
    t.sent(ttl, 0);
    let reply = |id: u16, token: Option<Token>, src: IpAddr| Incoming {
        recv_ns: 5_000_000,
        src,
        parsed: Parsed::EchoReply { id, seq16: seq, token, payload_len: 32 },
        ttl: None,
        offender: None,
        recv_delay_ns: None,
    };
    let ours = Token { nonce: 0xabcd, stream: u32::MAX, seq: u64::from(seq) };
    // Another program's echo, a measurement stream's echo, another trace's, an echo from elsewhere: none is ours.
    t.incoming(&reply(0x2222, Some(ours), dest));
    t.incoming(&reply(0x1111, Some(Token { stream: 3, ..ours }), dest));
    t.incoming(&reply(0x1111, Some(Token { nonce: 0x9999, ..ours }), dest));
    t.incoming(&reply(0x1111, Some(ours), "198.51.100.1".parse().unwrap()));
    assert_eq!(t.view(10_000_000).hops[0].replies, 0);
    // An answer the kernel stamped a moment before our own "sent" reading
    // is still an answer — seen on Linux loopback, where it discarded every reply.
    let mut early = Trace::new(dest, budget(), 1_000_000);
    early.expect(None, 0xabcd);
    let Step::Send { ttl: e_ttl, seq: e_seq } = early.poll(1_000_000) else { panic!() };
    early.sent(e_ttl, 1_000_000);
    early.incoming(&Incoming {
        recv_ns: 999_700,
        src: dest,
        parsed: Parsed::EchoReply { id: 7, seq16: e_seq, token: Some(Token { seq: u64::from(e_seq), ..ours }), payload_len: 32 },
        ttl: None,
        offender: None,
        recv_delay_ns: None,
    });
    let hop = &early.view(2_000_000).hops[0];
    assert_eq!((hop.replies, hop.last_us, hop.is_destination), (1, Some(0), true));
    // Ours is, once.
    t.incoming(&reply(0x1111, Some(ours), dest));
    t.incoming(&reply(0x1111, Some(ours), dest));
    let view = t.view(10_000_000);
    assert_eq!((view.hops[0].replies, view.hops[0].last_us, view.hops[0].is_destination), (1, Some(5_000), true));
}

// ───────────────────────── the service ─────────────────────────

#[tokio::test(start_paused = true)]
async fn only_one_trace_runs_at_a_time_and_names_arrive_off_the_probe_path() {
    use pathwatch_core::trace::Request;
    use std::sync::atomic::{AtomicU32, Ordering};
    let clock = Arc::new(ManualClock::new());
    let opened = Arc::new(AtomicU32::new(0));
    let logs: Arc<std::sync::Mutex<Vec<pathwatch_core::testing::SentLog>>> = Arc::default();
    let (requests, rx) = tokio::sync::mpsc::unbounded_channel();
    let (shown, mut view) = tokio::sync::watch::channel(None);
    let open = {
        let (clock, opened, logs) = (clock.clone(), opened.clone(), logs.clone());
        move |_family| {
            opened.fetch_add(1, Ordering::Relaxed);
            let (io, sent) = FakePath::new(clock.clone(), routers(3), dest());
            logs.lock().unwrap().push(sent);
            Ok(io)
        }
    };
    // Names with something in them that must not reach a terminal as it is.
    let names: pathwatch_core::trace::NameFn = Arc::new(|ip: IpAddr| (ip.to_string() == "10.0.0.2").then(|| "core\u{1b}[31m-router\u{7}.example.net".to_string()));
    let budget = Budget { rounds: 3, ..budget() };
    let service = tokio::spawn(trace::serve(rx, open, budget, clock.clone() as Arc<dyn pathwatch_core::clock::Clock>, Some(names), shown));

    // Three requests in a row, and one more while it runs: one trace.
    let target: IpAddr = DEST.parse().unwrap();
    requests.send(Request::Start(target)).unwrap();
    requests.send(Request::Start(target)).unwrap();
    requests.send(Request::Start("198.51.100.200".parse().unwrap())).unwrap();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    requests.send(Request::Start(target)).unwrap();
    assert_eq!(opened.load(Ordering::Relaxed), 1, "one transport was opened: the other requests started nothing");
    let running = view.borrow_and_update().clone().expect("a view while it runs");
    assert_eq!((running.state, running.dest), (State::Running, target));

    // It finishes on its own; the view says so, and carries the name, cleaned.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let done = view.borrow().clone().unwrap();
    assert_eq!((done.state, done.hops.len(), done.round), (State::Finished, 4, 3));
    assert_eq!(done.names.len(), 1, "{:?}", done.names);
    let (addr, name) = &done.names[0];
    assert_eq!(addr.to_string(), "10.0.0.2");
    // The escape and the bell are written out as visible text, not sent to the terminal.
    assert!(name.contains("router") && name.contains("example.net") && name.chars().all(|c| !c.is_control()), "sanitised before it is shown: {name:?}");
    let probes = logs.lock().unwrap()[0].lock().unwrap().len();
    assert!((12..=14).contains(&probes), "{probes} probes: three rounds of four hops");

    // Then another may start, and can be stopped.
    requests.send(Request::Start(target)).unwrap();
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(opened.load(Ordering::Relaxed), 2);
    requests.send(Request::Stop).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(view.borrow().clone().unwrap().state, State::Stopped);
    // A stop with nothing running is harmless.
    requests.send(Request::Stop).unwrap();
    drop(requests);
    service.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_trace_that_cannot_open_a_socket_says_so_in_the_view() {
    use pathwatch_core::trace::Request;
    let clock = Arc::new(ManualClock::new());
    let (requests, rx) = tokio::sync::mpsc::unbounded_channel();
    let (shown, view) = tokio::sync::watch::channel(None);
    let open = |_family| Err::<FakePath, _>(std::io::Error::from_raw_os_error(13));
    let service = tokio::spawn(trace::serve(rx, open, budget(), clock as Arc<dyn pathwatch_core::clock::Clock>, None, shown));
    requests.send(Request::Start(DEST.parse().unwrap())).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let failed = view.borrow().clone().expect("a view saying why");
    assert_eq!((failed.state, failed.probes_sent), (State::Failed, 0));
    assert!(failed.error.as_deref().is_some_and(|e| e.contains("cannot open a socket")), "{:?}", failed.error);
    drop(requests);
    service.await.unwrap();
}

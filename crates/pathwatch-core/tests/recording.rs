//! Recording to disk and reading it back: round trips, rotation, the size
//! cap, a full disk, a truncated file, redaction, and replay fidelity.
//! Scripted data and temporary directories only; no network.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use pathwatch_core::config::REDACTED;
use pathwatch_core::engine::HistoryLimits;
use pathwatch_core::model::{EventRec, Line, Msg, ProviderRec, Record, SegmentRec, SessionRec, StreamRec};
use pathwatch_core::processor::Processor;
use pathwatch_core::recorder::{self, Files, Options, Recorder, Scrubber, Segment, Stop, Storage};
use pathwatch_core::script::{self, Playback, Scenario};
use pathwatch_core::snapshot::{Mode, ViewSpec};

fn dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("recording-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn options(directory: PathBuf) -> Options {
    Options { directory, rotate_bytes: 64 << 20, rotate_age: Duration::from_secs(86_400), max_total_bytes: 1 << 30, min_free_bytes: 0, fsync_on_rotate: true }
}

fn header(session: &str) -> SegmentRec {
    SegmentRec {
        session: session.into(),
        segment: 1,
        utc: "2026-11-21T00:00:00.000Z".into(),
        app: "pathwatch".into(),
        version: "test".into(),
        os: "test".into(),
        arch: "test".into(),
        redacted: true,
        providers: [("cloudflare", "Cloudflare"), ("google", "Google"), ("nextdns", "NextDNS"), ("arelion", "Arelion")]
            .into_iter()
            .enumerate()
            .map(|(i, (key, name))| ProviderRec { key: key.into(), name: name.into(), slot: i as u8 + 1, state: String::new(), reason: None })
            .collect(),
        config: String::new(),
    }
}

/// The record a live collector opens a session with, for a scripted scenario.
fn session_start(sc: &Scenario, session: &str) -> Record {
    Record::Session(SessionRec {
        session: session.into(),
        state: "start".into(),
        utc: "2026-11-21T00:00:00.000Z".into(),
        app: "pathwatch".into(),
        version: "test".into(),
        os: "test".into(),
        arch: "test".into(),
        streams: sc
            .streams
            .iter()
            .map(|s| StreamRec {
                stream: s.info.id.clone(),
                provider: s.info.provider.clone(),
                target: s.info.target.clone(),
                protocol: s.info.protocol,
                family: s.info.family,
                configured: s.configured.into(),
                interval_ms: s.info.interval.as_millis() as u64,
                deadline_ms: s.info.deadline.as_millis() as u64,
                state: s.state.into(),
                reason: (s.state != "collecting").then(|| "as scripted".to_string()),
            })
            .collect(),
        counters: None,
        traffic: Vec::new(),
    })
}

/// A scenario as a live session would produce it: the opening record, then
/// every message in order.
fn session(sc: &Scenario) -> Vec<(u64, Msg)> {
    let mut msgs = vec![(0, Msg::Record(session_start(sc, "s1")))];
    msgs.extend(script::generate_timed(sc, 7));
    msgs
}

fn record(sc: &Scenario, opts: Options) -> (Recorder, Vec<Record>) {
    let mut rec = Recorder::start(opts, Box::new(Files), Scrubber::default(), header("s1"), 0).expect("start recording");
    let mut written = Vec::new();
    for (at, msg) in session(sc) {
        if let Msg::Record(r) = msg {
            rec.write(&r, at).expect("write");
            written.push(r);
        }
    }
    rec.finish();
    (rec, written)
}

#[test]
fn a_recording_reads_back_exactly_what_was_written() {
    let sc = script::by_name("late-dup-reorder").unwrap();
    let (rec, written) = record(&sc, options(dir("roundtrip")));
    assert_eq!(rec.records(), written.len() as u64);
    let back = recorder::read(rec.directory()).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.segments, 1);
    assert_eq!(back.header.as_ref().map(|h| (&*h.session, h.segment, h.redacted)), Some(("s1", 1, true)));
    assert_eq!(back.records, written);
    // The file is plain JSON lines, the header first: usable with jq, grep, tail.
    let text = std::fs::read_to_string(rec.directory().join("000001.jsonl")).unwrap();
    assert!(text.starts_with("{\"schema\":1,\"kind\":\"segment\"") && text.ends_with('\n'));
    assert_eq!(text.len() as u64, rec.bytes());
}

#[test]
fn replaying_a_recording_reproduces_the_live_sessions_statistics_exactly() {
    for name in ["isolated-failure", "dns-only", "sleep-resume", "late-dup-reorder", "flapping", "needs-config", "address-change"] {
        let sc = script::by_name(name).unwrap();
        let end = sc.duration.as_nanos() as u64;
        // Live: the processor sees every message, including "sent" notices.
        let mut live = Processor::new(sc.infos(), HistoryLimits::default());
        for (_, m) in session(&sc) {
            live.ingest(&m);
        }
        // Replay: only what was written to disk, read back.
        let (rec, _) = record(&sc, options(dir(&format!("fidelity-{name}"))));
        let replay = recorder::prepare(recorder::read(rec.directory()).unwrap()).unwrap();
        assert_eq!(replay.infos.len(), sc.streams.len());
        let mut play = Playback::of_recording(replay);
        play.advance_to(u64::MAX);

        for (idx, s) in sc.streams.iter().enumerate() {
            for window in [60, 300, 3600] {
                let w = Duration::from_secs(window);
                assert_eq!(play.processor.engine.stats(idx, w, end), live.engine.stats(idx, w, end), "{name}: {} over {window} s", s.info.id);
            }
            assert_eq!(play.processor.engine.session_stats(idx, end), live.engine.session_stats(idx, end), "{name}: {} since start", s.info.id);
        }
        // And the same screen data: series, outcome cells, events, statuses.
        for mode in Mode::ALL {
            let view = ViewSpec { mode, window: Duration::from_secs(300), ..ViewSpec::default() };
            let a = live.snapshot(&play.providers, &play.streams, &view, end);
            let b = play.processor.snapshot(&play.providers, &play.streams, &view, end);
            assert_eq!(format!("{:?}", a.cards), format!("{:?}", b.cards), "{name} {mode:?}");
            assert_eq!(a.events, b.events, "{name} {mode:?}");
            assert_eq!(format!("{:?}", a.streams), format!("{:?}", b.streams), "{name} {mode:?}");
        }
    }
}

#[test]
fn segments_rotate_by_size_and_each_one_describes_itself() {
    let sc = script::by_name("healthy").unwrap();
    let (rec, written) = record(&sc, Options { rotate_bytes: 20_000, ..options(dir("rotate")) });
    let mut files: Vec<PathBuf> = std::fs::read_dir(rec.directory()).unwrap().flatten().map(|e| e.path()).collect();
    files.sort();
    assert!(files.len() > 3, "{} segments", files.len());
    for (i, f) in files.iter().enumerate() {
        assert_eq!(f.file_name().unwrap().to_string_lossy(), format!("{:06}.jsonl", i + 1));
        let text = std::fs::read_to_string(f).unwrap();
        assert!(text.len() <= 20_000 + 2_000, "{}: {} bytes", f.display(), text.len());
        let mut lines = text.lines().map(|l| serde_json::from_str::<Line>(l).unwrap().record);
        assert!(matches!(lines.next(), Some(Record::Segment(h)) if h.segment == i as u32 + 1 && &*h.session == "s1"), "{}: header first", f.display());
        assert!(matches!(lines.next(), Some(Record::Session(s)) if s.state == "start" && !s.streams.is_empty()), "{}: then the streams", f.display());
        // So any one segment can be replayed alone.
        assert!(recorder::prepare(recorder::read(f).unwrap()).is_ok());
    }
    // Read together, the repeated headers add nothing: every record once.
    let back = recorder::read(rec.directory()).unwrap();
    assert_eq!((back.segments, back.records.len()), (files.len(), written.len()));
    assert_eq!(back.records, written);
}

#[test]
fn segments_rotate_by_age() {
    let sc = script::by_name("healthy").unwrap();
    let (rec, _) = record(&sc, Options { rotate_age: Duration::from_secs(30), ..options(dir("age")) });
    let n = std::fs::read_dir(rec.directory()).unwrap().count() as u64;
    assert_eq!(n, sc.duration.as_secs().div_ceil(30), "one segment per 30 s of session time");
}

#[test]
fn the_size_cap_deletes_the_oldest_segments_and_never_the_one_being_written() {
    let root = dir("cap");
    let sc = script::by_name("healthy").unwrap();
    // An older session, complete, then a new one that pushes past the cap.
    let mut old = Recorder::start(Options { rotate_bytes: 20_000, ..options(root.clone()) }, Box::new(Files), Scrubber::default(), header("old"), 0).unwrap();
    for (at, msg) in session(&sc) {
        if let Msg::Record(r) = msg {
            old.write(&r, at).unwrap();
        }
    }
    old.finish();
    let old_dir = old.directory().to_path_buf();
    assert!(std::fs::read_dir(&old_dir).unwrap().count() > 3);

    let cap = 60_000;
    let (rec, _) = record(&sc, Options { rotate_bytes: 20_000, max_total_bytes: cap, ..options(root.clone()) });
    let size = |d: &Path| -> u64 { std::fs::read_dir(d).map(|it| it.flatten().map(|e| e.metadata().unwrap().len()).sum()).unwrap_or(0) };
    assert!(!old_dir.exists(), "the older session went first, and its empty directory with it");
    let kept: Vec<String> = {
        let mut v: Vec<String> = std::fs::read_dir(rec.directory()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };
    assert!(size(rec.directory()) <= cap + 22_000, "kept {} bytes for a cap of {cap}", size(rec.directory()));
    assert!(!kept.contains(&"000001.jsonl".to_string()), "the session's own oldest segments are deleted too: {kept:?}");
    // What is left is the newest part of the session, readable on its own.
    let back = recorder::read(rec.directory()).unwrap();
    assert!(back.warnings.is_empty() && recorder::prepare(back).is_ok());
}

/// Storage that runs out of space after a number of bytes.
struct Tight {
    left: Arc<AtomicI64>,
    free: Option<u64>,
}

struct TightFile(Arc<AtomicI64>);

impl Write for TightFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.0.fetch_sub(buf.len() as i64, Ordering::Relaxed) < buf.len() as i64 {
            return Err(io::Error::from_raw_os_error(libc_enospc()));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Segment for TightFile {
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn libc_enospc() -> i32 {
    28 // ENOSPC on Linux and macOS
}

impl Storage for Tight {
    fn create(&mut self, _: &Path) -> io::Result<Box<dyn Segment>> {
        Ok(Box::new(TightFile(self.left.clone())))
    }

    fn free_bytes(&self, _: &Path) -> Option<u64> {
        self.free
    }
}

#[test]
fn a_full_disk_stops_recording_once_and_loses_no_measurement() {
    let sc = script::by_name("isolated-failure").unwrap();
    let storage = Tight { left: Arc::new(AtomicI64::new(100_000)), free: None };
    let mut rec = Recorder::start(options(PathBuf::from("/nonexistent")), Box::new(storage), Scrubber::default(), header("s1"), 0).unwrap();
    let mut processor = Processor::new(sc.infos(), HistoryLimits::default());
    let mut stops = Vec::new();
    let all = session(&sc);
    for (at, msg) in &all {
        // The processor is fed first and regardless: recording is beside the
        // measurement path, not on it.
        processor.ingest(msg);
        if let Msg::Record(r) = msg
            && let Err(stop) = rec.write(r, *at).and_then(|()| rec.flush())
        {
            stops.push(stop);
        }
    }
    assert!(!stops.is_empty() && stops.iter().all(|s| *s == stops[0]), "one reason, repeated: {:?}", &stops[..stops.len().min(3)]);
    assert!(matches!(&stops[0], Stop::Io(e) if e.contains("space")), "{:?}", stops[0]);
    assert!(rec.stopped().is_some() && rec.records() > 100 && rec.bytes() <= 100_000 + 8_192);
    // Every stream has all of its probes, exactly as without a recorder.
    let end = sc.duration.as_nanos() as u64;
    let mut reference = Processor::new(sc.infos(), HistoryLimits::default());
    all.iter().for_each(|(_, m)| reference.ingest(m));
    for (idx, stream) in sc.streams.iter().enumerate() {
        assert_eq!(processor.engine.session_stats(idx, end), reference.engine.session_stats(idx, end));
        assert!(processor.engine.session_stats(idx, end).slots >= 100 || stream.state != "collecting");
    }
}

#[test]
fn recording_stops_before_the_disk_is_full() {
    let storage = Tight { left: Arc::new(AtomicI64::new(i64::MAX)), free: Some(150 << 20) };
    let mut rec =
        Recorder::start(Options { min_free_bytes: 200 << 20, ..options(PathBuf::from("/nonexistent")) }, Box::new(storage), Scrubber::default(), header("s1"), 0).unwrap();
    let event = |at| Record::Event(EventRec { session: "s1".into(), at_ns: at, utc: String::new(), event: "note".into(), stream: None, detail: "x".into() });
    // Free space is looked at every few seconds, not on every record.
    assert_eq!(rec.write(&event(1), 1_000_000_000), Ok(()));
    let stop = rec.write(&event(2), 6_000_000_000).unwrap_err();
    assert_eq!(stop, Stop::LowDisk { free: 150 << 20, floor: 200 << 20 });
    assert!(stop.to_string().contains("150 MiB") && stop.to_string().contains("200 MiB"));
    assert_eq!(rec.write(&event(3), 7_000_000_000), Err(stop));
}

#[test]
fn a_recording_cut_short_replays_up_to_its_last_complete_record() {
    let sc = script::by_name("isolated-failure").unwrap();
    let (rec, written) = record(&sc, options(dir("truncated")));
    let file = rec.directory().join("000001.jsonl");
    let text = std::fs::read(&file).unwrap();
    // Cut in the middle of a line, as a crash or a full disk would.
    let cut = text.len() * 2 / 3;
    let cut = cut - usize::from(text[cut - 1] == b'\n');
    std::fs::write(&file, &text[..cut]).unwrap();
    let complete = text[..cut].iter().filter(|b| **b == b'\n').count();

    let back = recorder::read(&file).unwrap();
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(back.warnings[0].contains("incomplete") && back.warnings[0].contains("000001.jsonl"), "{}", back.warnings[0]);
    assert_eq!(back.records.len(), complete - 1, "every complete line but the header");
    assert_eq!(back.records[..], written[..complete - 1]);
    let replay = recorder::prepare(back).unwrap();
    assert!(replay.end_ns > 0 && replay.end_ns < sc.duration.as_nanos() as u64);
    let mut play = Playback::of_recording(replay);
    play.advance_to(u64::MAX);
    assert!(play.processor.engine.session_stats(0, play.end_ns()).sent > 50);
}

#[test]
fn unknown_lines_are_skipped_with_one_warning_and_a_recording_without_streams_is_refused() {
    let d = dir("odd");
    std::fs::create_dir_all(&d).unwrap();
    let sc = script::by_name("healthy").unwrap();
    let start = Line::new(session_start(&sc, "s1")).to_json();
    let future = r#"{"schema":2,"kind":"observation","new":"shape"}"#;
    std::fs::write(d.join("a.jsonl"), format!("{start}\nnot json at all\n{{\"schema\":1,\"kind\":\"hologram\"}}\n{future}\n\n")).unwrap();
    let back = recorder::read(&d.join("a.jsonl")).unwrap();
    assert_eq!(back.records.len(), 1);
    assert_eq!(back.warnings.len(), 3, "{:?}", back.warnings);
    assert!(back.warnings.iter().any(|w| w.contains("schema 2 is newer")) && back.warnings.iter().any(|w| w.contains("lines skipped in all")));
    assert!(back.header.is_none(), "the output of `sample` has no segment header and is still a recording");
    let replay = recorder::prepare(back).unwrap();
    assert_eq!(replay.providers.iter().map(|p| (&*p.key, p.name.as_str(), p.slot)).collect::<Vec<_>>()[0], ("cloudflare", "Cloudflare", 1));

    std::fs::write(d.join("b.jsonl"), "{\"schema\":1,\"kind\":\"gap\",\"session\":\"s\",\"cause\":\"stopped\",\"start_ns\":1,\"end_ns\":2,\"duration_ms\":0,\"end_utc\":\"\"}\n")
        .unwrap();
    let err = recorder::prepare(recorder::read(&d.join("b.jsonl")).unwrap()).err().unwrap();
    assert!(err.contains("no session record"), "{err}");
    assert!(recorder::read(&d.join("missing.jsonl")).is_err() && recorder::read(&dir("empty-nonexistent")).is_err());
}

#[test]
fn a_profile_id_never_reaches_the_disk_in_any_field() {
    const ID: &str = "ab12cd";
    let sc = script::by_name("healthy").unwrap();
    let mut start = session_start(&sc, "s1");
    let Record::Session(s) = &mut start else { unreachable!() };
    // The id in every place a stream could carry it: URL path, server name, free text.
    s.streams[0].configured = format!("https://dns.nextdns.io/{ID}").into();
    s.streams[0].target = format!("{ID}.dns.nextdns.io").into();
    s.streams[0].reason = Some(format!("certificate is for {ID}.dns.nextdns.io"));
    let mut hdr = header("s1");
    hdr.config = format!("[providers.nextdns]\nprofile_id = \"{ID}\"\n");
    let event = Record::Event(EventRec {
        session: "s1".into(),
        at_ns: 5,
        utc: String::new(),
        event: "bootstrap_failed".into(),
        stream: None,
        detail: format!("lookup {ID}.dns.nextdns.io failed"),
    });

    let mut rec = Recorder::start(options(dir("redact")), Box::new(Files), Scrubber::new([ID.to_string()]), hdr, 0).unwrap();
    rec.write(&start, 0).unwrap();
    rec.write(&event, 5).unwrap();
    rec.finish();
    let text = std::fs::read_to_string(rec.directory().join("000001.jsonl")).unwrap();
    assert!(!text.contains(ID), "the profile id is on disk:\n{text}");
    assert_eq!(text.matches(REDACTED).count(), 5, "every occurrence is marked as removed:\n{text}");
    // And it still reads back as a recording.
    let back = recorder::read(rec.directory()).unwrap();
    assert!(back.warnings.is_empty() && back.records.len() == 2);
}

#[test]
fn controls_in_a_recording_replay_as_controls_never_as_providers() {
    // Found by replaying the first live recording made with controls on:
    // they came back as a fifth and a sixth card.
    use pathwatch_core::model::{Family, Protocol};
    let mut sc = script::by_name("healthy").unwrap();
    sc.streams.push(script::stream("gateway", "auto", Protocol::Icmp, Family::V4, "192.168.1.1", 2, 300, 333));
    sc.streams.push(script::stream("local-dns", "auto", Protocol::DnsUdp, Family::V4, "203.0.113.53", 30, 900, 1_666));
    let cards_live = sc.metas().0.len();
    let (rec, _) = record(&sc, options(dir("controls")));
    // With the header's provider list, and without it (as for `sample` output).
    let with_header = recorder::read(rec.directory()).unwrap();
    let mut headerless = recorder::read(rec.directory()).unwrap();
    headerless.header = None;
    for (what, recording) in [("with a header", with_header), ("without a header", headerless)] {
        let replay = recorder::prepare(recording).unwrap();
        assert_eq!(replay.providers.len(), cards_live, "{what}: {:?}", replay.providers.iter().map(|p| &*p.key).collect::<Vec<_>>());
        assert!(replay.providers.iter().all(|p| !pathwatch_core::localnet::is_control(&p.key)), "{what}");
        let mut play = Playback::of_recording(replay);
        play.advance_to(u64::MAX);
        let snap = play.snapshot(&ViewSpec::default(), play.end_ns());
        assert_eq!(snap.cards.len(), cards_live, "{what}: four cards");
        let controls: Vec<(&str, Option<&str>)> = snap.controls.iter().map(|c| (c.label, c.tag)).collect();
        assert_eq!(controls, vec![("Gateway", None), ("System DNS", Some("remote"))], "{what}");
        assert!(snap.controls.iter().all(|c| c.stats.replied > 10), "{what}: and they carry their data");
    }
}

#[test]
fn a_reload_in_a_recording_changes_what_the_replay_shows_from_that_moment_on() {
    use pathwatch_core::model::{ConfigRec, Family, Protocol};
    use pathwatch_core::snapshot::Status;
    let sc = script::by_name("healthy").unwrap();
    // The session as recorded, with a reload at 60 s: Arelion is switched
    // off, and Google gains a DNS stream.
    let mut msgs = session(&sc);
    let Msg::Record(Record::Session(start)) = &msgs[0].1 else { unreachable!() };
    let mut streams: Vec<StreamRec> = start.streams.iter().filter(|s| &*s.provider != "arelion").cloned().collect();
    streams.push(StreamRec {
        stream: "google/resolver-v4/dns-udp/4".into(),
        provider: "google".into(),
        target: "resolver-v4".into(),
        protocol: Protocol::DnsUdp,
        family: Family::V4,
        configured: "8.8.8.8".into(),
        interval_ms: 5_000,
        deadline_ms: 3_000,
        state: "collecting".into(),
        reason: None,
    });
    let mut providers = header("s1").providers;
    providers.iter_mut().for_each(|p| p.state = if &*p.key == "arelion" { "disabled".into() } else { "collecting".into() });
    let at = 60_000_000_000u64;
    let reload = Record::Config(ConfigRec {
        session: "s1".into(),
        rev: 2,
        at_ns: at,
        utc: "2026-11-21T00:01:00.000Z".into(),
        streams,
        providers,
        summary: "1 started, 1 stopped, 0 restarted, 3 unchanged".into(),
    });
    let position = msgs.iter().position(|(t, _)| *t >= at).unwrap();
    msgs.insert(position, (at, Msg::Record(reload.clone())));

    let mut rec = Recorder::start(options(dir("reload")), Box::new(Files), Scrubber::default(), header("s1"), 0).unwrap();
    for (t, m) in &msgs {
        if let Msg::Record(r) = m {
            rec.write(r, *t).unwrap();
        }
    }
    rec.finish();
    let back = recorder::read(rec.directory()).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert!(back.records.contains(&reload), "the reload is in the recording as written");

    let mut play = Playback::of_recording(recorder::prepare(back).unwrap());
    let status = |play: &Playback, now_s: u64, key: &str| {
        let snap = play.snapshot(&ViewSpec::default(), now_s * 1_000_000_000);
        snap.cards.iter().find(|c| &*c.key == key).map(|c| c.status.clone())
    };
    // Before the reload: four providers, all collecting.
    play.advance_to(50_000_000_000);
    assert_eq!(play.streams.len(), 4);
    assert!(matches!(status(&play, 50, "arelion"), Some(Status::Ok | Status::WarmingUp)), "{:?}", status(&play, 50, "arelion"));
    // After it: still four cards; Arelion says it is switched off, the new
    // stream is known to the engine after the old ones, and history is kept.
    play.advance_to(90_000_000_000);
    assert_eq!(play.providers.len(), 4);
    assert_eq!(status(&play, 90, "arelion"), Some(Status::Disabled));
    assert_eq!(play.processor.engine.stream_index("google/resolver-v4/dns-udp/4"), Some(4));
    assert!(play.streams.iter().any(|s| s.protocol == Protocol::DnsUdp && &*s.provider == "google"));
    assert!(!play.streams.iter().any(|s| &*s.provider == "arelion"));
    let cloudflare = play.processor.engine.session_stats(0, 90_000_000_000);
    assert!(cloudflare.sent >= 89, "an unchanged stream's history runs straight through the reload: {cloudflare:?}");
}

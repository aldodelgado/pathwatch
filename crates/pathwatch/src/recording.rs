//! The recorder thread: takes records from the processor through a bounded
//! queue and writes them to disk. It is beside the measurement path — a slow
//! or full disk can cost recorded lines, never a probe or a statistic.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use pathwatch_core::clock::{Clock, utc_string};
use pathwatch_core::config::{Loaded, Plan, real_env};
use pathwatch_core::model::{ProviderRec, Record, SegmentRec};
use pathwatch_core::recorder::{self, Options, Recorder, Scrubber, Status, Storage};

/// Records waiting to be written. At the built-in rates this is minutes of
/// backlog; beyond it lines are dropped and counted, not queued without bound.
const QUEUE: usize = 16_384;

pub struct Handle {
    tx: Option<SyncSender<Record>>,
    pub status: Arc<Status>,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    /// Queues a record for writing. Never blocks.
    pub fn offer(&self, record: &Record) {
        if self.status.stopped().is_some() {
            return;
        }
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send(record.clone())
        {
            self.status.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Writes what is queued, closes the segment, and waits for that.
    pub fn finish(mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Where this session records to, if it records at all: the `--record` flag
/// (with or without a directory) or `[recording] enabled`.
pub fn directory(loaded: &Loaded, flag: Option<&str>) -> Option<PathBuf> {
    let configured = || recorder::expand_home(&loaded.config.recording.directory, real_env("HOME").as_deref());
    match flag {
        Some("") => Some(configured()),
        Some(dir) => Some(PathBuf::from(dir)),
        None => loaded.config.recording.enabled.then(configured),
    }
}

/// Starts recording a session. An error here is reported before anything is
/// measured: asking for a recording that cannot be made is a failure.
pub fn start(loaded: &Loaded, plan: &Plan, directory: PathBuf, session: Arc<str>, clock: Arc<dyn Clock>, storage: Box<dyn Storage>) -> Result<Handle, String> {
    let opts = Options::from_config(&loaded.config.recording, directory)?;
    // Profile ids are removed as lines are written, so they are never on
    // disk and an export cannot leak them, whoever runs it and wherever.
    let redact = loaded.config.redaction.profile_ids;
    let secrets = loaded.config.providers.values().filter_map(|p| p.profile_id_env.as_deref().and_then(real_env));
    let scrub = if redact { Scrubber::new(secrets) } else { Scrubber::default() };
    let header = SegmentRec {
        session,
        segment: 1,
        utc: utc_string(clock.wall()),
        app: "pathwatch".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        redacted: redact,
        providers: plan.providers.iter().map(ProviderRec::of).collect(),
        config: loaded.effective_toml(!redact),
    };
    let mut rec = Recorder::start(opts, storage, scrub, header, clock.mono_ns()).map_err(|e| e.to_string())?;
    let status = Arc::new(Status::new(rec.directory().display().to_string()));
    let (tx, rx) = sync_channel::<Record>(QUEUE);
    let shared = status.clone();
    let thread = std::thread::Builder::new()
        .name("recorder".into())
        .spawn(move || {
            // One batch per wake-up: everything queued, then one flush.
            while let Ok(first) = rx.recv() {
                let mut result = rec.write(&first, clock.mono_ns());
                while result.is_ok()
                    && let Ok(next) = rx.try_recv()
                {
                    result = rec.write(&next, clock.mono_ns());
                }
                if let Err(stop) = result.and_then(|()| rec.flush()) {
                    shared.stop(stop.to_string());
                    // Keep the queue empty so the sender never sees it full.
                    while rx.recv().is_ok() {}
                    break;
                }
                shared.bytes.store(rec.bytes(), Ordering::Relaxed);
                shared.records.store(rec.records(), Ordering::Relaxed);
            }
            rec.finish();
        })
        .map_err(|e| e.to_string())?;
    Ok(Handle { tx: Some(tx), status, thread: Some(thread) })
}

/// Says where the recording went, on standard error. Returns the exit code to
/// use when the recording did not complete.
pub fn report(status: &Status) -> Option<std::process::ExitCode> {
    let (records, bytes, dropped) = (status.records.load(Ordering::Relaxed), status.bytes.load(Ordering::Relaxed), status.dropped.load(Ordering::Relaxed));
    say!("pathwatch: recorded {records} records ({:.1} MB) to {}", bytes as f64 / 1e6, status.path);
    if dropped > 0 {
        say!("pathwatch: {dropped} records were not recorded because the disk could not keep up; measurement was not affected");
    }
    let why = status.stopped()?;
    say!("pathwatch: recording stopped early: {why}. Measurement was not affected.");
    Some(std::process::ExitCode::from(crate::EXIT_IO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathwatch_core::clock::ManualClock;
    use pathwatch_core::model::EventRec;
    use pathwatch_core::recorder::Segment;
    use std::io::{self, Write};
    use std::path::Path;

    struct Full;
    struct NoRoom(u32);

    impl Write for NoRoom {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            // Room for the header and a little more, then the disk is full.
            self.0 += 1;
            if self.0 > 2 { Err(io::Error::from_raw_os_error(libc::ENOSPC)) } else { Ok(buf.len()) }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Segment for NoRoom {
        fn sync(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Storage for Full {
        fn create(&mut self, _: &Path) -> io::Result<Box<dyn Segment>> {
            Ok(Box::new(NoRoom(0)))
        }

        fn free_bytes(&self, _: &Path) -> Option<u64> {
            None
        }
    }

    #[test]
    fn a_disk_that_fills_stops_the_recorder_with_one_notice_and_never_blocks_the_sender() {
        let loaded = Loaded::from_layers(None, None, &[]).unwrap();
        let plan = loaded.plan(&|_| None);
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new());
        let handle = start(&loaded, &plan, PathBuf::from("/nonexistent"), "s1".into(), clock, Box::new(Full)).unwrap();
        let event = |n: u64| Record::Event(EventRec { session: "s1".into(), at_ns: n, utc: String::new(), event: "note".into(), stream: None, detail: "x".repeat(200) });
        // Far more than the queue holds, as fast as possible: every call returns.
        let began = std::time::Instant::now();
        for n in 0..(QUEUE as u64 * 4) {
            handle.offer(&event(n));
        }
        assert!(began.elapsed() < std::time::Duration::from_secs(5), "offering records never waits for the disk");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while handle.status.stopped().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let why = handle.status.stopped().expect("the recorder reported that it stopped");
        assert!(why.contains("write failed") && why.contains("space"), "{why}");
        assert_eq!(handle.status.take_notice(), Some(why), "the reason is announced");
        assert_eq!(handle.status.take_notice(), None, "once");
        handle.offer(&event(0)); // ignored, and does not panic
        handle.finish();
    }

    #[test]
    fn the_flag_wins_over_the_configuration_and_neither_means_no_recording() {
        let off = Loaded::from_layers(None, None, &[]).unwrap();
        assert_eq!(directory(&off, None), None, "nothing is written unless asked");
        assert_eq!(directory(&off, Some("/tmp/x")), Some(PathBuf::from("/tmp/x")));
        let on = Loaded::from_layers(Some("[recording]\nenabled = true\ndirectory = \"/var/rec\"\n"), None, &[]).unwrap();
        assert_eq!(directory(&on, None), Some(PathBuf::from("/var/rec")));
        assert_eq!(directory(&on, Some("")), Some(PathBuf::from("/var/rec")), "--record alone uses the configured directory");
        assert_eq!(directory(&on, Some("/tmp/y")), Some(PathBuf::from("/tmp/y")));
    }
}

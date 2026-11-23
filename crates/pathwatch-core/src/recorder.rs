//! Recording a session to disk, reading a recording back, and preparing one
//! for replay. See `docs/design/architecture.md` §7.
//!
//! A recording is the session's records as JSON lines, in the order the
//! processor received them, in numbered segments under one directory per
//! session. Nothing here is on the measurement path: a recorder that fails
//! stops recording and says why, and collection carries on.

use std::borrow::Cow;
use std::fs;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::config::{self, REDACTED};
use crate::engine::StreamInfo;
use crate::model::{Line, Msg, ProviderRec, Record, SCHEMA_VERSION, SegmentRec, SessionRec};
use crate::snapshot::{ProviderMeta, StreamMeta};

/// Parses a size such as `64MiB`, `1GiB`, `200 MB`, or a plain byte count.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n: u64 = s[..digits].parse().map_err(|_| format!("{s:?} does not start with a whole number"))?;
    let unit = match s[digits..].trim() {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "kB" | "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        other => return Err(format!("unknown size unit {other:?}; use B, KiB, MiB, GiB, kB, MB, or GB")),
    };
    n.checked_mul(unit).ok_or_else(|| format!("{s:?} is too large"))
}

/// Replaces a leading `~/` with the home directory.
pub fn expand_home(path: &str, home: Option<&str>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Recordings live in `<directory>/<session id>/NNNNNN.jsonl`.
    pub directory: PathBuf,
    pub rotate_bytes: u64,
    pub rotate_age: Duration,
    /// Cap on everything under `directory`; oldest segments go first.
    pub max_total_bytes: u64,
    /// Recording stops when free space falls below this.
    pub min_free_bytes: u64,
    pub fsync_on_rotate: bool,
}

impl Options {
    pub fn from_config(r: &config::Recording, directory: PathBuf) -> Result<Self, String> {
        Ok(Self {
            directory,
            rotate_bytes: parse_size(&r.rotate_size).map_err(|e| format!("recording.rotate_size: {e}"))?.max(4096),
            rotate_age: r.rotate_age.0,
            max_total_bytes: parse_size(&r.max_total).map_err(|e| format!("recording.max_total: {e}"))?,
            min_free_bytes: parse_size(&r.min_free_disk).map_err(|e| format!("recording.min_free_disk: {e}"))?,
            fsync_on_rotate: r.fsync != "never",
        })
    }
}

/// Removes secrets from a line as it is written. The profile id is replaced
/// wherever it occurs in the serialised record — any field, URL, or server
/// name — so the guarantee does not depend on knowing which fields carry it.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    secrets: Vec<String>,
}

impl Scrubber {
    /// `secrets` shorter than four characters are ignored: replacing them
    /// would corrupt unrelated text and they identify nothing.
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        Self { secrets: secrets.into_iter().filter(|s| s.len() >= 4).collect() }
    }

    pub fn is_active(&self) -> bool {
        !self.secrets.is_empty()
    }

    pub fn scrub<'a>(&self, line: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(line);
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = Cow::Owned(out.replace(secret.as_str(), REDACTED));
            }
        }
        out
    }
}

/// Where segments are written. The real one is the filesystem; tests use one
/// that runs out of space on demand.
pub trait Storage: Send {
    fn create(&mut self, path: &Path) -> io::Result<Box<dyn Segment>>;
    /// Free space available to this process on the volume holding `dir`.
    fn free_bytes(&self, dir: &Path) -> Option<u64>;
}

pub trait Segment: Write + Send {
    fn sync(&mut self) -> io::Result<()>;
}

pub struct Files;

impl Segment for fs::File {
    fn sync(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

impl Storage for Files {
    fn create(&mut self, path: &Path) -> io::Result<Box<dyn Segment>> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        // A recording says where this host sits in its network: its address,
        // its interface, where it sends and what it is configured to do. It
        // is the owner's to read and nobody else's — the directories made
        // here and the file, whatever the umask would have allowed.
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        // Never overwrite: a segment that exists belongs to another session.
        Ok(Box::new(fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?))
    }

    fn free_bytes(&self, dir: &Path) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        // SAFETY: `c` is a valid NUL-terminated path and `st` is a valid
        // out-pointer; statvfs only writes into it.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| (st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
    }
}

/// Why recording stopped. Measurement is unaffected in every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    Io(String),
    LowDisk { free: u64, floor: u64 },
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::Io(e) => write!(f, "write failed: {e}"),
            Stop::LowDisk { free, floor } => write!(f, "free disk space ({} MiB) is below the configured floor ({} MiB)", free >> 20, floor >> 20),
        }
    }
}

/// What the rest of the program may know about a recorder running on another
/// thread: where it writes, how much, and whether it has stopped.
#[derive(Debug, Default)]
pub struct Status {
    pub path: String,
    pub bytes: AtomicU64,
    pub records: AtomicU64,
    /// Records that could not be queued because the recorder fell behind.
    pub dropped: AtomicU64,
    stopped: Mutex<Option<String>>,
    announced: std::sync::atomic::AtomicBool,
}

impl Status {
    pub fn new(path: String) -> Self {
        Self { path, ..Self::default() }
    }

    pub fn stop(&self, why: String) {
        self.stopped.lock().unwrap().get_or_insert(why);
    }

    pub fn stopped(&self) -> Option<String> {
        self.stopped.lock().unwrap().clone()
    }

    /// The reason recording stopped, the first time this is asked after it did.
    pub fn take_notice(&self) -> Option<String> {
        let why = self.stopped()?;
        (!self.announced.swap(true, Ordering::Relaxed)).then_some(why)
    }
}

const FREE_CHECK_EVERY_NS: u64 = 5_000_000_000;

pub struct Recorder {
    opts: Options,
    storage: Box<dyn Storage>,
    scrub: Scrubber,
    header: SegmentRec,
    /// The session's opening record, repeated at the top of every segment so
    /// each one names its streams even after earlier ones are deleted.
    session_start: Option<String>,
    dir: PathBuf,
    current: PathBuf,
    out: Option<BufWriter<Box<dyn Segment>>>,
    seg_bytes: u64,
    seg_records: u64,
    seg_opened_ns: u64,
    last_free_check_ns: u64,
    total_bytes: u64,
    records: u64,
    stopped: Option<Stop>,
}

impl Recorder {
    /// Creates the session directory and the first segment. `header` is the
    /// template for every segment's first line.
    pub fn start(opts: Options, mut storage: Box<dyn Storage>, scrub: Scrubber, header: SegmentRec, now_ns: u64) -> io::Result<Self> {
        let dir = opts.directory.join(&*header.session);
        let current = dir.join("000001.jsonl");
        let out = BufWriter::new(storage.create(&current)?);
        let mut r = Self {
            opts,
            storage,
            scrub,
            header: SegmentRec { segment: 1, ..header },
            session_start: None,
            dir,
            current,
            out: Some(out),
            seg_bytes: 0,
            seg_records: 0,
            seg_opened_ns: now_ns,
            last_free_check_ns: now_ns,
            total_bytes: 0,
            records: 0,
            stopped: None,
        };
        let line = r.line(&Record::Segment(r.header.clone()));
        r.put(&line)?;
        Ok(r)
    }

    pub fn directory(&self) -> &Path {
        &self.dir
    }

    pub fn bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    pub fn stopped(&self) -> Option<&Stop> {
        self.stopped.as_ref()
    }

    fn line(&self, record: &Record) -> String {
        let mut line = self.scrub.scrub(&Line::new(record.clone()).to_json()).into_owned();
        line.push('\n');
        line
    }

    fn put(&mut self, line: &str) -> io::Result<()> {
        let out = self.out.as_mut().ok_or_else(|| io::Error::other("no open segment"))?;
        out.write_all(line.as_bytes())?;
        self.seg_bytes += line.len() as u64;
        self.total_bytes += line.len() as u64;
        Ok(())
    }

    fn fail(&mut self, stop: Stop) -> Stop {
        // Keep what can be kept; a failure here changes nothing.
        if let Some(mut out) = self.out.take() {
            let _ = out.flush();
        }
        self.stopped.get_or_insert(stop).clone()
    }

    /// Appends one record. After the first error nothing more is written and
    /// the same error is returned.
    pub fn write(&mut self, record: &Record, now_ns: u64) -> Result<(), Stop> {
        if let Some(stop) = &self.stopped {
            return Err(stop.clone());
        }
        if now_ns.saturating_sub(self.last_free_check_ns) >= FREE_CHECK_EVERY_NS {
            self.last_free_check_ns = now_ns;
            if let Some(free) = self.storage.free_bytes(&self.dir).filter(|f| *f < self.opts.min_free_bytes) {
                return Err(self.fail(Stop::LowDisk { free, floor: self.opts.min_free_bytes }));
            }
        }
        let line = self.line(record);
        if matches!(record, Record::Session(SessionRec { state, .. }) if state == "start") {
            self.session_start = Some(line.clone());
        }
        let aged = now_ns.saturating_sub(self.seg_opened_ns) >= self.opts.rotate_age.as_nanos() as u64;
        if self.seg_records > 0 && (aged || self.seg_bytes + line.len() as u64 > self.opts.rotate_bytes) {
            self.rotate(now_ns).map_err(|e| self.fail(Stop::Io(e.to_string())))?;
        }
        self.put(&line).map_err(|e| self.fail(Stop::Io(e.to_string())))?;
        self.seg_records += 1;
        self.records += 1;
        Ok(())
    }

    fn rotate(&mut self, now_ns: u64) -> io::Result<()> {
        if let Some(mut out) = self.out.take() {
            out.flush()?;
            if self.opts.fsync_on_rotate {
                out.get_mut().sync()?;
            }
        }
        self.header.segment += 1;
        self.current = self.dir.join(format!("{:06}.jsonl", self.header.segment));
        self.out = Some(BufWriter::new(self.storage.create(&self.current)?));
        (self.seg_bytes, self.seg_records, self.seg_opened_ns) = (0, 0, now_ns);
        let header = self.line(&Record::Segment(self.header.clone()));
        self.put(&header)?;
        if let Some(start) = self.session_start.clone() {
            self.put(&start)?;
        }
        // Retention failing is not a reason to stop recording.
        let _ = enforce_cap(&self.opts.directory, self.opts.max_total_bytes, &self.current);
        Ok(())
    }

    /// Pushes buffered lines to the file. Called after each batch.
    pub fn flush(&mut self) -> Result<(), Stop> {
        if let Some(stop) = &self.stopped {
            return Err(stop.clone());
        }
        match self.out.as_mut().map(|o| o.flush()) {
            Some(Err(e)) => Err(self.fail(Stop::Io(e.to_string()))),
            _ => Ok(()),
        }
    }

    /// Flushes and syncs the last segment at the end of a session.
    pub fn finish(&mut self) {
        if let Some(mut out) = self.out.take() {
            let _ = out.flush();
            let _ = out.get_mut().sync();
        }
    }
}

/// Whether a file is a segment this program wrote: a regular file named as
/// segments are named, `NNNNNN.jsonl`, that opens with a segment header.
/// Retention deletes files, so it asks this of every file it would count.
fn is_segment(path: &Path) -> bool {
    use std::io::Read;
    let named = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".jsonl")).is_some_and(|n| n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()));
    if !named || !fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut start = [0u8; 48];
    let read = fs::File::open(path).and_then(|mut f| f.read(&mut start)).unwrap_or(0);
    let start = String::from_utf8_lossy(&start[..read]);
    start.strip_prefix("{\"schema\":").is_some_and(|rest| rest.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(",\"kind\":\"segment\""))
}

/// Every segment under `root`, oldest first, with its size. Only this
/// program's own: a directory that is reached through a link, a file with
/// another name, and a `.jsonl` file that is not a recording are not listed,
/// so they are neither counted against the cap nor deleted for it.
fn segments(root: &Path, keep: &Path) -> io::Result<Vec<(std::time::SystemTime, PathBuf, u64)>> {
    let mut found = Vec::new();
    for session in fs::read_dir(root)?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) {
        for seg in fs::read_dir(session.path())?.flatten() {
            let path = seg.path();
            // The one being written may not have its header on disk yet.
            if (path == keep || is_segment(&path))
                && let Ok(meta) = seg.metadata()
            {
                found.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path, meta.len()));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Deletes the oldest segments until this program's recordings under `root`
/// fit in `cap`. The segment being written is never deleted, and neither is
/// anything this program did not write. Returns what was removed.
pub fn enforce_cap(root: &Path, cap: u64, keep: &Path) -> io::Result<Vec<PathBuf>> {
    let all = segments(root, keep)?;
    let mut total: u64 = all.iter().map(|(_, _, n)| n).sum();
    let mut removed = Vec::new();
    for (_, path, len) in all {
        if total <= cap {
            break;
        }
        if path == keep {
            continue;
        }
        fs::remove_file(&path)?;
        total -= len;
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir(dir); // only succeeds once the session is empty
        }
        removed.push(path);
    }
    Ok(removed)
}

// ───────────────────────────── reading ─────────────────────────────

#[derive(Debug, Default)]
pub struct Recording {
    /// The first segment header found, if the file has one. The output of
    /// `sample --format jsonl` is a valid recording without it.
    pub header: Option<SegmentRec>,
    pub records: Vec<Record>,
    /// Lines that could not be used, in words. Never fatal.
    pub warnings: Vec<String>,
    pub segments: usize,
}

/// Reads a recording: a session directory, or a single `.jsonl` file. Every
/// complete, readable line is used; a truncated last line (a crash, a full
/// disk) and lines from a newer schema are skipped with a warning.
pub fn read(path: &Path) -> io::Result<Recording> {
    let mut files: Vec<PathBuf> =
        if path.is_dir() { fs::read_dir(path)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect() } else { vec![path.to_path_buf()] };
    files.sort();
    if files.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no .jsonl segments in {}", path.display())));
    }
    let mut rec = Recording { segments: files.len(), ..Recording::default() };
    let mut session_started = false;
    for file in &files {
        let name = file.file_name().map_or_else(String::new, |n| crate::sanitize::clean(&n.to_string_lossy(), 120));
        let mut reader = io::BufReader::new(fs::File::open(file)?);
        let (mut buf, mut n, mut unreadable) = (Vec::new(), 0u64, 0u64);
        let (mut warned_newer, mut warned_unreadable) = (false, false);
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            n += 1;
            let complete = buf.last() == Some(&b'\n');
            let text = String::from_utf8_lossy(&buf);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            // A line from a newer schema may not have today's shape at all,
            // so its version is read on its own before anything else.
            #[derive(serde::Deserialize)]
            struct Version {
                schema: u32,
            }
            let newer = serde_json::from_str::<Version>(text).ok().map(|v| v.schema).filter(|v| *v > SCHEMA_VERSION);
            if let Some(schema) = newer {
                unreadable += 1;
                if !warned_newer {
                    warned_newer = true;
                    rec.warnings.push(format!("{name} line {n}: schema {schema} is newer than this program reads ({SCHEMA_VERSION}); such lines are skipped"));
                }
                continue;
            }
            match parse_line(text) {
                Ok(line) => match line.record {
                    // Headers and the opening record repeat in every segment.
                    Record::Segment(h) => {
                        rec.header.get_or_insert(h);
                    }
                    Record::Session(s) if s.state == "start" && session_started => {}
                    record => {
                        session_started |= matches!(&record, Record::Session(s) if s.state == "start");
                        rec.records.push(record);
                    }
                },
                Err(_) if !complete => {
                    rec.warnings.push(format!("{name}: the last line is incomplete (the recording was cut short); it was skipped and everything before it was used"))
                }
                Err(e) => {
                    unreadable += 1;
                    if !warned_unreadable {
                        warned_unreadable = true;
                        // The parser quotes what it could not read, which is the file's text.
                        let why = crate::sanitize::clean(&e.to_string(), 200);
                        rec.warnings.push(format!("{name} line {n}: not a record this program reads ({why}); skipped"));
                    }
                }
            }
        }
        if unreadable > 1 {
            rec.warnings.push(format!("{name}: {unreadable} lines skipped in all"));
        }
    }
    Ok(rec)
}

/// One line of a recording as a record, with its text made safe to show.
///
/// A recording is a file, and a file can come from anywhere: text in it is
/// as untrusted as text off the network, and is cleaned here, where it is
/// read, the way live text is cleaned where it is produced. Our own writer
/// escapes control characters as JSON `\u` escapes and JSON forbids them
/// raw, so a line with no backslash and nothing at or above DEL has nothing
/// to clean and is parsed directly.
fn parse_line(text: &str) -> serde_json::Result<Line> {
    if !text.bytes().any(|b| b == b'\\' || b >= 0x7f) {
        return serde_json::from_str(text);
    }
    let mut value: serde_json::Value = serde_json::from_str(text)?;
    clean_value(&mut value, false);
    serde_json::from_value(value)
}

/// Cleans every string in `v`. The recorded configuration is TOML and keeps
/// its line breaks; nothing else in a record is more than one line.
fn clean_value(v: &mut serde_json::Value, multiline: bool) {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if s.chars().any(crate::sanitize::is_unsafe) {
                *s = if multiline { crate::sanitize::clean_multiline(s) } else { crate::sanitize::clean(s, usize::MAX) };
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| clean_value(item, multiline)),
        Value::Object(map) => {
            if map.keys().any(|k| k.chars().any(crate::sanitize::is_unsafe)) {
                *map = std::mem::take(map).into_iter().map(|(k, v)| (crate::sanitize::clean(&k, usize::MAX), v)).collect();
            }
            for (key, item) in map.iter_mut() {
                clean_value(item, key == "config");
            }
        }
        _ => {}
    }
}

// ───────────────────────────── replay ─────────────────────────────

/// A recording laid out for the processor: the streams it describes and its
/// records as timed messages.
pub struct Replay {
    pub session: String,
    pub infos: Vec<StreamInfo>,
    pub providers: Vec<ProviderMeta>,
    pub streams: Vec<StreamMeta>,
    pub msgs: Vec<(u64, Msg)>,
    pub end_ns: u64,
    /// UTC of the first record that has one, for the header.
    pub started_utc: Option<String>,
}

/// When a record became known to the session it was recorded in.
fn record_time(r: &Record, last: u64) -> u64 {
    let at = match r {
        // A reply is known when it arrives and a timeout when its deadline
        // passes; anything else (a refusal, a local error) at once.
        Record::Observation(o) => match (o.recv_ns, o.sent_ns, o.outcome) {
            (Some(recv), _, _) => recv,
            (None, Some(sent), crate::model::Outcome::Timeout) => sent + u64::from(o.deadline_ms) * 1_000_000,
            (None, sent, _) => sent.unwrap_or(o.sched_ns),
        },
        Record::Gap(g) => g.end_ns,
        Record::Event(e) => e.at_ns,
        _ => last,
    };
    // Recorded order is the order of arrival; time never runs backwards.
    at.max(last)
}

/// Prepares a recording for replay. Fails only if it does not say which
/// streams it contains.
pub fn prepare(rec: Recording) -> Result<Replay, String> {
    let start = rec
        .records
        .iter()
        .find_map(|r| match r {
            Record::Session(s) if s.state == "start" => Some(s.clone()),
            _ => None,
        })
        .ok_or("the recording has no session record, so its streams are unknown")?;
    if start.streams.is_empty() {
        return Err("the recording's session record lists no streams".into());
    }
    let infos: Vec<StreamInfo> = start.streams.iter().map(StreamInfo::from).collect();
    // Providers as the header lists them; the streams add any it does not.
    let listed: Vec<ProviderRec> = rec.header.as_ref().map(|h| h.providers.clone()).unwrap_or_default();
    let (providers, streams) = crate::snapshot::metas_from(&start.streams, &listed, &|id| start.streams.iter().position(|s| &*s.stream == id));

    let started_utc = rec.records.iter().find_map(|r| match r {
        Record::Session(s) => Some(s.utc.clone()),
        _ => None,
    });
    let mut last = 0;
    let msgs: Vec<(u64, Msg)> = rec
        .records
        .into_iter()
        .map(|r| {
            last = record_time(&r, last);
            (last, Msg::Record(r))
        })
        .collect();
    Ok(Replay { session: start.session.to_string(), infos, providers, streams, end_ns: last, msgs, started_utc })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_with_binary_and_decimal_units() {
        assert_eq!(parse_size("64MiB"), Ok(64 << 20));
        assert_eq!(parse_size("1 GiB"), Ok(1 << 30));
        assert_eq!(parse_size("200MB"), Ok(200_000_000));
        assert_eq!(parse_size("4096"), Ok(4096));
        assert!(parse_size("lots").is_err() && parse_size("5 furlongs").is_err() && parse_size("99999999999999GiB").is_err());
    }

    #[test]
    fn home_is_expanded_only_as_a_leading_component() {
        assert_eq!(expand_home("~/.local/state/x", Some("/home/me")), PathBuf::from("/home/me/.local/state/x"));
        assert_eq!(expand_home("/var/x/~/y", Some("/home/me")), PathBuf::from("/var/x/~/y"));
        assert_eq!(expand_home("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn the_scrubber_removes_a_secret_wherever_it_is_and_ignores_trivial_ones() {
        let s = Scrubber::new(["abc123".to_string(), "ab".to_string(), String::new()]);
        assert_eq!(
            s.scrub(r#"{"url":"https://dns.nextdns.io/abc123","sni":"abc123.dns.nextdns.io","note":"ab"}"#),
            format!(r#"{{"url":"https://dns.nextdns.io/{REDACTED}","sni":"{REDACTED}.dns.nextdns.io","note":"ab"}}"#)
        );
        assert!(matches!(s.scrub("nothing here"), Cow::Borrowed(_)));
        assert!(!Scrubber::default().is_active());
    }
}

//! The interactive commands: `run` (live) and `demo` (synthetic scenarios).
//!
//! Three threads, as designed: the collector owns sockets and timers; the
//! processor owns history and builds snapshots; this (main) thread draws them
//! and reads the keyboard. The renderer can stall on a slow terminal without
//! touching a measurement.

use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pathwatch_core::capability;
use pathwatch_core::clock::{Clock, SystemClock, utc_string};
use pathwatch_core::collector::{self, PauseSwitch};
use pathwatch_core::config::{Endpoint, Loaded, Plan, PlanState, real_env};
use pathwatch_core::engine::HistoryLimits;
use pathwatch_core::model::{EventRec, Family, Msg, Protocol, Record};
use pathwatch_core::processor::Processor;
use pathwatch_core::script::{self, Playback};
use pathwatch_core::snapshot::{ProviderMeta, Snapshot, StreamMeta, StreamState, ViewSpec};
use pathwatch_tui::theme::{ColorMode, Glyphs, Theme};
use pathwatch_tui::ui::{Chrome, Session};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::recording;
use crate::{EXIT_IO, EXIT_USAGE, spawn_collector};

/// How often the processor publishes a fresh snapshot. Collection cadence does
/// not depend on this, and neither do the numbers.
const SNAPSHOT_EVERY: Duration = Duration::from_millis(100);

/// The same for a frozen view, which has nothing new to show but the clock
/// and the count of what has arrived since. Collection is not slowed: what
/// the collector sends waits in its channel for at most this long.
const FROZEN_EVERY: Duration = Duration::from_secs(1);

pub struct Look {
    pub color: String,
    pub ascii: bool,
    /// Capture the mouse. Off unless asked for.
    pub mouse: bool,
    /// The palette by name; `None` leaves it to the configuration, or to
    /// the default where there is none.
    pub theme: Option<String>,
    pub reduced_motion: bool,
}

impl Look {
    fn theme(&self) -> Theme {
        let color = match self.color.as_str() {
            "never" => ColorMode::Mono,
            "truecolor" => ColorMode::True,
            "256" => ColorMode::C256,
            "16" => ColorMode::C16,
            // `always` keeps colour even when NO_COLOR is set.
            "always" => Theme::detect_color(&|k| if k == "NO_COLOR" { None } else { real_env(k) }),
            _ => Theme::detect_color(&real_env),
        };
        let color = match self.theme.as_deref() {
            Some("mono") => ColorMode::Mono,
            // Where colour was refused — NO_COLOR, `--color never` — that stands.
            Some("high-contrast") if color != ColorMode::Mono => ColorMode::HighContrast,
            _ => color,
        };
        Theme { color, glyphs: if self.ascii { Glyphs::Ascii } else { Glyphs::Unicode } }
    }
}

/// Interactive commands need a real terminal. Without one they say so and
/// never write escape sequences into a pipe.
fn require_terminal(command: &str) -> Result<(), ExitCode> {
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        say!("pathwatch: `{command}` needs a terminal on standard input and output. For headless measurement use `pathwatch sample`.");
        return Err(ExitCode::from(EXIT_USAGE));
    }
    if std::env::var("TERM").is_ok_and(|t| t == "dumb") {
        say!("pathwatch: TERM=dumb cannot show the dashboard. Use `pathwatch sample --format summary`.");
        return Err(ExitCode::from(EXIT_USAGE));
    }
    Ok(())
}

/// Where the processor gets its data and its sense of "now".
enum Source {
    Demo {
        play: Box<Playback>,
        started: Instant,
        preroll_ns: u64,
        speed: f64,
    },
    Live {
        rx: mpsc::Receiver<Msg>,
        clock: Arc<dyn Clock>,
        processor: Box<Processor>,
        providers: Vec<ProviderMeta>,
        streams: Vec<StreamMeta>,
        session: Arc<str>,
        recorder: Option<Arc<recording::Handle>>,
        /// Requests to read the configuration again (the `R` key, `SIGHUP`).
        reloads: std::sync::mpsc::Receiver<()>,
        reloader: Box<crate::reload::Reloader>,
    },
}

impl Source {
    fn snapshot(&mut self, view: &ViewSpec) -> Snapshot {
        match self {
            Source::Demo { play, started, preroll_ns, speed } => {
                let now = (*preroll_ns + (started.elapsed().as_nanos() as f64 * *speed) as u64).min(play.end_ns());
                play.advance_to(now);
                play.snapshot(view, now)
            }
            Source::Live { rx, clock, processor, providers, streams, session, recorder, reloads, reloader } => {
                // However many times it was asked for, one reload. A refusal
                // is an event; a reload that is applied is announced by the
                // collector itself, as a new revision.
                if reloads.try_iter().count() > 0
                    && let Err(why) = reloader.reload()
                {
                    let refused = Record::Event(EventRec {
                        session: session.clone(),
                        at_ns: clock.mono_ns(),
                        utc: utc_string(clock.wall()),
                        event: "config_reload_failed".into(),
                        stream: None,
                        detail: crate::reload::refused(&why),
                    });
                    if let Some(rec) = &recorder {
                        rec.offer(&refused);
                    }
                    processor.ingest(&Msg::Record(refused));
                }
                while let Ok(msg) = rx.try_recv() {
                    // Measurement first; the recorder only ever gets a copy.
                    processor.ingest(&msg);
                    // A new revision changes which cards and streams there are.
                    if let Msg::Record(Record::Config(c)) = &msg {
                        let engine = &processor.engine;
                        (*providers, *streams) = pathwatch_core::snapshot::metas_from(&c.streams, &c.providers, &|id| engine.stream_index(id));
                    }
                    if let (Some(rec), Msg::Record(record)) = (&recorder, &msg) {
                        rec.offer(record);
                    }
                }
                // A recorder that stopped says so once, in the event list.
                if let Some(why) = recorder.as_ref().and_then(|r| r.status.take_notice()) {
                    processor.ingest(&Msg::Record(Record::Event(EventRec {
                        session: session.clone(),
                        at_ns: clock.mono_ns(),
                        utc: utc_string(clock.wall()),
                        event: "recording_stopped".into(),
                        stream: None,
                        detail: format!("recording stopped: {why}. Measurement continues; restart to record again"),
                    })));
                }
                processor.snapshot(providers, streams, view, clock.mono_ns())
            }
        }
    }
}

/// Runs the processor and the dashboard until the user quits. `pause` is the
/// collector's stop/resume switch, absent when there is no collector.
/// Starts the route-trace service. It ends when the dashboard drops its link.
fn spawn_tracer(budget: pathwatch_core::trace::Budget, reverse_dns: bool, clock: Arc<dyn Clock>) -> pathwatch_tui::app::TraceLink {
    use pathwatch_core::probe::icmp::RealIcmpIo;
    use pathwatch_core::trace;
    let (requests, rx) = tokio::sync::mpsc::unbounded_channel();
    let (shown, view) = watch::channel(None);
    let names: Option<trace::NameFn> = reverse_dns.then(|| Arc::new(pathwatch_core::localnet::reverse_name) as trace::NameFn);
    let spawned = std::thread::Builder::new().name("tracer".into()).spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        // Each trace opens a socket of its own, so the measurement sockets
        // are never touched and never share a TTL with a trace.
        let open = {
            let clock = clock.clone();
            move |family| RealIcmpIo::open(family, clock.clone())
        };
        rt.block_on(trace::serve(rx, open, budget, clock, names, shown));
    });
    // Without the thread the link still exists; requests simply go nowhere.
    drop(spawned);
    pathwatch_tui::app::TraceLink { requests, view }
}

/// What only a live session has: a collector to stop and a file to reload.
struct Live {
    pause: PauseSwitch,
    reload: std::sync::mpsc::Sender<()>,
    trace: pathwatch_tui::app::TraceLink,
}

/// Whether standard input is still a working terminal. After the terminal
/// is closed it reports a hang-up, and its settings can no longer be read;
/// either is taken as gone.
fn terminal_is_there() -> bool {
    let mut fds = [libc::pollfd { fd: libc::STDIN_FILENO, events: 0, revents: 0 }];
    // SAFETY: one valid pollfd and a zero timeout; an all-zero termios is a
    // valid value to be overwritten, and tcgetattr only writes into it.
    unsafe {
        let hung_up = libc::poll(fds.as_mut_ptr(), 1, 0) > 0 && fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
        let mut t: libc::termios = std::mem::zeroed();
        !hung_up && libc::tcgetattr(libc::STDIN_FILENO, &mut t) == 0
    }
}

/// The source is handed back so that a live session's last records — those the
/// collector produces while shutting down — can still be taken from it.
fn show(mut source: Source, theme: Theme, chrome: Chrome, live: Option<Live>, panic_after: Option<Duration>) -> (io::Result<()>, Option<Source>) {
    let controls = live.map_or_else(pathwatch_tui::app::Controls::default, |l| pathwatch_tui::app::Controls { pause: Some(l.pause), reload: Some(l.reload), trace: Some(l.trace) });
    let reload = controls.reload.clone();
    let (view_tx, view_rx) = watch::channel(ViewSpec::default());
    let (snap_tx, snap_rx) = watch::channel(Arc::new(source.snapshot(&ViewSpec::default())));
    let stop = Arc::new(AtomicBool::new(false));
    let stop_processor = stop.clone();
    // Wakes the processor before its time: when the view changes, so the
    // screen follows a key at once however slowly it is ticking, and when
    // the session ends, so leaving does not wait for a tick.
    let (nudge, nudged) = std::sync::mpsc::channel::<()>();
    let mut view_changes = view_tx.subscribe();
    let processor = std::thread::Builder::new()
        .name("processor".into())
        .spawn(move || {
            while !stop_processor.load(Ordering::Relaxed) {
                let view = view_rx.borrow().clone();
                snap_tx.send_replace(Arc::new(source.snapshot(&view)));
                let wait = if view.frozen_at_ns.is_some() { FROZEN_EVERY } else { SNAPSHOT_EVERY };
                if nudged.recv_timeout(wait) == Err(std::sync::mpsc::RecvTimeoutError::Disconnected) {
                    // Nobody is left to wake it: keep time by the clock alone.
                    std::thread::sleep(wait);
                }
                while nudged.try_recv().is_ok() {}
            }
            source
        })
        .expect("processor thread");

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("dashboard runtime");
    // `init` enters raw mode and the alternate screen and installs a panic
    // hook that restores the terminal before anything is printed.
    let mut terminal = ratatui::init();
    let mouse = chrome.mouse;
    if mouse {
        // That hook knows nothing of the mouse, so one goes in front of it:
        // a terminal left reporting clicks to the shell is not restored.
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = pathwatch_tui::mouse::capture(false);
            earlier(info);
        }));
        let _ = pathwatch_tui::mouse::capture(true);
    }
    let result = rt.block_on(async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        let forced_panic = async {
            match panic_after {
                Some(after) => tokio::time::sleep(after).await,
                None => std::future::pending().await,
            }
        };
        let app = pathwatch_tui::app::run(&mut terminal, theme, chrome, snap_rx, view_tx, controls);
        tokio::pin!(app, forced_panic);
        loop {
            tokio::select! {
                r = &mut app => break r,
                _ = term.recv() => break Ok(()),
                Ok(()) = view_changes.changed() => {
                    let _ = nudge.send(());
                }
                _ = hup.recv() => match &reload {
                    // A hang-up with the terminal still there is the usual
                    // request to re-read configuration; with the terminal
                    // gone it means what it says, and so does a session
                    // that has no configuration to re-read.
                    Some(r) if terminal_is_there() => {
                        let _ = r.send(());
                        let _ = nudge.send(());
                    }
                    _ => break Ok(()),
                },
                _ = &mut forced_panic => panic!("forced panic (--panic-after-ms): the terminal should be usable after this message"),
            }
        }
    });
    // Restore the terminal first, on every path out. Every step is allowed
    // to fail in silence: the terminal may have been closed, and the
    // library's own way of reporting a failure is to print to it — which
    // would end the program here, with a recording still to close.
    if mouse {
        let _ = pathwatch_tui::mouse::capture(false);
    }
    let shown = terminal.show_cursor().is_ok();
    let restored = ratatui::try_restore().is_ok();
    if shown && restored {
        drop(terminal);
    } else {
        // Dropping it would try the cursor again and print the failure.
        std::mem::forget(terminal);
    }
    stop.store(true, Ordering::Relaxed);
    let _ = nudge.send(());
    // A processor that died took the snapshots with it: that is a failure,
    // not a clean exit, even though the terminal has been restored.
    match processor.join() {
        Ok(source) => (result, Some(source)),
        Err(_) => (Err(io::Error::other("the processor thread panicked")), None),
    }
}

pub fn demo(scenario: Option<String>, list: bool, speed: f64, at_s: u64, panic_after: Option<Duration>, look: Look) -> ExitCode {
    if list {
        for sc in script::all() {
            println!("{:<20} {}", sc.name, sc.shows);
        }
        return ExitCode::SUCCESS;
    }
    let name = scenario.unwrap_or_else(|| "isolated-failure".into());
    let Some(sc) = script::by_name(&name) else {
        say!("pathwatch: no scenario named {name:?}. `pathwatch demo --list` shows them.");
        return ExitCode::from(EXIT_USAGE);
    };
    if let Err(code) = require_terminal("demo") {
        return code;
    }
    let play = Playback::new(sc, 7);
    let chrome = Chrome {
        session: Session::Demo { name, end_ns: play.end_ns() },
        source: "no network activity".into(),
        notes: vec![format!("{speed}× speed")],
        recording: None,
        trace_budget: None,
        mouse: look.mouse,
        reduced_motion: look.reduced_motion,
    };
    let source = Source::Demo { play: Box::new(play), started: Instant::now(), preroll_ns: at_s * 1_000_000_000, speed: speed.max(0.1) };
    finish(show(source, look.theme(), chrome, None, panic_after).0)
}

/// Plays a recording through the dashboard. Reads files only: nothing is sent.
pub fn replay(path: &std::path::Path, speed: Option<f64>, at_s: u64, look: Look) -> ExitCode {
    let recording = match pathwatch_core::recorder::read(path) {
        Ok(r) => r,
        Err(e) => {
            say!("pathwatch: cannot read {}: {e}", path.display());
            return ExitCode::from(EXIT_IO);
        }
    };
    // Said before the screen is taken over, and again in the header.
    for w in &recording.warnings {
        say!("pathwatch: warning: {w}");
    }
    let warnings = recording.warnings.len();
    let replay = match pathwatch_core::recorder::prepare(recording) {
        Ok(r) => r,
        Err(e) => {
            say!("pathwatch: {} cannot be replayed: {e}", path.display());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    if let Err(code) = require_terminal("replay") {
        return code;
    }
    let name = replay.session.clone();
    let mut notes = vec![speed.map_or_else(|| "shown complete".to_string(), |s| format!("{s}× speed"))];
    if warnings > 0 {
        notes.push(format!("{warnings} warning(s) on reading; see the terminal after quitting"));
    }
    let play = Playback::of_recording(replay);
    let end_ns = play.end_ns();
    let chrome = Chrome {
        session: Session::Replay { name, end_ns },
        source: "from a recording".into(),
        notes,
        recording: None,
        trace_budget: None,
        mouse: look.mouse,
        reduced_motion: look.reduced_motion,
    };
    // No speed means "show it whole": start at the end.
    let (preroll_ns, speed) = match speed {
        Some(s) => (at_s * 1_000_000_000, s.max(0.1)),
        None => (end_ns, 1.0),
    };
    finish(show(Source::Demo { play: Box::new(play), started: Instant::now(), preroll_ns, speed }, look.theme(), chrome, None, None).0)
}

fn finish(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            say!("pathwatch: terminal error: {e}");
            ExitCode::from(1)
        }
    }
}

/// Describes the plan to the dashboard, with what this host cannot do.
fn metas(plan: &Plan, icmp_unavailable: &dyn Fn(Family) -> Option<String>) -> (Vec<ProviderMeta>, Vec<StreamMeta>) {
    let streams = plan
        .streams
        .iter()
        .enumerate()
        .map(|(idx, s)| StreamMeta {
            idx,
            provider: s.provider.clone(),
            protocol: s.protocol,
            family: s.family,
            fresh: s.fresh,
            configured: s.configured.clone(),
            state: match (&s.state, s.protocol, icmp_unavailable(s.family)) {
                (PlanState::Collecting, Protocol::Icmp, Some(why)) => StreamState::Unsupported(why),
                (PlanState::Collecting, _, _) => StreamState::Collecting,
                (PlanState::NeedsConfig(r), _, _) => StreamState::NeedsConfig(r.clone()),
                (PlanState::Unsupported(r), _, _) => StreamState::Unsupported(r.clone()),
                (PlanState::Disabled, _, _) => StreamState::Disabled,
            },
        })
        .collect();
    let providers = plan
        .providers
        .iter()
        .map(|p| ProviderMeta {
            key: p.key.clone(),
            name: p.name.clone(),
            slot: p.slot,
            needs_config: matches!(p.state, PlanState::NeedsConfig(_)).then(|| p.state.reason().unwrap_or("no target configured").to_string()),
            disabled: p.state == PlanState::Disabled,
        })
        .collect();
    (providers, streams)
}

pub fn run(loaded: Loaded, look: Look, record: Option<String>, config_flag: Option<std::path::PathBuf>) -> ExitCode {
    if let Err(code) = require_terminal("run") {
        return code;
    }
    // Command-line flags win; otherwise the configuration file decides.
    let look = Look {
        color: if look.color == "auto" { loaded.config.ui.color.clone() } else { look.color },
        ascii: look.ascii || loaded.config.ui.glyphs == "ascii",
        mouse: look.mouse || loaded.config.ui.mouse,
        theme: look.theme.or_else(|| Some(loaded.config.ui.theme.clone())),
        reduced_motion: look.reduced_motion || loaded.config.ui.reduced_motion,
    };
    let plan = crate::plan_for(&loaded);
    let caps = capability::probe();
    let icmp_unavailable = |f: Family| caps.icmp.iter().find(|c| c.family == f && !c.available).map(|c| c.detail.clone());
    let (providers, streams) = metas(&plan, &icmp_unavailable);

    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let (tx, rx) = mpsc::channel::<Msg>(4096);
    let cancel = CancellationToken::new();
    let limits = HistoryLimits { samples: loaded.config.history.samples.0, records: loaded.config.history.records.0, events: 10_000 };
    let processor = Processor::with_incidents(collector::stream_infos(&plan), limits, (&loaded.config.incidents).into());
    let source_ip = plan.collecting().find_map(|s| match &s.endpoint {
        Endpoint::Ip(ip) if s.family == Family::V4 => pathwatch_core::probe::source_for(*ip).and_then(|src| src.ip),
        _ => None,
    });
    // A recording that was asked for and cannot be made is an error now, not
    // a surprise after an hour of measuring.
    let session = crate::new_session_id();
    let recorder = match recording::directory(&loaded, record.as_deref()) {
        None => None,
        Some(dir) => match recording::start(&loaded, &plan, dir.clone(), session.clone(), clock.clone(), Box::new(pathwatch_core::recorder::Files)) {
            Ok(handle) => Some(Arc::new(handle)),
            Err(e) => {
                say!("pathwatch: cannot record to {}: {e}", dir.display());
                return ExitCode::from(EXIT_IO);
            }
        },
    };
    // Route tracing: a service of its own, on its own thread and sockets,
    // that does nothing until asked and runs one trace at a time.
    let trace_budget = pathwatch_core::trace::Budget::from_config(&loaded.config.trace);
    let trace = spawn_tracer(trace_budget.clone(), loaded.config.trace.reverse_dns, clock.clone());
    let chrome = Chrome {
        recording: recorder.as_ref().map(|r| r.status.clone()),
        trace_budget: Some(trace_budget.describe()),
        mouse: look.mouse,
        reduced_motion: look.reduced_motion,
        session: Session::Live,
        source: source_ip.map_or_else(|| "source address unknown".into(), |ip| format!("via {ip}")),
        notes: vec![if caps.icmp.iter().any(|c| c.available) { "ICMP: unprivileged datagram socket, kernel-stamped".into() } else { "ICMP unsupported on this host".into() }],
    };
    let pause = PauseSwitch::default();
    let (reloader, reload_rx) = crate::reload::Reloader::new(config_flag, loaded.config.clone());
    let (reload_tx, reloads) = std::sync::mpsc::channel();
    let collector_thread =
        spawn_collector(&loaded, plan, clock.clone(), tx, cancel.clone(), crate::Wiring { session: session.clone(), pause: pause.clone(), reload: Some(reload_rx) });
    let source = Source::Live { rx, clock, processor: Box::new(processor), providers, streams, session, recorder: recorder.clone(), reloads, reloader: Box::new(reloader) };
    let (result, source) = show(source, look.theme(), chrome, Some(Live { pause, reload: reload_tx, trace }), None);
    // Orderly shutdown: stop scheduling, let pending probes finalise as cancelled.
    cancel.cancel();
    // What the collector says while it shuts down — probes cancelled in
    // flight, its counters, the session's closing record — belongs in the
    // recording too. The channel closes when the collector is done.
    if let Some(Source::Live { mut rx, recorder: Some(rec), .. }) = source {
        while let Some(msg) = rx.blocking_recv() {
            if let Msg::Record(record) = &msg {
                rec.offer(record);
            }
        }
    }
    let _ = collector_thread.join();
    let code = finish(result);
    // The dashboard is gone, so this is on the normal screen.
    let Some(handle) = recorder.and_then(Arc::into_inner) else { return code };
    let status = handle.status.clone();
    handle.finish();
    recording::report(&status).unwrap_or(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(color: &str, theme: Option<&str>) -> Look {
        Look { color: color.into(), ascii: false, mouse: false, theme: theme.map(str::to_string), reduced_motion: false }
    }

    #[test]
    fn the_palette_is_chosen_by_name_and_never_overrides_a_refusal_of_colour() {
        assert_eq!(look("16", None).theme().color, ColorMode::C16);
        assert_eq!(look("16", Some("default")).theme().color, ColorMode::C16);
        assert_eq!(look("16", Some("high-contrast")).theme().color, ColorMode::HighContrast);
        assert_eq!(look("truecolor", Some("high-contrast")).theme().color, ColorMode::HighContrast);
        assert_eq!(look("truecolor", Some("mono")).theme().color, ColorMode::Mono);
        // `--color never`, like NO_COLOR, is a refusal of colour: it stands.
        assert_eq!(look("never", Some("high-contrast")).theme().color, ColorMode::Mono);
    }
}

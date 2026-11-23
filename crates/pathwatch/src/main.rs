//! `pathwatch` — command-line entry point.
//!
//! Headless commands: `sample`, `doctor`, `config`. Interactive: `run` (live)
//! and `demo` (synthetic scenarios, no network activity).
//!
//! A successful exit means the *process* worked. It says nothing about the
//! network. Exit codes: 0 ran normally · 1 internal error · 2 usage ·
//! 3 invalid configuration · 4 no stream could be started · 5 output I/O
//! error · 10 a `sample --assert` check did not hold · 130/143 ended by
//! SIGINT/SIGTERM.

/// As `eprintln!`, for what is said on the way out of a session. Standard
/// error may be a terminal that has just been closed; that is a reason to
/// say nothing, not to panic before the recording has been closed.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

mod assert;
mod dashboard;
mod man;
mod recording;
mod reload;
mod summary;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use pathwatch_core::capability;
use pathwatch_core::clock::{Clock, SystemClock};
use pathwatch_core::collector::{self, PauseSwitch, Setup};
use pathwatch_core::config::{ConfigError, Dur, Loaded, Origin, Plan, PlanState, real_env};
use pathwatch_core::engine::{Engine, HistoryLimits};
use pathwatch_core::model::{CollectorCounters, Family, Line, Msg, Protocol, Record};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const EXIT_USAGE: u8 = 2;
const EXIT_CONFIG: u8 = 3;
const EXIT_NO_STREAMS: u8 = 4;
const EXIT_IO: u8 = 5;
const EXIT_ASSERT: u8 = 10;
const EXIT_SIGINT: u8 = 130;
const EXIT_SIGTERM: u8 = 143;

const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\n",
    "backends: icmp-dgram (unprivileged datagram socket), dns-udp with TCP fallback, dns-tcp, dot and doh-h2 (hyper + rustls, OS trust store)\n",
    "timestamps: ICMP round trips end at the kernel receive stamp (kernel-rx); DNS, DoT and DoH at this process's read (userspace-monotonic)\n",
    "not in this build: source binding, raw-socket ICMP, retries, DoH by GET or through a proxy, UDP or TCP route tracing"
);

/// The configuration reference, as shipped in `docs/`.
const CONFIG_REFERENCE: &str = include_str!("../../../docs/configuration.md");

/// Writes text that a reader may stop reading early, such as `head`.
fn print_text(text: &str) -> ExitCode {
    match io::stdout().lock().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pathwatch: output error: {e}");
            ExitCode::from(EXIT_IO)
        }
    }
}

#[derive(Parser)]
#[command(name = "pathwatch", version, long_version = LONG_VERSION, about = "Continuously measures four DNS/network providers.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive dashboard. With the built-in configuration this probes Cloudflare and Google
    Run(RunArgs),
    /// The dashboard on built-in synthetic scenarios. No network activity
    Demo(DemoArgs),
    /// Headless collection: observations as JSONL, a summary table, or plain sentences
    Sample(SampleArgs),
    /// Play a recording through the dashboard. Reads files; sends nothing
    Replay(ReplayArgs),
    /// Convert a recording to JSONL, CSV, or a summary. Reads files; sends nothing
    Export(ExportArgs),
    /// Reports what this host permits. Opens sockets; sends nothing unless --probe
    Doctor(DoctorArgs),
    /// Inspect and validate configuration. No network activity
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// What every word and number on screen means, as plain text. No network activity
    Glossary {
        /// As a Markdown document instead
        #[arg(long)]
        markdown: bool,
    },
    /// The manual page, as roff: `pathwatch man | man -l -` on Linux, or save it as pathwatch.1
    Man,
}

#[derive(Args)]
struct ConfigOpt {
    /// Configuration file (default: $PATHWATCH_CONFIG, then ~/.config/pathwatch/config.toml if it exists)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum SampleFormat {
    Jsonl,
    /// A fixed-width table, one row per stream
    Summary,
    /// The same numbers as sentences: for a screen reader, or to read line by line
    Plain,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum Mode {
    Icmp,
    Dns,
    DnsTcp,
    Doh,
    Dot,
}

#[derive(Args, Default)]
struct LookArgs {
    /// Colour: auto follows the terminal and NO_COLOR
    #[arg(long, value_parser = ["auto", "always", "never"], default_value = "auto")]
    color: String,
    /// Use only ASCII characters
    #[arg(long)]
    ascii: bool,
    /// Capture the mouse: click to focus or place the cursor, wheel to scroll or change the window. Selecting text then needs your terminal's modifier key
    #[arg(long)]
    mouse: bool,
    /// Palette: high-contrast is bright bold colours and white on black, with nothing dimmed; mono is no colour
    #[arg(long, value_parser = ["default", "high-contrast", "mono"])]
    theme: Option<String>,
    /// Redraw for new data at most four times a second; keys are still answered at once
    #[arg(long)]
    reduced_motion: bool,
}

impl LookArgs {
    fn look(self) -> dashboard::Look {
        dashboard::Look { color: self.color, ascii: self.ascii, mouse: self.mouse, theme: self.theme, reduced_motion: self.reduced_motion }
    }
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    config: ConfigOpt,
    #[command(flatten)]
    look: LookArgs,
    #[command(flatten)]
    record: RecordOpt,
}

#[derive(Args, Default)]
struct RecordOpt {
    /// Record the session as JSONL segments under DIR/<session id>/ (default DIR: [recording] directory)
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = "")]
    record: Option<String>,
}

#[derive(Args)]
struct ReplayArgs {
    /// A session directory, or one .jsonl file (a segment, or the output of `sample`)
    path: PathBuf,
    /// Play at this multiple of real time. Without it the whole session is shown at once
    #[arg(long)]
    speed: Option<f64>,
    /// With --speed: start this many seconds into the session
    #[arg(long, default_value_t = 0)]
    at: u64,
    #[command(flatten)]
    look: LookArgs,
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum ExportFormat {
    Jsonl,
    Csv,
    Summary,
}

#[derive(Args)]
struct ExportArgs {
    /// A session directory, or one .jsonl file
    path: PathBuf,
    #[arg(long, value_enum, default_value = "jsonl")]
    format: ExportFormat,
    /// Write here instead of standard output
    #[arg(long)]
    output: Option<PathBuf>,
    /// Also remove this host's own addresses, its interface names, and its gateway and resolver
    #[arg(long)]
    redact_local: bool,
    /// Leave the records exactly as recorded. Profile ids are removed by default
    #[arg(long)]
    no_redact: bool,
}

#[derive(Args)]
struct DemoArgs {
    /// Scenario to play (default: isolated-failure)
    scenario: Option<String>,
    /// List the scenarios and exit
    #[arg(long)]
    list: bool,
    /// Playback speed
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    /// Start this many seconds into the scenario
    #[arg(long, default_value_t = 50)]
    at: u64,
    /// Panic after this many milliseconds: for testing that the terminal is restored
    #[arg(long, hide = true)]
    panic_after_ms: Option<u64>,
    #[command(flatten)]
    look: LookArgs,
}

#[derive(Args)]
struct SampleArgs {
    #[command(flatten)]
    config: ConfigOpt,
    /// Stop after this long, e.g. 30s, 5m (default: run until interrupted)
    #[arg(long, value_parser = parse_dur)]
    duration: Option<Dur>,
    /// Stop after this many finalised probes in total
    #[arg(long)]
    count: Option<u64>,
    /// What to write: one JSON record per line as it happens, a summary table at the end, or that summary as plain sentences
    #[arg(long, value_enum, default_value = "jsonl")]
    format: SampleFormat,
    /// Write records here instead of standard output
    #[arg(long)]
    output: Option<PathBuf>,
    /// Restrict to one address family
    #[arg(long, value_parser = ["4", "6"])]
    family: Option<String>,
    /// Restrict to one measurement mode
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    /// Statistics window for the summary
    #[arg(long, value_parser = parse_dur, default_value = "5m")]
    window: Dur,
    /// Do not print the summary to standard error after a JSONL run
    #[arg(long)]
    quiet: bool,
    /// Check the sample when it ends and exit 10 if a check does not hold:
    /// "[<streams>:] <metric> <comparison> <value>", e.g. "no-reply <= 1%",
    /// "cloudflare: p50 < 40ms", "*/dns-udp/4: replied >= 3". Repeatable.
    /// Metrics: sent, replied, no-reply, error-responses, local-errors,
    /// skipped (counts; the two in the middle also as %), and min, mean,
    /// p50, p95, p99, max (ms). A check with too little data does not pass.
    /// Needs --duration or --count
    #[arg(long = "assert", value_name = "EXPR")]
    assert: Vec<String>,
    #[command(flatten)]
    record: RecordOpt,
}

#[derive(Args)]
struct DoctorArgs {
    /// The report as JSON
    #[arg(long)]
    json: bool,
    /// Also send three echo requests to this host's loopback addresses
    #[arg(long)]
    probe: bool,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Validate the configuration and list every problem
    Check(ConfigOpt),
    /// Print the effective configuration
    Show {
        #[command(flatten)]
        config: ConfigOpt,
        /// One line per setting with where it came from, plus the stream plan
        #[arg(long)]
        effective: bool,
        #[arg(long, value_parser = ["toml", "json"], default_value = "toml")]
        format: String,
        /// Show secrets (NextDNS profile ids) instead of masking them
        #[arg(long)]
        show_secrets: bool,
    },
    /// Print the configuration file path in use
    Path(ConfigOpt),
    /// Print a commented example configuration
    Example,
    /// Explain every setting: its default, what it does, and whether a reload applies it
    Reference,
}

fn parse_dur(s: &str) -> Result<Dur, String> {
    Dur::parse(s)
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // Help and version are not errors; anything else is a usage error.
            let code = if e.use_stderr() { EXIT_USAGE } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    let command = cli.command.unwrap_or_else(|| {
        Command::Run(RunArgs {
            config: ConfigOpt { config: None },
            look: LookArgs { color: "auto".into(), ascii: false, mouse: false, theme: None, reduced_motion: false },
            record: RecordOpt::default(),
        })
    });
    match command {
        Command::Run(args) => match load(&args.config, &[]) {
            Ok(loaded) => dashboard::run(loaded, args.look.look(), args.record.record, args.config.config.clone()),
            Err(code) => code,
        },
        Command::Demo(args) => dashboard::demo(args.scenario, args.list, args.speed, args.at, args.panic_after_ms.map(Duration::from_millis), args.look.look()),
        Command::Sample(args) => sample(args),
        Command::Replay(args) => dashboard::replay(&args.path, args.speed, args.at, args.look.look()),
        Command::Export(args) => export(args),
        Command::Doctor(args) => doctor(args),
        Command::Config { command } => config(command),
        Command::Glossary { markdown } => print_text(&if markdown { pathwatch_core::glossary::markdown() } else { pathwatch_core::glossary::text() }),
        Command::Man => {
            use clap::CommandFactory;
            let mut root = Cli::command();
            root.build();
            print_text(&man::page(&root, env!("CARGO_PKG_VERSION")))
        }
    }
}

// ───────────────────────────── configuration ─────────────────────────────

fn default_config_path() -> Option<PathBuf> {
    use etcetera::BaseStrategy;
    let base = etcetera::choose_base_strategy().ok()?;
    Some(base.config_dir().join("pathwatch").join("config.toml"))
}

/// Resolves which configuration file to use, if any: the flag, then
/// `PATHWATCH_CONFIG`, then the default path when that file exists.
fn config_path(opt: &ConfigOpt) -> Option<PathBuf> {
    opt.config.clone().or_else(|| real_env("PATHWATCH_CONFIG").map(PathBuf::from)).or_else(|| default_config_path().filter(|p| p.exists()))
}

fn load(opt: &ConfigOpt, overrides: &[(String, toml::Value)]) -> Result<Loaded, ExitCode> {
    let path = config_path(opt);
    match Loaded::load(path.as_deref(), overrides) {
        Ok(l) => {
            for w in &l.warnings {
                eprintln!("pathwatch: warning: {w}");
            }
            Ok(l)
        }
        Err(ConfigError::Invalid(problems)) => {
            eprintln!("pathwatch: configuration is invalid — {} problem(s):", problems.len());
            for p in &problems {
                eprintln!("  {p}");
            }
            Err(ExitCode::from(EXIT_CONFIG))
        }
        Err(e) => {
            eprintln!("pathwatch: {e}");
            Err(ExitCode::from(EXIT_CONFIG))
        }
    }
}

fn describe_plan(plan: &Plan, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "providers:")?;
    for p in &plan.providers {
        let reason = p.state.reason().map(|r| format!(" — {r}")).unwrap_or_default();
        writeln!(out, "  {} {:<12} {}{}", p.slot, p.name, p.state.label(), reason)?;
    }
    writeln!(out, "streams:")?;
    for s in &plan.streams {
        let reason = s.state.reason().map(|r| format!(" — {r}")).unwrap_or_default();
        writeln!(out, "  {:<34} {:<12} every {:<5} deadline {:<5} {}{}", s.id, s.state.label(), Dur(s.interval).to_string(), Dur(s.deadline).to_string(), s.configured, reason)?;
    }
    writeln!(out, "steady-state rate: {:.2} probe transactions per second", plan.steady_rate)
}

fn config(cmd: ConfigCommand) -> ExitCode {
    let mut out = io::stdout().lock();
    let result = (|| -> Result<io::Result<()>, ExitCode> {
        Ok(match cmd {
            ConfigCommand::Example => out.write_all(pathwatch_core::config::EXAMPLE_CONFIG.as_bytes()),
            ConfigCommand::Reference => out.write_all(CONFIG_REFERENCE.as_bytes()),
            ConfigCommand::Path(opt) => match config_path(&opt) {
                Some(p) => writeln!(out, "{}", p.display()),
                None => writeln!(
                    out,
                    "(none — using the built-in configuration; create {} to override)",
                    default_config_path().map_or_else(|| "~/.config/pathwatch/config.toml".into(), |p| p.display().to_string())
                ),
            },
            ConfigCommand::Check(opt) => {
                let l = load(&opt, &[])?;
                // With the controls, when enabled, as a session would run them.
                let plan = plan_for(&l);
                let n = plan.collecting().count();
                writeln!(out, "configuration is valid: {n} stream(s) would collect").and_then(|()| describe_plan(&plan, &mut out))
            }
            ConfigCommand::Show { config, effective, format, show_secrets } => {
                let l = load(&config, &[])?;
                if effective {
                    (|| {
                        for (key, value, origin) in l.effective_settings(show_secrets) {
                            let tag = if origin == Origin::Default { String::new() } else { format!("  # {}", origin.as_str()) };
                            writeln!(out, "{key} = {value}{tag}")?;
                        }
                        writeln!(out)?;
                        describe_plan(&l.plan(&real_env), &mut out)
                    })()
                } else if format == "json" {
                    let table: toml::Table = l.effective_toml(show_secrets).parse().expect("effective configuration parses");
                    writeln!(out, "{}", serde_json::to_string_pretty(&table).expect("a table serialises"))
                } else {
                    out.write_all(l.effective_toml(show_secrets).as_bytes())
                }
            }
        })
    })();
    match result {
        Err(code) => code,
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Ok(Err(e)) => {
            eprintln!("pathwatch: {e}");
            ExitCode::from(EXIT_IO)
        }
    }
}

// ───────────────────────────── doctor ─────────────────────────────

fn doctor(args: DoctorArgs) -> ExitCode {
    let report = capability::probe();
    let loopback = args.probe.then(|| loopback_probe(&report));
    if args.json {
        let mut v = serde_json::to_value(&report).expect("report serialises");
        if let Some(lb) = &loopback {
            v["loopback_probe"] = serde_json::to_value(lb).expect("probe result serialises");
        }
        println!("{}", serde_json::to_string_pretty(&v).expect("value serialises"));
        return ExitCode::SUCCESS;
    }
    println!("host            {} {}{}", report.os, report.arch, report.kernel.as_ref().map(|k| format!(" (kernel {k})")).unwrap_or_default());
    if report.running_as_root {
        println!("warning         running as root; pathwatch does not need it and is designed to run without");
    }
    for c in &report.icmp {
        let state = if c.available { "available" } else { "UNSUPPORTED" };
        println!("ICMP IPv{}       {state} — {}", c.family.as_str(), c.detail);
        if let Some(r) = &c.remedy {
            println!("                {r}");
        }
    }
    // What the optional controls would use, read from this host's own
    // configuration. Nothing is sent to find it.
    let local = pathwatch_core::localnet::discover();
    let found = |r: &Result<std::net::IpAddr, String>| r.as_ref().map_or_else(|why| format!("not found — {why}"), |ip| ip.to_string());
    // The source context: asking the kernel how it would route to a
    // documentation address tells which address and interface it would use.
    {
        use pathwatch_core::netwatch::{NetProbe, SystemNet};
        for (label, toward) in [("source IPv4 ", "192.0.2.1"), ("source IPv6 ", "2001:db8::1")] {
            let line = match toward.parse().ok().and_then(|d| SystemNet.source_for(d)) {
                Some(s) => {
                    format!("{}{}{}", s.ip, s.ifname.as_deref().map_or_else(String::new, |n| format!(" on {n}")), if s.looks_like_tunnel() { " (looks like a tunnel)" } else { "" })
                }
                None => "no route".to_string(),
            };
            println!("{label}    {line}");
        }
    }
    println!("gateway IPv4    {}", found(&local.gateway_v4));
    println!("gateway IPv6    {}", found(&local.gateway_v6));
    match &local.resolvers {
        Ok(list) => {
            use pathwatch_core::localnet::ResolverKind;
            let shown: Vec<String> = list
                .iter()
                .map(|ip| match ResolverKind::of(*ip) {
                    ResolverKind::Stub => format!("{ip} (a loopback stub)"),
                    ResolverKind::Local => format!("{ip} (on the local network)"),
                    ResolverKind::Remote => format!("{ip} (remote: not on the local network)"),
                })
                .collect();
            println!("system DNS      {} — from /etc/resolv.conf", shown.join(", "));
        }
        Err(why) => println!("system DNS      not found — {why}"),
    }
    if let Some(range) = &report.ping_group_range {
        println!("ping groups     net.ipv4.ping_group_range = \"{range}\"; your group ids: {:?}", report.group_ids);
    }
    println!(
        "echo identifier {}",
        if report.echo_identifier_rewritten_by_kernel { "rewritten by the kernel (replies demultiplexed per socket)" } else { "preserved (replies filtered in user space)" }
    );
    println!("ICMP errors     {}", if report.icmp_errors_via_error_queue { "read from the socket error queue" } else { "delivered on the socket" });
    match report.suspend_clock_pair {
        Some((excl, incl)) => println!("sleep detection {excl} vs {incl}"),
        None => println!("sleep detection wall-clock comparison only"),
    }
    println!("DNS over UDP    {}", report.dns_udp);
    println!("DNS over TCP    {}", report.dns_tcp);
    println!("DNS over TLS    {}", report.dot);
    println!("DNS over HTTPS  {}", report.doh);
    println!("not in build    {}", report.not_in_this_build.join(", "));
    match loopback {
        Some(results) => {
            for r in results {
                println!("loopback {:<7} {}", r.address, r.result);
            }
        }
        None => println!("\nNothing was sent. `pathwatch doctor --probe` also sends three echo requests to this host's own loopback addresses."),
    }
    ExitCode::SUCCESS
}

#[derive(serde::Serialize)]
struct LoopbackResult {
    address: &'static str,
    result: String,
}

/// Sends three echo requests each to 127.0.0.1 and ::1 through the same
/// collector the real measurements use. Nothing leaves this host.
fn loopback_probe(report: &capability::Report) -> Vec<LoopbackResult> {
    const CONFIG: &str = "[icmp]\ninterval = \"500ms\"\n[[providers.cloudflare.targets]]\nid = \"lo4\"\naddress = \"127.0.0.1\"\nprotocols = [\"icmp\"]\n[[providers.google.targets]]\nid = \"lo6\"\naddress = \"::1\"\nprotocols = [\"icmp\"]\n";
    let loaded = Loaded::from_layers(Some(CONFIG), None, &[]).expect("built-in loopback configuration is valid");
    let plan = loaded.plan(&|_| None);
    let run = collect(&loaded, plan, Some(Duration::from_millis(1_400)), None, None, None, &mut |_| true).expect("no recording was asked for, so none can fail");
    [("127.0.0.1", "cloudflare/lo4/icmp/4", Family::V4), ("::1", "google/lo6/icmp/6", Family::V6)]
        .into_iter()
        .map(|(address, stream, family)| {
            let available = report.icmp.iter().any(|c| c.family == family && c.available);
            let result = match (available, run.engine.stream_index(stream)) {
                (false, _) => "not attempted (ICMP unsupported for this family)".to_string(),
                (true, Some(i)) => {
                    let s = run.engine.session_stats(i, run.end_ns);
                    match (s.replied, s.min_us, s.local_error) {
                        (0, _, l) if l > 0 => format!("local error on send ({l} of {} slots) — no usable address or route for this family", s.slots),
                        (r, Some(min), _) => format!("{r} of {} echo requests answered, fastest {:.3} ms", s.sent, f64::from(min) / 1000.0),
                        _ => format!("0 of {} echo requests answered", s.sent),
                    }
                }
                (true, None) => "not planned".to_string(),
            };
            LoopbackResult { address, result }
        })
        .collect()
}

// ───────────────────────────── sample ─────────────────────────────

struct Collected {
    engine: Engine,
    end_ns: u64,
    counters: CollectorCounters,
    signal: Option<u8>,
    io_error: Option<io::Error>,
    stream_states: Vec<pathwatch_core::model::StreamRec>,
    traffic: Vec<pathwatch_core::model::TrafficRow>,
    /// A recording was asked for and stopped before the session ended.
    recording_failed: bool,
}

/// How a session's collector is reached while it runs.
struct Wiring {
    session: Arc<str>,
    /// The user's stop/resume switch.
    pause: PauseSwitch,
    /// Configuration reloads, for a session that takes them.
    reload: Option<mpsc::Receiver<collector::Reload>>,
}

/// The plan for a session on this host: the configured streams, plus the
/// controls when they are enabled. Reading the routing table and the
/// resolver configuration sends nothing.
fn plan_for(loaded: &Loaded) -> Plan {
    let mut plan = loaded.plan(&real_env);
    if loaded.config.controls.gateway.enabled || loaded.config.controls.local_dns.enabled {
        plan.add_controls(&loaded.config, &pathwatch_core::localnet::discover());
    }
    plan
}

fn new_session_id() -> Arc<str> {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("{ms:x}-{:04x}", std::process::id() & 0xffff).into()
}

/// Runs the collector on its own thread and feeds the processor on this one
/// until the duration or count is reached, a signal arrives, or `sink` asks to
/// stop (by returning false — a broken pipe, for instance).
/// Starts the collector on its own thread with a current-thread runtime, so
/// nothing else in the process can delay a send or a receive timestamp.
fn spawn_collector(
    loaded: &Loaded,
    plan: Plan,
    clock: Arc<dyn Clock>,
    tx: mpsc::Sender<Msg>,
    cancel: CancellationToken,
    wiring: Wiring,
) -> std::thread::JoinHandle<CollectorCounters> {
    let Wiring { session, pause, reload } = wiring;
    let setup = Setup {
        config: Arc::new(loaded.config.clone()),
        plan,
        session,
        config_rev: 1,
        clock: clock.clone(),
        tx,
        cancel,
        tls: None,
        resolver: None,
        dns_port: 53,
        pause,
        net: None,
        reload,
    };
    std::thread::Builder::new()
        .name("collector".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("collector runtime");
            rt.block_on(async move {
                // Opened by the collector when an ICMP stream first needs one.
                let v4 = collector::icmp_on_demand(Family::V4, clock.clone());
                let v6 = collector::icmp_on_demand(Family::V6, clock);
                collector::run(setup, v4, v6).await
            })
        })
        .expect("collector thread")
}

fn collect(
    loaded: &Loaded,
    plan: Plan,
    duration: Option<Duration>,
    count: Option<u64>,
    record_to: Option<PathBuf>,
    // `Some` to take configuration reloads on SIGHUP: the file named on the
    // command line, if one was.
    reload_from: Option<Option<PathBuf>>,
    sink: &mut dyn FnMut(&Record) -> bool,
) -> Result<Collected, String> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    let session = new_session_id();
    let recorder = match record_to {
        Some(dir) => Some(
            recording::start(loaded, &plan, dir.clone(), session.clone(), clock.clone(), Box::new(pathwatch_core::recorder::Files))
                .map_err(|e| format!("cannot record to {}: {e}", dir.display()))?,
        ),
        None => None,
    };
    let (tx, mut rx) = mpsc::channel::<Msg>(4096);
    let cancel = CancellationToken::new();
    let mut engine =
        Engine::new(collector::stream_infos(&plan), HistoryLimits { samples: loaded.config.history.samples.0, records: loaded.config.history.records.0, events: 10_000 });

    let (mut reloader, reload_rx) = match reload_from {
        Some(explicit) => {
            let (reloader, rx) = reload::Reloader::new(explicit, loaded.config.clone());
            (Some(reloader), Some(rx))
        }
        None => (None, None),
    };
    let collector_thread = spawn_collector(loaded, plan, clock.clone(), tx, cancel.clone(), Wiring { session, pause: PauseSwitch::default(), reload: reload_rx });

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("processor runtime");
    let mut signal = None;
    let mut finalised: u64 = 0;
    let mut stream_states = Vec::new();
    let mut traffic = Vec::new();
    let mut stopped_by_sink = false;
    rt.block_on(async {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).expect("signal handler");
        let deadline = async {
            match duration {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    engine.ingest(&msg);
                    if let Msg::Record(record) = &msg {
                        if let Some(rec) = &recorder {
                            rec.offer(record);
                        }
                        match record {
                            Record::Session(s) if s.state == "start" => stream_states = s.streams.clone(),
                            Record::Session(s) if s.state == "end" => traffic = s.traffic.clone(),
                            Record::Observation(o) if o.attempt == 1 => finalised += 1,
                            _ => {}
                        }
                        if !stopped_by_sink && !sink(record) {
                            stopped_by_sink = true;
                            cancel.cancel();
                        }
                        if count.is_some_and(|c| finalised >= c) {
                            cancel.cancel();
                        }
                    }
                }
                _ = &mut deadline, if !cancel.is_cancelled() => cancel.cancel(),
                _ = tokio::signal::ctrl_c(), if signal.is_none() => {
                    signal = Some(EXIT_SIGINT);
                    cancel.cancel();
                }
                _ = sigterm.recv(), if signal.is_none() => {
                    signal = Some(EXIT_SIGTERM);
                    cancel.cancel();
                }
                // Headless, a hang-up is the request to read the
                // configuration again. What happened is said on standard
                // error; applied, it is also a `config` record in the output.
                _ = sighup.recv(), if reloader.is_some() => {
                    match reloader.as_mut().map(|r| r.reload()) {
                        Some(Ok(())) => eprintln!("pathwatch: configuration reloaded"),
                        Some(Err(why)) => eprintln!("pathwatch: {}", reload::refused(&why)),
                        None => {}
                    }
                }
            }
        }
    });
    let counters = collector_thread.join().unwrap_or_default();
    let recording_failed = recorder.is_some_and(|handle| {
        let status = handle.status.clone();
        handle.finish();
        recording::report(&status).is_some()
    });
    Ok(Collected { engine, end_ns: clock.mono_ns(), counters, signal, io_error: None, stream_states, traffic, recording_failed })
}

fn sample(args: SampleArgs) -> ExitCode {
    let mut overrides = Vec::new();
    if let Some(f) = &args.family {
        let name = if f == "4" { "ipv4" } else { "ipv6" };
        overrides.push(("general.families".to_string(), toml::Value::Array(vec![toml::Value::String(name.into())])));
    }
    let loaded = match load(&args.config, &overrides) {
        Ok(l) => l,
        Err(code) => return code,
    };
    // Assertions are read before anything is sent: a check that cannot be
    // understood, or that would never end, is a usage error.
    let mut assertions = Vec::new();
    for text in &args.assert {
        match assert::Assertion::parse(text) {
            Ok(a) => assertions.push(a),
            Err(e) => {
                eprintln!("pathwatch: --assert {text:?}: {e}");
                return ExitCode::from(EXIT_USAGE);
            }
        }
    }
    if !assertions.is_empty() && args.duration.is_none() && args.count.is_none() {
        eprintln!("pathwatch: --assert needs --duration or --count: a check has to end by itself to have a result");
        return ExitCode::from(EXIT_USAGE);
    }
    let mut plan = plan_for(&loaded);
    if let Some(mode) = args.mode {
        for s in &mut plan.streams {
            let keep = match mode {
                Mode::Icmp => s.protocol == Protocol::Icmp,
                Mode::Dns => s.protocol == Protocol::DnsUdp,
                Mode::DnsTcp => s.protocol == Protocol::DnsTcp,
                Mode::Doh => s.protocol == Protocol::Doh,
                Mode::Dot => s.protocol == Protocol::Dot,
            };
            if !keep && s.state == PlanState::Collecting {
                s.state = PlanState::Disabled;
            }
        }
    }

    // Decide up front whether anything can run at all on this host.
    let caps = capability::probe();
    let icmp_ok = |f: Family| caps.icmp.iter().any(|c| c.family == f && c.available);
    let runnable = plan.collecting().filter(|s| s.protocol != Protocol::Icmp || icmp_ok(s.family)).count();
    if runnable == 0 {
        eprintln!("pathwatch: no stream can be started.");
        let mut err = io::stderr().lock();
        let _ = describe_plan(&plan, &mut err);
        for c in caps.icmp.iter().filter(|c| !c.available) {
            eprintln!("  ICMP IPv{}: {}", c.family.as_str(), c.detail);
        }
        return ExitCode::from(EXIT_NO_STREAMS);
    }
    // Nor is one that could never be checked: it would only ever fail, and
    // say so after the probes had been sent.
    for a in &assertions {
        let selected: Vec<_> = plan.streams.iter().filter(|s| a.selects(&s.id)).collect();
        if selected.is_empty() {
            eprintln!("pathwatch: --assert {:?} selects no stream. The streams are:", a.text);
            for s in &plan.streams {
                eprintln!("  {}", s.id);
            }
            return ExitCode::from(EXIT_USAGE);
        }
        if !selected.iter().any(|s| s.state == PlanState::Collecting && (s.protocol != Protocol::Icmp || icmp_ok(s.family))) {
            eprintln!("pathwatch: --assert {:?} selects only streams that will not run in this sample:", a.text);
            for s in selected {
                let why = match &s.state {
                    PlanState::Collecting => "unsupported — ICMP is not available on this host".to_string(),
                    other => format!("{}{}", other.label().replace('_', " "), other.reason().map(|r| format!(" — {r}")).unwrap_or_default()),
                };
                eprintln!("  {}  {why}", s.id);
            }
            return ExitCode::from(EXIT_USAGE);
        }
    }

    let mut out: Box<dyn Write> = match &args.output {
        Some(path) => match std::fs::File::create(path) {
            Ok(f) => Box::new(io::BufWriter::new(f)),
            Err(e) => {
                eprintln!("pathwatch: cannot write {}: {e}", path.display());
                return ExitCode::from(EXIT_IO);
            }
        },
        None => Box::new(io::stdout().lock()),
    };

    let jsonl = args.format == SampleFormat::Jsonl;
    let mut io_error: Option<io::Error> = None;
    let record_to = recording::directory(&loaded, args.record.record.as_deref());
    let collected = collect(&loaded, plan.clone(), args.duration.map(|d| d.0), args.count, record_to, Some(args.config.config.clone()), &mut |record| {
        if !jsonl {
            return true;
        }
        // One JSON object per line, flushed, so pipes see records as they happen.
        match writeln!(out, "{}", Line::new(record.clone()).to_json()).and_then(|()| out.flush()) {
            Ok(()) => true,
            Err(e) => {
                io_error = Some(e);
                false
            }
        }
    });
    let mut run = match collected {
        Ok(run) => run,
        Err(e) => {
            eprintln!("pathwatch: {e}");
            return ExitCode::from(EXIT_IO);
        }
    };
    run.io_error = io_error;

    let window = args.window.0;
    let color = !jsonl && args.output.is_none() && io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    let budget = pathwatch_core::traffic::budget(&loaded.config, &plan);
    let text = if args.format == SampleFormat::Plain {
        summary::render_plain(&run.engine, &plan, &run.stream_states, &run.counters, &run.traffic, &budget, window, run.end_ns)
    } else {
        summary::render(&run.engine, &plan, &run.stream_states, &run.counters, &run.traffic, &budget, window, run.end_ns, color)
    };
    let written = if jsonl {
        if !args.quiet && run.io_error.is_none() {
            eprint!("{text}");
        }
        Ok(())
    } else {
        out.write_all(text.as_bytes()).and_then(|()| out.flush())
    };

    // A sample that was cut short has no result to check: saying so, with
    // a status that is not success, is the only honest answer to a check.
    let cut_short = run.signal.is_some() || run.io_error.is_some();
    let not_held = if assertions.is_empty() {
        0
    } else if cut_short {
        eprintln!("pathwatch: the sample ended before it was complete; the assertions were not checked");
        0
    } else {
        check_assertions(&assertions, &run, window, args.quiet)
    };
    let unchecked = !assertions.is_empty() && cut_short;

    match (run.io_error.or(written.err()), run.signal) {
        // The reader went away: stop quietly. That is not a failure — unless
        // it took with it a check that was asked for.
        (Some(e), _) if e.kind() == io::ErrorKind::BrokenPipe => match (unchecked, not_held) {
            (true, _) => ExitCode::from(EXIT_IO),
            (false, 0) => ExitCode::SUCCESS,
            (false, _) => ExitCode::from(EXIT_ASSERT),
        },
        (Some(e), _) => {
            eprintln!("pathwatch: output error: {e}");
            ExitCode::from(EXIT_IO)
        }
        (None, Some(code)) => ExitCode::from(code),
        (None, None) if run.recording_failed => ExitCode::from(EXIT_IO),
        (None, None) if not_held > 0 => ExitCode::from(EXIT_ASSERT),
        (None, None) => ExitCode::SUCCESS,
    }
}

/// Checks each assertion against every stream it selects that ran, says on
/// standard error what it found, and returns how many checks did not hold —
/// false, or without the data to be judged. Passing checks are not listed
/// when `quiet`; the others always are.
fn check_assertions(assertions: &[assert::Assertion], run: &Collected, window: Duration, quiet: bool) -> usize {
    use assert::Verdict;
    let (mut checks, mut not_held) = (0, 0);
    for a in assertions {
        let (ran, left_out): (Vec<_>, Vec<_>) = run.stream_states.iter().filter(|s| a.selects(&s.stream)).partition(|s| s.state == "collecting");
        if ran.is_empty() {
            checks += 1;
            not_held += 1;
            eprintln!("assert NO DATA  {:?}: no stream it selects ran", a.text);
        }
        for s in &ran {
            checks += 1;
            let verdict = match run.engine.stream_index(&s.stream) {
                Some(idx) => a.judge(&run.engine.stats(idx, window, run.end_ns)),
                None => Verdict::NoData("no statistics were kept for this stream".into()),
            };
            match verdict {
                Verdict::Held(what) if quiet => drop(what),
                Verdict::Held(what) => eprintln!("assert ok       {}: {what}", s.stream),
                Verdict::Failed(what) => {
                    not_held += 1;
                    eprintln!("assert FAILED   {}: {what}", s.stream);
                }
                Verdict::NoData(why) => {
                    not_held += 1;
                    eprintln!("assert NO DATA  {}: {why}", s.stream);
                }
            }
        }
        // Named by the user and not checked: said, so a pass is not read
        // as covering them.
        if a.selector.is_some() && !ran.is_empty() && !left_out.is_empty() {
            let names: Vec<String> = left_out.iter().map(|s| format!("{} ({})", s.stream, s.state.replace('_', " "))).collect();
            eprintln!("assert note     {:?} also selects streams that did not run and were not checked: {}", a.text, names.join(", "));
        }
    }
    if not_held > 0 {
        eprintln!("pathwatch: {not_held} of {checks} assertion check{} did not hold, over a window of {} (exit {EXIT_ASSERT})", if checks == 1 { "" } else { "s" }, Dur(window));
    }
    not_held
}

// ───────────────────────────── export ─────────────────────────────

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// The word a value is written as in the log (`timeout`, `no_route`).
fn word<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn export_text(rec: &pathwatch_core::recorder::Recording, format: ExportFormat) -> Result<String, String> {
    use std::fmt::Write as _;
    let mut out = String::new();
    match format {
        ExportFormat::Jsonl => {
            if let Some(h) = &rec.header {
                let _ = writeln!(out, "{}", Line::new(Record::Segment(h.clone())).to_json());
            }
            for r in &rec.records {
                let _ = writeln!(out, "{}", Line::new(r.clone()).to_json());
            }
        }
        ExportFormat::Csv => {
            out.push_str("stream,seq,attempt,sent_utc,sched_ns,sent_ns,recv_ns,outcome,rtt_us,dest_ip,epoch,error,detail,flags\n");
            let opt = |v: Option<u64>| v.map_or_else(String::new, |v| v.to_string());
            for o in rec.records.iter().filter_map(|r| if let Record::Observation(o) = r { Some(o) } else { None }) {
                let (error, detail) = o.error.as_ref().map_or_else(Default::default, |e| (word(&e.category), e.detail.clone().unwrap_or_default()));
                let flags = o.flags.iter().map(word).collect::<Vec<_>>().join(" ");
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                    csv_field(&o.stream),
                    o.seq,
                    o.attempt,
                    o.sent_utc.as_deref().unwrap_or(""),
                    o.sched_ns,
                    opt(o.sent_ns),
                    opt(o.recv_ns),
                    word(&o.outcome),
                    opt(o.rtt_us.map(u64::from)),
                    o.dest_ip.map_or_else(String::new, |ip| ip.to_string()),
                    o.epoch,
                    error,
                    csv_field(&detail),
                    flags
                );
            }
        }
        ExportFormat::Summary => {
            // The same statistics the dashboard shows "since start", from the
            // same engine, fed the recorded records.
            let replay = pathwatch_core::recorder::prepare(pathwatch_core::recorder::Recording {
                header: rec.header.clone(),
                records: rec.records.clone(),
                warnings: Vec::new(),
                segments: rec.segments,
            })?;
            let end = replay.end_ns;
            let whole = Duration::from_nanos(end.max(1)) + Duration::from_secs(1);
            let mut engine = Engine::new(replay.infos.clone(), HistoryLimits { samples: whole, records: Duration::ZERO, events: 10_000 });
            for (_, m) in &replay.msgs {
                engine.ingest(m);
            }
            let ms = |g: pathwatch_core::stats::Gated| match g {
                pathwatch_core::stats::Gated::Ok(v) => format!("{:.1}", v / 1000.0),
                pathwatch_core::stats::Gated::LowN(v) => format!("{:.1}·", v / 1000.0),
                pathwatch_core::stats::Gated::Insufficient => "—".into(),
            };
            let _ = writeln!(
                out,
                "session {} · {} of session time · times in ms · `—` not enough samples · `·` low sample count",
                replay.session,
                Dur(Duration::from_millis(end / 1_000_000))
            );
            let _ = writeln!(out, "{:<34} {:>6} {:>8} {:>9} {:>8} {:>7} {:>7} {:>7} {:>7}", "stream", "sent", "replied", "no-reply", "err-resp", "local", "p50", "p95", "max");
            for (idx, info) in replay.infos.iter().enumerate() {
                let s = engine.session_stats(idx, end);
                if s.slots == 0 {
                    let _ = writeln!(out, "{:<34} nothing was sent", info.id);
                    continue;
                }
                let _ = writeln!(
                    out,
                    "{:<34} {:>6} {:>8} {:>9} {:>8} {:>7} {:>7} {:>7} {:>7}",
                    info.id,
                    s.sent,
                    s.replied,
                    s.unanswered,
                    s.errors,
                    s.local_error,
                    ms(s.p50_us),
                    ms(s.p95_us),
                    s.max_us.map_or("—".into(), |v| format!("{:.1}", f64::from(v) / 1000.0))
                );
            }
            let gaps = engine.gaps().len();
            if gaps > 0 {
                let _ = writeln!(out, "{gaps} observation gap(s): nothing was sent in them and they are in no rate");
            }
        }
    }
    Ok(out)
}

/// Replaces an address wherever it stands on its own: `10.0.0.1` is
/// replaced, the same digits inside `10.0.0.194` or `210.0.0.1` are not.
fn replace_address(text: &str, address: &str, with: &str) -> String {
    let part_of_address = |c: char| c.is_ascii_hexdigit() || c == '.' || c == ':';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(address) {
        let (before, after) = (&rest[..at], &rest[at + address.len()..]);
        // A colon after an IPv4 address is a port separator, not more address.
        let joined_before = before.chars().next_back().is_some_and(|c| c.is_ascii_digit() || c == '.');
        let joined_after = after
            .chars()
            .next()
            .is_some_and(|c| part_of_address(c) && !(c == ':' && !address.contains(':')) && !(c == '.' && !after[1..].starts_with(|d: char| d.is_ascii_digit())));
        out.push_str(before);
        out.push_str(if joined_before || joined_after { address } else { with });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn export(args: ExportArgs) -> ExitCode {
    let mut rec = match pathwatch_core::recorder::read(&args.path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pathwatch: cannot read {}: {e}", args.path.display());
            return ExitCode::from(EXIT_IO);
        }
    };
    for w in &rec.warnings {
        eprintln!("pathwatch: warning: {w}");
    }
    let mut local_addresses = std::collections::BTreeSet::new();
    if args.redact_local {
        // This host's own addresses, and what the controls reveal about its
        // network: the gateway's address and the resolver the system uses.
        // Those are removed wherever they occur — a record, an error detail,
        // the recorded configuration — once the text has been produced.
        let is_control = |provider: &str| pathwatch_core::localnet::is_control(provider);
        for r in &mut rec.records {
            match r {
                Record::Observation(o) => {
                    // The address is also in the source-context events.
                    local_addresses.extend(o.src.take().and_then(|s| s.ip).map(|ip| ip.to_string()));
                    if is_control(&o.provider) {
                        local_addresses.extend(o.dest_ip.map(|ip| ip.to_string()));
                        local_addresses.insert(o.configured.to_string());
                    }
                }
                Record::Session(s) => local_addresses.extend(s.streams.iter().filter(|st| is_control(&st.provider)).map(|st| st.configured.to_string())),
                // The source context is this host's network in words: its
                // address, the interface's name, the gateway — whether or
                // not a control was probing that gateway. Which context it
                // was stays; what it said goes.
                Record::Event(e) if e.event == "source_context" || e.event == "source_context_changed" => {
                    let which = e.detail.split(':').next().filter(|p| p.starts_with("source context ")).unwrap_or("source context");
                    e.detail = format!("{which}: ‹redacted:local›");
                }
                _ => {}
            }
        }
        // Only things that are addresses; "auto" is not a secret.
        local_addresses.retain(|a| a.parse::<std::net::IpAddr>().is_ok());
    }
    let mut text = match export_text(&rec, args.format) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pathwatch: {} cannot be summarised: {e}", args.path.display());
            return ExitCode::from(EXIT_USAGE);
        }
    };
    for address in &local_addresses {
        text = replace_address(&text, address, "‹redacted:local›");
    }
    // Recordings are written without profile ids. This catches one made with
    // redaction switched off, when the id is known here.
    if !args.no_redact {
        let ids = ["PATHWATCH_NEXTDNS_PROFILE"].into_iter().filter_map(real_env);
        text = pathwatch_core::recorder::Scrubber::new(ids).scrub(&text).into_owned();
        if rec.header.as_ref().is_some_and(|h| !h.redacted) {
            eprintln!("pathwatch: warning: this recording was made with profile-id redaction off; only an id set in PATHWATCH_NEXTDNS_PROFILE here could be removed");
        }
    }
    let written = match &args.output {
        Some(path) => std::fs::write(path, &text),
        None => io::stdout().lock().write_all(text.as_bytes()),
    };
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pathwatch: output error: {e}");
            ExitCode::from(EXIT_IO)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::replace_address;

    #[test]
    fn an_address_is_replaced_only_where_it_stands_on_its_own() {
        let text = r#"{"dest_ip":"10.0.0.1","src":"10.0.0.194","note":"via 10.0.0.1:53, not 210.0.0.1 or 10.0.0.12; gateway 10.0.0.1.","v6":"fe80::1"}"#;
        let got = replace_address(text, "10.0.0.1", "X");
        assert_eq!(got, r#"{"dest_ip":"X","src":"10.0.0.194","note":"via X:53, not 210.0.0.1 or 10.0.0.12; gateway X.","v6":"fe80::1"}"#);
        assert_eq!(replace_address("to fe80::1 and fe80::12 and [fe80::1]:53", "fe80::1", "X"), "to X and fe80::12 and [X]:53");
        assert_eq!(replace_address("nothing here", "10.0.0.1", "X"), "nothing here");
    }
}

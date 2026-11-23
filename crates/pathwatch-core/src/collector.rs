//! The collector: owns every socket and timer, and nothing else.
//!
//! It runs on its own current-thread runtime so that receive timestamps are
//! never delayed by statistics or by a slow terminal. Each stream is its own
//! task; a silent target occupies only its own pending entries.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::ClientConfig;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::clock::{Clock, ClockFinding, GapDetector, utc_string};
use crate::config::{Config, Endpoint, Plan, PlanState, StreamPlan};
use crate::discovery::{self, ResolveFn, Resolved, ResolverSpec};
use crate::model::{
    Backend, CollectorCounters, ConnInfo, ConnectionRec, ErrorCategory, ErrorInfo, Family, Flag, GapCause, GapRec, Msg, Observation, Outcome, Phases, Protocol, Record, SessionRec,
    Source, StreamRec,
};
use crate::probe::dns::{self, DnsParams, Exchange, QuerySpec};
use crate::probe::doh::{self, DohConn, DohTarget};
use crate::probe::icmp::{self, IcmpCmd, IcmpIo, IcmpRequest, RealIcmpIo};
use crate::probe::{Counters, GapMark, Outbox, Shared, StreamCtx, us};
use crate::schedule::{SlotGrid, TokenBucket};

/// A timer that fires this much late, without a host suspend, is a collector
/// stall and is recorded as an observation gap instead of as skipped slots.
pub const STALL_THRESHOLD: Duration = Duration::from_secs(2);
const GAP_TICK: Duration = Duration::from_millis(250);
const LAG_TICK: Duration = Duration::from_millis(50);

pub enum IcmpSetup<I> {
    Use(I),
    /// The host does not permit it; the reason is shown, nothing is sent, and
    /// the affected streams never count as loss.
    Unavailable(String),
    /// Opened when the first ICMP stream of the family is about to start —
    /// at the outset, or after a reload adds one — and never in a session
    /// that has none. An open ICMP socket is not free: on macOS it is handed
    /// every echo reply arriving at the host, whoever asked for it, and each
    /// would be read and counted as unmatched for nothing. The error is the
    /// reason to show, as for `Unavailable`.
    OnDemand(Box<dyn FnOnce() -> Result<I, String> + Send>),
}

/// Opens the real unprivileged ICMP socket for a family, now.
pub fn open_icmp(family: Family, clock: Arc<dyn Clock>) -> IcmpSetup<RealIcmpIo> {
    match RealIcmpIo::open(family, clock) {
        Ok(io) => IcmpSetup::Use(io),
        Err(e) => IcmpSetup::Unavailable(crate::capability::explain_icmp_open_error(family, &e)),
    }
}

/// The real unprivileged ICMP socket for a family, opened only if a stream
/// comes to need it. Must be opened on the collector's runtime, which is
/// where the collector does it.
pub fn icmp_on_demand(family: Family, clock: Arc<dyn Clock>) -> IcmpSetup<RealIcmpIo> {
    IcmpSetup::OnDemand(Box::new(move || RealIcmpIo::open(family, clock).map_err(|e| crate::capability::explain_icmp_open_error(family, &e))))
}

/// The running ICMP actors, for whoever must tell them all something. One
/// can be added after the session has started.
pub type IcmpActors = Arc<Mutex<Vec<mpsc::Sender<IcmpCmd>>>>;

/// What starting an ICMP actor takes, apart from its socket.
struct IcmpEnv {
    payload: usize,
    late_window_ns: u64,
    capacity: usize,
    shared: Shared,
    actors: IcmpActors,
}

fn wants_icmp(plan: &Plan, family: Family) -> bool {
    plan.collecting().any(|s| s.protocol == Protocol::Icmp && s.family == family)
}

/// Settles one family's ICMP transport if it is not settled yet: starts its
/// actor, or records why there is none. A transport to be opened on demand
/// is left alone until `wanted`.
fn bring_up<I: IcmpIo>(
    pending: &mut Option<IcmpSetup<I>>,
    family: Family,
    wanted: bool,
    env: &IcmpEnv,
    tasks: &mut JoinSet<()>,
    icmp_tx: &mut HashMap<Family, mpsc::Sender<IcmpCmd>>,
    unavailable: &mut HashMap<Family, String>,
) {
    let opened = match pending.take() {
        None => return,
        Some(IcmpSetup::OnDemand(open)) if !wanted => {
            *pending = Some(IcmpSetup::OnDemand(open));
            return;
        }
        Some(IcmpSetup::OnDemand(open)) => open(),
        Some(IcmpSetup::Use(io)) => Ok(io),
        Some(IcmpSetup::Unavailable(why)) => Err(why),
    };
    match opened {
        Ok(io) => {
            let (t, r) = mpsc::channel(env.capacity);
            icmp_tx.insert(family, t.clone());
            env.actors.lock().unwrap().push(t);
            tasks.spawn(icmp::run_actor(io, family, env.payload, env.late_window_ns, env.shared.clone(), r));
        }
        Err(why) => {
            unavailable.insert(family, why);
        }
    }
}

pub struct Setup {
    pub config: Arc<Config>,
    pub plan: Plan,
    pub session: Arc<str>,
    pub config_rev: u32,
    pub clock: Arc<dyn Clock>,
    pub tx: mpsc::Sender<Msg>,
    pub cancel: CancellationToken,
    /// TLS configuration for DoH; `None` uses the OS trust store.
    pub tls: Option<Arc<ClientConfig>>,
    /// Hostname resolver; `None` uses the OS resolver.
    pub resolver: Option<ResolveFn>,
    /// Destination port for plain DNS (53 outside tests).
    pub dns_port: u16,
    /// The user's stop/resume switch. Leave it at its default to never stop.
    pub pause: PauseSwitch,
    /// How this host's network is read; `None` asks the operating system.
    pub net: Option<Arc<dyn crate::netwatch::NetProbe>>,
    /// Configuration reloads to apply while running; `None` if there are none.
    pub reload: Option<mpsc::Receiver<Reload>>,
}

/// Stops and resumes collection at the user's request. While it is set no
/// slot is attempted, so nothing is sent; the stopped interval is recorded as
/// an observation gap when collection resumes or the session ends. Probes
/// already in flight finish normally — the collector is still listening.
///
/// This is not "freeze", which only pins the view.
#[derive(Debug, Clone)]
pub struct PauseSwitch(watch::Sender<bool>);

impl Default for PauseSwitch {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl PauseSwitch {
    pub fn set(&self, stopped: bool) {
        self.0.send_if_modified(|v| std::mem::replace(v, stopped) != stopped);
    }

    pub fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }
}

/// The streams of a plan as the processor needs to know them, in the index
/// order the collector uses.
pub fn stream_infos(plan: &Plan) -> Vec<crate::engine::StreamInfo> {
    plan.streams
        .iter()
        .map(|s| crate::engine::StreamInfo {
            id: s.id.clone(),
            provider: s.provider.clone(),
            target: s.target.clone(),
            protocol: s.protocol,
            family: s.family,
            interval: s.interval,
            deadline: s.deadline,
        })
        .collect()
}

/// Runs until `setup.cancel` fires. Must be called on a Tokio runtime; the
/// production binary gives it a dedicated current-thread runtime.
pub async fn run<I4: IcmpIo, I6: IcmpIo>(setup: Setup, v4: IcmpSetup<I4>, v6: IcmpSetup<I6>) -> CollectorCounters {
    let Setup { config, plan, session, config_rev, clock, tx, cancel, tls, resolver, dns_port, pause, net, reload } = setup;
    // The revision every record is tagged with; a reload raises it.
    let rev = Arc::new(AtomicU32::new(config_rev));
    let net = net.unwrap_or_else(|| Arc::new(crate::netwatch::SystemNet));
    let (context_tx, context_rx) = watch::channel(1u32);
    let counters = Arc::new(Counters::default());
    let outbox = Arc::new(Outbox::new(tx, counters.clone()));
    let (gap_tx, gap_rx) = watch::channel(GapMark::default());
    let shared = Shared {
        clock: clock.clone(),
        outbox: outbox.clone(),
        counters: counters.clone(),
        bucket: Arc::new(Mutex::new(TokenBucket::new(config.limits.max_transactions_per_second, config.limits.burst))),
        gap: gap_rx,
        pause: pause.0.subscribe(),
        net: net.clone(),
        context: context_rx,
        cancel: cancel.clone(),
        session: session.clone(),
    };

    let mut tasks = JoinSet::new();
    let mut icmp_tx: HashMap<Family, mpsc::Sender<IcmpCmd>> = HashMap::new();
    let mut icmp_unavailable: HashMap<Family, String> = HashMap::new();
    let icmp_env = IcmpEnv {
        payload: usize::from(config.icmp.payload_bytes),
        late_window_ns: config.icmp.late_window.0.as_nanos() as u64,
        capacity: config.limits.max_in_flight as usize,
        shared: shared.clone(),
        actors: IcmpActors::default(),
    };
    // A transport handed over ready is started now. One to be opened on
    // demand is opened now only if this plan has a stream for it.
    let (mut v4, mut v6) = (Some(v4), Some(v6));
    bring_up(&mut v4, Family::V4, wants_icmp(&plan, Family::V4), &icmp_env, &mut tasks, &mut icmp_tx, &mut icmp_unavailable);
    bring_up(&mut v6, Family::V6, wants_icmp(&plan, Family::V6), &icmp_env, &mut tasks, &mut icmp_tx, &mut icmp_unavailable);

    // Announce the session and every stream's state before anything is sent.
    let stream_recs = stream_recs(&plan, &icmp_unavailable);
    let session_rec = |state: &str, streams: Vec<StreamRec>, counters: Option<CollectorCounters>, traffic: Vec<crate::model::TrafficRow>| {
        Record::Session(SessionRec {
            session: session.clone(),
            state: state.into(),
            utc: utc_string(clock.wall()),
            app: "pathwatch".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            streams,
            counters,
            traffic,
        })
    };
    outbox.record(session_rec("start", stream_recs, None, Vec::new()));
    for (name, value) in [("HTTPS_PROXY", std::env::var("HTTPS_PROXY")), ("https_proxy", std::env::var("https_proxy")), ("ALL_PROXY", std::env::var("ALL_PROXY"))] {
        if value.is_ok_and(|v| !v.is_empty()) && plan.collecting().any(|s| s.protocol == Protocol::Doh) {
            shared.event("proxy_detected", None, format!("{name} is set; DoH probes connect directly and do not use it"));
            break;
        }
    }

    tasks.spawn(loop_lag_monitor(shared.clone()));
    tasks.spawn(gap_monitor(shared.clone(), gap_tx.clone(), icmp_env.actors.clone()));
    tasks.spawn(pause_monitor(shared.clone()));

    let resolve = resolver.unwrap_or_else(discovery::system_resolver);
    let specs: Arc<Vec<QuerySpec>> = Arc::new(config.dns.query.iter().filter_map(|q| QuerySpec::from_cfg(q).ok()).collect());
    let params = DnsParams { edns_udp_size: config.dns.edns_udp_size, recursion_desired: config.dns.recursion_desired, tcp_fallback: config.dns.tcp_fallback };
    let mut spawner = Spawner {
        config: config.clone(),
        session: session.clone(),
        rev: rev.clone(),
        shared: shared.clone(),
        icmp_tx,
        resolvers: HashMap::new(),
        resolve,
        tls,
        tls_tried: false,
        specs,
        params,
        dns_port,
        followed: Vec::new(),
        index: HashMap::new(),
        numbering: HashMap::new(),
        running: HashMap::new(),
    };
    for s in &plan.streams {
        spawner.start(s, &mut tasks, false);
    }
    // Which route stands for "the way out" in each family: that of the first
    // stream with a known address, else a documentation address — asking
    // the kernel how it would route there sends nothing.
    let reference = |family: Family, fallback: &str| {
        plan.collecting()
            .filter(|s| s.family == family && !crate::localnet::is_control(&s.provider))
            .find_map(|s| match (&s.endpoint, s.pinned) {
                (_, Some(ip)) => Some(ip),
                (Endpoint::Ip(ip), None) => Some(*ip),
                _ => None,
            })
            .or_else(|| fallback.parse().ok())
    };
    tasks.spawn(crate::netwatch::run(
        shared.clone(),
        crate::netwatch::Watch {
            probe: net,
            reference: [reference(Family::V4, "192.0.2.1"), reference(Family::V6, "2001:db8::1")],
            controls: std::mem::take(&mut spawner.followed),
            context: context_tx,
            gap: gap_tx,
            actors: icmp_env.actors.clone(),
        },
    ));

    // Run until cancelled, applying each configuration reload as it comes.
    let mut reload = reload;
    loop {
        let next = async {
            match reload.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => break,
            request = next => match request {
                Some(request) => {
                    // The new plan may be the first to have an ICMP stream.
                    bring_up(&mut v4, Family::V4, wants_icmp(&request.plan, Family::V4), &icmp_env, &mut tasks, &mut spawner.icmp_tx, &mut icmp_unavailable);
                    bring_up(&mut v6, Family::V6, wants_icmp(&request.plan, Family::V6), &icmp_env, &mut tasks, &mut spawner.icmp_tx, &mut icmp_unavailable);
                    let (streams, summary) = spawner.apply(request, &icmp_unavailable, &mut tasks, &outbox);
                    shared.event("config_reloaded", None, format!("configuration revision {}: {summary}", rev.load(Ordering::Relaxed)));
                    drop(streams);
                }
                None => reload = None, // nobody can ask any more
            },
        }
    }
    // The ICMP actors end with the session, as every task does.
    drop(spawner);
    while tasks.join_next().await.is_some() {}
    let totals = counters.snapshot();
    outbox.record(session_rec("end", Vec::new(), Some(totals), counters.traffic.snapshot()));
    outbox.drain().await;
    totals
}

/// A request to run a new configuration. It has been parsed and validated in
/// full, and differs from the running one only in what can be applied live.
pub struct Reload {
    pub config: Arc<Config>,
    pub plan: Plan,
}

/// Every stream of a plan as it is announced, with what this host cannot do.
fn stream_recs(plan: &Plan, icmp_unavailable: &HashMap<Family, String>) -> Vec<StreamRec> {
    plan.streams
        .iter()
        .map(|s| {
            let (state, reason) = match (&s.state, s.protocol, icmp_unavailable.get(&s.family)) {
                (PlanState::Collecting, Protocol::Icmp, Some(why)) => ("unsupported", Some(why.clone())),
                (st, _, _) => (st.label(), st.reason().map(str::to_string)),
            };
            StreamRec {
                stream: s.id.clone(),
                provider: s.provider.clone(),
                target: s.target.clone(),
                protocol: s.protocol,
                family: s.family,
                configured: s.configured.clone(),
                interval_ms: s.interval.as_millis() as u64,
                deadline_ms: s.deadline.as_millis() as u64,
                state: state.to_string(),
                reason,
            }
        })
        .collect()
}

/// Where a stream's numbering stands. Kept when a reload restarts the
/// stream, so its sequence numbers keep rising and its new definition is a
/// new destination epoch: the two are never pooled in one statistic.
#[derive(Default)]
struct Numbering {
    next_seq: AtomicU64,
    epoch: AtomicU32,
}

struct Running {
    plan: StreamPlan,
    stop: CancellationToken,
}

/// Whether two plans describe the same stream doing the same thing. The
/// phase is left out: it only spreads streams within an interval, and a
/// stream that is otherwise unchanged keeps the schedule it has.
fn same_definition(a: &StreamPlan, b: &StreamPlan) -> bool {
    (&a.endpoint, a.pinned, a.follow, a.interval, a.deadline, a.protocol, a.family, a.fresh, &a.configured, &a.provider, &a.target)
        == (&b.endpoint, b.pinned, b.follow, b.interval, b.deadline, b.protocol, b.family, b.fresh, &b.configured, &b.provider, &b.target)
}

/// Starts and stops streams. Everything a stream needs that outlives it.
struct Spawner {
    config: Arc<Config>,
    session: Arc<str>,
    rev: Arc<AtomicU32>,
    shared: Shared,
    icmp_tx: HashMap<Family, mpsc::Sender<IcmpCmd>>,
    resolvers: HashMap<(String, Family), watch::Receiver<Resolved>>,
    resolve: ResolveFn,
    tls: Option<Arc<ClientConfig>>,
    tls_tried: bool,
    specs: Arc<Vec<QuerySpec>>,
    params: DnsParams,
    dns_port: u16,
    /// Controls on `auto` get a destination that can be changed when the
    /// host's gateway or resolver does.
    followed: Vec<crate::netwatch::FollowedControl>,
    /// Stream id → the index it has for the whole session.
    index: HashMap<Arc<str>, u32>,
    numbering: HashMap<Arc<str>, Arc<Numbering>>,
    running: HashMap<Arc<str>, Running>,
}

impl Spawner {
    /// TLS for DoH, set up the first time a DoH stream needs it.
    fn tls(&mut self) -> Option<Arc<ClientConfig>> {
        if self.tls.is_none() && !self.tls_tried {
            self.tls_tried = true;
            match doh::default_tls() {
                Ok(t) => self.tls = Some(t),
                Err(e) => self.shared.event("tls_unavailable", None, e),
            }
        }
        self.tls.clone()
    }

    /// Gives the stream its index and, if it is to collect, starts it.
    /// `mid_session` is true for a stream started by a reload.
    fn start(&mut self, s: &StreamPlan, tasks: &mut JoinSet<()>, mid_session: bool) {
        let next = self.index.len() as u32;
        let idx = *self.index.entry(s.id.clone()).or_insert(next);
        if s.state != PlanState::Collecting {
            return;
        }
        let ctx = Arc::new(StreamCtx { idx, session: self.session.clone(), rev: self.rev.clone(), plan: s.clone(), backend: backend_for(s) });
        let dest = match &s.endpoint {
            // Static bootstrap: the configured address is dialled and the
            // hostname is never resolved, so a filtering or broken local
            // resolver cannot redirect the stream.
            _ if s.pinned.is_some() => discovery::fixed(s.pinned.expect("checked above")),
            Endpoint::Ip(ip) if crate::localnet::is_control(&s.provider) && &*s.target == "auto" => {
                let (tx, rx) = watch::channel(Resolved { ready: true, ip: Some(*ip), epoch: 1, error: None });
                self.followed.push(crate::netwatch::FollowedControl { provider: s.provider.clone(), family: s.family, dest: tx });
                rx
            }
            Endpoint::Ip(ip) => discovery::fixed(*ip),
            Endpoint::Host(host) | Endpoint::Url { host, .. } => match host.parse::<IpAddr>() {
                Ok(ip) => discovery::fixed(ip),
                Err(_) => {
                    let (config, resolve, shared) = (self.config.clone(), self.resolve.clone(), self.shared.clone());
                    self.resolvers
                        .entry((host.clone(), s.family))
                        .or_insert_with(|| {
                            discovery::spawn_resolver(
                                ResolverSpec {
                                    provider: s.provider.clone(),
                                    target: s.target.clone(),
                                    host: host.clone(),
                                    display: crate::config::redact(host).into(),
                                    family: s.family,
                                    follow: s.follow,
                                    refresh: config.bootstrap.refresh.0,
                                    keep_on_failure: config.bootstrap.on_failure == "keep",
                                },
                                resolve,
                                shared,
                            )
                        })
                        .clone()
                }
            },
        };
        let stop = self.shared.cancel.child_token();
        let numbering = self.numbering.entry(s.id.clone()).or_default().clone();
        let slots = Slots::new(ctx, self.shared.clone(), stop.clone(), numbering, mid_session);
        let (specs, params) = (self.specs.clone(), self.params);
        let started = match s.protocol {
            // Without a socket for this family the stream was announced as
            // unsupported, and nothing is sent.
            Protocol::Icmp => self.icmp_tx.get(&s.family).cloned().map(|tx| tasks.spawn(icmp_stream(slots, dest, tx))).is_some(),
            Protocol::DnsUdp => {
                tasks.spawn(dns_udp_stream(slots, dest, specs, params, self.dns_port));
                true
            }
            // DoH and DoT differ only in what is spoken after the handshake.
            Protocol::Doh | Protocol::Dot => match (self.tls(), &s.endpoint) {
                (Some(tls), Endpoint::Url { host, port, path }) => {
                    let dot = s.protocol == Protocol::Dot;
                    let tls = if dot { doh::for_dot(&tls) } else { tls };
                    let transport = if dot { doh::Transport::Tls } else { doh::Transport::Https };
                    let target = DohTarget { host: host.clone(), port: *port, path: path.clone(), transport };
                    if s.fresh {
                        tasks.spawn(doh_fresh_stream(slots, dest, specs, params, target, doh::without_resumption(&tls)));
                    } else {
                        tasks.spawn(doh_stream(slots, dest, specs, params, target, tls));
                    }
                    true
                }
                _ => false,
            },
            Protocol::DnsTcp => {
                tasks.spawn(dns_tcp_stream(slots, dest, specs, params, self.dns_port));
                true
            }
        };
        if started {
            self.running.insert(s.id.clone(), Running { plan: s.clone(), stop });
        }
    }

    /// Applies a new plan as the next revision. A stream whose definition is
    /// unchanged is not touched: it keeps its schedule, its connection, and
    /// its history. One that changed is stopped and started again as a new
    /// epoch. Probes already in flight finish under the revision they were
    /// sent in. Returns the streams announced and what changed, in counts.
    fn apply(&mut self, request: Reload, icmp_unavailable: &HashMap<Family, String>, tasks: &mut JoinSet<()>, outbox: &Outbox) -> (Vec<StreamRec>, String) {
        let Reload { config, plan } = request;
        self.config = config;
        let (mut started, mut stopped, mut restarted, mut unchanged) = (0, 0, 0, 0);
        let mut to_start: Vec<&StreamPlan> = Vec::new();
        for s in &plan.streams {
            let collecting = s.state == PlanState::Collecting;
            match self.running.get(&s.id) {
                Some(r) if collecting && same_definition(&r.plan, s) => unchanged += 1,
                Some(r) => {
                    r.stop.cancel();
                    self.running.remove(&s.id);
                    if collecting {
                        restarted += 1;
                        to_start.push(s);
                    } else {
                        stopped += 1;
                    }
                }
                None => to_start.push(s),
            }
        }
        // Streams the new configuration no longer has at all.
        let gone: Vec<Arc<str>> = self.running.keys().filter(|id| !plan.streams.iter().any(|s| &s.id == *id)).cloned().collect();
        for id in gone {
            if let Some(r) = self.running.remove(&id) {
                r.stop.cancel();
                stopped += 1;
            }
        }
        // The revision and what it runs are announced before anything is
        // sent under it, so every later record is read against it.
        let rev = self.rev.fetch_add(1, Ordering::Relaxed) + 1;
        let streams = stream_recs(&plan, icmp_unavailable);
        let before = self.running.len();
        for s in &to_start {
            // Indices are given in plan order, running or not: the engine
            // numbers the streams of the record below the same way.
            let next = self.index.len() as u32;
            self.index.entry(s.id.clone()).or_insert(next);
        }
        let providers = plan.providers.iter().map(crate::model::ProviderRec::of).collect();
        let pending: Vec<StreamPlan> = to_start.into_iter().cloned().collect();
        // Count what will newly run (not merely be listed) for the summary.
        let will_run = pending.iter().filter(|s| s.state == PlanState::Collecting).count();
        started += will_run.saturating_sub(restarted);
        let summary = format!("{started} started, {stopped} stopped, {restarted} restarted, {unchanged} unchanged");
        outbox.record(Record::Config(crate::model::ConfigRec {
            session: self.session.clone(),
            rev,
            at_ns: self.shared.clock.mono_ns(),
            utc: utc_string(self.shared.clock.wall()),
            streams: streams.clone(),
            providers,
            summary: summary.clone(),
        }));
        for s in &pending {
            self.start(s, tasks, true);
        }
        let _ = before;
        (streams, summary)
    }
}

fn backend_for(s: &StreamPlan) -> Backend {
    Backend::new(match s.protocol {
        Protocol::Icmp => "icmp-dgram",
        Protocol::DnsUdp => "dns-udp",
        Protocol::DnsTcp => "dns-tcp",
        Protocol::Dot => "dot",
        Protocol::Doh => "doh-h2",
    })
}

async fn loop_lag_monitor(shared: Shared) {
    let period = LAG_TICK.as_nanos() as u64;
    let mut next = shared.clock.mono_ns() + period;
    loop {
        tokio::select! {
            _ = shared.cancel.cancelled() => return,
            _ = shared.sleep_until_ns(next) => {}
        }
        let now = shared.clock.mono_ns();
        let lag_us = now.saturating_sub(next) / 1_000;
        shared.counters.last_loop_lag_us.store(lag_us, Ordering::Relaxed);
        shared.counters.max_loop_lag_us.fetch_max(lag_us, Ordering::Relaxed);
        shared.counters.loop_lag_checks.fetch_add(1, Ordering::Relaxed);
        if lag_us > 2_000 {
            shared.counters.loop_lag_over_2ms.fetch_add(1, Ordering::Relaxed);
        }
        next += period;
        if next <= now {
            next = now + period;
        }
    }
}

/// Turns the user's stop and resume into records: an event at each, and one
/// observation gap covering the whole stopped interval.
async fn pause_monitor(shared: Shared) {
    let mut pause = shared.pause.clone();
    let mut stopped_at: Option<u64> = None;
    loop {
        let ending = tokio::select! {
            _ = shared.cancel.cancelled() => true,
            changed = pause.changed() => changed.is_err(),
        };
        let stopped = *pause.borrow_and_update() && !ending;
        let now = shared.clock.mono_ns();
        match (stopped_at, stopped) {
            (None, true) => {
                stopped_at = Some(now);
                shared.event("collection_stopped", None, "collection stopped by the user; nothing is sent until it is resumed");
            }
            (Some(start), false) => {
                stopped_at = None;
                shared.outbox.record(Record::Gap(GapRec {
                    session: shared.session.clone(),
                    cause: GapCause::Stopped,
                    start_ns: start,
                    end_ns: now,
                    duration_ms: now.saturating_sub(start) / 1_000_000,
                    end_utc: utc_string(shared.clock.wall()),
                }));
                if !ending {
                    shared.event("collection_resumed", None, "collection resumed by the user");
                }
            }
            _ => {}
        }
        if ending {
            return;
        }
    }
}

async fn gap_monitor(shared: Shared, gap_tx: watch::Sender<GapMark>, actors: IcmpActors) {
    let mut detector = GapDetector::new(shared.clock.as_ref(), GAP_TICK, STALL_THRESHOLD);
    loop {
        tokio::select! {
            _ = shared.cancel.cancelled() => return,
            _ = tokio::time::sleep(GAP_TICK) => {}
        }
        for finding in detector.tick(shared.clock.as_ref()) {
            match finding {
                ClockFinding::Gap { cause, start_ns, end_ns, duration_ns } => {
                    shared.outbox.record(Record::Gap(GapRec {
                        session: shared.session.clone(),
                        cause,
                        start_ns,
                        end_ns,
                        duration_ms: duration_ns / 1_000_000,
                        end_utc: utc_string(shared.clock.wall()),
                    }));
                    gap_tx.send_modify(|m| {
                        m.generation += 1;
                        m.end_ns = end_ns;
                    });
                    // Replies to probes that were in flight may have been lost
                    // while the host slept: cancelled, not timed out.
                    for a in actors.lock().unwrap().iter() {
                        let _ = a.try_send(IcmpCmd::CancelPending(ErrorCategory::Gap));
                    }
                }
                ClockFinding::WallStep { delta_ms } => {
                    shared.event("clock_step", None, format!("wall clock moved {delta_ms} ms relative to the monotonic clock; durations are unaffected"));
                }
            }
        }
    }
}

/// The slot loop every stream shares: waits for the next slot, accounts for
/// slots that were passed over, and applies the global rate limit.
struct Slots {
    ctx: Arc<StreamCtx>,
    shared: Shared,
    grid: SlotGrid,
    /// Stops this stream alone: a reload removed or redefined it.
    stop: CancellationToken,
    numbering: Arc<Numbering>,
    /// Added to the grid's slot number to give the sequence number, and to
    /// the destination's epoch: both carry on from the stream's earlier run.
    seq_base: u64,
    epoch_base: u32,
}

impl Slots {
    fn new(ctx: Arc<StreamCtx>, shared: Shared, stop: CancellationToken, numbering: Arc<Numbering>, mid_session: bool) -> Self {
        // The grid is anchored at session start, offset by the stream's phase.
        let mut grid = SlotGrid::new(ctx.plan.phase, ctx.plan.interval);
        if mid_session {
            // Slots before now were never this stream's: nothing to catch up on.
            grid.resume_after_gap(shared.clock.mono_ns());
        }
        // Sequence numbers never repeat within a stream id: a restarted
        // stream continues above where it stopped.
        let seq_base = numbering.next_seq.load(Ordering::Relaxed).saturating_sub(grid.next_index());
        let epoch_base = numbering.epoch.load(Ordering::Relaxed);
        Self { ctx, shared, grid, stop, numbering, seq_base, epoch_base }
    }

    /// Returns the next slot to attempt as `(seq, scheduled time)`, or `None`
    /// once collection is cancelled.
    async fn next(&mut self) -> Option<(u64, u64)> {
        loop {
            if *self.shared.pause.borrow() {
                let mut pause = self.shared.pause.clone();
                tokio::select! {
                    _ = self.shared.cancel.cancelled() => return None,
                    _ = self.stop.cancelled() => return None,
                    resumed = pause.wait_for(|stopped| !*stopped) => resumed.ok()?,
                };
                // No slots existed while collection was stopped: nothing to
                // report as skipped, and the grid keeps its original phase.
                self.grid.resume_after_gap(self.shared.clock.mono_ns());
            }
            let due = self.grid.next_due_ns();
            tokio::select! {
                _ = self.shared.cancel.cancelled() => return None,
                _ = self.stop.cancelled() => return None,
                _ = self.shared.sleep_until_ns(due) => {}
            }
            if *self.shared.pause.borrow() {
                continue; // stopped while waiting for this slot: do not send it
            }
            let now = self.shared.clock.mono_ns();
            let Some(wake) = self.grid.on_wake(now) else { continue };
            if now.saturating_sub(due) <= STALL_THRESHOLD.as_nanos() as u64 {
                for k in wake.skipped.clone() {
                    self.shared.counters.late_timer_skips.fetch_add(1, Ordering::Relaxed);
                    self.emit(self.ctx.skipped(k + self.seq_base, self.grid.slot_time_ns(k), ErrorCategory::LateTimer));
                }
            } // otherwise the gap monitor records an observation gap: no slots existed
            let sched = self.grid.slot_time_ns(wake.fire);
            let seq = wake.fire + self.seq_base;
            self.numbering.next_seq.store(seq + 1, Ordering::Relaxed);
            if !self.shared.bucket.lock().unwrap().try_take(now) {
                self.shared.counters.rate_limit_skips.fetch_add(1, Ordering::Relaxed);
                self.emit(self.ctx.skipped(seq, sched, ErrorCategory::RateLimit));
                continue;
            }
            return Some((seq, sched));
        }
    }

    fn emit(&self, o: Observation) {
        self.shared.outbox.emit(Msg::obs(o));
    }

    fn backpressure(&self, seq: u64, sched: u64) {
        self.shared.counters.backpressure_skips.fetch_add(1, Ordering::Relaxed);
        self.emit(self.ctx.skipped(seq, sched, ErrorCategory::Backpressure));
    }

    fn gap_generation(&self) -> u64 {
        self.shared.gap.borrow().generation
    }

    /// Waits for a transaction to finish, unless the session ends first.
    /// Leaving must not wait for an answer that may never come: whatever
    /// is in flight at that moment is given up, and recorded as cancelled.
    async fn unless_ended<T>(&self, work: impl std::future::Future<Output = T>) -> Option<T> {
        tokio::select! {
            biased;
            done = work => Some(done),
            _ = self.shared.cancel.cancelled() => None,
        }
    }
}

/// Waits for the first resolution result (immediate for literal addresses).
async fn first_resolution(dest: &mut watch::Receiver<Resolved>, shared: &Shared) -> bool {
    tokio::select! {
        _ = shared.cancel.cancelled() => false,
        r = dest.wait_for(|r| r.ready) => r.is_ok(),
    }
}

struct DestCache {
    epoch: u32,
    /// The source context the cached source belongs to.
    context: u32,
    src: Option<Source>,
}

/// Reads the current destination. `Err` carries a finished local-error record
/// for slots that cannot be attempted because no address is known.
fn current_dest(slots: &Slots, dest: &watch::Receiver<Resolved>, cache: &mut DestCache, seq: u64, sched: u64) -> Result<(IpAddr, u32, Option<Source>), Box<Observation>> {
    let mut r = dest.borrow().clone();
    // Epochs carry on from the stream's earlier run, if a reload restarted it.
    r.epoch += slots.epoch_base;
    slots.numbering.epoch.fetch_max(r.epoch, Ordering::Relaxed);
    match r.ip {
        Some(ip) => {
            // The source is looked up again whenever the destination or the
            // network has changed, and carries the id of its context.
            let context = *slots.shared.context.borrow();
            if cache.epoch != r.epoch || cache.context != context {
                (cache.epoch, cache.context) = (r.epoch, context);
                cache.src = slots.shared.net.source_for(ip).map(|s| Source { ctx: context, ifname: s.ifname, ip: Some(s.ip) });
            }
            Ok((ip, r.epoch, cache.src.clone()))
        }
        None => {
            let mut o = slots.ctx.observation(seq, sched);
            o.epoch = r.epoch;
            o.outcome = Outcome::LocalError;
            o.error = Some(ErrorInfo::new(ErrorCategory::Bootstrap).detail(r.error.unwrap_or_else(|| "hostname did not resolve".into())));
            Err(Box::new(o))
        }
    }
}

async fn icmp_stream(mut slots: Slots, mut dest: watch::Receiver<Resolved>, tx: mpsc::Sender<IcmpCmd>) {
    if !first_resolution(&mut dest, &slots.shared).await {
        return;
    }
    let mut cache = DestCache { epoch: 0, context: 0, src: None };
    while let Some((seq, sched)) = slots.next().await {
        let (ip, epoch, src) = match current_dest(&slots, &dest, &mut cache, seq, sched) {
            Ok(d) => d,
            Err(o) => {
                slots.emit(*o);
                continue;
            }
        };
        let Some(reservation) = slots.shared.outbox.reserve() else {
            slots.backpressure(seq, sched);
            continue;
        };
        let req = IcmpRequest { ctx: slots.ctx.clone(), seq, sched_ns: sched, dest: ip, epoch, src, reservation };
        // Never wait on the engine: a full queue is a skipped slot, not a delay.
        if let Err(TrySendError::Full(IcmpCmd::Send(req)) | TrySendError::Closed(IcmpCmd::Send(req))) = tx.try_send(IcmpCmd::Send(Box::new(req))) {
            slots.shared.counters.backpressure_skips.fetch_add(1, Ordering::Relaxed);
            req.reservation.finish_unsent(slots.ctx.skipped(seq, sched, ErrorCategory::Backpressure));
        }
    }
}

/// Turns one transport attempt into a record.
fn dns_observation(ctx: &StreamCtx, seq: u64, sched: u64, dest: SocketAddr, epoch: u32, src: Option<Source>, ex: Exchange) -> Observation {
    let mut o = ctx.observation(seq, sched);
    o.dest_ip = Some(dest.ip());
    o.dest_port = Some(dest.port());
    o.epoch = epoch;
    o.src = src;
    o.sent_ns = ex.sent_ns;
    o.sent_utc = ex.sent_utc;
    o.recv_ns = ex.recv_ns;
    o.sched_lag_us = ex.sent_ns.map(|s| u64::from(us(s.saturating_sub(sched))));
    o.outcome = ex.outcome;
    o.error = ex.error;
    o.rtt_us = ex.rtt_us;
    o.phase = ex.phase;
    o.dns = Some(ex.info);
    if ex.truncated {
        o.flags.push(Flag::Truncated);
    }
    o
}

/// A transaction that was in flight across an observation gap did not time
/// out — the instrument was not watching.
fn cancel_if_gap(o: &mut Observation, slots: &Slots, generation_before: u64) {
    if slots.gap_generation() != generation_before && o.outcome == Outcome::Timeout {
        o.outcome = Outcome::Cancelled;
        o.error = Some(ErrorInfo::new(ErrorCategory::Gap));
    }
}

async fn dns_udp_stream(mut slots: Slots, mut dest: watch::Receiver<Resolved>, specs: Arc<Vec<QuerySpec>>, params: DnsParams, port: u16) {
    if specs.is_empty() || !first_resolution(&mut dest, &slots.shared).await {
        return;
    }
    let mut cache = DestCache { epoch: 0, context: 0, src: None };
    let clock = slots.shared.clock.clone();
    while let Some((seq, sched)) = slots.next().await {
        let (ip, epoch, src) = match current_dest(&slots, &dest, &mut cache, seq, sched) {
            Ok(d) => d,
            Err(o) => {
                slots.emit(*o);
                continue;
            }
        };
        let Some(reservation) = slots.shared.outbox.reserve() else {
            slots.backpressure(seq, sched);
            continue;
        };
        // Every provider sees the same query sequence: slot k sends query k mod n.
        let spec = &specs[(seq % specs.len() as u64) as usize];
        let addr = SocketAddr::new(ip, port);
        let generation = slots.gap_generation();
        let (started_ns, sent_utc) = (clock.mono_ns(), utc_string(clock.wall()));
        let fin = reservation.sent(slots.ctx.idx, seq, started_ns);
        let Some(ex) = slots.unless_ended(dns::udp_exchange(clock.as_ref(), addr, spec, params, slots.ctx.plan.deadline, &slots.shared.counters)).await else {
            fin.send(abandoned(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec));
            return;
        };
        let truncated = ex.truncated;
        let mut o = dns_observation(&slots.ctx, seq, sched, addr, epoch, src.clone(), ex);
        cancel_if_gap(&mut o, &slots, generation);
        fin.send(Msg::obs(o));

        if truncated && params.tcp_fallback {
            // The fallback is its own attempt, linked by transaction; the UDP
            // attempt above stands as the stream's first-attempt result.
            let Some(ex) = slots.unless_ended(dns::tcp_exchange(clock.as_ref(), addr, spec, params, slots.ctx.plan.deadline, &slots.shared.counters)).await else { return };
            let mut o = dns_observation(&slots.ctx, seq, sched, addr, epoch, src, ex);
            o.attempt = 2;
            o.protocol = Protocol::DnsTcp;
            slots.emit(o);
        }
    }
}

/// DNS over TCP as a stream of its own: a new connection for every probe,
/// one query on it, and the connection closed. The headline time is the
/// whole transaction, connect and query together, because that is what a
/// client falling back to TCP waits for; the two parts are recorded as
/// phases. Nothing here retries: a refused or unanswered connection is the
/// result for that slot.
async fn dns_tcp_stream(mut slots: Slots, mut dest: watch::Receiver<Resolved>, specs: Arc<Vec<QuerySpec>>, params: DnsParams, port: u16) {
    if specs.is_empty() || !first_resolution(&mut dest, &slots.shared).await {
        return;
    }
    let mut cache = DestCache { epoch: 0, context: 0, src: None };
    let clock = slots.shared.clock.clone();
    while let Some((seq, sched)) = slots.next().await {
        let (ip, epoch, src) = match current_dest(&slots, &dest, &mut cache, seq, sched) {
            Ok(d) => d,
            Err(o) => {
                slots.emit(*o);
                continue;
            }
        };
        let Some(reservation) = slots.shared.outbox.reserve() else {
            slots.backpressure(seq, sched);
            continue;
        };
        // The same rotation as every other DNS stream: slot k sends query k mod n.
        let spec = &specs[(seq % specs.len() as u64) as usize];
        let addr = SocketAddr::new(ip, port);
        let generation = slots.gap_generation();
        let (started_ns, sent_utc) = (clock.mono_ns(), utc_string(clock.wall()));
        let fin = reservation.sent(slots.ctx.idx, seq, started_ns);
        let Some(ex) = slots.unless_ended(dns::tcp_exchange(clock.as_ref(), addr, spec, params, slots.ctx.plan.deadline, &slots.shared.counters)).await else {
            fin.send(abandoned(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec));
            return;
        };
        let mut o = dns_observation(&slots.ctx, seq, sched, addr, epoch, src, ex);
        // Whenever an answer arrived, error responses included, the time
        // reported runs from the start of the connection to its last byte.
        if let (Some(_), Some(sent), Some(recv)) = (o.rtt_us, o.sent_ns, o.recv_ns) {
            o.rtt_us = Some(us(recv.saturating_sub(sent)));
        }
        cancel_if_gap(&mut o, &slots, generation);
        fin.send(Msg::obs(o));
    }
}

/// What a transaction leaves behind when the session ends under it: it was
/// sent, and it was cancelled — not timed out, because nobody waited.
fn abandoned(slots: &Slots, at: Attempt, spec: &QuerySpec) -> Msg {
    let Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc } = at;
    let mut o = slots.ctx.observation(seq, sched);
    o.dest_ip = Some(addr.ip());
    o.dest_port = Some(addr.port());
    o.epoch = epoch;
    o.src = src;
    o.sent_ns = Some(started_ns);
    o.sent_utc = Some(sent_utc);
    o.sched_lag_us = Some(u64::from(us(started_ns.saturating_sub(sched))));
    o.outcome = Outcome::Cancelled;
    o.error = Some(ErrorInfo::new(ErrorCategory::Shutdown));
    o.dns = Some(spec.info(0));
    Msg::obs(o)
}

fn conn_info(c: &DohConn, now_ns: u64, http_status: Option<u16>) -> ConnInfo {
    ConnInfo { id: c.id, reused: c.requests > 1, age_ms: now_ns.saturating_sub(c.opened_ns) / 1_000_000, alpn: c.alpn.clone(), tls: c.tls.clone(), resumed: c.resumed, http_status }
}

fn connection_record(slots: &Slots, id: u64, state: &str, peer: Option<IpAddr>, phase: Option<Phases>, c: Option<&DohConn>, reason: Option<String>) {
    slots.shared.outbox.record(Record::Connection(ConnectionRec {
        session: slots.shared.session.clone(),
        at_ns: slots.shared.clock.mono_ns(),
        stream: slots.ctx.plan.id.clone(),
        conn: id,
        state: state.into(),
        peer,
        phase,
        alpn: c.and_then(|c| c.alpn.clone()),
        tls: c.and_then(|c| c.tls.clone()),
        resumed: c.and_then(|c| c.resumed),
        reason,
    }));
}

/// Where and when one slot's connection attempt was made.
struct Attempt {
    seq: u64,
    sched: u64,
    addr: SocketAddr,
    epoch: u32,
    src: Option<Source>,
    started_ns: u64,
    sent_utc: String,
}

/// A connection attempt that failed, as a record for the slot that needed it.
/// It names the query the slot was for (with id 0: it was never encoded), so
/// the failure is listed against that query rather than against nothing.
fn connect_failure(slots: &Slots, at: Attempt, spec: &QuerySpec, f: doh::ConnectFailure) -> Observation {
    let Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc } = at;
    let mut o = slots.ctx.observation(seq, sched);
    o.dest_ip = Some(addr.ip());
    o.dest_port = Some(addr.port());
    o.epoch = epoch;
    o.src = src;
    if f.sent {
        o.sent_ns = Some(started_ns);
        o.sent_utc = Some(sent_utc);
        o.sched_lag_us = Some(u64::from(us(started_ns.saturating_sub(sched))));
    }
    o.outcome = f.outcome;
    o.error = Some(f.error);
    o.phase = Some(f.phase);
    o.dns = Some(spec.info(0));
    o
}

/// DoH on one long-lived connection, reconnecting when it closes or the
/// destination changes. Reused-connection statistics exclude the first request
/// on each connection, which absorbs connection setup effects.
async fn doh_stream(mut slots: Slots, mut dest: watch::Receiver<Resolved>, specs: Arc<Vec<QuerySpec>>, params: DnsParams, target: DohTarget, tls: Arc<ClientConfig>) {
    if specs.is_empty() || !first_resolution(&mut dest, &slots.shared).await {
        return;
    }
    let mut cache = DestCache { epoch: 0, context: 0, src: None };
    let clock = slots.shared.clock.clone();
    let mut conn: Option<DohConn> = None;
    let mut conn_context = 0u32;
    let mut next_conn_id = 1u64;
    while let Some((seq, sched)) = slots.next().await {
        let (ip, epoch, src) = match current_dest(&slots, &dest, &mut cache, seq, sched) {
            Ok(d) => d,
            Err(o) => {
                slots.emit(*o);
                continue;
            }
        };
        let Some(reservation) = slots.shared.outbox.reserve() else {
            slots.backpressure(seq, sched);
            continue;
        };
        let spec = &specs[(seq % specs.len() as u64) as usize];
        let addr = SocketAddr::new(ip, target.port);
        let generation = slots.gap_generation();
        let started_ns = clock.mono_ns();
        let sent_utc = utc_string(clock.wall());
        let until_ns = started_ns + slots.ctx.plan.deadline.as_nanos() as u64;
        let fin = reservation.sent(slots.ctx.idx, seq, started_ns);

        // A connection made on the previous network is bound to an address
        // this host may no longer have: it is replaced, not waited on.
        let context = *slots.shared.context.borrow();
        if let Some(c) = &conn
            && (c.is_closed() || c.peer != ip || conn_context != context)
        {
            let reason = if c.peer != ip {
                "destination changed"
            } else if conn_context != context {
                "the network changed"
            } else {
                "closed by peer or error"
            };
            connection_record(&slots, c.id, "closed", Some(c.peer), None, Some(c), Some(reason.into()));
            conn = None;
        }
        if conn.is_none() {
            let id = next_conn_id;
            next_conn_id += 1;
            let Some(connected) = slots.unless_ended(doh::connect(clock.as_ref(), &tls, &target, ip, id, until_ns, &slots.shared.counters)).await else {
                fin.send(abandoned(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec));
                return;
            };
            match connected {
                Ok((c, phase)) => {
                    connection_record(&slots, id, "opened", Some(ip), Some(phase), Some(&c), None);
                    conn = Some(c);
                    conn_context = context;
                }
                Err(f) => {
                    connection_record(&slots, id, "failed", Some(ip), Some(f.phase), None, f.error.detail.clone());
                    let mut o = connect_failure(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec, f);
                    cancel_if_gap(&mut o, &slots, generation);
                    fin.send(Msg::obs(o));
                    continue;
                }
            }
        }
        let c = conn.as_mut().expect("connection established above");
        let Some(r) = slots.unless_ended(doh::request(clock.as_ref(), c, &target, spec, params, until_ns, &slots.shared.counters)).await else {
            fin.send(abandoned(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec));
            return;
        };
        let mut o = dns_observation(&slots.ctx, seq, sched, addr, epoch, src, r.exchange);
        o.conn = Some(conn_info(c, clock.mono_ns(), r.http_status));
        if c.requests <= 1 {
            o.flags.push(Flag::AfterReconnect);
        }
        cancel_if_gap(&mut o, &slots, generation);
        fin.send(Msg::obs(o));
        if r.broken {
            connection_record(&slots, c.id, "closed", Some(c.peer), None, Some(c), Some("request failed; connection discarded".into()));
            conn = None;
        }
    }
}

/// The fresh-connection cohort: a new connection per probe with a full TLS
/// handshake, closed afterwards. Its round-trip time is the total of TCP
/// connect, TLS handshake, and the first query; the phases are recorded too.
async fn doh_fresh_stream(mut slots: Slots, mut dest: watch::Receiver<Resolved>, specs: Arc<Vec<QuerySpec>>, params: DnsParams, target: DohTarget, tls: Arc<ClientConfig>) {
    if specs.is_empty() || !first_resolution(&mut dest, &slots.shared).await {
        return;
    }
    let mut cache = DestCache { epoch: 0, context: 0, src: None };
    let clock = slots.shared.clock.clone();
    while let Some((seq, sched)) = slots.next().await {
        let (ip, epoch, src) = match current_dest(&slots, &dest, &mut cache, seq, sched) {
            Ok(d) => d,
            Err(o) => {
                slots.emit(*o);
                continue;
            }
        };
        let Some(reservation) = slots.shared.outbox.reserve() else {
            slots.backpressure(seq, sched);
            continue;
        };
        let spec = &specs[(seq % specs.len() as u64) as usize];
        let addr = SocketAddr::new(ip, target.port);
        let generation = slots.gap_generation();
        let started_ns = clock.mono_ns();
        let sent_utc = utc_string(clock.wall());
        let until_ns = started_ns + slots.ctx.plan.deadline.as_nanos() as u64;
        let fin = reservation.sent(slots.ctx.idx, seq, started_ns);

        let Some(connected) = slots.unless_ended(doh::connect(clock.as_ref(), &tls, &target, ip, seq + 1, until_ns, &slots.shared.counters)).await else {
            fin.send(abandoned(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec));
            return;
        };
        let mut o = match connected {
            Err(f) => connect_failure(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec, f),
            Ok((mut c, setup)) => {
                let Some(r) = slots.unless_ended(doh::request(clock.as_ref(), &mut c, &target, spec, params, until_ns, &slots.shared.counters)).await else {
                    fin.send(abandoned(&slots, Attempt { seq, sched, addr, epoch, src, started_ns, sent_utc }, spec));
                    return;
                };
                let request_phase = r.exchange.phase;
                let mut o = dns_observation(&slots.ctx, seq, sched, addr, epoch, src, r.exchange);
                o.conn = Some(conn_info(&c, clock.mono_ns(), r.http_status));
                o.phase = Some(Phases {
                    connect_us: setup.connect_us,
                    tls_us: setup.tls_us,
                    ttfb_us: request_phase.and_then(|p| p.ttfb_us),
                    request_us: request_phase.and_then(|p| p.request_us),
                });
                // The headline time for this cohort is the whole transaction.
                o.sent_ns = Some(started_ns);
                o.sent_utc = Some(sent_utc);
                o.sched_lag_us = Some(u64::from(us(started_ns.saturating_sub(sched))));
                if o.outcome == Outcome::Success
                    && let Some(recv) = o.recv_ns
                {
                    o.rtt_us = Some(us(recv.saturating_sub(started_ns)));
                }
                o
            }
        };
        o.flags.push(Flag::Fresh);
        cancel_if_gap(&mut o, &slots, generation);
        fin.send(Msg::obs(o));
    }
}

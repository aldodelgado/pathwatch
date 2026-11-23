//! Hostname bootstrap and destination epochs.
//!
//! Resolution always happens off the timing path, in its own task. A stream
//! reads the latest result without waiting. Policy (see
//! `planning/01-design-spec.md` §3.4): take the first address of the stream's
//! family and stay on it while it remains in the answer set; when it leaves,
//! either follow (new destination epoch, recorded) or stay pinned and say so.

use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::model::{Family, Record, ResolutionRec};
use crate::probe::Shared;

pub type ResolveFuture = Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;
/// Resolves a hostname to addresses. Injected so tests never touch a real resolver.
pub type ResolveFn = Arc<dyn Fn(String) -> ResolveFuture + Send + Sync>;

/// The operating system's resolver — what the user's own clients would get,
/// including any hostname steering. Its duration is recorded, never reported
/// as a provider's response time.
pub fn system_resolver() -> ResolveFn {
    Arc::new(|host: String| {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// False until the first resolution attempt has finished.
    pub ready: bool,
    pub ip: Option<IpAddr>,
    /// Increments every time the selected address changes. Statistics never
    /// pool two epochs.
    pub epoch: u32,
    pub error: Option<String>,
}

/// A literal address: no resolution, one epoch for the whole session.
pub fn fixed(ip: IpAddr) -> watch::Receiver<Resolved> {
    watch::channel(Resolved { ready: true, ip: Some(ip), epoch: 1, error: None }).1
}

pub struct ResolverSpec {
    pub provider: Arc<str>,
    pub target: Arc<str>,
    /// The real hostname to resolve.
    pub host: String,
    /// The hostname as it may be recorded (secrets redacted).
    pub display: Arc<str>,
    pub family: Family,
    pub follow: bool,
    pub refresh: Duration,
    pub keep_on_failure: bool,
}

/// Starts a resolver task and returns the channel streams read from.
pub fn spawn_resolver(spec: ResolverSpec, resolve: ResolveFn, shared: Shared) -> watch::Receiver<Resolved> {
    let (tx, rx) = watch::channel(Resolved { ready: false, ip: None, epoch: 0, error: None });
    tokio::spawn(async move {
        let mut warned_pinned: Option<IpAddr> = None;
        loop {
            let t0 = shared.clock.mono_ns();
            let result = resolve(spec.host.clone()).await;
            let duration_us = (shared.clock.mono_ns() - t0) / 1_000;
            let (all, error) = match result {
                Ok(v) => (v, None),
                Err(e) => (Vec::new(), Some(e.to_string())),
            };
            let mut mine: Vec<IpAddr> = Vec::new();
            for ip in &all {
                if Family::of(ip) == spec.family && !mine.contains(ip) {
                    mine.push(*ip);
                }
            }
            shared.outbox.record(Record::Resolution(ResolutionRec {
                session: shared.session.clone(),
                at_ns: t0,
                provider: spec.provider.clone(),
                target: spec.target.clone(),
                name: spec.display.clone(),
                resolver: "system".into(),
                duration_us,
                addresses: mine.clone(),
                error: error.clone(),
            }));

            let current = tx.borrow().clone();
            let mut next = Resolved { ready: true, ..current.clone() };
            let label = Some(Arc::<str>::from(format!("{}/{}", spec.provider, spec.target)));
            match (current.ip, mine.first().copied()) {
                (Some(cur), Some(_)) if mine.contains(&cur) => next.error = None,
                (None, Some(first)) => {
                    next.ip = Some(first);
                    next.epoch = current.epoch + 1;
                    next.error = None;
                }
                (Some(cur), Some(first)) if spec.follow => {
                    next.ip = Some(first);
                    next.epoch = current.epoch + 1;
                    next.error = None;
                    shared.event("address_changed", label, format!("{} IPv{}: {cur} → {first}; new destination epoch {}", spec.display, spec.family.as_str(), next.epoch));
                }
                (Some(cur), Some(first)) => {
                    next.error = None;
                    if warned_pinned != Some(first) {
                        warned_pinned = Some(first);
                        shared.event(
                            "address_pinned",
                            label,
                            format!("{} no longer resolves to the pinned address {cur} (now {first}); follow is off, staying on {cur}", spec.display),
                        );
                    }
                }
                (cur, None) => {
                    let why = error.unwrap_or_else(|| format!("no IPv{} address", spec.family.as_str()));
                    if cur.is_some() && spec.keep_on_failure {
                        shared.event("bootstrap_failed", label, format!("{}: {why}; keeping the last known address", spec.display));
                    } else {
                        if cur.is_some() || !current.ready {
                            shared.event("bootstrap_failed", label, format!("{}: {why}", spec.display));
                        }
                        next.ip = None;
                    }
                    next.error = Some(why);
                }
            }
            if next != current {
                let _ = tx.send(next);
            }
            tokio::select! {
                _ = shared.cancel.cancelled() => return,
                _ = tx.closed() => return,
                _ = tokio::time::sleep(spec.refresh) => {}
            }
        }
    });
    rx
}

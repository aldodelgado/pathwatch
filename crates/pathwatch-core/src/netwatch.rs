//! Source context — which local address and interface this host sends from,
//! and through which gateway — and noticing when it changes.
//!
//! A change of network (Wi-Fi to Ethernet, a VPN coming up, a new address)
//! is not something the targets did. When it happens, probes in flight are
//! cancelled rather than left to time out, the moment is recorded as a short
//! observation gap, and the new context is recorded as an event. Nothing is
//! sent to find any of this out: the kernel is asked which source address it
//! would use, and the routing table is read.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::clock::utc_string;
use crate::discovery::Resolved;
use crate::localnet::{self, LocalNet};
use crate::model::{ErrorCategory, Family, GapCause, GapRec, Record};
use crate::probe::icmp::IcmpCmd;
use crate::probe::{GapMark, Shared};

/// How often the source addresses are compared with what they were.
pub const POLL: Duration = Duration::from_secs(2);
/// How often the gateway and resolvers are re-read when a control uses them,
/// to catch a change that leaves the source address as it was.
const LOCAL_EVERY: u32 = 15;

/// The local end of a route to some destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceAddr {
    pub ip: IpAddr,
    pub ifname: Option<String>,
}

impl SourceAddr {
    /// Interfaces that are usually a VPN or another tunnel, by name. A hint
    /// for the reader, never a fact acted upon.
    pub fn looks_like_tunnel(&self) -> bool {
        self.ifname.as_deref().is_some_and(|n| ["utun", "tun", "tap", "wg", "ppp", "ipsec", "gif", "tailscale", "zt"].iter().any(|p| n.starts_with(p)))
    }
}

/// What the collector asks about this host's network. The real answer comes
/// from the operating system; tests supply their own.
pub trait NetProbe: Send + Sync {
    /// The address and interface the OS would send from to reach `dest`.
    /// Sends nothing.
    fn source_for(&self, dest: IpAddr) -> Option<SourceAddr>;
    /// The default gateways and the system's resolvers. Sends nothing, but
    /// may be slow (on macOS it runs the system's `route` tool).
    fn local(&self) -> LocalNet;
    /// True when the calls are cheap enough to make on the collector thread.
    fn inline(&self) -> bool {
        false
    }
}

pub struct SystemNet;

impl NetProbe for SystemNet {
    fn source_for(&self, dest: IpAddr) -> Option<SourceAddr> {
        let ip = crate::platform::icmp_sock::local_source_for(dest)?;
        // An unspecified address means "no route": there is no source.
        (!ip.is_unspecified()).then(|| SourceAddr { ifname: interface_of(ip), ip })
    }

    fn local(&self) -> LocalNet {
        localnet::discover()
    }
}

/// The name of the interface that carries `ip`, from the system's interface
/// list.
pub fn interface_of(ip: IpAddr) -> Option<String> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list that stays valid
    // until freeifaddrs, which is called on every path below. Each node's
    // pointers are checked for null before use, and `sa_family` is read
    // before the address is reinterpreted as the matching sockaddr type.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut node = list;
        while !node.is_null() {
            let ifa = &*node;
            if !ifa.ifa_addr.is_null() && !ifa.ifa_name.is_null() {
                let addr = match i32::from((*ifa.ifa_addr).sa_family) {
                    libc::AF_INET => Some(IpAddr::from((*ifa.ifa_addr.cast::<libc::sockaddr_in>()).sin_addr.s_addr.to_ne_bytes())),
                    libc::AF_INET6 => Some(IpAddr::from((*ifa.ifa_addr.cast::<libc::sockaddr_in6>()).sin6_addr.s6_addr)),
                    _ => None,
                };
                if addr == Some(ip) {
                    found = Some(std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned());
                    break;
                }
            }
            node = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
        found
    }
}

/// The source context of one address family.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FamilyContext {
    pub source: Option<SourceAddr>,
    pub gateway: Option<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Context {
    pub v4: FamilyContext,
    pub v6: FamilyContext,
}

impl Context {
    fn family(&self, f: Family) -> &FamilyContext {
        match f {
            Family::V4 => &self.v4,
            Family::V6 => &self.v6,
        }
    }
}

fn describe_family(c: &FamilyContext) -> String {
    match &c.source {
        None => "no route".to_string(),
        Some(s) => {
            let on = s.ifname.as_deref().map_or_else(String::new, |n| format!(" on {n}{}", if s.looks_like_tunnel() { " (looks like a tunnel)" } else { "" }));
            let via = c.gateway.map_or_else(|| ", no gateway found".to_string(), |g| format!(", gateway {g}"));
            format!("{}{on}{via}", s.ip)
        }
    }
}

/// The context in words, for the event recorded at start.
pub fn describe(c: &Context) -> String {
    format!("IPv4 {}; IPv6 {}", describe_family(&c.v4), describe_family(&c.v6))
}

/// What changed between two contexts, in words. Families that did not
/// change are left out.
pub fn describe_change(before: &Context, after: &Context) -> String {
    let parts: Vec<String> = [(Family::V4, "IPv4"), (Family::V6, "IPv6")]
        .into_iter()
        .filter(|(f, _)| before.family(*f) != after.family(*f))
        .map(|(f, name)| format!("{name} {} → {}", describe_family(before.family(f)), describe_family(after.family(f))))
        .collect();
    parts.join("; ")
}

/// A control whose destination follows the host's configuration.
pub struct FollowedControl {
    pub provider: Arc<str>,
    pub family: Family,
    pub dest: watch::Sender<Resolved>,
}

/// The destination a control should have now, given what the host says.
fn control_address(provider: &str, family: Family, local: &LocalNet) -> Result<IpAddr, String> {
    if provider == localnet::GATEWAY {
        local.gateway(family).clone().map_err(|why| format!("no default gateway found: {why}"))
    } else {
        match &local.resolvers {
            Ok(list) => list.iter().copied().find(|ip| Family::of(ip) == family).ok_or_else(|| format!("the system lists no IPv{} resolver", family.as_str())),
            Err(why) => Err(format!("no system resolver found: {why}")),
        }
    }
}

pub struct Watch {
    pub probe: Arc<dyn NetProbe>,
    /// One destination per family whose route stands for "the route out".
    pub reference: [Option<IpAddr>; 2],
    pub controls: Vec<FollowedControl>,
    pub context: watch::Sender<u32>,
    pub gap: watch::Sender<GapMark>,
    pub actors: crate::collector::IcmpActors,
}

async fn ask<T: Send + 'static>(probe: &Arc<dyn NetProbe>, f: impl FnOnce(&dyn NetProbe) -> T + Send + 'static) -> Option<T> {
    if probe.inline() {
        return Some(f(probe.as_ref()));
    }
    // Off the collector's thread: reading the routing table must never
    // delay a send or a receive timestamp.
    let probe = probe.clone();
    tokio::task::spawn_blocking(move || f(probe.as_ref())).await.ok()
}

async fn read(w: &Watch, gateways: Option<&Context>) -> Option<Context> {
    let reference = w.reference;
    let (v4, v6) = ask(&w.probe, move |p| (reference[0].and_then(|d| p.source_for(d)), reference[1].and_then(|d| p.source_for(d)))).await?;
    // The gateways are carried over unless asked for: they need the slower call.
    let (g4, g6) = match gateways {
        Some(prev) => (prev.v4.gateway, prev.v6.gateway),
        None => {
            let local = ask(&w.probe, |p| p.local()).await?;
            (local.gateway_v4.ok(), local.gateway_v6.ok())
        }
    };
    Some(Context { v4: FamilyContext { source: v4, gateway: g4 }, v6: FamilyContext { source: v6, gateway: g6 } })
}

/// Records the context at start, then watches for changes until cancelled.
pub async fn run(shared: Shared, w: Watch) {
    let Some(mut known) = read(&w, None).await else { return };
    shared.event("source_context", None, format!("source context 1: {}", describe(&known)));
    let follow_local = !w.controls.is_empty();
    let (mut last_poll_ns, mut polls) = (shared.clock.mono_ns(), 0u32);
    loop {
        tokio::select! {
            _ = shared.cancel.cancelled() => return,
            _ = tokio::time::sleep(POLL) => {}
        }
        polls += 1;
        // Mostly the cheap question; the routing table only now and then,
        // and only when a control depends on it.
        let full = follow_local && polls.is_multiple_of(LOCAL_EVERY);
        let Some(mut now) = read(&w, (!full).then_some(&known)).await else { continue };
        let at = shared.clock.mono_ns();
        if now == known {
            last_poll_ns = at;
            continue;
        }
        // Something changed. Whatever the first reading was, get the whole
        // picture before describing it.
        if !full && let Some(whole) = read(&w, None).await {
            now = whole;
        }
        let id = *w.context.borrow() + 1;
        // The change happened somewhere since the last look: that interval
        // is not observation, it is the network being rearranged.
        shared.outbox.record(Record::Gap(GapRec {
            session: shared.session.clone(),
            cause: GapCause::NetworkChange,
            start_ns: last_poll_ns,
            end_ns: at,
            duration_ms: at.saturating_sub(last_poll_ns) / 1_000_000,
            end_utc: utc_string(shared.clock.wall()),
        }));
        w.gap.send_modify(|m| {
            m.generation += 1;
            m.end_ns = at;
        });
        // Replies to probes in flight may arrive on a path that no longer
        // exists, or not at all: cancelled, not timed out.
        for a in w.actors.lock().unwrap().iter() {
            let _ = a.try_send(IcmpCmd::CancelPending(ErrorCategory::Gap));
        }
        w.context.send_replace(id);
        shared.event("source_context_changed", None, format!("source context {id}: {}. Probes in flight were cancelled, not counted as unanswered", describe_change(&known, &now)));

        // Controls on `auto` follow the host: a new gateway or resolver is a
        // new destination, with its own statistics.
        if follow_local && let Some(local) = ask(&w.probe, |p| p.local()).await {
            for c in &w.controls {
                let wanted = control_address(&c.provider, c.family, &local);
                c.dest.send_if_modified(|r| match &wanted {
                    Ok(ip) if r.ip == Some(*ip) => false,
                    Ok(ip) => {
                        *r = Resolved { ready: true, ip: Some(*ip), epoch: r.epoch + 1, error: None };
                        true
                    }
                    Err(why) if r.ip.is_none() && r.error.as_deref() == Some(why.as_str()) => false,
                    Err(why) => {
                        *r = Resolved { ready: true, ip: None, epoch: r.epoch + 1, error: Some(why.clone()) };
                        true
                    }
                });
            }
        }
        (known, last_poll_ns) = (now, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(ip: &str, ifname: &str) -> Option<SourceAddr> {
        Some(SourceAddr { ip: ip.parse().unwrap(), ifname: Some(ifname.into()) })
    }

    #[test]
    fn a_context_and_a_change_are_described_in_words() {
        let home = Context { v4: FamilyContext { source: src("192.0.2.10", "en0"), gateway: Some("192.0.2.1".parse().unwrap()) }, v6: FamilyContext::default() };
        assert_eq!(describe(&home), "IPv4 192.0.2.10 on en0, gateway 192.0.2.1; IPv6 no route");
        let vpn = Context { v4: FamilyContext { source: src("198.51.100.7", "utun3"), gateway: None }, v6: FamilyContext::default() };
        assert_eq!(describe_change(&home, &vpn), "IPv4 192.0.2.10 on en0, gateway 192.0.2.1 → 198.51.100.7 on utun3 (looks like a tunnel), no gateway found");
        let offline = Context::default();
        assert_eq!(describe_change(&vpn, &offline), "IPv4 198.51.100.7 on utun3 (looks like a tunnel), no gateway found → no route");
        assert_eq!(describe_change(&home, &home), "", "nothing changed, nothing said");
    }

    #[test]
    fn tunnels_are_recognised_by_name_only_as_a_hint() {
        for name in ["utun3", "tun0", "wg0", "ppp0", "tailscale0", "ipsec0"] {
            assert!(SourceAddr { ip: "10.0.0.1".parse().unwrap(), ifname: Some(name.into()) }.looks_like_tunnel(), "{name}");
        }
        for name in ["en0", "eth0", "wlan0", "enp3s0", "bridge0", "lo0"] {
            assert!(!SourceAddr { ip: "10.0.0.1".parse().unwrap(), ifname: Some(name.into()) }.looks_like_tunnel(), "{name}");
        }
        assert!(!SourceAddr { ip: "10.0.0.1".parse().unwrap(), ifname: None }.looks_like_tunnel());
    }

    #[test]
    fn the_loopback_interface_is_found_by_its_address() {
        // Every host this runs on has one; its name differs (lo, lo0).
        let name = interface_of("127.0.0.1".parse().unwrap()).expect("the loopback interface");
        assert!(name.starts_with("lo"), "{name}");
        assert_eq!(interface_of("203.0.113.199".parse().unwrap()), None, "an address this host does not have");
    }

    #[test]
    fn controls_follow_what_the_host_says() {
        let local = LocalNet { gateway_v4: Ok("192.168.1.1".parse().unwrap()), gateway_v6: Err("none".into()), resolvers: Ok(vec!["192.168.1.1".parse().unwrap()]) };
        assert_eq!(control_address(localnet::GATEWAY, Family::V4, &local), Ok("192.168.1.1".parse().unwrap()));
        assert_eq!(control_address(localnet::GATEWAY, Family::V6, &local), Err("no default gateway found: none".into()));
        assert_eq!(control_address(localnet::LOCAL_DNS, Family::V6, &local), Err("the system lists no IPv6 resolver".into()));
    }
}

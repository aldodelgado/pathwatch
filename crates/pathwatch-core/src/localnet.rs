//! What this host's own network configuration says: the default gateway and
//! the resolvers the system is set to use. Read from system state — the
//! routing table and the resolver configuration — never inferred by sending
//! anything or by timing a lookup.
//!
//! Used only for the two optional controls. Nothing here is assumed: when a
//! value cannot be found, the reason is carried to the screen.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::model::Family;

/// Provider keys reserved for the controls. A control is a stream like any
/// other to the collector, but it never has a card and is never counted
/// among the streams being compared.
pub const GATEWAY: &str = "gateway";
pub const LOCAL_DNS: &str = "local-dns";

pub fn is_control(provider: &str) -> bool {
    provider == GATEWAY || provider == LOCAL_DNS
}

/// Where the resolver a system uses actually is. It decides what a
/// "local DNS" control can and cannot say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverKind {
    /// On this host (`127.0.0.53`, a local forwarder): the control measures
    /// the stub, its cache included, plus whatever it forwards to.
    Stub,
    /// On a private or link-local network: a resolver on the local segment.
    Local,
    /// A public address: the control measures that resolver over the
    /// internet and says nothing about the local segment.
    Remote,
}

impl ResolverKind {
    pub fn of(ip: IpAddr) -> Self {
        let private = match ip {
            // RFC 1918, link-local, and the shared range carriers hand out.
            IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || (v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64),
            // Unique local and link-local.
            IpAddr::V6(v6) => v6.segments()[0] & 0xfe00 == 0xfc00 || v6.segments()[0] & 0xffc0 == 0xfe80,
        };
        if ip.is_loopback() {
            ResolverKind::Stub
        } else if private {
            ResolverKind::Local
        } else {
            ResolverKind::Remote
        }
    }

    /// A word for a narrow line, where one is needed.
    pub fn tag(self) -> Option<&'static str> {
        match self {
            ResolverKind::Stub => Some("stub"),
            ResolverKind::Local => None,
            ResolverKind::Remote => Some("remote"),
        }
    }

    /// The same as a sentence.
    pub fn note(self) -> Option<&'static str> {
        match self {
            ResolverKind::Stub => Some("a loopback stub: this measures the stub plus whatever it forwards to"),
            ResolverKind::Local => None,
            ResolverKind::Remote => Some("not on the local network: this measures that resolver over the internet, not the local segment"),
        }
    }

    /// How the control is named where it is cited in an event.
    pub fn cited(self) -> &'static str {
        match self {
            ResolverKind::Stub => "local DNS (a loopback stub)",
            ResolverKind::Local => "local DNS",
            ResolverKind::Remote => "system DNS (a remote resolver, not on the local network)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalNet {
    pub gateway_v4: Result<IpAddr, String>,
    pub gateway_v6: Result<IpAddr, String>,
    /// In the order the system lists them.
    pub resolvers: Result<Vec<IpAddr>, String>,
}

impl LocalNet {
    /// For hosts and tests where nothing is to be discovered.
    pub fn unknown(why: &str) -> Self {
        Self { gateway_v4: Err(why.into()), gateway_v6: Err(why.into()), resolvers: Err(why.into()) }
    }

    pub fn gateway(&self, family: Family) -> &Result<IpAddr, String> {
        match family {
            Family::V4 => &self.gateway_v4,
            Family::V6 => &self.gateway_v6,
        }
    }
}

/// `nameserver` lines of a resolv.conf, in order. A zone suffix on an IPv6
/// address (`fe80::1%en0`) is dropped; lines that are not addresses are ignored.
pub fn parse_resolv_conf(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split(['#', ';']).next()?.split_whitespace();
            (words.next()? == "nameserver").then(|| words.next())?
        })
        .filter_map(|addr| addr.split('%').next()?.parse().ok())
        .collect()
}

/// The IPv4 default gateway from Linux's `/proc/net/route`: the default
/// route that is up and has a gateway, lowest metric first.
pub fn parse_proc_route(text: &str) -> Option<Ipv4Addr> {
    const UP_AND_GATEWAY: u32 = 0x1 | 0x2;
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (dest, gateway, flags, metric) = (*f.get(1)?, *f.get(2)?, u32::from_str_radix(f.get(3)?, 16).ok()?, f.get(6)?.parse::<u32>().ok()?);
            (dest == "00000000" && flags & UP_AND_GATEWAY == UP_AND_GATEWAY).then_some(())?;
            // The kernel prints the address as a host-order integer: on the
            // little-endian machines this runs on, the bytes are reversed.
            Some((metric, Ipv4Addr::from(u32::from_str_radix(gateway, 16).ok()?.swap_bytes())))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, ip)| ip)
}

/// The IPv6 default gateway from Linux's `/proc/net/ipv6_route`.
pub fn parse_proc_ipv6_route(text: &str) -> Option<Ipv6Addr> {
    const GATEWAY_FLAG: u32 = 0x2;
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (dest, prefix, next_hop, metric, flags) = (*f.first()?, *f.get(1)?, *f.get(4)?, u32::from_str_radix(f.get(5)?, 16).ok()?, u32::from_str_radix(f.get(8)?, 16).ok()?);
            (dest.bytes().all(|b| b == b'0') && prefix == "00" && flags & GATEWAY_FLAG != 0).then_some(())?;
            let ip = Ipv6Addr::from(u128::from_str_radix(next_hop, 16).ok()?);
            (!ip.is_unspecified()).then_some((metric, ip))
        })
        .min_by_key(|(metric, _)| *metric)
        .map(|(_, ip)| ip)
}

/// The `gateway:` line of `route -n get default` (macOS and the BSDs).
pub fn parse_route_get(text: &str) -> Option<IpAddr> {
    text.lines().find_map(|line| line.trim().strip_prefix("gateway:")?.trim().split('%').next()?.parse().ok())
}

/// An IPv6 gateway is almost always a link-local address, which can only be
/// reached with an interface scope. That is not built, and saying so is
/// better than a control that reports "no route".
fn usable_v6(ip: Ipv6Addr) -> Result<IpAddr, String> {
    if ip.segments()[0] & 0xffc0 == 0xfe80 {
        Err(format!("the IPv6 default gateway is a link-local address ({ip}); probing it needs an interface scope, which is not built"))
    } else {
        Ok(IpAddr::V6(ip))
    }
}

#[cfg(target_os = "linux")]
fn gateways() -> (Result<IpAddr, String>, Result<IpAddr, String>) {
    let read = |path: &str| std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"));
    let v4 = read("/proc/net/route").and_then(|t| parse_proc_route(&t).map(IpAddr::V4).ok_or_else(|| "the routing table has no IPv4 default route".to_string()));
    let v6 = read("/proc/net/ipv6_route").and_then(|t| parse_proc_ipv6_route(&t).ok_or_else(|| "the routing table has no IPv6 default route".to_string())).and_then(usable_v6);
    (v4, v6)
}

/// macOS has no routing table in the filesystem. The system's own `route`
/// tool is asked, by absolute path, with fixed arguments and no shell; its
/// output is used only if it parses as an address.
#[cfg(not(target_os = "linux"))]
fn gateways() -> (Result<IpAddr, String>, Result<IpAddr, String>) {
    let ask = |args: &[&str], family: &str| -> Result<IpAddr, String> {
        let out = std::process::Command::new("/sbin/route").args(args).stdin(std::process::Stdio::null()).output().map_err(|e| format!("cannot run /sbin/route: {e}"))?;
        parse_route_get(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| format!("the routing table has no {family} default route"))
    };
    let v4 = ask(&["-n", "get", "default"], "IPv4").and_then(|ip| if ip.is_ipv4() { Ok(ip) } else { Err("the routing table has no IPv4 default route".into()) });
    let v6 = ask(&["-n", "get", "-inet6", "default"], "IPv6").and_then(|ip| match ip {
        IpAddr::V6(ip) => usable_v6(ip),
        IpAddr::V4(_) => Err("the routing table has no IPv6 default route".into()),
    });
    (v4, v6)
}

/// The name the system's resolver gives for an address, if it has one. This
/// asks the resolver, so it does send a query; it is called only for hops a
/// route trace has found, on a thread of its own, and only when reverse
/// lookups are switched on.
pub fn reverse_name(ip: IpAddr) -> Option<String> {
    let addr = socket2::SockAddr::from(std::net::SocketAddr::new(ip, 0));
    let mut host = [0 as libc::c_char; 256];
    // SAFETY: `addr` is a valid socket address of the length given, `host`
    // is a writable buffer of the length given, and no service name is asked
    // for. With NI_NAMEREQD the call fails rather than return the address.
    let rc = unsafe { libc::getnameinfo(addr.as_ptr().cast(), addr.len(), host.as_mut_ptr(), host.len() as libc::socklen_t, std::ptr::null_mut(), 0, libc::NI_NAMEREQD) };
    if rc != 0 {
        return None;
    }
    // SAFETY: on success getnameinfo wrote a NUL-terminated string into `host`.
    Some(unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }.to_string_lossy().into_owned())
}

/// Reads this host's gateway and resolver configuration. Sends nothing.
pub fn discover() -> LocalNet {
    let (gateway_v4, gateway_v6) = gateways();
    let resolvers = std::fs::read_to_string("/etc/resolv.conf").map_err(|e| format!("cannot read /etc/resolv.conf: {e}")).and_then(|t| {
        let found = parse_resolv_conf(&t);
        if found.is_empty() { Err("/etc/resolv.conf lists no nameserver".into()) } else { Ok(found) }
    });
    LocalNet { gateway_v4, gateway_v6, resolvers }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolv_conf_is_read_as_the_system_reads_it() {
        let text = "# Generated by NetworkManager\nsearch lan example.org\nnameserver 127.0.0.53\nnameserver 192.168.1.1 # the router\n; nameserver 9.9.9.9\nnameserver fe80::1%eth0\nnameserver not-an-address\noptions edns0 trust-ad\n";
        let got = parse_resolv_conf(text);
        assert_eq!(got, vec!["127.0.0.53".parse::<IpAddr>().unwrap(), "192.168.1.1".parse().unwrap(), "fe80::1".parse().unwrap()]);
        assert!(got[0].is_loopback(), "the stub case is recognisable from the address");
        assert_eq!(parse_resolv_conf("search lan\n"), Vec::<IpAddr>::new());
    }

    #[test]
    fn the_ipv4_default_gateway_is_the_lowest_metric_default_route_with_a_gateway() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                     eth0\t00000000\tFE01A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                     tun0\t00000000\t00000000\t0001\t0\t0\t50\t00000000\t0\t0\t0\n";
        assert_eq!(parse_proc_route(table), Some(Ipv4Addr::new(192, 168, 1, 254)), "the wired route wins on metric; a default route with no gateway is not one");
        assert_eq!(parse_proc_route("Iface\tDestination\tGateway\tFlags\n"), None);
        assert_eq!(parse_proc_route(""), None);
    }

    #[test]
    fn the_ipv6_default_gateway_is_found_and_a_link_local_one_is_declined_with_a_reason() {
        let table = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 eth0\n\
                     20010db8000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0\n";
        let gw = parse_proc_ipv6_route(table).unwrap();
        assert_eq!(gw, "fe80::1".parse::<Ipv6Addr>().unwrap());
        let why = usable_v6(gw).unwrap_err();
        assert!(why.contains("link-local") && why.contains("not built"), "{why}");
        assert_eq!(usable_v6("2001:db8::1".parse().unwrap()), Ok("2001:db8::1".parse().unwrap()));
        assert_eq!(
            parse_proc_ipv6_route(
                "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000001 00000000 00200200 lo\n"
            ),
            None
        );
    }

    #[test]
    fn route_get_output_yields_only_a_parsable_gateway() {
        let v4 = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.100.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>\n";
        assert_eq!(parse_route_get(v4), Some("192.168.100.1".parse().unwrap()));
        assert_eq!(parse_route_get("    gateway: fe80::1%en0\n"), Some("fe80::1".parse().unwrap()));
        assert_eq!(parse_route_get("route: writing to routing socket: not in table\n"), None);
        assert_eq!(parse_route_get("    gateway: $(reboot)\n"), None, "anything that is not an address is ignored");
    }

    #[test]
    fn a_resolver_is_a_stub_local_or_remote_by_its_address() {
        let kind = |s: &str| ResolverKind::of(s.parse().unwrap());
        for stub in ["127.0.0.53", "127.0.0.1", "::1"] {
            assert_eq!(kind(stub), ResolverKind::Stub, "{stub}");
        }
        for local in ["192.168.1.1", "10.0.0.2", "172.16.5.1", "172.31.255.254", "169.254.1.1", "100.64.0.1", "100.127.255.254", "fd00::53", "fe80::1"] {
            assert_eq!(kind(local), ResolverKind::Local, "{local}");
        }
        for remote in ["203.0.113.53", "172.32.0.1", "100.128.0.1", "100.63.255.255", "2001:db8::53", "198.51.100.7"] {
            assert_eq!(kind(remote), ResolverKind::Remote, "{remote}");
        }
        assert_eq!((ResolverKind::Local.tag(), ResolverKind::Local.note()), (None, None), "a resolver on the local network needs no caveat");
        assert!(ResolverKind::Remote.note().unwrap().contains("not the local segment") && ResolverKind::Remote.cited().starts_with("system DNS"));
    }

    #[test]
    fn the_control_keys_are_reserved() {
        assert!(is_control("gateway") && is_control("local-dns") && !is_control("cloudflare") && !is_control("Gateway"));
    }
}

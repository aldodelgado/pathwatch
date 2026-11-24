//! Host capability detection. Opens sockets and reads system state; sends
//! nothing and changes nothing. See `docs/design/architecture.md` §5.

use std::io;

use serde::Serialize;

use crate::model::Family;
use crate::platform::icmp_sock::{self, IcmpSocket};

#[derive(Debug, Clone, Serialize)]
pub struct IcmpCapability {
    pub family: Family,
    pub available: bool,
    /// `datagram` when available.
    pub backend: Option<&'static str>,
    pub detail: String,
    /// What the operating system documents as the way to enable it. Shown to
    /// the user; never executed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub os: &'static str,
    pub arch: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    pub running_as_root: bool,
    pub group_ids: Vec<u32>,
    /// Linux `net.ipv4.ping_group_range`, which governs both address families.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_group_range: Option<String>,
    pub icmp: Vec<IcmpCapability>,
    pub echo_identifier_rewritten_by_kernel: bool,
    pub icmp_errors_via_error_queue: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspend_clock_pair: Option<(&'static str, &'static str)>,
    pub dns_udp: &'static str,
    pub dns_tcp: &'static str,
    pub dot: &'static str,
    pub doh: &'static str,
    pub not_in_this_build: Vec<&'static str>,
}

pub fn ping_group_range() -> Option<String> {
    std::fs::read_to_string("/proc/sys/net/ipv4/ping_group_range").ok().map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn group_ids() -> Vec<u32> {
    // SAFETY: getgid has no preconditions; getgroups writes at most `len`
    // entries into the buffer we pass.
    unsafe {
        let mut ids = vec![libc::getgid()];
        let n = libc::getgroups(0, std::ptr::null_mut());
        if n > 0 {
            let mut buf = vec![0 as libc::gid_t; n as usize];
            let got = libc::getgroups(n, buf.as_mut_ptr());
            if got > 0 {
                buf.truncate(got as usize);
                ids.extend(buf);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

fn kernel_release() -> Option<String> {
    // SAFETY: all-zero is a valid utsname, and uname fills it.
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        if libc::uname(&raw mut u) != 0 {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(u.release.as_ptr()).to_string_lossy().into_owned())
    }
}

/// A sentence explaining why the unprivileged ICMP socket could not be opened.
pub fn explain_icmp_open_error(family: Family, e: &io::Error) -> String {
    let v = family.as_str();
    match e.raw_os_error() {
        Some(libc::EACCES | libc::EPERM) if cfg!(target_os = "linux") => {
            let range = ping_group_range().unwrap_or_else(|| "unknown".into());
            format!("unprivileged ICMP is disabled on this host for your groups (net.ipv4.ping_group_range = \"{range}\")")
        }
        Some(libc::EAFNOSUPPORT | libc::EPROTONOSUPPORT) => format!("this host does not support IPv{v} ICMP datagram sockets"),
        _ => format!("cannot open an IPv{v} ICMP datagram socket: {e}"),
    }
}

fn remedy(e: &io::Error) -> Option<String> {
    if cfg!(target_os = "linux") && matches!(e.raw_os_error(), Some(libc::EACCES | libc::EPERM)) {
        // SAFETY: getgid has no preconditions.
        let gid = unsafe { libc::getgid() };
        Some(format!(
            "An administrator can allow it with the sysctl systemd's defaults use — `net.ipv4.ping_group_range = 0 2147483647` in a file under /etc/sysctl.d/ — \
             or, inside a container with a narrow group mapping, a range covering only your group (`{gid} {gid}`). pathwatch never changes this for you; DNS and DoH measurement work without it."
        ))
    } else {
        None
    }
}

/// Probes what this host permits. Opens and closes sockets; sends nothing.
pub fn probe() -> Report {
    let icmp = [Family::V4, Family::V6]
        .into_iter()
        .map(|family| match IcmpSocket::open(family) {
            Ok(_) => IcmpCapability { family, available: true, backend: Some("datagram"), detail: "unprivileged ICMP datagram socket opened".into(), remedy: None },
            Err(e) => IcmpCapability { family, available: false, backend: None, detail: explain_icmp_open_error(family, &e), remedy: remedy(&e) },
        })
        .collect();
    Report {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        kernel: kernel_release(),
        // SAFETY: geteuid has no preconditions.
        running_as_root: unsafe { libc::geteuid() } == 0,
        group_ids: group_ids(),
        ping_group_range: ping_group_range(),
        icmp,
        echo_identifier_rewritten_by_kernel: icmp_sock::KERNEL_REWRITES_ID,
        icmp_errors_via_error_queue: icmp_sock::HAS_ERROR_QUEUE,
        suspend_clock_pair: crate::platform::suspend_clock_names(),
        dns_udp: "available (no privilege needed)",
        dns_tcp: "available (no privilege needed)",
        dot: "available (no privilege needed)",
        doh: "available (no privilege needed)",
        not_in_this_build: vec![
            "source/interface binding",
            "raw-socket ICMP",
            "ICMP and DNS retries",
            "DoH by GET or through a proxy",
            "TLS session resumption for fresh-connection probes",
            "an explicit bootstrap resolver",
            "UDP or TCP route tracing",
        ],
    }
}

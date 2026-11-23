//! Everything that touches `libc` directly lives under this module.
//!
//! The two supported systems differ in what an unprivileged ICMP datagram
//! socket delivers; see `planning/02-architecture.md` §3.2 and §5.

pub mod icmp_sock;

/// Reads a (suspend-excluding, suspend-including) pair of monotonic clocks in
/// nanoseconds. Their difference grows by the time the host spends suspended.
///
/// Linux: `CLOCK_MONOTONIC` / `CLOCK_BOOTTIME`.
/// macOS: `CLOCK_UPTIME_RAW` / `CLOCK_MONOTONIC_RAW`.
pub fn suspend_clock_pair() -> Option<(u64, u64)> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let (excl, incl) = (libc::CLOCK_MONOTONIC, libc::CLOCK_BOOTTIME);
    #[cfg(target_vendor = "apple")]
    let (excl, incl) = (libc::CLOCK_UPTIME_RAW, libc::CLOCK_MONOTONIC_RAW);
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    return None;

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    Some((clock_ns(excl)?, clock_ns(incl)?))
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn clock_ns(id: libc::clockid_t) -> Option<u64> {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    let rc = unsafe { libc::clock_gettime(id, &mut ts) };
    (rc == 0).then(|| ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

/// Names of the clock pair in use, for `doctor`.
pub fn suspend_clock_names() -> Option<(&'static str, &'static str)> {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        Some(("CLOCK_MONOTONIC", "CLOCK_BOOTTIME"))
    } else if cfg!(target_vendor = "apple") {
        Some(("CLOCK_UPTIME_RAW", "CLOCK_MONOTONIC_RAW"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_pair_is_readable_and_ordered() {
        let (a0, b0) = suspend_clock_pair().expect("supported platform");
        let (a1, b1) = suspend_clock_pair().unwrap();
        assert!(a1 >= a0 && b1 >= b0);
    }
}

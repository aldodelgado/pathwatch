//! Unprivileged ICMP datagram sockets (`SOCK_DGRAM` + `IPPROTO_ICMP[V6]`).
//!
//! What the socket delivers differs by system (verified against kernel and XNU
//! source; see `docs/design/architecture.md` §3.2):
//!
//! | | Linux | macOS |
//! |---|---|---|
//! | Echo identifier | rewritten to the socket's port; replies demultiplexed by it | preserved; not used for filtering |
//! | `recv` returns | Echo Reply only, from the ICMP header | every inbound ICMP message, IPv4 with its IP header |
//! | ICMP errors | only via the error queue (`IP_RECVERR`) | on the same socket, if unconnected |
//! | Reply TTL | `IP_TTL` control message (`IP_RECVTTL`) | from the delivered IP header |

use std::io;
use std::mem::{size_of, size_of_val, zeroed};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::{AsRawFd, RawFd};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::model::Family;

/// True where the kernel overwrites the echo identifier and filters replies itself.
pub const KERNEL_REWRITES_ID: bool = cfg!(any(target_os = "linux", target_os = "android"));
/// True where IPv4 datagrams arrive with their IP header.
pub const V4_INCLUDES_IP_HEADER: bool = cfg!(target_vendor = "apple");
/// True where ICMP errors are read from a separate socket error queue.
pub const HAS_ERROR_QUEUE: bool = cfg!(any(target_os = "linux", target_os = "android"));

pub struct IcmpSocket {
    sock: Socket,
    family: Family,
}

#[derive(Debug, Clone, Copy)]
pub struct Datagram {
    pub len: usize,
    pub src: Option<IpAddr>,
    pub ttl: Option<u8>,
    /// How long the datagram sat between the kernel receiving it and this
    /// process reading it, where the kernel stamps arrivals. A diagnostic of
    /// the instrument's own delay; it is not subtracted from anything.
    pub recv_delay_ns: Option<u64>,
}

/// An entry from the socket error queue: the kernel's report of an ICMP error
/// for a packet this socket sent. The data read alongside it is the start of
/// the offending packet (our echo request, from its ICMP header).
#[derive(Debug, Clone, Copy)]
pub struct QueuedError {
    pub len: usize,
    /// Destination of the packet that caused the error.
    pub dest: Option<IpAddr>,
    /// The router that reported it.
    pub offender: Option<IpAddr>,
    pub from_icmp: bool,
    pub icmp_type: u8,
    pub code: u8,
    pub errno: u32,
}

impl AsRawFd for IcmpSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }
}

/// The receive buffer asked for: several hundred replies' worth.
const RECV_BUFFER: usize = 256 * 1024;

impl IcmpSocket {
    /// Opens a non-blocking unprivileged ICMP socket. Fails with `EACCES` on
    /// Linux when `net.ipv4.ping_group_range` excludes the process's groups.
    pub fn open(family: Family) -> io::Result<Self> {
        let (domain, proto) = match family {
            Family::V4 => (Domain::IPV4, Protocol::ICMPV4),
            Family::V6 => (Domain::IPV6, Protocol::ICMPV6),
        };
        let sock = Socket::new(domain, Type::DGRAM, Some(proto))?;
        sock.set_nonblocking(true)?;
        // Room for a burst. On macOS every ICMP socket is handed a copy of
        // every echo reply arriving at the host — other programs' included —
        // and the default buffer is 8 KiB, a few dozen of them. Once it is
        // full the kernel drops what arrives without a word, our own answer
        // included, which would then be reported as a timeout the network
        // never caused: seen with other ping and trace tools running beside us. Only ever enlarged; if the
        // system refuses, the default stands.
        if sock.recv_buffer_size().is_ok_and(|have| have < RECV_BUFFER) {
            let _ = sock.set_recv_buffer_size(RECV_BUFFER);
        }
        let fd = sock.as_raw_fd();
        match family {
            Family::V4 => {
                #[cfg(any(target_os = "linux", target_os = "android"))]
                {
                    set_int(fd, libc::IPPROTO_IP, libc::IP_RECVERR, 1)?;
                    set_int(fd, libc::IPPROTO_IP, libc::IP_RECVTTL, 1)?;
                }
            }
            Family::V6 => {
                #[cfg(any(target_os = "linux", target_os = "android"))]
                set_int(fd, libc::IPPROTO_IPV6, libc::IPV6_RECVERR, 1)?;
                set_int(fd, libc::IPPROTO_IPV6, libc::IPV6_RECVHOPLIMIT, 1)?;
            }
        }
        // Ask the kernel to stamp each arrival, on a clock we can read back:
        // the monotonic clock on macOS, the realtime clock on Linux. Used only
        // to measure our own receive delay; if refused, that diagnostic is absent.
        #[cfg(target_vendor = "apple")]
        let _ = set_int(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMP_MONOTONIC, 1);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let _ = set_int(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMPNS, 1);
        let _ = fd;
        // Binding fixes the local "port", which on Linux is the echo identifier.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let any: IpAddr = match family {
                Family::V4 => Ipv4Addr::UNSPECIFIED.into(),
                Family::V6 => Ipv6Addr::UNSPECIFIED.into(),
            };
            sock.bind(&SockAddr::from(SocketAddr::new(any, 0)))?;
        }
        Ok(Self { sock, family })
    }

    pub fn family(&self) -> Family {
        self.family
    }

    /// The receive buffer the kernel actually granted, in bytes.
    pub fn recv_buffer(&self) -> io::Result<usize> {
        self.sock.recv_buffer_size()
    }

    /// The identifier the kernel will stamp on our echo requests, where it does so.
    pub fn kernel_id(&self) -> Option<u16> {
        if !KERNEL_REWRITES_ID {
            return None;
        }
        self.sock.local_addr().ok()?.as_socket().map(|a| a.port())
    }

    /// Sets the TTL (hop limit) of packets sent from now on. Used only by
    /// route tracing, on a socket of its own.
    pub fn set_ttl(&self, ttl: u8) -> io::Result<()> {
        match self.family {
            Family::V4 => set_int(self.sock.as_raw_fd(), libc::IPPROTO_IP, libc::IP_TTL, libc::c_int::from(ttl)),
            Family::V6 => set_int(self.sock.as_raw_fd(), libc::IPPROTO_IPV6, libc::IPV6_UNICAST_HOPS, libc::c_int::from(ttl)),
        }
    }

    pub fn send_to(&self, dest: IpAddr, packet: &[u8]) -> io::Result<usize> {
        self.sock.send_to(packet, &SockAddr::from(SocketAddr::new(dest, 0)))
    }

    /// Reads one datagram. `WouldBlock` when none is queued.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<Datagram> {
        let m = recvmsg(self.sock.as_raw_fd(), buf, 0)?;
        Ok(Datagram { len: m.len, src: m.name, ttl: m.ttl, recv_delay_ns: m.recv_delay_ns })
    }

    /// Reads one entry from the error queue. `WouldBlock` when it is empty.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn recv_error(&self, buf: &mut [u8]) -> io::Result<QueuedError> {
        let m = recvmsg(self.sock.as_raw_fd(), buf, libc::MSG_ERRQUEUE)?;
        let e = m.ext_err.ok_or_else(|| io::Error::other("error-queue message without extended error"))?;
        Ok(QueuedError {
            len: m.len,
            dest: m.name,
            offender: m.offender,
            from_icmp: matches!(e.ee_origin, libc::SO_EE_ORIGIN_ICMP | libc::SO_EE_ORIGIN_ICMP6),
            icmp_type: e.ee_type,
            code: e.ee_code,
            errno: e.ee_errno,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub fn recv_error(&self, _buf: &mut [u8]) -> io::Result<QueuedError> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}

fn set_int(fd: RawFd, level: libc::c_int, name: libc::c_int, value: libc::c_int) -> io::Result<()> {
    // SAFETY: `value` is a valid c_int for the duration of the call and the
    // length passed matches its size.
    let rc = unsafe { libc::setsockopt(fd, level, name, (&raw const value).cast(), size_of::<libc::c_int>() as libc::socklen_t) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// `SCM_TIMESTAMP_MONOTONIC` from the macOS SDK's `sys/socket.h`: a `uint64_t`
/// in `mach_absolute_time` units. Not exported by the `libc` crate.
#[cfg(target_vendor = "apple")]
const SCM_TIMESTAMP_MONOTONIC: libc::c_int = 0x04;

/// Reads the clock the kernel stamps arrivals with, in nanoseconds. On macOS
/// that is `CLOCK_UPTIME_RAW`, which is `mach_absolute_time` in nanoseconds.
#[cfg(target_vendor = "apple")]
fn rx_clock_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &raw mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// The one Mach call needed to convert the kernel's tick-valued stamp. Declared
/// here because the `libc` crate's bindings for it are deprecated.
#[cfg(target_vendor = "apple")]
mod mach {
    #[repr(C)]
    pub struct TimebaseInfo {
        pub numer: u32,
        pub denom: u32,
    }

    unsafe extern "C" {
        pub fn mach_timebase_info(info: *mut TimebaseInfo) -> i32;
    }
}

#[cfg(target_vendor = "apple")]
fn mach_ticks_to_ns(ticks: u64) -> u64 {
    static TIMEBASE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = mach::TimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: `info` is a valid, writable struct with the layout of
        // mach_timebase_info_data_t (two 32-bit unsigned integers).
        let rc = unsafe { mach::mach_timebase_info(&raw mut info) };
        if rc == 0 && info.denom != 0 { (u64::from(info.numer), u64::from(info.denom)) } else { (1, 1) }
    });
    (u128::from(ticks) * u128::from(numer) / u128::from(denom)) as u64
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn rx_clock_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &raw mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn rx_clock_ns() -> u64 {
    0
}

struct RawMsg {
    len: usize,
    recv_delay_ns: Option<u64>,
    name: Option<IpAddr>,
    ttl: Option<u8>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    ext_err: Option<libc::sock_extended_err>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    offender: Option<IpAddr>,
}

/// Control-message buffer with the alignment `cmsghdr` needs.
#[repr(C, align(8))]
struct ControlBuf([u8; 512]);

fn recvmsg(fd: RawFd, buf: &mut [u8], flags: libc::c_int) -> io::Result<RawMsg> {
    // SAFETY: all-zero is a valid value for these plain C structs.
    let mut name: libc::sockaddr_storage = unsafe { zeroed() };
    let mut control = ControlBuf([0; 512]);
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    // SAFETY: as above; msghdr has platform-specific padding that must be zero.
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_name = (&raw mut name).cast();
    msg.msg_namelen = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = size_of_val(&control.0) as _;

    // SAFETY: `msg` points at buffers that live for the whole call, with the
    // lengths set above.
    let n = unsafe { libc::recvmsg(fd, &raw mut msg, flags) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    // Read the stamping clock at once, before any parsing, so the delay
    // measured is the time the datagram waited for this call.
    let read_at_ns = rx_clock_ns();
    let mut kernel_rx_ns: Option<u64> = None;

    let mut out = RawMsg {
        len: n as usize,
        recv_delay_ns: None,
        // SAFETY: the kernel wrote at most `msg_namelen` bytes into `name`.
        name: unsafe { ip_from_sockaddr((&raw const name).cast(), msg.msg_namelen as usize) },
        ttl: None,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        ext_err: None,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        offender: None,
    };

    // SAFETY: the CMSG_* macros walk the control buffer the kernel filled,
    // bounded by `msg_controllen`; each header is checked for null before use.
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&raw const msg);
        while !c.is_null() {
            let hdr = &*c;
            let data = libc::CMSG_DATA(c);
            let data_len = (hdr.cmsg_len as usize).saturating_sub(data as usize - c as usize);
            match (hdr.cmsg_level, hdr.cmsg_type) {
                // Linux reports the IPv4 TTL as an IP_TTL message carrying an int;
                // BSD-derived systems use IP_RECVTTL carrying one byte.
                (libc::IPPROTO_IP, libc::IP_TTL) if data_len >= size_of::<libc::c_int>() => {
                    out.ttl = Some(std::ptr::read_unaligned(data.cast::<libc::c_int>()) as u8);
                }
                (libc::IPPROTO_IP, libc::IP_RECVTTL) if data_len >= 1 => {
                    out.ttl = Some(*data);
                }
                (libc::IPPROTO_IPV6, libc::IPV6_HOPLIMIT) if data_len >= size_of::<libc::c_int>() => {
                    out.ttl = Some(std::ptr::read_unaligned(data.cast::<libc::c_int>()) as u8);
                }
                #[cfg(target_vendor = "apple")]
                (libc::SOL_SOCKET, SCM_TIMESTAMP_MONOTONIC) if data_len >= size_of::<u64>() => {
                    kernel_rx_ns = Some(mach_ticks_to_ns(std::ptr::read_unaligned(data.cast::<u64>())));
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                (libc::SOL_SOCKET, libc::SCM_TIMESTAMPNS) if data_len >= size_of::<libc::timespec>() => {
                    let ts = std::ptr::read_unaligned(data.cast::<libc::timespec>());
                    kernel_rx_ns = Some(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64);
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                (libc::IPPROTO_IP, libc::IP_RECVERR) | (libc::IPPROTO_IPV6, libc::IPV6_RECVERR) if data_len >= size_of::<libc::sock_extended_err>() => {
                    out.ext_err = Some(std::ptr::read_unaligned(data.cast::<libc::sock_extended_err>()));
                    // SO_EE_OFFENDER: the reporting router's address follows the struct.
                    let off = size_of::<libc::sock_extended_err>();
                    out.offender = ip_from_sockaddr(data.add(off), data_len - off);
                }
                _ => {}
            }
            c = libc::CMSG_NXTHDR(&raw const msg, c);
        }
    }
    // Discard implausible values (a realtime clock step between stamp and read).
    out.recv_delay_ns = kernel_rx_ns.and_then(|k| read_at_ns.checked_sub(k)).filter(|d| *d < 60_000_000_000);
    Ok(out)
}

/// Reads an IP address out of a `sockaddr` of `len` valid bytes.
///
/// # Safety
/// `p` must point to at least `len` readable bytes.
unsafe fn ip_from_sockaddr(p: *const u8, len: usize) -> Option<IpAddr> {
    if len < size_of::<libc::sockaddr>() {
        return None;
    }
    // SAFETY: at least `sizeof(sockaddr)` bytes are readable per the check above.
    let family = libc::c_int::from(unsafe { std::ptr::read_unaligned(p.cast::<libc::sockaddr>()) }.sa_family);
    if family == libc::AF_INET && len >= size_of::<libc::sockaddr_in>() {
        // SAFETY: length checked for sockaddr_in.
        let sin = unsafe { std::ptr::read_unaligned(p.cast::<libc::sockaddr_in>()) };
        let v4 = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        (!v4.is_unspecified()).then_some(IpAddr::V4(v4))
    } else if family == libc::AF_INET6 && len >= size_of::<libc::sockaddr_in6>() {
        // SAFETY: length checked for sockaddr_in6.
        let sin6 = unsafe { std::ptr::read_unaligned(p.cast::<libc::sockaddr_in6>()) };
        let v6 = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
        (!v6.is_unspecified()).then_some(IpAddr::V6(v6))
    } else {
        None
    }
}

/// The local address the OS would use to reach `dest`, found by connecting a
/// UDP socket. Nothing is sent.
pub fn local_source_for(dest: IpAddr) -> Option<IpAddr> {
    let bind: SocketAddr = match dest {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let s = std::net::UdpSocket::bind(bind).ok()?;
    s.connect((dest, 9)).ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

//! Debug aid: what does the unprivileged ICMP socket deliver on loopback?
use pathwatch_core::model::Family;
use pathwatch_core::platform::icmp_sock::IcmpSocket;
use pathwatch_core::probe::icmp_packet::{Token, build_echo};
use std::time::{Duration, Instant};

fn main() {
    for (family, dest) in [(Family::V4, "127.0.0.1"), (Family::V6, "::1")] {
        let s = IcmpSocket::open(family).unwrap();
        println!("--- {dest} kernel_id={:?}", s.kernel_id());
        let mut buf = [0u8; 2048];
        for seq in 0..3u64 {
            let pkt = build_echo(family, 0x1234, Token { nonce: 1, stream: 0, seq }, 56);
            let r = s.send_to(dest.parse().unwrap(), &pkt);
            println!("send seq {seq}: {r:?}");
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(300) {
                match s.recv(&mut buf) {
                    Ok(d) => println!(
                        "  recv len={} src={:?} ttl={:?} head={:02x?} icmp={:02x?}",
                        d.len,
                        d.src,
                        d.ttl,
                        &buf[..12.min(d.len)],
                        &buf[d.len.min(if family == Family::V4 { 20 } else { 0 })..d.len.min(if family == Family::V4 { 28 } else { 8 })]
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                    Err(e) => {
                        println!("  recv error {e}");
                        break;
                    }
                }
            }
        }
    }
}

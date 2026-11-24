# pathwatch — references and assumptions

These are the primary sources behind [design.md](design.md) and [architecture.md](architecture.md). Each row names the claims it supports in those documents. The "Supports" column paraphrases the source; addresses, versions, dates, and licences are given as the source stated them.

The sources were read when the design was written. Providers and projects change their documentation, so re-check a claim against its source before relying on it.

Section 1.3 lists sources for the Go and Python stacks that were compared with Rust; the comparison itself is in [architecture.md](architecture.md).

---

## 1. Sources

### 1.1 Rust UI and runtime

| Source | Supports |
|---|---|
| https://ratatui.rs/ · https://ratatui.rs/concepts/backends/ | Ratatui 0.30.2; Crossterm 0.29 default and 0.28 by feature flag; warning against two Crossterm majors; use the re-export |
| https://crates.io/api/v1/crates/ratatui (and the same path for every crate named in the [candidate dependency set](architecture.md#31-dependency-set)) | versions, release dates, licences, minimum Rust versions, feature names |
| https://raw.githubusercontent.com/ratatui/ratatui/main/BREAKING-CHANGES.md · `ARCHITECTURE.md` · `MAINTAINERS.md` | workspace split since 0.30; queued 0.31 breaking changes; four maintainers; no 1.0 plan found |
| https://docs.rs/ratatui/latest/ratatui/widgets/index.html and widget pages; `ratatui-widgets/src/chart.rs` at tag `ratatui-v0.30.2` | widget list; no histogram or heatmap; `Chart` joins every consecutive point (no gaps); axis-label limitation; `Sparkline` absent values; marker types |
| https://docs.rs/ratatui/latest/ratatui/fn.init.html · `ratatui/src/init.rs` at the tag | `init`/`restore`/`run`; panic hook; no mouse capture, paste, or focus handling in `init` |
| https://docs.rs/ratatui/latest/ratatui/style/enum.Color.html · `ratatui-crossterm/src/lib.rs` | colours passed through unchanged; no automatic downgrade |
| https://ratatui.rs/recipes/testing/snapshots/ · https://docs.rs/ratatui/latest/ratatui/backend/struct.TestBackend.html | `TestBackend`; `insta` snapshots capture text, not styles |
| https://ratatui.rs/recipes/apps/terminal-and-event-handler/ | suspend recipe (exit, raise `SIGTSTP`, re-enter) |
| https://github.com/ratatui/ratatui/issues/2610 | accessibility: nothing implemented in the library |
| https://docs.rs/crossterm/latest/crossterm/event/index.html · `EventStream` page · `src/style.rs` and `style/types/colored.rs` at 0.29 · `CHANGELOG.md` on master | event types and opt-in commands; `EventStream`; `NO_COLOR` handled in Crossterm; no release since 2025-04; breaking release pending |
| https://docs.rs/tokio/latest/tokio/time/enum.MissedTickBehavior.html · `tokio/src/time/interval.rs` at tag `tokio-1.54.1` | exact semantics of `Burst` (default), `Delay`, `Skip`; the 5 ms note |
| https://docs.rs/tokio/latest/tokio/time/fn.pause.html · `fn.advance.html` · `tokio/src/time/clock.rs` | paused clock needs the current-thread runtime; auto-advance; caveats of `advance` |
| https://docs.rs/tokio/latest/tokio/sync/mpsc/fn.channel.html · https://docs.rs/tokio/latest/tokio/sync/watch/index.html | bounded channel backpressure and `try_send`; `watch` retains only the last value |
| https://doc.rust-lang.org/stable/std/time/struct.Instant.html · `library/std/src/sys/time/unix.rs` at 1.100.0 | `Instant` uses `CLOCK_MONOTONIC` on Linux and `CLOCK_UPTIME_RAW` on macOS; suspend behaviour is platform-specific |
| https://doc.rust-lang.org/stable/rustc/platform-support.html · https://doc.rust-lang.org/reference/linkage.html | target tiers; static linking notes |
| https://github.com/axodotdev/cargo-dist · https://github.com/rust-cross/cargo-zigbuild | release tooling status |

### 1.2 Rust networking

| Source | Supports |
|---|---|
| https://docs.rs/socket2/0.6.5/x86_64-unknown-linux-gnu/socket2/struct.Socket.html · `src/sys/unix.rs` at `v0.6.5` | socket creation, TTL, `recvmsg`, interface binding calls; no `IP_RECVERR` or IPv4 `IP_RECVTTL` setter |
| https://github.com/kolapapa/surge-ping (`src/client.rs`, `ping.rs`, `icmp/icmpv4.rs`, `CHANGELOG.md`) | datagram-then-raw; send timestamp after the send call; duplicates dropped; no error-queue handling; no payload matching |
| https://docs.rs/trippy-core/0.13.0/trippy_core/ · source at tag `0.13.0` · `RELEASES.md` | 0.13.0 (2025-05-05), Apache-2.0; "the public API is not stable"; `Builder`/`Tracer`/`Round`/`ProbeStatus`; thread-based; `max_rounds`; no stop API in 0.13.0; `SystemTime` probe timestamps |
| https://trippy.rs/guides/privileges/ | raw sockets normally required; `CAP_NET_RAW` on Linux; unprivileged mode only on macOS; Paris/Dublin unavailable unprivileged |
| Trippy CLI reference for 0.13.0 (docs in the repository) | report modes including `json`; no schema-stability statement |
| https://raw.githubusercontent.com/traviscross/mtr/master/man/mtr.8.in · iputils `doc/ping.xml` · Apple `ping.8` · gping readme | machine-readable output available in `mtr`; none in macOS `ping`; experimental and unreleased in iputils; none in gping |
| https://github.com/hickory-dns/hickory-dns/releases/tag/v0.26.0 · https://docs.rs/hickory-proto/0.26.3/hickory_proto/op/struct.Message.html · https://docs.rs/hickory-net/0.26.3/hickory_net/client/struct.Client.html | `hickory-client` folded into `hickory-net`; `hickory-proto` is codec-only; `Message` encode/decode API; no phase timing or peer address in the client |
| https://raw.githubusercontent.com/rustls/tokio-rustls/v/0.26.6/src/client.rs · https://docs.rs/rustls-pki-types/latest/rustls_pki_types/enum.ServerName.html · https://docs.rs/rustls/0.23.46/rustls/client/struct.ClientConnection.html | server name passed separately from the connected stream; SNI for DNS names, none for IP names; ALPN, version, and handshake kind readable |
| https://aws.github.io/aws-lc-rs/requirements/linux.html · `apple.html` · `platform_support.html` · rustls README | default crypto provider; C compiler needed; no CMake for non-FIPS; musl targets listed |
| https://github.com/rustls/rustls-platform-verifier (README) | OS trust store verification; revocation on macOS |
| https://docs.rs/hyper/1.12.0/hyper/client/conn/http2/fn.handshake.html · https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html | connection-level HTTP/2 API; reqwest exposes neither phases nor reuse |
| https://docs.rs/netdev/0.47.0/netdev/ · netwatcher README · https://docs.rs/system-configuration/0.8.0/ · https://www.freedesktop.org/software/systemd/man/latest/systemd-resolved.service.html | default interface/gateway; change events; macOS resolver configuration; the `127.0.0.53` stub |

### 1.3 Go and Python stacks

| Source | Supports |
|---|---|
| https://github.com/charmbracelet/bubbletea (README, releases, `UPGRADE_GUIDE_V2.md`) · https://pkg.go.dev/charm.land/bubbletea/v2 | v2.1.2; `charm.land` import path; declarative view; panic recovery; suspend/resume; synchronized output; built-in colour downsampling |
| https://github.com/charmbracelet/lipgloss · https://github.com/charmbracelet/bubbles (`go.mod`, upgrade guides) · https://pkg.go.dev/github.com/charmbracelet/colorprofile | v2 versions; v2-with-v2 requirement; components; colour profile detection |
| https://github.com/NimbleMarkets/ntcharts (v2 branch README and `go.mod`) | v2.9.0 on Bubble Tea v2; chart types; API still subject to change |
| https://pkg.go.dev/golang.org/x/net/icmp · https://github.com/prometheus-community/pro-bing | unprivileged `udp4`/`udp6` ICMP endpoints on Darwin and Linux; pro-bing callbacks and licence change |
| https://github.com/miekg/dns (README) · https://codeberg.org/miekg/dns | v1 fixes-only and to be archived; v2 pre-1.0 |
| https://pkg.go.dev/net/http/httptrace · https://pkg.go.dev/time#NewTicker · https://pkg.go.dev/testing/synctest · https://go.dev/doc/go1.25 | DoH phase hooks; ticker drops ticks; fake-clock testing |
| https://goreleaser.com/customization/homebrew_casks/ · https://pkg.go.dev/cmd/cgo | packaging |
| https://pypi.org/pypi/textual/json · https://textual.textualize.io/widget_gallery/ · https://textual.textualize.io/blog/2025/05/07/the-future-of-textualize/ · Textual `docs/roadmap.md` and bug-report template | 8.2.8 (2026-06-30), no release since; widgets; no built-in charts; maintenance statements; accessibility roadmap |
| https://pypi.org/pypi/textual-plot/json · https://pypi.org/pypi/textual-plotext/json | third-party chart widgets and their status |
| https://github.com/ValentinBELYN/icmplib · https://dnspython.readthedocs.io/en/stable/query.html and `dns/asyncquery.py` · https://github.com/pydantic/httpx2 | icmplib status and licence; dnspython destination/SNI control and single total time; httpx2 |
| https://docs.python.org/3/library/asyncio-eventloop.html · `asyncio-dev.html` | monotonic loop clock; blocking callbacks delay all tasks |
| https://docs.astral.sh/uv/guides/tools/ · https://pyinstaller.org/en/stable/operating-mode.html | Python distribution options and caveats |

### 1.4 Providers

| Source | Supports |
|---|---|
| https://developers.cloudflare.com/1.1.1.1/ip-addresses/ | Cloudflare resolver IPv4/IPv6 addresses (and the filtered variants) |
| https://developers.cloudflare.com/1.1.1.1/encryption/dns-over-https/ · `.../make-api-requests/` · https://developers.cloudflare.com/1.1.1.1/encryption/dns-over-tls/ | DoH endpoint `cloudflare-dns.com/dns-query`, formats, HTTP/2 minimum; DoT on 853 with `one.one.one.one` |
| https://developers.cloudflare.com/1.1.1.1/troubleshooting/ · `/faq/` · `/infrastructure/network-operators/` · `/infrastructure/extended-dns-error-codes/` | UDP and TCP examples; no ICMP statement; no numeric rate limit; no ECS; supported EDE codes |
| https://blog.cloudflare.com/unauthorized-issuance-of-certificates-for-1-1-1-1 | resolver certificate has carried IP subject alternative names (point in time, not a guarantee) |
| https://developers.google.com/speed/public-dns/docs/using · `/docs/doh` · `/docs/dns-over-tls` · `/docs/secure-transports` | Google addresses; `dns.google` DoH and DoT; request size limit; TLS versions |
| https://developers.google.com/speed/public-dns/docs/troubleshooting | ICMP not blocked but ICMP errors rate-limited and ICMP possibly de-prioritised; ping shows network latency, not resolution speed; traceroute to `dns.google` recommended |
| https://developers.google.com/speed/public-dns/docs/isp · `/docs/security` · `/faq` | 1,500 queries/s per address (the troubleshooting page mentions 1,000); truncation under load; ECS use; no filtering |
| https://help.nextdns.io/t/h7hmvak/what-is-anycast-and-ultralow | Anycast vs Ultralow; Ultralow needs hostname-based transports |
| https://help.nextdns.io/t/x2hmvas/ · https://help.nextdns.io/t/m1hmv0k/which-setup-type-to-use · https://help.nextdns.io/t/x2h76ay/device-information-log-enrichment · https://help.nextdns.io/t/g9hmvan/what-is-linked-ip | supported transports; DoH and DoT endpoint shapes; profile id in the server name; linked IP for plain IPv4 |
| https://github.com/nextdns/nextdns (`run.go`, wiki Configuration) · https://github.com/nextdns/diag (`main.go`) | fixed anycast addresses `45.90.28.0`, `45.90.30.0`, `2a07:a8c0::`, `2a07:a8c1::`; what NextDNS's own diagnostic tool measures |
| https://nextdns.github.io/api/ · NextDNS help forum staff replies | block-page setting; blocked answers may be the unspecified address |
| Arelion, *Important Information Regarding Your IP Address Assignment*, V 4.1, 2023-02-23 (URL not recorded here) | five resolver address pairs described as Arelion's public DNS resolvers; no transport, access policy, or encrypted DNS stated |
| https://www.peeringdb.com/asn/1299 · https://lg.twelve99.net/ (landing page only) · https://www.arelion.com/our-network · Arelion IP Services SLA v3.0.1 | AS1299 identity; looking glass exists; no published customer measurement target; no documentation of router hostname conventions |

### 1.5 Operating systems

| Source | Supports |
|---|---|
| https://man7.org/linux/man-pages/man7/icmp.7.html · https://docs.kernel.org/networking/ip-sysctl.html · Linux `net/ipv4/ping.c`, `net/ipv6/ping.c`, `net/ipv4/icmp.c` | `ping_group_range` and its default; identifier rewriting; only Echo Reply delivered; errors via the error queue; TTL and timestamps on ping sockets; kernel versions |
| https://man7.org/linux/man-pages/man2/IP_RECVERR.2const.html · https://man7.org/linux/man-pages/man8/traceroute.8.html | error-queue format; unprivileged traceroute methods |
| systemd `NEWS` (v243) and `sysctl.d/50-default.conf` · Fedora change page · Debian `linux-base` sources · https://bugs.launchpad.net/ubuntu/+source/iputils/+bug/2089938 · https://github.com/moby/moby/pull/41030 | which distributions and containers enable unprivileged ICMP |
| https://man7.org/linux/man-pages/man7/raw.7.html · `capabilities.7` · `execve.2` · `ld.so.8` | raw sockets need `CAP_NET_RAW`; file-capability caveats |
| Linux `net/core/sock.c` at v5.6 and v5.7 · https://man7.org/linux/man-pages/man7/socket.7.html | `SO_BINDTODEVICE` privilege change in 5.7 |
| XNU `bsd/man/man4/icmp.4`, `icmp6.4`, `bsd/netinet/ip_icmp.c`, `raw_ip.c`, `bsd/netinet6/icmp6.c`, `in.h`, `in6.h`, `ip_output.c` (github.com/apple-oss-distributions/xnu) | unprivileged datagram ICMP on macOS; identifier preserved; all ICMP delivered with IP header; errors delivered; permitted options; `IP_BOUND_IF` without privilege; raw sockets need root |
| https://man7.org/linux/man-pages/man2/clock_gettime.2.html · Apple Libc `gen/clock_gettime.3` · XNU `osfmk/mach/mach_time.h` | which clocks count suspended time on each system |
| https://docs.kernel.org/networking/timestamping.html · XNU `bsd/sys/socket.h` | kernel receive timestamp options |

### 1.6 Standards and interpretation

| Source | Supports |
|---|---|
| https://www.cloudflare.com/learning/network-layer/what-is-mtr/ | loss at intermediate hops that is absent at the final hop is not path loss; operators, Cloudflare included, limit ICMP reaching router control planes |
| R. Steenbergen, *A Practical Guide to (Correctly) Troubleshooting with Traceroute*, NANOG 80 deck (2020) | hop latency includes the router's ICMP generation; ICMP generation is rate-limited and low priority; real problems persist to later hops; asymmetric return paths; multiple paths; MPLS effects |
| https://www.rfc-editor.org/rfc/rfc1812.txt §4.3.2.8 · https://www.rfc-editor.org/rfc/rfc4443.txt §2.4(f) · https://www.rfc-editor.org/rfc/rfc6192.txt | ICMP rate limiting by routers |
| https://www.rfc-editor.org/info/rfc3393/ · https://www.rfc-editor.org/rfc/rfc3550.txt §6.4.1 · https://www.rfc-editor.org/rfc/rfc5481.txt | IPDV and RTP jitter are one-way quantities — hence our "RTT variation" naming |
| https://www.rfc-editor.org/rfc/rfc2681.txt · https://www.rfc-editor.org/rfc/rfc6673.txt | round-trip delay undefined beyond a stated threshold; round-trip loss cannot tell direction and ICMP may be treated specially |
| https://paris-traceroute.net/about/ | flow identifiers and per-flow load balancing |
| https://www.rfc-editor.org/rfc/rfc4786.txt · https://www.rfc-editor.org/rfc/rfc7094.txt | anycast instance selection can change |
| https://www.rfc-editor.org/rfc/rfc8484.html · `rfc7858.html` · `rfc7766.html` · `rfc1035.html` · `rfc6891.html` · `rfc8914.html` · `rfc9499.html` · https://www.dnsflagday.net/2020/ | DoH (media type, ID 0, HTTP/2), DoT (port 853, reuse), DNS over TCP framing, truncation, EDNS, 1232-byte recommendation, Extended DNS Errors 15–17, RCODE terminology |
| https://stat.ethz.ch/R-manual/R-devel/library/stats/html/quantile.html · https://numpy.org/doc/stable/reference/generated/numpy.percentile.html | Hyndman–Fan quantile types; type 1 = inverse empirical CDF (NumPy `inverted_cdf`) |

### 1.7 Terminal and CLI conventions

| Source | Supports |
|---|---|
| https://no-color.org/ | `NO_COLOR` applies when set and non-empty; colour only |
| https://github.com/termstandard/colors | `COLORTERM=truecolor`/`24bit` convention |
| https://github.com/contour-terminal/vt-extensions (synchronized output) | mode 2026 |
| https://pubs.opengroup.org/onlinepubs/9699919799/utilities/V3_chap02.html · https://www.gnu.org/software/bash/manual/html_node/Exit-Status.html · `signal.7` · `write.2` | exit status above 128 for signals; `EPIPE` behaviour |

---

## 2. What could not be confirmed

These items were looked for and could not be confirmed from a primary source.

| Item | Status |
|---|---|
| `https://1.1.1.1/dns-query` and `1dot1dot1dot1.cloudflare-dns.com` | not in Cloudflare's documentation when the design was written; not used as defaults |
| NextDNS DoQ port, exact per-profile addresses, `test.nextdns.io` response fields, behaviour of the anycast IPs without a profile | not officially documented or behind login |
| Whether Arelion's 2023 resolver list is current, and whether those resolvers answer non-customers | unknown |
| `*.twelve99.net` hostname convention | no Arelion source found |
| Default `ping_group_range` on Debian 12 and current Ubuntu LTS | not confirmed from a package manifest |
| Crossterm's exact output under `NO_COLOR` (possible reset of bold/reverse) | source reading only — to be confirmed by a test |
| Kernel receive timestamps on macOS ICMPv6 datagram sockets; routing semantics of `IP_BOUND_IF` | source-inferred only |
| NANOG 47 original of the traceroute tutorial | paywalled; the NANOG 80 edition was used instead |

---

## 3. Assumptions

Identifiers are stable so that other documents can cite them. A10 is not used.

| # | Assumption | If wrong |
|---|---|---|
| A1 | The NextDNS path to measure is DoH | if DoT or plain DNS is needed instead, the NextDNS encrypted-transport stream is retargeted; see [verification.md](verification.md) |
| A2 | User-space timestamps add tens to hundreds of microseconds, occasionally more | the loop-lag flag fires often; kernel timestamps move up the list |
| A3 | `. NS`, `example.com A`, and `wikipedia.org AAAA` are answerable from cache by all four providers and are not blocked by the NextDNS profile in use | change the query set; expectations make a block visible rather than silent |
| A4 | Truncated UDP answers are rare with the default queries | fallback statistics become prominent; adjust queries |
| A5 | Any of the three stacks has ample performance for this workload | none — performance was not a selection criterion |
| A6 | History for ~26 streams fits in roughly 17 MB; total resident memory under 64 MB | shorten compact history or re-baseline |
| A7 | A full recording is roughly 0.3–0.5 GB per day | lower the retention cap or bring compression forward |
| A8 | Terminals render braille and block characters at single-cell width | `--ascii`, or adjust glyph sets |
| A9 | Native IPv6 may not be available everywhere; v6 streams must degrade cleanly | no change — already designed for |
| A11 | Browser-downloaded unsigned macOS binaries are quarantined; Homebrew and `cargo install` avoid that | add signing to the release process |

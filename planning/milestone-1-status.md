# Milestone 1 — status report

**Date:** 2026-11-21 · started on your explicit go-ahead the same day.

**In one line:** the headless measurement foundation is built and tested on loopback and local fixtures on macOS and Linux; it has **not** yet been pointed at any provider, and two planned items are open.

Nothing in this report is a measurement of your network or of any provider. No probe has been sent to Cloudflare, Google, NextDNS, or Arelion. Nothing was committed (the workspace is still not a git repository), no system setting was changed, and nothing ran with elevated privileges.

## What exists

| Area | Delivered |
|---|---|
| Workspace | `crates/pathwatch-core` (no terminal dependencies) and `crates/pathwatch` (binary). The dashboard crate will be added with milestone 2 rather than as an empty shell now |
| Model and schema (1.1) | records, outcomes, error categories, JSONL schema v1 with a golden file |
| Configuration (1.2) | layered TOML (built-in ← file ← flags), unknown-key rejection, validation that reports every problem, provenance, secret masking, stream plan with stagger; `config check / show / path / example` |
| Clocks and gaps (1.3) | monotonic vs wall clock, suspend detection from the platform clock pair, collector-stall and wall-step detection |
| Capability detection (1.4) | `doctor` (human and JSON), with `--probe` limited to this host's loopback addresses |
| Scheduler (1.5) | slot grid with skip semantics and explicit accounting, stagger, token bucket |
| ICMP engine (1.6) | packet codec, pure matching state machine, `libc` socket layer for both systems, one actor per family |
| DNS over UDP + TCP fallback (1.7) | explicit queries, four-layer result, scenario expectations, classification |
| DoH (1.8) | own dial → TLS with separate server name → HTTP/2; reused and fresh cohorts; bootstrap resolution and destination epochs |
| Processor (1.9) | bounded history, pending tracking, gap timeline, window statistics with minimum-sample gating |
| Headless `sample` (1.10) | JSONL and summary output, duration/count limits, signals, broken-pipe handling, exit codes |
| Scripted source (1.11) | all 13 scenarios from the plan, seeded and deterministic |

About 8,500 lines of Rust including tests.

## Test evidence

84 tests, all passing, run on:

- **macOS 26.7, arm64** (this machine), as your user;
- **Linux, aarch64, kernel 7.0** in a Docker container — as root, as a non-root user, and with `net.ipv4.ping_group_range = "1 0"` to force the "unprivileged ICMP disabled" path.

Lint (`clippy` with warnings denied) and formatting are clean.

| Suite | Tests | Uses |
|---|---:|---|
| Unit tests | 57 | no I/O |
| Scripted collector | 9 | scripted ICMP transport, paused clock |
| Scenarios | 13 | scripted source |
| Real sockets | 5 | loopback ICMP, local DNS fixture (UDP/TCP), local DoH fixture (TLS + HTTP/2) |

## Acceptance criteria ([03 §2](03-implementation-plan.md#2-milestone-1--measurement-foundation))

| # | Criterion | Status |
|---|---|---|
| 1 | Same scenario → byte-identical JSONL and statistics | **Met.** Every scenario is generated twice and compared; the scripted collector run is compared byte for byte; the golden file passes on both systems |
| 2 | Counter identities hold for every stream and window | **Met.** Asserted for all 13 scenarios at four window sizes and in the collector tests |
| 3 | Deadline edges, duplicates, reordering, sequence wraparound | **Met** in the matching state machine's tests and the wraparound scenario |
| 4 | No RTT of 0 or the deadline for unanswered probes; field absent | **Met.** Asserted on records and on the serialised lines |
| 5 | Simulated suspend → one gap, no burst, unchanged rates | **Met** for a simulated 10-minute suspend. A real laptop sleep has not been tested |
| 6 | ICMP denied → `unsupported`, DNS runs, no rates | **Met**, simulated and for real in a Linux container with the sysctl off. With an ICMP-only configuration the process exits 4 ("no stream could be started") as specified |
| 7 | Error RCODEs, empty and blocked answers are never timeouts | **Met** against the local resolver fixture |
| 8 | Loopback ICMP and DNS run unprivileged on both systems | **Met** on macOS as your user and on Linux as a non-root user in a container. Not yet run on an Ubuntu or Debian host of yours |
| 9 | Encrypted transport: connect by IP with separate server name, certificate mismatch, separate phases, cohorts never mixed | **Met** against the local DoH fixture. The OS trust-store path was not exercised, because the fixture uses its own certificate |
| 10 | A silent target does not delay other streams | **Met**: the healthy stream's scheduling lag is exactly zero under the scripted clock |
| 11 | Invalid configuration rejected by key; profile id masked | **Met** |
| 12 | Measured traffic within ±10 % of the computed budget | **Met** for ICMP and DNS over UDP on 2026-11-21 (see "Traffic accounting" below). DoH is measured but has no computable budget |

## Open items

| Item | Why it is open | What it needs |
|---|---|---|
| **Task 1.12 — your real endpoints** | needs your consent to probe from your network, plus the Arelion router address and resolver city, and the NextDNS profile id in `PATHWATCH_NEXTDNS_PROFILE` | your go-ahead and those details |
| **Criterion 12 — traffic accounting** | byte and packet counters are not implemented | about half a day |
| Linux ICMP error queue | the code path compiles and runs on Linux, but no test makes a router send an unreachable, so it is unexercised end to end | a reproducible unreachable target (task 1.12, or a network namespace in CI) |
| DoH with the OS trust store | only fixture certificates were used | first run against a real DoH endpoint (task 1.12) |
| Real suspend/resume | only simulated | close the laptop lid during a `sample` run |
| Your Ubuntu and Debian hosts | only a generic Linux container was used | running `pathwatch doctor` there |
| File logging | no log file yet; diagnostics go to standard error | small; milestone 2 needs it before the screen is taken over |
| CI | the workflow file exists but has never run — there is no repository | `git init` and a remote, when you want them |
| `cargo deny` | policy file written but the tool was not installed or run | run in CI |

## Deviations from the plan

- **Two crates, not three**, as noted above.
- **Source and interface binding** is rejected by the validator ("not implemented in this build") instead of being checked by `doctor`. It is planned for milestone 3.
- **Not-yet-built options are rejected, not ignored**: `dns.doh.proxy = "env"`, `icmp.backend = "raw"`, controls, retries, and explicit bootstrap resolvers all fail validation with a message saying so.
- **The first request on each DoH connection** is tagged `after_reconnect` and excluded from reused-connection latency statistics, including the very first one. The plan said this for forced reconnects; applying it to every new connection is stricter and simpler.

## Observations worth knowing

- **Timing overhead on loopback (debug build, this Mac):** our echo round trips measured 0.2–0.5 ms while the system `ping` measured 0.06–0.14 ms over the same period. That 0.2–0.3 ms is the user-space scheduling cost the design anticipated; a release build and real network distances will make it proportionally small, but it is not zero.
- **Scheduling lag (debug build, this Mac):** sends left 0.5–2.2 ms after their slot. Within the proposed 5 ms budget; to be re-measured on a release build.
- **An unexplained blip:** in the first four runs on this Mac, IPv4 loopback echo replies after the first went missing (IPv6 was fine). A minimal program with no async code showed the same, so it is at or below the socket layer, not in the matching logic. It then stopped: the next 60-odd probes, including 21 run alongside the system `ping`, were all answered. Cause unknown; worth watching.
- **Other tools' replies are visible on macOS**, as expected: the ICMP socket there receives every process's ICMP. With your existing ping tools running, those packets are counted as "unmatched" and ignored.
- **A current-API correction:** Tokio has deprecated `AsyncFd::with_interest` as unsound; the code uses its replacement. This is the kind of thing the "no obsolete examples" rule was for.

## First run against real providers (2026-11-21, with your consent)

One 30-second `pathwatch sample` with the built-in configuration, from this Mac, debug build. Sixteen streams; about 170 probe transactions in total. **Thirty seconds and a handful of samples per stream are far too little to say anything about either provider** — this run was to see whether the instrument behaves on a real network. It does, and it found three things.

| Stream | Result |
|---|---|
| Cloudflare ICMP IPv4 (`1.1.1.1`) | 28 of 30 answered; median 9.9 ms, p95 24.0 ms; 2 unanswered |
| Google ICMP IPv4 (`8.8.8.8`) | 26 of 29 answered; median 10.0 ms, p95 19.3 ms; 3 unanswered |
| Cloudflare DNS/UDP IPv4 | 7 of 7 answered; median 14.3 ms (low sample count) |
| Google DNS/UDP IPv4 | 6 of 6 answered; median 51.3 ms (low sample count) |
| Google DoH IPv4 (`dns.google` → `8.8.4.4`) | 6 of 6 answered over one reused HTTP/2 connection; 39–44 ms per request; connection setup was 18.5 ms TCP + 37.8 ms TLS |
| Cloudflare DoH IPv4 | **0 of 6: certificate verification failed** — see finding 1 |
| Everything IPv6 | not sent: this network has no IPv6 route — see finding 2 |

### Finding 1 — Cloudflare's DoH hostname resolved to an address that is not Cloudflare's

The system resolver answered `cloudflare-dns.com` with `108.61.72.30`. That is not an address Cloudflare documents for its resolver. The server there presented a certificate that does not chain to a trusted authority (`UnknownIssuer`), so every connection was refused at the TLS step.

- The instrument did what the design requires: it never disabled verification, it recorded the address it actually dialled, and it reported a **certificate error — not loss, and not an outage**.
- A likely explanation, which I have not verified: the DNS service this Mac uses is filtering that name and answering with a block-page address. Filtering resolvers commonly do this for other providers' DoH hostnames, and NextDNS documents answering blocked names with a block-page server. `dns.google` was *not* affected — it resolved to Google's own addresses.
- Consequence for the product: with a filtering resolver in the path, the hostname-bootstrapped Cloudflare DoH stream measures the filter, not Cloudflare. The planned `static` bootstrap option (dial `1.1.1.1`, verify the certificate for `cloudflare-dns.com`) is the fix and should move up.

### Finding 2 — no IPv6 on this network, reported correctly

Every IPv6 stream reported a local error ("No route to host") or a bootstrap error ("no IPv6 address"), not loss. One quirk: the very first IPv6 echo was accepted by the kernel before it reported the missing route, so each IPv6 ICMP stream shows one probe "sent" and timed out, then 29 local errors. That single probe is counted as unanswered; with fewer than 20 finalised probes no percentage is shown.

### Finding 3 — 5 of 59 IPv4 echo requests unanswered, cause not established

Two to Cloudflare and three to Google timed out, none of them near a moment when the collector was busy. To check the instrument, a one-minute loopback comparison followed (five probes a second, nothing leaving the machine): our engine got 305 of 305 on IPv4 and 305 of 305 on IPv6 while the system `ping` got 300 of 300. So the engine does not drop replies under that test — but that does not prove the five were real network loss. A side-by-side run against the system `ping` on the same external targets would settle it, and needs your consent.

### Instrument health in that run

- **Scheduling lag** for ICMP and DNS: median 2.4–3.2 ms, worst 5.4 ms (debug build). The proposed budget is p99 ≤ 5 ms; a release build needs measuring.
- **First-slot lag on hostname targets:** the first DoH slot of each stream started 38–60 ms late because it waits for bootstrap resolution. Expected, and only the first slot.
- **Unmatched packets:** 945 in 30 s — other programs' ICMP traffic on this Mac (your existing ping tools), which macOS delivers to every ICMP socket. Ignored, as designed.
- **Fresh-connection cohort:** only Cloudflare's fired within 30 s. The eight fresh streams are staggered across their five-minute interval, 37.5 s apart, so a short run sees one.
- **Latency shape:** IPv4 echo times were bimodal, about 6–8 ms and 17–21 ms. One run cannot say why.

### Changed as a result

- The summary now prints the dominant reason and the dialled address under any stream with errors, for example `↳ 6× certificate: certificate verification failed: UnknownIssuer (destination 108.61.72.30)`. Verified locally against a closed loopback port.

### Suggested next steps from this run

1. Implement `static` bootstrap addresses for DoH targets (about half a day), so Cloudflare DoH can be measured regardless of the local resolver.
2. A 60-second side-by-side with the system `ping` on `1.1.1.1` and `8.8.8.8`, to classify finding 3.
3. A release-build run to re-measure scheduling lag and loopback overhead.

## Side-by-side with the system `ping` (2026-11-21, with your consent)

Sixty seconds, ICMP over IPv4 only, our sampler (debug build) and two system `ping` processes against the same two addresses at one probe per second each.

| | Ours | System `ping` |
|---|---|---|
| `1.1.1.1` answered | 61 of 62 | 59 of 60 |
| `8.8.8.8` answered | 62 of 62 | 59 of 60 |
| `1.1.1.1` median / p95 | 7.7 / 9.0 ms | 6.4 / 7.8 ms |
| `8.8.8.8` median / p95 | 6.3 / 18.0 ms | 5.1 / 6.9 ms |
| `1.1.1.1` round trips over 12 ms | 2 of 61 | 1 of 59 |
| `8.8.8.8` round trips over 12 ms | 9 of 62 | 2 of 59 |

**Finding 3 is settled as far as one minute can settle it: the unanswered probes are real events on the path, not the instrument dropping replies.** The one probe we lost to `1.1.1.1` was sent at 15:55:41.825 UTC; the one the system `ping` lost to `1.1.1.1` was sent within about 30 ms of that. Two independent tools lost the same instant. The system `ping` also lost one to `8.8.8.8` at 15:55:38.8 that fell between two of our probes (sent half a second either side, both answered), so these are brief events. The earlier run's 5 of 59 was a worse half-minute on the same path, not a different kind of result.

**A new finding: our round-trip times read about 1.2–1.3 ms higher than the system `ping`'s** (medians 7.7 vs 6.4 ms and 6.3 vs 5.1 ms). On loopback the gap was only 0.2–0.3 ms. The likely reason is that on a real network the collector thread has gone idle by the time a reply arrives and has to be woken, and the user-space receive timestamp is taken only after that wake-up — in a debug build. At single-digit-millisecond distances a 1.2 ms bias is about 20 %, which is too large to leave unexamined.

**And one thing not explained:** our `8.8.8.8` stream recorded nine round trips over 12 ms where the system `ping` recorded two. Either probes sent at our stream's phase really did take longer (possible on Wi-Fi, where timing relative to the access point matters), or our receive-side wake-up was sometimes several milliseconds late. The records cannot distinguish the two, because both timestamps are taken in user space.

### What this changes

The plan deferred kernel receive timestamps ([01 §4.1](01-design-spec.md#41-clocks-and-timestamp-points)) because of clock-domain concerns. This run is a concrete reason to bring them forward, at least as a **diagnostic**: on macOS the kernel can stamp each received packet on the same monotonic clock our timers use, so every observation could carry "time between the kernel receiving the reply and our code seeing it". That number would say directly how much of each round trip is our own delay, and would resolve the unexplained item above. Two smaller steps go with it: measure a release build, and consider a dedicated receive thread that blocks on the socket instead of waking through the async runtime.

## Kernel receive timestamps as a diagnostic (2026-11-21)

Each ICMP observation now carries `recv_delay_us`: the time between the kernel receiving the reply and this process reading it. It is measured, not assumed, and it is **not subtracted** from the round-trip time — `rtt_us` keeps its documented user-space definition. The summary prints the delay's median, p95, and maximum per stream, and what the round trip would be without it.

| | How the kernel stamps arrivals | Verified |
|---|---|---|
| macOS | `SO_TIMESTAMP_MONOTONIC`, on the same clock our timers use | works on IPv4 **and IPv6** datagram ICMP sockets (IPv6 was previously only inferred from source) |
| Linux | `SO_TIMESTAMPNS`, on the realtime clock; the delay is taken as a difference of two realtime readings microseconds apart, and discarded if implausible | works on ping sockets in the container |

All 84 tests pass on both systems; the loopback test now asserts that the delay is present and never exceeds the round trip.

**What it shows on loopback** (release build, this Mac, 155 probes per family alongside the system `ping`):

| | Round trip | Our receive delay (median) | Round trip without it | System `ping` |
|---|---|---|---|---|
| `127.0.0.1` | 0.4 ms | 0.22 ms | 0.1 ms | 0.14 ms average |
| `::1` | 0.2 ms | 0.03 ms | 0.2 ms | — |

So on loopback the diagnostic accounts for the whole gap between our figure and the system `ping`'s. Release-build scheduling lag on loopback: median 1.6 ms, p99 2.3 ms, worst 3.9 ms over 311 probes — inside the proposed 5 ms budget.

**Not yet measured:** the same figures on your real path, which is where the 1.2 ms bias and the unexplained slow `8.8.8.8` samples appeared. That needs one more external run.

Notes for later: the Mach timebase call is declared locally because the `libc` crate's bindings for it are deprecated; DNS and DoH observations do not have this diagnostic yet (their sockets go through the async runtime's own receive path).

## Release-build side-by-side with the receive-delay diagnostic (2026-11-21, with your consent)

Sixty seconds, ICMP over IPv4, release build, alongside two system `ping` processes.

| | Ours, as reported | Our receive delay | Ours without it | System `ping` |
|---|---|---|---|---|
| `1.1.1.1` median | 7.6 ms | 0.77 ms | 6.8 ms | 6.4 ms |
| `1.1.1.1` p95 | 8.7 ms | 1.00 ms | 7.8 ms | 8.0 ms |
| `8.8.8.8` median | 6.4 ms | 0.84 ms | 5.5 ms | 5.2 ms |
| `8.8.8.8` p95 | 9.3 ms | 1.10 ms | 8.2 ms | 6.5 ms |
| `1.1.1.1` answered | 61 of 62 | | | 60 of 60 |
| `8.8.8.8` answered | 61 of 62 | | | 60 of 60 |
| Round trips over 12 ms | 1 and 2 | | | 2 and 2 |

**The bias is explained.** Of the 1.1–1.2 ms by which our medians exceed the system `ping`'s, about 0.8 ms is our own receive delay: the reply sits in the kernel while the collector thread is woken. It is consistent (p95 1.0–1.1 ms, worst 1.6 ms) and a release build did not shrink it, so it is the wake-up path, not slow code. About 0.3 ms remains, which is plausibly the send side and the fact that `ping` itself uses a kernel receive stamp.

**The slow samples are the path, not us.** The three round trips over 12 ms in this run had receive delays of 0.6–1.1 ms, like every other probe. Our wake-up never took more than 1.6 ms, so it cannot produce a 17–25 ms reading. The system `ping` saw the same number of slow replies this time (two per target). The earlier minute's nine-versus-two on `8.8.8.8` did not recur and was most likely ordinary variation between two tools sampling at different instants.

**Unanswered probes, across both side-by-side minutes:** ours 3 of 247, system `ping` 2 of 240.

| Our unanswered probe | What the system `ping` saw nearby |
|---|---|
| `1.1.1.1`, first minute | lost its own probe sent within about 30 ms |
| `1.1.1.1`, second minute | its probe sent 150 ms later was answered, but took 19.3 ms against a 6.4 ms median |
| `8.8.8.8`, second minute | nothing unusual half a second either side |

Two of our three coincide with a disturbance the other tool also saw; the third is uncorroborated, and one `ping` loss in the first minute fell between two of our probes. That is what brief real events on the path look like when two tools sample once a second at different phases. It is not proof for each individual probe.

**Instrument health:** scheduling lag median 1.7 ms, worst 3.3 ms; 2,413 ICMP packets belonging to other programs arrived on our socket in 62 s and were ignored.

### Decision this puts in front of you

`rtt_us` currently includes the ~0.8 ms the reply waits for our process. The design chose a user-space definition because kernel stamps looked like a clock-domain risk. The diagnostic removes that risk: the delay is measured per probe on both systems, so a kernel-stamped round trip is simply `rtt_us − recv_delay_us`. Options:

1. **Make the kernel-stamped round trip the headline** (recommended): cards and statistics use it, `ts_source` says `kernel-rx`, and the user-space figure stays in every record. Numbers then agree with `ping` to within about 0.3 ms.
2. Keep the user-space figure as the headline and show the delay beside it, as now.

Either way DNS and DoH need the same diagnostic before their numbers can be treated alike; today only ICMP has it.

## ICMP round trips now end at the kernel receive stamp (2026-11-21, your decision)

- `rtt_us` for ICMP ends when the kernel receives the reply. Each such record says `ts_source: "kernel-rx"`, keeps the user-space figure as `rtt_user_us`, and keeps the wait as `recv_delay_us`. Statistics and the summary use `rtt_us`.
- The deadline is judged on the same moment: a reply the kernel received before the deadline is on time even if our process read it just after.
- If the kernel supplies no stamp, or one that would precede the send, the record falls back to the user-space figure and says `userspace-monotonic`.
- **DNS and DoH are unchanged** and still end at our read. Until they get the same treatment, an ICMP time and a DNS time differ in definition by roughly the receive delay — another reason they are never drawn on one line.
- The schema version stays at 1: `ts_source` was put in every record so the clock could change without ambiguity, and the new fields are additions.

Verified: all 84 tests pass on macOS and in the Linux container; the loopback test asserts the clock source and that user-space = kernel-stamped + delay. On loopback (release build, 155 probes alongside the system `ping`) the headline median is now 0.1 ms for `127.0.0.1` against `ping`'s 0.19 ms average, where it read 0.4 ms before.

Not re-measured on your real path since the change. From the last side-by-side the expected medians are about 6.8 ms and 5.5 ms against `ping`'s 6.4 ms and 5.2 ms.

## Milestone 1 — where it stands now

| Still open | Needs |
|---|---|
| ~~Traffic accounting (acceptance criterion 12)~~ | **done** — see below |
| ~~Static bootstrap addresses for DoH~~ | **done** — see below; not yet tried against Cloudflare |
| Kernel receive stamps for DNS and DoH | to scope; their sockets go through the async runtime's receive path |
| Your NextDNS and Arelion endpoints (task 1.12) | the Arelion router address and resolver city; the profile id in `PATHWATCH_NEXTDNS_PROFILE` |
| Ubuntu and Debian hosts | running `pathwatch doctor` there |
| File logging; CI and `cargo deny` never run | small; a repository when you want one |

## Static bootstrap addresses (2026-11-21)

A `url` or `hostname` target can now list `addresses = [...]`. For each address family the first listed address is dialled; the hostname is never resolved and stays the TLS server name the certificate is verified against. A family with no listed address gets no stream. The recorded target says where it is dialled (`https://cloudflare-dns.com/dns-query via 1.1.1.1`), and a pinned stream stays in destination epoch 1.

- Validation rejects a non-IP entry, static addresses on a target that already has a literal `address`, and `follow = true` together with static addresses.
- Three new tests, 87 in total, passing on macOS and in the Linux container. One runs the DoH fixture behind a resolver that would send the stream to a useless address — as a filtering resolver does — and asserts that the resolver is never consulted, the request succeeds, and no resolution record is written.
- The built-in configuration is **unchanged**: Cloudflare and Google DoH still bootstrap through the system resolver. The example file shows the `addresses` line commented out, because Cloudflare documents `1.1.1.1` as a resolver address, not specifically as a DoH endpoint for `cloudflare-dns.com`. Whether it answers DoH under that name is unverified until it is tried.

### Tried against Cloudflare (2026-11-21, with your consent)

Twenty seconds, release build, Cloudflare DoH only, pinned to `1.1.1.1` and `2606:4700:4700::1111`.

- **`1.1.1.1` answers DoH for `cloudflare-dns.com`, and its certificate verifies against the operating system's trust store** — HTTP/2 over TLS 1.3. This is one observation from this network, not a documented guarantee.
- Five of five requests on the reused connection answered `NOERROR` with HTTP 200, taking 6.9–12.0 ms each; four count toward the reused-connection statistics, the first being excluded as designed.
- The one fresh-connection probe took 57.3 ms in total: 33.7 ms TCP connect, 15.7 ms TLS handshake, 7.9 ms for the query. The long-lived connection opened at the same moment measured 14.2 ms and 33.8 ms for the same two phases, so the two connections starting together at launch disturbed each other's setup timing. One sample each; start-up figures should not be read closely.
- IPv6 was not sent (no route here), reported as a local error with the reason.
- With this, the trust-store path of the DoH client has been exercised against both Google (first run) and Cloudflare.

To use it permanently, add this to your configuration file:

```toml
[[providers.cloudflare.targets]]
id        = "resolver-v4"
address   = "1.1.1.1"
protocols = ["icmp", "dns-udp"]

[[providers.cloudflare.targets]]
id        = "resolver-v6"
address   = "2606:4700:4700::1111"
protocols = ["icmp", "dns-udp"]

[[providers.cloudflare.targets]]
id        = "doh"
url       = "https://cloudflare-dns.com/dns-query"
addresses = ["1.1.1.1", "2606:4700:4700::1111"]
protocols = ["doh"]
```

(The target list replaces the built-in one for that provider as a whole, which is why the two resolver targets are repeated.)

## Traffic accounting (2026-11-21) — acceptance criterion 12

The collector now counts what it sends and receives, by probe kind and address family, and the session's closing record carries the totals. ICMP and UDP are counted as IP-layer bytes (payload plus transport and IP headers), which is exact. DoH and the DNS/TCP fallback are counted as TCP payload bytes — everything written to and read from the socket, TLS included — because segments and acknowledgements are not visible from user space.

The expected budget is computed from the configuration: ICMP from the payload size, DNS over UDP from the actual encoded size of each configured query. The summary prints measured beside expected.

**Sixty seconds, loopback only, default intervals, release build:**

| Kind | Family | Probes | Bytes sent | Measured | Budget | Difference |
|---|---|---:|---:|---:|---:|---:|
| ICMP | IPv4 | 60 | 5,040 | 84.0 B/s | 84.0 B/s | 0.0 % |
| ICMP | IPv6 | 60 | 6,240 | 104.0 B/s | 104.0 B/s | 0.0 % |
| DNS/UDP | IPv4 | 13 | 832 | 13.9 B/s | 12.9 B/s | +7.2 % |
| DNS/UDP | IPv6 | 12 | 1,016 | 16.9 B/s | 16.9 B/s | 0.0 % |

All within ±10 %. The one visible difference is an edge effect: a 60-second run of a 5-second stream that starts at zero contains 13 slots, not 12.

- Tests: 89 in total, passing on macOS and in the Linux container. The integration tests assert that ICMP bytes equal packets × 84 (IPv4) or × 104 (IPv6), out and back; that DNS/UDP bytes equal the sum of the exact query sizes; that the TCP fallback and DoH are accounted separately as TCP payload.
- Corrections to [01 §13](01-design-spec.md#13-defaults-and-traffic-budget): the default DNS queries are 28, 40, and 42 bytes, so a DNS/UDP query is 56–70 bytes on IPv4 and 76–90 on IPv6 at the IP layer — slightly below the "≈ 70–95" written there.
- **Still an estimate:** the DoH figures in that section (per-request and per-handshake bytes) have no computed budget and have not been measured against a real server since the counters were added. The next real run will print them.

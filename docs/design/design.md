# pathwatch — design

This is the design the implementation follows: what `pathwatch` measures, how every number on screen is defined, and how the screens, keys, commands and configuration behave. Where the implementation differs from the design or stops short of it, the section says so in a note marked **As built** or **Not built**. [`ROADMAP.md`](../../ROADMAP.md) tracks what is not built and what has not been verified.

Claim labels: **[V]** verified against a primary source · **[R]** engineering recommendation · **[A]** assumption · **[T]** to be validated. Sources and assumptions are listed in [references.md](references.md). How the program is built is in [architecture.md](architecture.md); how it is checked is in [verification.md](verification.md).

---

## 1. Product goals and scope

**Goal.** One terminal instrument that does the one job otherwise spread across `trippy`, `gping`, `ping` and `prettyping`: continuously watching four named providers (Cloudflare, Google, NextDNS, Twelve99/Arelion) and showing, truthfully and at a glance, which one is misbehaving, on which protocol and address family, and since when.

**Equal priorities.** (1) Measurements whose definitions can be read and whose denominators can be checked. (2) A dashboard that is pleasant to leave open for days.

**In scope**

- ICMP echo round-trip time and reply/no-reply accounting per target and address family.
- Explicit DNS transactions to a chosen resolver address over UDP, TCP, DoT, and DoH.
- On-demand route inspection with a packet budget.
- Optional secondary controls: local default gateway and the system's configured DNS service.
- Bounded in-memory history, opt-in JSONL recording, export, deterministic replay.
- macOS and Linux, IPv4 and IPv6, unprivileged by default.

**Out of scope** (deferred unless a real need appears): web dashboard, cloud account, external notifications, automatic remediation, speed/throughput tests, distributed collectors, a general plugin system, Windows.

**What the instrument never claims.** Bandwidth, throughput capacity, one-way delay, complete Internet availability, or a definitive ISP fault. There is no composite "health score".

---

## 2. Measurement identity

A provider label is not an endpoint. Six things are modelled separately and every observation carries all of them.

| Concept | Definition | Example (illustrative) |
|---|---|---|
| **Provider** | A display slot (1–4) with a name and an identity colour. Owns zero or more targets. | `cloudflare` |
| **Target** | What is configured: a literal IP, or a hostname plus a role. | `1.1.1.1`, or `dns.nextdns.io` as a DoH endpoint |
| **Destination** | The IP address actually dialled for a probe. Equal to the target for literal IPs; the result of a recorded resolution for hostnames. | `1.1.1.1` |
| **Stream** | The unit of scheduling and statistics: one (target, protocol, address family, scenario) tuple. Statistics are never computed across streams. | Cloudflare · ICMP · IPv4 |
| **Source context** | Egress interface name and index, local source address, default gateway, and whether the interface looks like a tunnel. Captured at start and on every network change. | `en0`, `192.0.2.10`, gw `192.0.2.1` |
| **Connection state** | For TCP-based transports: connection id, fresh or reused, age, negotiated ALPN, TLS version, resumed or full handshake. | DoH conn #1, reused, `h2`, 05:00 old |

**Destination epochs.** When a hostname target resolves to a different address and the selection policy (§3.4) switches to it, the stream starts a new *destination epoch*. An `address_changed` event is recorded, the chart draws a vertical marker, and **statistics cover the current epoch only** — two different destinations are never pooled into one percentile. Earlier epochs stay visible in the chart and raw view.

> **Not built:** the chart draws no epoch marker. The change is recorded as an `address_changed` event and listed with `◆` among the events.

**Family separation.** IPv4 and IPv6 are different streams with separate card states. The overview shows one family at a time (`f` toggles); a small per-card badge shows the *other* family's status so a v6 failure stays visible while v4 is on screen.

---

## 3. Providers, targets, and capabilities

Everything in this section comes from the providers' own published material, as read when the design was written ([references.md §1.4](references.md#14-providers)). None of it is a measured result. "Not documented" means the provider's documentation does not mention it — not that it does not work.

### 3.1 Documented endpoints

| Provider | Endpoint | Role | Source status |
|---|---|---|---|
| Cloudflare | `1.1.1.1`, `1.0.0.1`, `2606:4700:4700::1111`, `2606:4700:4700::1001` | public resolver, UDP/TCP 53 and DoT 853 | **[V]** |
| Cloudflare | `https://cloudflare-dns.com/dns-query` | DoH (wire format GET/POST; HTTP/2 minimum recommended) | **[V]** |
| Cloudflare | `one.one.one.one` | DoT server name | **[V]** |
| Cloudflare | `https://1.1.1.1/dns-query` | DoH by IP literal | **not in current docs** — not used as a default |
| Google | `8.8.8.8`, `8.8.4.4`, `2001:4860:4860::8888`, `2001:4860:4860::8844` | public resolver, UDP/TCP 53 | **[V]** |
| Google | `https://dns.google/dns-query` | DoH (RFC 8484 GET/POST; request ≤ 512 bytes) | **[V]** |
| Google | `dns.google` : 853 | DoT | **[V]** |
| NextDNS | `https://dns.nextdns.io/<profile-id>` | DoH for one profile | **[V]** shape; the profile id is the user's own |
| NextDNS | `<profile-id>.dns.nextdns.io` : 853 | DoT for one profile (the profile id travels in the TLS server name) | **[V]** shape |
| NextDNS | `45.90.28.0`, `45.90.30.0`, `2a07:a8c0::`, `2a07:a8c1::` | fixed anycast addresses (primary / secondary) | **[V]** in NextDNS's published client source, not in its help articles |
| NextDNS | per-profile IPv4 / IPv6 addresses, linked IP | plain DNS attributed to one profile | existence **[V]**; the actual addresses are shown only on that profile's Setup page |
| Arelion | `213.248.100.54` / `2001:2030:c000:4::4` (Stockholm), `213.248.76.210` / `2001:2030:c000:2::4` (Frankfurt), `209.95.129.122` / `2607:41c0:c000:2::4` (New York), `209.170.66.202` / `2607:41c0:c000:4::4` (Los Angeles), `202.163.0.2` / `2407:1600:c000::4` (Singapore) | listed by Arelion as its "public DNS resolvers" | **[V]** in a customer document dated 2023-02-23; no transport, port, access policy, or currency statement |
| Arelion | `*.ip.twelve99.net` router interface names | backbone router interfaces | **no Arelion documentation**; not a resolver and not a published measurement target |

### 3.2 Capability matrix

`doc` = documented by the provider. `undoc` = not documented; the application attempts it only on a target the user enabled and labels results *undocumented capability — observed behaviour only*. `n/a` = does not apply. Privilege row: what the **host** needs, detailed in [architecture.md §5](architecture.md#5-platform-capability-matrix) and in [capabilities.md](../capabilities.md).

| Provider · target | ICMP echo | DNS/UDP | DNS/TCP | DoT | DoH | Route trace | Notes |
|---|---|---|---|---|---|---|---|
| Cloudflare · resolver IPs | undoc | doc | doc | doc (`one.one.one.one`) | doc via `cloudflare-dns.com` | ICMP trace: undoc | Cloudflare's docs suggest `traceroute 1.1.1.1` for troubleshooting but make no statement about echo replies or rate limits |
| Google · resolver IPs | doc, with caveats | doc | doc | doc (`dns.google`) | doc (`dns.google`) | doc (traceroute to `dns.google` recommended) | Google: ICMP is not blocked, but ICMP error replies are rate-limited and ICMP may be de-prioritised; ping shows network latency, not resolution speed |
| NextDNS · profile hostname | undoc | n/a | n/a | doc | doc | undoc | Hostname steering ("Ultralow") applies only here; resolves to a nearby unicast site |
| NextDNS · fixed anycast IPs | undoc | doc only with a linked IP (IPv4) or per-profile address; no-profile behaviour undoc | undoc | unknown | bootstrap use only | undoc | A different network path from the hostname-steered one — never merged with it |
| Arelion · listed resolver IPs | undoc | **unknown** | **unknown** | not documented — treated as `UNSUPPORTED` | not documented — treated as `UNSUPPORTED` | undoc | Whether non-customers are served is not stated. A `REFUSED` answer would be a `service_error`, not an outage |
| Arelion · router hostname | undoc | `UNSUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` | undoc | Routers commonly rate-limit and de-prioritise ICMP addressed to themselves; expect a noisier no-reply rate than a server |
| **Host privilege** | none on macOS; Linux needs `ping_group_range` to include the user's group | none | none | none | none | as ICMP echo | |

DoQ and DoH3 are out of scope and deferred (NextDNS documents them; Cloudflare and Google do not document DoQ).

### 3.3 Default target policy

| Card | Default | State at first run |
|---|---|---|
| 1 Cloudflare | ICMP and DNS/UDP to `1.1.1.1` and `2606:4700:4700::1111`; DoH to `cloudflare-dns.com`, dialled at those same two addresses without resolving the hostname (static addresses, §3.4) | collecting |
| 2 Google | ICMP and DNS/UDP to `8.8.8.8` and `2001:4860:4860::8888`; DoH to `dns.google` | collecting |
| 3 NextDNS | DoH to the profile hostname once a profile id is supplied; nothing before that | `NEEDS CONFIG` |
| 4 Arelion | two streams: a backbone router (ICMP) and a listed resolver (ICMP, DNS/UDP); both addresses are placeholders | `NEEDS CONFIG` until they are supplied |

- All four cards are always present. An unconfigured card says `NEEDS CONFIG`; a protocol the target does not offer says `UNSUPPORTED`. Neither ever shows a loss percentage, and the application never substitutes an address of its own choosing.
- **NextDNS.** The default is DoH to the profile hostname. The provider ships disabled, and the profile id is supplied at run time through an environment variable (`PATHWATCH_NEXTDNS_PROFILE`); until the provider is enabled with an id the card reads `NEEDS CONFIG`. Plain-DNS addresses from the profile's Setup page are optional. Two streams are kept distinct when both are configured: *profile endpoint (hostname-steered)* and *fixed anycast address*. The fixed anycast IPs are an ICMP target only by the user's explicit choice.
- **Arelion.** Both kinds of target are supported, as separate streams that are never merged: a specific backbone router the user sees in their own traces (ICMP only; a hop on the path, not a service), and one of the five listed resolver addresses (ICMP, and DNS only if it turns out to answer). The application does not pick the addresses. Whether either kind answers is untested ([ROADMAP.md](../../ROADMAP.md)).
- **Several targets under one provider.** When a provider has more than one target for the same protocol and family, the first enabled one in configuration order is the card's *primary* stream for that mode. For Arelion that is the router in ICMP mode and the resolver in DNS mode. The others appear as a secondary line on the large layout's card and as rows in the detail view's Summary tab, each with its own statistics.
- Secondary resolver addresses (`1.0.0.1`, `8.8.4.4`, …) are available as additional targets but are off by default to keep the traffic budget small.

### 3.4 Hostname resolution and bootstrap

| Decision | Policy |
|---|---|
| Literal IP targets | used as given; no resolution, one destination epoch for the session |
| Hostname targets | resolved **off the timing path** before the stream starts and again on refresh; every resolution is recorded: resolver used, duration, all addresses returned, per family |
| Bootstrap resolver | `system` (default — what the host's own clients get, including hostname steering), or an explicit list of resolver IPs, or **static addresses on the target** (`addresses = [...]`): the listed address for each family is dialled, the hostname is never resolved, and it remains the TLS server name the certificate is verified against. A statically pinned stream says so in its recorded target (`… via 1.1.1.1`). Static addresses are for hosts whose local resolver filters or rewrites the hostname |
| Selecting among several addresses | per family, take the first address in resolver order and **stay on it** while it remains in the answer set |
| Refresh | at TTL expiry when the bootstrap resolver exposes TTLs (minimum 30 s), otherwise every 5 minutes |
| When the selected address leaves the answer set | `follow = true` (default for steered hostnames such as NextDNS): switch, record `address_changed`, start a new destination epoch, reconnect. `follow = false`: keep the pinned address and raise a notice that the hostname no longer resolves to it |
| Connection reuse across refresh | an established DoT/DoH connection is kept while its address is still valid; it is not torn down merely because a refresh happened |
| Pooling | never. One stream, one destination at a time; alternatives are visible in the Settings view but not probed unless configured as their own targets |

> **Not built:** an explicit bootstrap resolver list (`bootstrap.servers` has no effect) — a hostname gets its address from the system resolver or from static `addresses` on the target. The system resolver exposes no TTLs, so refresh is always on a fixed interval (`bootstrap.refresh`, 5 minutes by default). There is no Settings view (§9.6); the alternatives are in the recorded resolution.

### 3.5 Secondary controls

Two optional, visually secondary rows (grey, below or beside the four cards, never in a provider slot):

- **Gateway** — ICMP echo to the default gateway of the egress interface, discovered from the routing table at start and on every network change. No router administration access, interface name, or address is assumed; if discovery fails the row reads `UNSUPPORTED (no default gateway found)`. Some gateways do not answer echo; that is reported as no-reply on a control, with a note that controls can legitimately be silent.
- **Local DNS** — explicit DNS/UDP transactions to the resolver address(es) the system is configured to use (read from system configuration, never by timing `getaddrinfo`). On hosts where that address is a loopback stub (`127.0.0.53`, a local forwarder), the row says so, because it then measures the stub plus whatever it forwards to.

Controls exist to answer one question during an incident: *is the local segment still fine?* They are off by default.

> **As built:** the IPv6 gateway control is declined, with the reason, when the gateway is a link-local address (probing it needs an interface scope). The local-DNS control reads `/etc/resolv.conf` and uses one resolver per family, the first listed.

---

## 4. Measurement semantics

These rules are fixed before any chart is drawn. Every displayed number traces back to §5.

### 4.1 Clocks and timestamp points

- **Durations and deadlines** use a monotonic clock. **Wall-clock UTC** is stored separately per observation for logs and correlation, and is never used to compute a duration. A wall-clock step (NTP, manual change) therefore cannot corrupt an RTT; it is recorded as a `clock_step` event when the wall/monotonic offset moves by more than 500 ms.
- **`t_send`**: monotonic timestamp read in user space immediately *before* the send system call for the first byte of the probe.
- **`t_recv`**, ICMP: the moment the **kernel** received the reply. The kernel stamps each arrival, but in a different clock domain from the send timestamp, so the stamp is not used as `t_recv` directly. Instead the process reads its own clock immediately after the receive call returns and takes off the measured wait between the kernel's stamp and that read (`recv_delay`). Records made this way say `ts_source = "kernel-rx"`. Where no kernel stamp is available, `t_recv` is the process's read time and the record says `userspace-monotonic`.
- **`t_recv`**, DNS and DoH: monotonic timestamp read in user space immediately *after* the final byte of the response is read (`userspace-monotonic`).
- **RTT = `t_recv − t_send`.** For ICMP this includes the kernel send path and the network round trip, and ends when the kernel takes delivery of the reply; the wait for the process to be scheduled is **excluded** and recorded separately as `recv_delay_us`, with the user-space figure kept as `rtt_user_us`. It is still *not* wire time. **[R]** Display resolution is 0.1 ms; stored resolution is 1 µs.
- **Scheduling uncertainty.** User-space timestamping biases RTT upward, typically by tens to hundreds of microseconds and occasionally by milliseconds when the process is descheduled **[A]**. Two guards make this visible instead of pretending to precision:
  - each observation records `sched_lag` = actual send time − scheduled slot time;
  - a loop-lag monitor timestamps a 50 ms heartbeat on the receive runtime; any observation received while heartbeat lateness exceeds 5 ms is flagged `timing_suspect` and drawn with a hollow marker.
- `ts_source` records which clock ended a round-trip time, so a backend can change that without ambiguity.

> **As built:** kernel receive timestamps are used for ICMP only. DNS, DoT and DoH transactions end at the process's read and are recorded as `userspace-monotonic`. The `timing_suspect` flag is set on ICMP observations and named in the Raw tab's notes; no hollow marker is drawn.

### 4.2 Probe identity and reply matching

- Every probe has a **probe id** = (`session_id`, `stream_id`, `seq`), where `seq` is a 64-bit counter per stream that never wraps in practice.
- **ICMP on the wire:** 16-bit identifier, 16-bit sequence (= `seq mod 65536`), and a 56-byte payload — the same ICMP size as the default `ping`, so the numbers are comparable with `ping`'s. The payload starts with a 4-byte magic, a 4-byte random session nonce, and the full 64-bit `seq`.
- **Matching** requires: source address equals the probed destination, ICMP type is Echo Reply, and the payload magic, nonce, and `seq` match a pending probe. The identifier is checked where the OS preserves it and ignored where the kernel rewrites it (Linux datagram ICMP sockets — see [architecture.md §5](architecture.md#5-platform-capability-matrix)). Matching on payload, not on the 16-bit sequence, makes **sequence wraparound** (every 18.2 hours at 1 Hz) harmless.
- **Duplicates:** a second matching reply for a probe that already succeeded increments `duplicates`; the first reply's RTT stands.
- **Out-of-order completion:** replies are matched by probe id, never by arrival order. Statistics order samples by `seq` (send order).
- **Late replies:** a reply that arrives after the probe's deadline does not change its outcome (the deadline is part of the metric's definition, as in RFC 2681's loss threshold). The probe stays `timeout`; the observation gains `late_rtt` and `late_replies` increments. Finalised timeouts are remembered for 30 s for this purpose, then forgotten; anything later is counted as `unmatched`.
- **Foreign packets** (other processes' echo replies on macOS, malformed packets) are counted as `unmatched` and otherwise ignored.
- **DNS:** matching requires source address and port equal to the destination, transaction ID equal (random 16-bit per query from a CSPRNG; 0 for DoH), QR=1, and the question section echoing the one sent. A non-matching datagram on the query socket is counted as `unmatched` and the wait continues until the deadline.

### 4.3 Probe outcomes

Each scheduled slot ends in exactly one outcome. Only the first five are *network* outcomes.

| Outcome | Meaning | Sent on the wire? | In network denominators? |
|---|---|---|---|
| `success` | Valid matching reply before the deadline (and, for DNS, the expected semantic result — §6.3) | yes | yes |
| `timeout` | Sent; no matching reply by the deadline | yes | yes |
| `unreachable` | Sent; an ICMP error (destination/port/host unreachable, admin-prohibited, time exceeded) attributable to this probe arrived before the deadline | yes | yes |
| `service_error` | Sent; a valid matching reply arrived whose content is an error or violates the scenario's expectation (DNS `SERVFAIL`, `REFUSED`, unexpected `NXDOMAIN`, TLS alert, HTTP 4xx/5xx) | yes | yes (separate rate) |
| `protocol_error` | Sent; a reply arrived that cannot be parsed or fails validation (truncated message, wrong question, certificate verification failure) | yes | yes (separate rate) |
| `local_error` | The local stack refused or failed the send: no route, no address in this family, permission denied, interface down, `ENOBUFS` | **no** | **no** |
| `skipped` | The slot was not attempted: previous transaction on the stream still in flight, global rate limit, collector backpressure, or the timer fired too late | no | no |
| `cancelled` | In flight when collection stopped, a gap began, or the stream was reconfigured | maybe | no |

Transient states: `scheduled` (slot exists, nothing sent) and `pending` (sent, awaiting reply or deadline). Neither is ever counted as loss.

Stream-level states (shown as the card status, never as a per-probe outcome): `needs_config`, `disabled`, `unsupported`, `warming_up`, `ok`, `lossy`, `no_reply`, `error` (service errors dominate), `local_error`, `stale` (no finalised observation for max(3 × interval, 5 s) while collection is supposedly running), `paused`.

> **As built:** there is no `paused` stream state (§9.4), and the `stale` period is max(3 × interval, 5 s) plus the stream's deadline.

### 4.4 Windows, cohorts, and denominators

- **Window membership is by send time.** An observation belongs to window `[now − w, now)` if and only if its `t_send` (or, for unsent slots, its scheduled time) falls inside. Reply arrival time is irrelevant to membership.
- **Statistics use finalised observations only.** Pending probes are in the window but excluded from every rate and percentile; they are shown as their own count.
- **Internal consistency rule:** for any stream and window, `sent = pending + replied + timed_out + unreachable + service_error + protocol_error + cancelled`, and `slots = sent + local_error + skipped`. The detail view shows every term; a test asserts the identity.

**ICMP no-reply rate — the precise denominator.**

```
finalised = replied + timed_out + unreachable        (cancelled excluded)
no_reply_rate = (timed_out + unreachable) / finalised
```

It is labelled **"no-reply"**, not "packet loss", because it cannot distinguish a lost echo request, a lost reply, a rate-limited reply, or a reply that arrived after the deadline. Help text says so. Local errors, skipped slots, pending probes, cancelled probes, and observation gaps are outside both numerator and denominator.

- A missing RTT is **never** replaced by 0 ms or by the timeout value. Timeouts have no RTT; they appear in the outcome timeline and the no-reply rate.
- **First attempt vs retries.** The default retry count is 0 for every protocol, so "first attempt" and "transaction" coincide. With retries enabled, every attempt is its own observation and the attempts of one transaction share a transaction id; the headline rates and latencies stay first-attempt, and a separate "transaction succeeded after retry" count is shown. There is no hidden failover to another address, transport, or resolver.

> **Not built:** retries. `icmp.retries` and `dns.retries` accept only `0`; a probe is sent once and its result stands.

### 4.5 Observation gaps

A gap is an interval in which the instrument was not observing. It is not an outage.

| Cause | Detection | Effect |
|---|---|---|
| Host suspend/resume | The monotonic clock used for timers may not advance during sleep, so it cannot detect sleep by itself. Each tick, compare a suspend-excluding clock with a suspend-including one — Linux `CLOCK_MONOTONIC` vs `CLOCK_BOOTTIME`, macOS `CLOCK_UPTIME_RAW` vs `CLOCK_MONOTONIC_RAW` **[V]** — with wall-clock as a cross-check; a divergence > 2 s is a suspend. Thresholds **[T]** | `gap(suspend)` from last pre-sleep tick to first post-resume tick |
| Collection stopped by the user (`P`) | explicit | `gap(stopped)` |
| Process stopped (`Ctrl-Z` / `SIGSTOP`) | same clock comparison on `SIGCONT` | `gap(process_stopped)` |
| Interface / route change | source-context change notification or failed-send pattern | pending probes cancelled; a short `gap(network_change)`; new source context recorded |
| Collector stall | a tick fires more than max(3 × interval, 2 s) late without a suspend signature | `gap(collector_stall)` |

During a gap no slots are created, so no probes are "missed". Probes pending when a gap begins become `cancelled`. After a gap the scheduler resumes on its original phase grid with **no catch-up burst**. Gaps reduce *coverage* (§5), break RTT-variation adjacency, and are drawn as their own glyph.

---

## 5. Metric dictionary

All windowed metrics use the window selected in the UI (1 m, **5 m default**, 15 m, 1 h, 6 h, 24 h), send-time membership, finalised observations, and the current destination epoch. "Min n" is the sample count below which the value is replaced by `—` plus the count; "low n" means shown dimmed with a `·` marker. The same definitions, in the words the program itself prints, are in [glossary.md](../glossary.md).

| # | Metric (UI label) | Formula | Unit | Timing boundaries | Eligible observations | Min n | Interpretation limits |
|---|---|---|---|---|---|---|---|
| 1 | Echo RTT | `t_recv − t_send` | ms | §4.1: user-space send stamp, kernel receive stamp | ICMP `success` | 1 | Send-side scheduling is still included; a round trip over two possibly different paths; not one-way delay |
| 2 | Last RTT · age (`last`) | RTT of the newest `success`; age = now − its `t_recv` | ms · s | — | newest `success` in the session | 1 | Age shown always; value dims when age > 3 × interval |
| 3 | Min / Max | extreme of eligible RTTs | ms | — | `success` | 1 | Max is the most scheduling-sensitive number on screen |
| 4 | Mean | arithmetic mean | ms | — | `success` | 5 (low n < 20) | Pulled by outliers; read beside p50 |
| 5 | Median (`p50`) | nearest-rank percentile, §5.1 | ms | — | `success` | 5 (low n < 20) | — |
| 6 | `p95` | nearest-rank | ms | — | `success` | 20 | At n = 20 it is the 19th of 20 values |
| 7 | `p99` (detail) | nearest-rank | ms | — | `success` | 100 | — |
| 8 | Std dev `sd` (detail) | sample standard deviation (n − 1) | ms | — | `success` | 20 | Assumes nothing about distribution shape; latency is usually skewed |
| 9 | RTT variation (`Δ`) | mean of \|RTTᵢ₊₁ − RTTᵢ\| over adjacent pairs, §5.2 | ms | — | pairs of consecutive-`seq` `success` | 10 pairs | **Not** RFC 3393 IPDV and **not** RFC 3550 jitter; both are one-way |
| 10 | Samples (`n`) | count of eligible RTTs | — | — | `success` | — | The n behind every latency statistic on that card |
| 11 | Sent, pending, replied, timed out, unreachable, late, duplicate, local error, skipped, cancelled | counts, §4.3–4.4 | — | — | all slots in window | — | Shown individually in the detail view |
| 12 | No-reply rate (`no-reply`) | (timed_out + unreachable) / (replied + timed_out + unreachable) | % | deadline 2000 ms default | finalised ICMP probes | 20 (below: raw `k / n`) | Not proof of path loss — may be reply loss, rate limiting, or a late reply |
| 13 | Coverage | sent / (window ÷ interval) | % | — | slots in window | — | 100 % only when every slot in a full window was transmitted; a young session or a gap lowers it |
| 14 | Consecutive unanswered | current run length; longest run in window | probes | — | finalised, in `seq` order | — | A run is broken by one `success`, or by a gap |
| 15 | Recovery duration | `t_send` of first probe in the closing success run − `t_send` of first probe in the failure run | s | — | per incident (§8) | — | Quantised to the probe interval |
| 16 | DNS transaction time | `t_recv`(last byte of valid matching response) − `t_send`(first byte of query) on an established transport | ms | excludes connection setup unless the stream says "fresh" | DNS `success` | as 4–7 | Cache-influenced for ordinary names; measures resolver + path, not the OS resolver |
| 17 | DNS no-response rate (`no-resp`) | timeout ÷ finalised | % | deadline 3000 ms default | finalised DNS transactions | 20 | Transport-level only |
| 18 | DNS error-response rate (`err-resp`) | (service_error + protocol_error) ÷ finalised | % | — | finalised DNS transactions | 20 | An answer arrived; it was an error or not what the scenario expected |
| 19 | TCP connect time | connect call start → socket writable | ms | — | fresh connections | 1 (low n < 20) | SYN → SYN-ACK plus local scheduling |
| 20 | TLS handshake time | first handshake byte written → handshake complete | ms | — | fresh connections | 1 (low n < 20) | Full vs resumed handshake recorded; never mixed in one statistic |
| 21 | Fresh transaction total | connect + handshake + first query | ms | — | scheduled fresh-connection probes | 1 (low n < 20) | Low cadence (default every 5 min), so usually low n |
| 22 | Scheduling lag (collector health) | actual send − scheduled slot | ms | — | all sent probes | — | An instrument-quality metric, not a network metric |

Session-wide ("since start") versions of 3–8 and 11–13 appear in the detail view, labelled as such.

### 5.1 Percentile method

**Nearest-rank** (inverse empirical CDF, Hyndman–Fan type 1): sort the n eligible values; the p-th percentile is the value at 1-based rank ⌈p·n⌉. **[R]**

- It always returns a value that was actually observed and needs no interpolation rule to explain.
- Percentiles are computed **exactly** from stored samples for every window, including 24 h (86,400 values per stream is a few hundred kilobytes and a millisecond-scale selection). No sketches, no bucketed approximations, and therefore no averaging of per-bucket percentiles — a longer window is always recomputed from its raw samples.
- Small-sample behaviour: with n < 20, ⌈0.95·n⌉ = n, so p95 would silently equal the maximum. That is why p95 has min n = 20 and p99 has min n = 100. Below the minimum the cell shows `—` and the sample count, not an unstable number.

### 5.2 RTT variation

`Δ` = mean absolute difference between the RTTs of two probes that are **adjacent in send order**: probe `seq` and probe `seq + 1`, both `success`, same stream, same destination epoch. A pair is dropped if either probe has any other outcome or if a gap, skip, or epoch change lies between them. It needs 10 valid pairs.

It is described on screen as "RTT variation (mean |Δ| of consecutive probes)". It is a round-trip quantity sampled at 1 Hz. It is not the one-way packet delay variation of RFC 3393 and not the smoothed interarrival jitter of RFC 3550, and the help text says so.

### 5.3 Comparison rules

- A comparison view only ever contains streams with the same protocol, family, scenario set, and window.
- ICMP RTT, DNS transaction time, TCP connect, TLS handshake, and HTTP request time are separate metrics with separate series. They are never drawn on one line or summed without a label saying what was added.
- Latency is always shown beside its no-reply/no-response rate and coverage. A card cannot show a healthy-looking median without the fraction of probes it is based on.

---

## 6. DNS measurement

DNS probes are real DNS transactions that the application builds, sends to a chosen address, and parses. The OS resolver API (`getaddrinfo`) is never timed and never presented as a provider's response time.

### 6.1 Query

| Setting | Default | Notes |
|---|---|---|
| Query set | `. NS`, `example.com. A`, `wikipedia.org. AAAA` | Configurable. Chosen to be answerable from cache by any recursive resolver and not operated by any measured provider **[A]** |
| Selection | round-robin by slot index: stream slot k sends query k mod 3 | Every provider sees the same query sequence. Streams are phase-staggered (see [architecture.md §4](architecture.md#4-modules-and-concurrency)), so the comparison is reproducible without synchronised bursts |
| Header | RD = 1, CD = 0, AD = 0, opcode QUERY, one question | |
| EDNS(0) | present; UDP payload size 1232; DO = 0; no Client Subnet, no cookie, no padding (padding considered for DoT/DoH later) | |
| ID | random 16-bit (CSPRNG) per query on UDP/TCP/DoT; 0 on DoH | |
| Source port | a new connected UDP socket per query (OS-chosen ephemeral port) | A connected socket also turns ICMP port-unreachable into an explicit `unreachable` outcome |
| Deadline | 3000 ms per attempt | Must stay below the 5 s interval so a stream never overlaps itself; the config validator enforces this |
| Retries | 0 | §4.4 |
| Cadence | one transaction per stream every 5 s | |
| Client cache | none | The application has no DNS cache and no fallback resolver on the measurement path |

### 6.2 Four layers of result

Each transaction records all four; the outcome in §4.3 is derived from them.

1. **Transport** — did bytes come back before the deadline, and how long did it take?
2. **Validity** — is it a well-formed DNS response matching the query's ID and question?
3. **RCODE and shape** — `NOERROR` with answers, `NOERROR` with no data, `NXDOMAIN`, `SERVFAIL`, `REFUSED`, other; plus TC flag, answer count, response size, minimum answer TTL, and any Extended DNS Error codes.
4. **Scenario expectation** — does layer 3 match what this query was configured to expect?

Each query declares an expectation: `noerror` (default: `NOERROR`, answers optional), `answer` (`NOERROR` with ≥ 1 record of the type asked for), `nxdomain`, `blocked`, or `any`. Fixed answer addresses are never required, because legitimate CDN answers vary.

### 6.3 Classification

| What came back | Outcome | Counted in |
|---|---|---|
| Nothing by the deadline | `timeout` | no-response rate |
| ICMP port/host unreachable on the connected socket | `unreachable` | no-response rate |
| Valid response, expectation met | `success` | latency statistics |
| Valid response: `SERVFAIL`, `REFUSED`, unexpected `NXDOMAIN`, unexpected empty answer | `service_error` (RCODE kept) | error-response rate; its time is kept in the raw view, not in latency statistics |
| Valid response that looks policy-blocked (answer `0.0.0.0` / `::`, or EDE 15/16/17) when the scenario did not expect blocking | `service_error` (`blocked`) | error-response rate |
| Same, when the scenario is `blocked` | `success` | latency statistics |
| Unparseable, wrong ID, wrong question | `protocol_error` | error-response rate |

`NXDOMAIN`, `SERVFAIL`, `REFUSED`, empty answers, and blocked answers are therefore never shown as timeouts, and never as loss.

### 6.4 Cache influence

Repeating ordinary names measures a warm resolver cache plus the path to it. Every DNS series is labelled **cache-influenced**. Random or unique hostnames do not guarantee an uncached recursion (negative caching, aggressive NSEC, wildcard synthesis, and resolver prefetch all interfere), so the application does not claim that. A controlled cold-cache experiment needs an authoritative zone under the experimenter's control with unique labels and known TTLs; that is a separate optional benchmark and is deferred ([ROADMAP.md](../../ROADMAP.md)).

### 6.5 Truncation and TCP fallback

- A UDP response with TC = 1 is a valid UDP exchange. The UDP attempt is recorded with its own time and `truncated = true`.
- Fallback to TCP is on by default. The TCP attempt is a second observation with the same transaction id.
- The DNS/UDP series plots UDP attempt time only. Transactions that needed fallback are marked on the timeline and their **total** time (UDP attempt + TCP connect + TCP query) is shown in the raw view, labelled as including fallback.
- With the default query set and a 1232-byte EDNS size, truncation should be rare; a rising count is itself worth seeing. **[A]**

### 6.6 Plain TCP, DoT, DoH

| Stream | Connection policy | What is timed | Default cadence |
|---|---|---|---|
| DNS/TCP | new connection per probe (2-byte length framing) | connect, then query → response; reported separately and as a total | every 60 s (off unless enabled) |
| DoT · reused | one long-lived TLS connection per stream, reconnect on close/error | query → response on the established connection | every 5 s |
| DoH · reused | one long-lived HTTP/2 connection per stream | HTTP request sent → complete response body on the established connection | every 5 s |
| DoT/DoH · fresh | a new connection that is closed afterwards; no TLS session reuse by default | TCP connect, TLS handshake, first query — three phases and a total | every 5 min |

- **Reused and fresh are different cohorts** with separate statistics. A reconnect forced by the server or an error is recorded as a `connection` event with its phase timings; the query that triggered it is tagged `after_reconnect` and excluded from the reused-connection statistics.
- **Choosing the IP while keeping TLS honest.** The application dials the selected destination IP itself and passes the configured hostname as SNI and as the certificate verification name. Verification is never disabled. For IP-literal endpoints the certificate must contain that IP as a subject alternative name, or the stream reports `protocol_error (certificate)`.
- **Recorded per connection:** bootstrap resolution (resolver used, duration, all addresses returned), selected IP, SNI, ALPN, TLS version, full/resumed, each phase duration, connection id, and the reason the previous connection ended.
- **Phases the client cannot expose** are listed with the chosen libraries in [architecture.md §3.4](architecture.md#34-what-each-layer-can-and-cannot-time).
- DoH uses POST with `application/dns-message` and DNS ID 0. HTTP status other than 200 is a `service_error` carrying the status code.

> **Not built:** TLS session resumption for fresh-connection probes (a fresh probe always makes a full handshake; `fresh_handshake` accepts only `full`), and DoH by GET (`dns.doh.method` accepts only `post`).

### 6.7 NextDNS specifics, proxies, VPNs

- Queries to a NextDNS profile are subject to that profile's blocklists and rewrites. The query set expectations (§6.2) are where the user states what the profile should do; an unexpected block shows as `service_error (blocked)`, not as an outage.
- **The profile ID is sensitive configuration.** It appears in the DoH path and in the DoT/DoQ server name. It is masked in the UI by default (`ab••••`), redacted in logs, and replaced by `‹redacted:nextdns-profile›` in exports and shared reports unless an explicit flag is passed. With DoT the profile ID is part of the TLS server name on the wire; that is a property of the protocol, not something the application can hide.
- **VPNs:** sockets follow the OS routing table unless an interface binding is configured, so an active VPN is used, not bypassed. The header always shows the egress interface and flags tunnel-like interfaces.
- **HTTP proxies:** DoH probes connect directly by default, because a query through a proxy would time the proxy. If `HTTPS_PROXY`/`ALL_PROXY` is set, that is **not silent**: the header shows a persistent notice, an event is logged, and `doh.proxy = "env"` routes through the proxy with the series relabelled "via proxy". **[R]**

> **As built:** the profile id is never shown in part. The whole id is replaced by `‹redacted:nextdns-profile›` on screen, and it is removed from a recording as the recording is written by default (`redaction.profile_ids`), so it does not reach the disk. `config show --show-secrets` prints it.
>
> **Not built:** source and interface binding (§12.4), and DoH through a proxy (`dns.doh.proxy` accepts only `direct`). When a proxy variable is set, the session says so in its events and connects directly.

---

## 7. Route diagnostics

Route inspection is a separate, on-demand diagnostic. It never runs by default and never feeds the latency series.

- **Trigger:** `t` on a focused provider opens the Route view with the proposed budget; `Enter` starts it. One trace at a time.
- **Budget (defaults):** ICMP probes, max TTL 30, 10 rounds, one round per second, hard cap 300 probes per trace and 30 probes per second. The view shows probes used against the cap. A trace stops at the cap, at the round limit, on `t`, or when the view is left (configurable to keep running).
- **Per hop:** TTL, every responding address seen at that TTL with its count, probes sent, replies, no-reply share, last/average/best/worst RTT, first-seen and last-seen timestamps.
- **Reverse DNS** is resolved asynchronously on a separate task after a hop first appears, never on the probe path, and can be switched off (`n`). Names are untrusted text and are sanitised before display (§12.5).
- **The trace protocol is shown in the title.** An ICMP trace does not necessarily follow the path that DNS-over-UDP or HTTPS traffic takes, and the view says so.

> **Not built:** UDP, TCP and Paris-style tracing — a trace is ICMP only (`trace.protocol` accepts only `icmp`). A trace always stops when its view is left (`trace.stop_on_leave` accepts only `true`). Traces are not recorded or exported, and there is no headless trace command.

**How the view explains itself** (see the sketch in §9.6). Annotations are generated from the observations and state their basis:

| Observation | Annotation | Why |
|---|---|---|
| A hop never replies; later hops do | "does not send ICMP Time Exceeded — not evidence of loss" | Routers may be configured not to answer |
| A hop replies to some probes; later hops reply to more | "consistent with ICMP rate limiting at this hop, not forwarding loss" | Routers rate-limit and deprioritise ICMP they generate; loss that does not persist to later hops is not forwarding loss |
| No-reply share rises at a hop and stays at least that high through the destination | "no-reply begins at or before this hop and persists" | The only pattern that suggests real loss — still not proof of where |
| Several addresses at one TTL | "per-flow load balancing (ECMP) or a route change" | Different probes may hash to different paths |
| A middle hop is slower than the destination | "hop RTT reflects that router's own reply handling and return path" | Each row is an independent round trip; rows are not additive and adjacent rows must not be subtracted to get a link latency |
| Destination is an anycast address | "the answering site can change over time and between protocols" | Anycast |

**Summaries cite observations and stop short of blame.** The event rules (§8) produce sentences such as *"All four ICMP v4 targets stopped replying at 14:02:10; the gateway still replies (5/5); DNS/UDP to Cloudflare and Google still answers"*. They never produce *"ISP outage confirmed"*.

---

## 8. Events and incidents

Events are facts with a timestamp: probe-run changes, address changes, source-context changes, gaps, local errors, configuration reloads, trace start/stop, recorder problems. Incidents are events with an open and a close, governed by thresholds, minimum samples, hysteresis, and a cooldown.

| Rule | Opens when | Closes when | Min samples | Notes |
|---|---|---|---|---|
| `no_reply_run` | 5 consecutive finalised ICMP probes unanswered (DNS: 3 consecutive no-response) | 10 consecutive replies (DNS: 3) | — | Recovery duration per §5 #15 |
| `elevated_no_reply` | no-reply rate ≥ 5 % over the last 60 s | ≤ 1 % over 60 s, held 30 s | 30 finalised in the 60 s | Suppressed while a `no_reply_run` is open on the same stream |
| `latency_shift` | p50 over the last 60 s > max(1.5 × baseline, baseline + 10 ms); baseline = p50 of the preceding 15 min | < 1.2 × baseline for 60 s | 30 recent, 300 baseline | The baseline freezes while the incident is open |
| `error_responses` (DNS) | ≥ 3 service/protocol errors in the last 10 transactions | 10 consecutive successes | 10 | Carries the RCODE mix |
| `local_error` | first local send failure on a stream | first successful send | — | Never contributes to network rates |

- **Cooldown:** a rule cannot reopen on the same stream within 60 s of closing. More than 3 open/close cycles in 10 minutes collapse into one `flapping` incident that closes after 5 quiet minutes.
- **Correlation:** incidents opening within 10 s of each other on streams of the same protocol and family are grouped into one event that lists which streams are affected and which comparable streams are not, including the gateway and local-DNS controls when enabled. Refused sends (`local_error`) are grouped by family across protocols. The wording is assembled from counts, never from a guessed cause.
- **Gaps never open incidents.** After a gap, run counters restart.
- **Replay and demo sessions** have their own session id and their own incident state. They can never write to a live recording, and the header shows `REPLAY` or `DEMO` for the whole session.
- External notifications are out of scope.

> **As built:** the correlation window is fixed at 10 s and is not a setting. Only openings are grouped: recoveries are reported per stream.

---

## 9. Visual design

### 9.1 Principles

- **Hierarchy:** the eye should land on four status badges, then four big numbers, then the chart. Everything else is quieter.
- **Restrained chrome:** rounded single-line borders around panels only; no nested boxes; one blank column of padding inside every panel; dim separators (`·`, `│`) instead of more borders.
- **Readable numbers:** fixed-width, right-aligned, one decimal for milliseconds, units stated once per panel title.
- **Colour means identity; text and symbol mean health.** A provider keeps its colour in every view. Health is carried by a symbol plus a word, so it survives monochrome and colour-blind viewing and never fights the identity colour.
- **Motion means an event happened.** Nothing moves unless a probe was sent, a reply arrived, or a state changed.

### 9.2 Layouts

Three layouts: large at ≥ 160 × 45, standard at ≥ 120 × 35, compact at ≥ 80 × 24. The breakpoints are to be validated on real terminals **[T]**. Width and height are evaluated independently (a 200 × 30 terminal uses standard height rules). Below 80 × 24 the application shows a single line asking for a larger window rather than drawing clipped content.

> **As built:** one layout is chosen from both dimensions together — large needs at least 160 columns *and* 45 rows, standard at least 120 *and* 35 — so a 200 × 30 terminal gets the compact layout.

The screens below are **design sketches with illustrative data**. The program's actual screens are kept as golden files in [`crates/pathwatch-tui/tests/golden/`](../../crates/pathwatch-tui/tests/golden/). Addresses in `192.0.2.0/24`, `198.51.100.0/24`, and `203.0.113.0/24` are documentation ranges standing in for a local network and for targets that have not been configured.

**Standard, 120 × 35** — the reference layout.

```text
 pathwatch ▸ COLLECTING    mode ICMP echo    family IPv4    via en0 192.0.2.10 → gw 192.0.2.1    every 1 s   up 01:42:17
 window 5 m · LIVE · ICMP backend: unprivileged datagram socket · recording off · 14:03:22 UTC       [ILLUSTRATIVE DATA]
╭─ 1 Cloudflare ─────────────╮╭─ 2 Google ─────────────────╮╭─ 3 NextDNS ────────────────╮╭─ 4 Arelion ────────────────╮
│ ● OK              ICMP · v4││ ● OK              ICMP · v4││ ? NEEDS CONFIGURATION      ││ ▲ LOSSY           ICMP · v4│
│ 1.1.1.1                    ││ 8.8.8.8                    ││ no target configured       ││ 198.51.100.7 (placeholder) │
│  12.4 ms     last 0.4 s ago││  18.9 ms     last 0.1 s ago││                            ││  31.0 ms     last 0.9 s ago│
│ p50 12.1  p95 14.8  Δ 0.6  ││ p50 18.2  p95 22.0  Δ 0.9  ││ Add your endpoint under    ││ p50 30.2  p95 44.9  Δ 3.1  │
│ no-reply 0.0 %     0 / 300 ││ no-reply 0.0 %     0 / 300 ││ [providers.nextdns] and    ││ no-reply 12.7 %   38 / 300 │
│ n 300 · coverage 100 %     ││ n 300 · coverage 100 %     ││ restart. Nothing is being  ││ n 262 · coverage 100 %     │
│ ▁▁▂▁▁▁▁▁▁▁▁▁▁▁▁▁▁▁█▆▁▁▁▁▁▁ ││ ▂▁▁▁▁▂▁▁▁▁▁▂▂▁▁▁▁▁██▁▁▂▁▁▁ ││ sent; not counted as loss. ││ ▂▂▂▂▂▂▂▆██▁××▃▂▂▂▂▅▇▂▁▂▁▂▂ │
╰────────────────────────────╯╰────────────────────────────╯╰────────────────────────────╯╰────────────────────────────╯
╭─ Latency · ICMP echo RTT · IPv4 · common scale, ms ─────────────── ━ Cloudflare  ━ Google  ┄ NextDNS n/a  ━ Arelion ─╮
│  64 ┤                                                                                                                │
│     │                                 ⡠⢄⠒⠤⠄                                         ⢀                                │
│  48 ┤                               ⠠⠔    ⠈⠁                                      ⠤⠤⠂⠄⠄                              │
│     │  ⡀⡀   ⠠⡀ ⢀     ⠄ ⡀⡀  ⢀ ⡀⢀    ⠐⠄      ⠐⠂⡀⠄             ⡀⠐⣀ ⡀⢀⡀⠠ ⡀⢀⠠ ⡀⡀⡀  ⠠⠄⠐⠑   ⠈⢀  ⡀       ⡀  ⡀   ⡀⢀ ⠄   ⢀     │
│  32 ┤⠉⠊⠈⠐⠑⠊⠉⠁⠠⠑⠂⠡⠑⠒⠊⠊⠐⠊⠐⠠⠊⠒⠁⠑⠈⠁⠊⠉⠊⠒⠂        ⠐⠈⠐             ⠈⠁ ⠑⠈⠁⠈⠁⠑⠈⠁⠁⠉⠐⠈⠐⠡⠊⠁⠠⠂⡠⠊⠔⢁⡀ ⠑⠑⠐⠁⠐⢁⠉⠉⠑⠡⠐⠑⠒⠐⠌⠉⠒⠐⠁⠡⠈⠑⠒⠑⠁⠉ AR │
│     │⠢⠤⢄⠤⡠⠤⡠⠄⠢⠤⠤⠤⡐⡠⠤⠤⠠⠤⢂⢄⢄⡐⠤⢄⡠⡠⡠⡐⠤⣀⢄⠤⠤⠤⠤⠢⡠⢄⠤⡠⠤⢄⠤⠤⠤⠔⠤⠤⠔⡠⠤⠔⠒⠢⠤⠤⠔⠤⠤⠤⣀⠤⠢⢄⢂⢂⠤⠤⠤⠤⠤⠤⠤⠤⠢⠔⠔⠊⠢⢄⠈⠔⣀⠤⡠⠤⠤⠤⠤⠤⡠⠔⠔⠢⡠⠤⡠⠤⢄⡠⠤⠤⠢⠤⢄⠢⡠⠔ GO │
│  16 ┤⠒⠒⠒⠢⠒⠒⠒⠜⠊⠒⠒⠒⠒⠒⠒⠒⠓⠒⠒⠒⠑⠒⠢⠒⠢⠒⠢⠔⠒⠒⠒⠢⠒⠔⠒⠒⠒⠒⠒⠔⠢⠒⠔⠒⠒⠒⠢⠒⠒⠒⠑⠒⠒⠒⠔⠒⠒⠒⠢⠢⠊⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠔⠊    ⠒⠒⠒⠒⠒⠑⠊⠊⠒⠒⠒⠢⠒⠡⠒⠒⠒⠒⠒⠒⠊⠒⠒⠒⠒⠢⠒⠒ CF │
│     │                                                                                                                │
│   0 └┴────────────────────┴─────────────────────┴────────────────────┴─────────────────────┴────────────────────┴    │
│      -5m                 -4m                   -3m                  -2m                   -1m                 now    │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Outcomes · same time axis ──────────────────── · replied  ▒ some unanswered  █ none answered  ! local error  ╌ gap ─╮
│ 1 CF ············································································································    │
│ 2 GO ············································································································    │
│ 3 ND   needs configuration — nothing sent                                                                            │
│ 4 AR ··········································█████████████······························▒▒·····················    │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Events ──────────────────────────────────────────────────────────────────────── newest first · 3 of 17 · e to open ─╮
│ 14:02:32  ▲ Arelion ICMP v4     2 consecutive probes unanswered (below incident threshold of 5)                      │
│ 14:01:06  ● Arelion ICMP v4     replies resumed after 36 s; 36 of 36 probes in the run were unanswered               │
│ 14:00:35  ▲ Arelion ICMP v4     no-reply run opened: 5 consecutive timeouts · Cloudflare, Google replied 10/10       │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 focus 1 Cloudflare │ 5 m │ LIVE │ 1-4 select · ⏎ details · m mode · f family · +/- window · ␣ freeze · ? help · q quit
```

**Large, 160 × 45** — adds a focus panel beside the chart, a taller chart, more card rows, a time axis and legend on the timeline, and more events. (View without line wrapping.)

```text
 pathwatch ▸ COLLECTING      mode ICMP echo      family IPv4      via en0 192.0.2.10 → gateway 192.0.2.1      every 1 s      up 01:42:17      rev 3
 window 5 m · LIVE · ICMP backend: unprivileged datagram socket · DNS UDP/DoH collecting in background · recording off · 14:03:22 UTC        [ILLUSTRATIVE DATA]
╭─ 1 Cloudflare ───────────────────────╮╭─ 2 Google ───────────────────────────╮╭─ 3 NextDNS ──────────────────────────╮╭─ 4 Arelion ──────────────────────────╮
│ ● OK                       ICMP · v4 ││ ● OK                       ICMP · v4 ││ ● OK                       ICMP · v4 ││ ▲ LOSSY                    ICMP · v4 │
│ 1.1.1.1                              ││ 8.8.8.8                              ││ 203.0.113.28 (placeholder)           ││ 198.51.100.7 (placeholder)           │
│  12.4 ms              last 0.4 s ago ││  18.9 ms              last 0.1 s ago ││  24.6 ms              last 0.7 s ago ││  31.0 ms              last 0.9 s ago │
│ p50 12.1   p95 14.8   Δ 0.6          ││ p50 18.2   p95 22.0   Δ 0.9          ││ p50 23.9   p95 41.7   Δ 1.6          ││ p50 30.2   p95 44.9   Δ 3.1          │
│ min 10.9   mean 12.3   max 21.6      ││ min 16.0   mean 18.6   max 30.2      ││ min 20.8   mean 25.4   max 55.3      ││ min 26.1   mean 31.9   max 47.8      │
│ no-reply 0.0 %   0 of 279 finalized  ││ no-reply 0.0 %   0 of 279 finalized  ││ no-reply 0.0 %   0 of 279 finalized  ││ no-reply 12.9 % 36 of 279 finalized  │
│ sent 280 · pending 1 · coverage 93 % ││ sent 279 · pending 0 · coverage 93 % ││ sent 279 · pending 0 · coverage 93 % ││ sent 279 · pending 0 · coverage 93 % │
│ ▁▁▁▂▁▁▁▂▁▁  ▁▁▁▁▁▁▂▁▁▁▂▁▁▁█▆▁▂▁▂▂▁▂▁ ││ ▃▁▁▂▂▁▂▁▃▁  ▁▂▁▁▂▂▂▂▂▂▂▂▂▁██▂▃▂▁▁▂▂▁ ││ ▁▂▁▁██▄▁▁▁  ▁▁▂▁▁▂▁▁▁▁▁▁▁▁▄▃▁▁▁▁▁▂▂▁ ││ ▁▂▂▁▂▁▂▂▂▁  ▁▂▂▂▁▂×××▁▁▂▂▁▇█▄▃▁▂▁▁▁▂ │
╰──────────────────────────────────────╯╰──────────────────────────────────────╯╰──────────────────────────────────────╯╰──────────────────────────────────────╯
╭─ Latency · ICMP echo RTT · IPv4 · common scale, ms ──────────── ━ Cloudflare ━ Google ━ NextDNS ━ Arelion ─╮╭─ Focus · 1 Cloudflare · ICMP v4 ───────────────╮
│  64 ┤                                                                                                      ││ 1.1.1.1 · literal IP · epoch 1 (no addr change)│
│     │                                                                                                      ││ source en0 192.0.2.10 · route via 192.0.2.1    │
│  55 ┤             ⠢⠄                                                                                       ││ RTT distribution, ms (n 279)                   │
│     │            ⡈ ⢀                                                          ⡈⠂                           ││  10–11 ▏██▌                              20    │
│  47 ┤               ⠁                                                        ⠠  ⢀                          ││  11–12 ▏███████████▊                     95    │
│     │           ⠐   ⠠                                                        ⠁ ⠈⠂                          ││  12–13 ▏██████████████▏                 113    │
│  38 ┤       ⠠   ⠂    ⡀                     ⡀  ⡀    ⢀               ⢀   ⢀    ⠑⢀⡀  ⠁  ⠠ ⠄                    ││  13–14 ▏████▊                            38    │
│     │⡠⠠⠢⠁⡀⢀⠤⠄⠂⠄⢈⡀⢄ ⢀⢀⠐⢀⠂⠁⢀ ⠔⣀⠡⡈       ⠤⡀⡐⠔⠡ ⡀⢂ ⠈ ⡠⢀ ⣀           ⢂⡐⠢⡀⠄⢂⡐⠄⠤⠂⢂⠑⢀⠂⠐⢂ ⠠⠄ ⡀⢄ ⣀⠄⢁⠄ ⡀⣀⣀⠄⠄⠂⠄ ⠁⠠⢄ AR ││  14–16 ▏█▏                                9    │
│  30 ┤⢀⠁ ⠔⠈⡁  ⠠⠈⠁⢀ ⠒⠁⠁⠐⠂⠈⠠⠂⠊ ⢀         ⠄⡠ ⢀ ⠠ ⠄⠈⡀⠔ ⠁⡄⠠    ⡀  ⡀  ⡀    ⠐ ⢀  ⠈ ⢀⢄⠉⠒⢁⠄ ⠐⠒⢀ ⠈ ⠈  ⠐   ⠈⠠⠔⠌⠉⠐⠂     ││  16–32 ▏▌                                 4    │
│     │⠂⠌⠒⠐⠊⠐⠡⠒⣀⡐⠂      ⠈⠑⠉⠑⠤⠔⠄⡠⠄        ⠈⠊⠂⠒⠒⠪⢀⠐⠊⠒⠉⠒⠈⠄⠡⠌⠤⡠⠐⡈⠊⠐⠒⠌⠐⠒⠤⠢⢂⠑⠄⠂⠔⠌⠌⠊⠂⠂⣀  ⢒⠊⠡⠑⡄⠡⠊⠔⢁⠉⢁⠡⠉⠢⠌⠌⠑⠐⢀⠈⠑⢂⠤ ND ││                                                │
│  21 ┤⢁⠤⢂⢄⣀⠤⡠⡀⠢⡠⠤⡠⠈⡠⠤⠔⠠⢅⢂⠄⠄⠈⠢⠄⡠⠨       ⠪⠢⠡⡠⣀⠔⠠⠤⢆⠢⠤⢄⠌⠒⢂⠊⡐⠢⠒⠑⠡⠢⠤⠒⠤⠢⠢⣀⠒⠢⠄⢊⢂⡠⢄⠒⠤⡈⡐ ⠑  ⠢⠒⠠⢀⠔⠔⣀⢄⡐⠤⠤⢄⠢⠔⡠⠔⠊⠡⡑⠤⡐⠤ GO ││ min 10.9  mean 12.3  sd 1.2    p50 12.1        │
│     │⣀    ⡀ ⢐⠠ ⡀⡀⢁⡀⢀⢀⢄ ⡀⠈⠌⣁ ⡈⡀⢁          ⣀⡀ ⠁ ⡀  ⢀  ⡀⡀ ⣀  ⢀  ⡀⣀⡀⡀⢀  ⠈⣀ ⣀⣀ ⢀⡐   ⠡ ⣀⣀⠂⡠ ⢀⢀⢀⡀⠄⢀⠠   ⡀ ⡀ ⢀⣀     ││ p95 14.8  p99 19.4   max 21.6  Δ 0.6           │
│  13 ┤ ⠉⠉⠑⠉⠈⠉⠂⠁⠉⠈⠈⠁⠈⠁⠁⠁⠉⠈⠉⠈ ⠑⠈⠐⠁       ⠊⠉⠉ ⠈⠉⠌⠑⠈⠊⠉⠁⠉⠑⠈⠈⠉ ⠉⠉⠁⠊⠉⠈ ⠐⠐⠁⠉⠉⠉ ⠉  ⠉⠁     ⠉  ⠉ ⠉⠁⠁⠁⠈⠈⠁⠁⠉⠉⠉⠐⠉⠐⠉⠁ ⠉ CF ││                                                │
│     │                                                                                                      ││ sent 280 · replied 279 · timed out 0           │
│   4 ┤                                                                                                      ││ unreachable 0 · late 0 · duplicate 0           │
│   0 └┴──────────────────┴───────────────────┴──────────────────┴───────────────────┴──────────────────┴    ││ local error 0 · skipped 0 · pending 1          │
│      -5m               -4m                 -3m                -2m                 -1m               now    ││ gap 1 (00:21, system sleep) → coverage 93 %    │
╰────────────────────────────────────────────────────────────────────────────────────────────────────────────╯╰────────────────────────────────────────────────╯
╭─ Outcomes · same time axis as the chart ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ 1 CF ··························╌╌╌╌╌╌╌·································································    │ · all probes in bucket replied                  │
│ 2 GO ··························╌╌╌╌╌╌╌·································································    │ ▒ some unanswered   █ none answered             │
│ 3 ND ··························╌╌╌╌╌╌╌·································································    │ E error response    ! local error (not loss)    │
│ 4 AR ··························╌╌╌╌╌╌╌···············███████████··························▒▒···········    │ ╌ observation gap (nothing sent)                │
│      -5m               -4m                 -3m                -2m                 -1m               now    │ 1 cell ≈ 3 s · Enter on a cell lists its probes │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Events ───────────────────────────────────────────────────────────────────────────────────────────────────── newest first · 4 of 17 · e to open · / filter ─╮
│ 14:02:44  ▲ Arelion ICMP v4     2 consecutive probes unanswered (below incident threshold of 5)                                                              │
│ 14:01:23  ● Arelion ICMP v4     replies resumed after 34 s; 34 of 34 probes in the run were unanswered · other three ICMP v4 targets replied throughout      │
│ 14:00:54  ▲ Arelion ICMP v4     no-reply run opened: 5 consecutive timeouts · gateway replied 5/5 · Arelion DNS/UDP not configured                           │
│ 14:00:03  ╌ all streams         observation gap 00:21 (system sleep detected) · 0 probes sent · excluded from all rates                                      │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 focus 1 Cloudflare │ 5 m │ LIVE │ Tab focus · 1-4 provider · ⏎ details · m mode · f family · +/- window · ␣ freeze · c cursor · t trace · ? help · q quit
```

**Compact, 80 × 24** — a focused view with navigation, not a clipped dashboard. Cards become a five-line table, the chart shows the focused provider (`a` overlays all four), events collapse to the newest line, and everything else is one keypress away.

```text
 pathwatch ▸ COLLECTING  ICMP v4  en0  1 s  5 m  LIVE      up 01:42:17  [DEMO]
    provider    target         status         last    p50    p95  no-reply   n
 ▸1 Cloudflare  1.1.1.1        ● OK           12.4   12.1   14.8    0.0 %  300
  2 Google      8.8.8.8        ● OK           18.9   18.2   22.0    0.0 %  300
  3 NextDNS     —              ? NEEDS CONFIG    —      —      —       —     —
  4 Arelion     198.51.100.7   ▲ LOSSY        31.0   30.2   44.9   14.0 %  258
╭─ 1 Cloudflare · ICMP RTT v4, ms ─────────────────────────────── a: all four ─╮
│  32 ┤                                                                        │
│     │                                                                        │
│  24 ┤                                                 ⢀⢂                     │
│     │                                                 ⠄ ⠂                    │
│  16 ┤⠤⡀⢀ ⣀⡀⣀⢀⡠⣀⢄⣀⡠⣀⢀⣀⡠⣀⡀⡀⢄⣀⡀⣀⡀⡠⠄⢀⣀⡀⡠⡀⢀⢀⢀⣀⢄⢄⢀⢀⡀⡀⢀ ⡠⣀⡀⠄⡐  ⠈⢀⢀⢀ ⣀⡠⠄⡀⡠⣀⣀⡀⢄⣀⡠⣀ CF │
│     │ ⠈⠁⠉ ⠈ ⠁      ⠁   ⠈⠈  ⠈ ⠈ ⠈⠁ ⠈ ⠈⠁⠁⠁   ⠁⠂⠈⠈⠂⠉  ⠈⠈    ⠁⠁⠁⠉  ⠈⠈   ⠈        │
│   8 ┤                                                                        │
│     │                                                                        │
│   0 └┴────────────┴─────────────┴────────────┴─────────────┴────────────┴    │
│      -5m         -4m           -3m          -2m           -1m         now    │
╰──────────────────────────────────────────────────────────────────────────────╯
 1 CF  ····································································
 2 GO  ····································································
 3 ND    needs configuration — nothing sent
 4 AR  ····················█████████··························█············
 14:00:30 ● Arelion ICMP v4 replies resumed after 40 s (40/40 unanswered)
 1-4 select · ⏎ details · m mode · f family · +/- window · ␣ freeze · ? · q
```

| Element | Large 160 × 45 | Standard 120 × 35 | Compact 80 × 24 |
|---|---|---|---|
| Header | 2 lines, full labels, config revision | 2 lines | 1 line, abbreviated |
| Provider cards | 4 × (40 × 10): adds min/mean/max and sent/pending | 4 × (30 × 9) | 4-row table: status, last, p50, p95, no-reply, n |
| Sparkline | 36 cells | 26 cells | moved to detail view |
| Comparison chart | 110 × 19, four series | 120 × 12, four series | 80 × 12, focused series; `a` for all four |
| Focus panel (distribution, counters) | beside the chart | moved to detail view (`Enter`) | moved to detail view |
| Outcome timeline | 4 rows + axis + legend | 4 rows, legend in title | 4 rows, no border, same x-axis as the chart |
| Events | 4 lines | 3 lines | newest 1 line; `e` opens the list |
| Footer | full key hints | common key hints | essential key hints; `?` for the rest |

### 9.3 Overview components

1. **Header.** Collection state (`COLLECTING`, `PAUSED`, `REPLAY`, `DEMO`), measurement mode, family, source context (interface, source address, gateway), interval, session duration; second line: window, `LIVE`/`FROZEN`, ICMP backend in use, background collection, recording state, UTC time, and any standing notice (proxy detected, capability degraded, collector backpressure).
2. **Provider card.** Slot number and name in the border; status symbol + word; protocol · family; the *actual destination* (and the configured hostname if different); last RTT large with its age; p50, p95, Δ; no-reply rate with its raw fraction; n and coverage; sparkline. A card whose stream is `needs_config`, `unsupported`, or `disabled` says so in words, explains what to do, and states that nothing is being sent. It never shows 100 % loss and never substitutes another address.
3. **Comparison chart.** Four labelled series on a common time axis and a common latency axis with visible units; legend in the title; focused series drawn brighter and last.
4. **Outcome timeline.** One row per provider on the chart's exact x-axis. This is where timeouts, errors, and gaps live, so a missing latency sample is always accounted for directly under the hole it left in the chart.
5. **Events.** Newest first; time, symbol, stream, sentence.
6. **Footer.** Focused provider, window, `LIVE`/`FROZEN`, key hints sized to the width.

> **As built:** a session stopped with `P` shows `STOPPED` in the header, not `PAUSED` (§9.4). The chart's latency axis covers the highest round trip of 95 % of the time columns, or up to twice that if the peak fits; higher values are drawn to the top row and marked `▲`, with the true maximum in the title. One slow reply therefore cannot flatten the chart, and a change lasting more than a twentieth of the window still sets the scale.

### 9.4 Status vocabulary

| Symbol | Word | Meaning | ASCII fallback |
|---|---|---|---|
| `●` | `OK` | no open incident; enough samples | `*` |
| `◐` | `WARMING UP` | fewer than 20 finalised probes | `~` |
| `▲` | `LOSSY` / `SLOW` / `ERRORS` | an `elevated_no_reply`, `latency_shift`, or `error_responses` incident is open | `^` |
| `✖` | `NO REPLY` | a `no_reply_run` incident is open | `x` |
| `!` | `LOCAL ERR` | sends are failing locally; not counted as loss | `!` |
| `◌` | `STALE` | collection should be running but nothing finalised recently | `o` |
| `‖` | `PAUSED` | collection stopped by the user | `=` |
| `?` | `NEEDS CONFIG` | no usable target configured | `?` |
| `⊘` | `UNSUPPORTED` | this protocol is not offered by this target or not available on this host | `/` |
| `–` | `DISABLED` | switched off in configuration | `-` |

> **Not built:** the `PAUSED` card status. A session stopped by the user shows `STOPPED` in the header and footer, and its cards go `STALE`.

### 9.5 Colour

Identity colours are taken from the Okabe–Ito colour-blind-safe palette so the four series stay distinguishable under the common colour-vision deficiencies. **[R]** They are deliberately not the vendors' brand colours.

| Role | Truecolor | 256-colour | 16-colour | Monochrome |
|---|---|---|---|---|
| Cloudflare | `#E69F00` orange | 214 | yellow | marker `1` / dense line |
| Google | `#56B4E9` sky blue | 75 | bright blue | marker `2` |
| NextDNS | `#009E73` bluish green | 36 | cyan | marker `3` |
| Arelion | `#CC79A7` reddish purple | 175 | magenta | marker `4` |
| Controls (gateway, local DNS) | `#999999` grey | 246 | white dim | marker `g` / `d` |
| Warning badge | `#F0E442` on default background | 227 | bright yellow, bold | bold |
| Failure badge | `#D55E00`, reverse video | 166 | red, reverse | reverse |
| Text, dim text, borders | terminal default, default + dim, 8-bit grey 240 | — | default / dim | default |

- **No green-means-good.** `OK` is rendered in the default foreground so health never collides with NextDNS's green.
- **Capability selection:** truecolor when `COLORTERM` is `truecolor`/`24bit`; else 256 when `TERM` advertises it; else 16; monochrome when `NO_COLOR` is set to a non-empty value or `--color never` is given (monochrome keeps bold, dim, and reverse — `NO_COLOR` is about colour only). With `TERM=dumb` the interactive commands refuse to start and point to `sample`. `--color` and the config override detection. Colours are downgraded by lookup in the table above, never by automatic nearest-match of a truecolor value.
- **Glyph modes:** `unicode` (default: braille charts, block sparklines, rounded borders), `ascii` (`--ascii`: `+-|` borders, `.:-=+*#` sparklines, `*` plot points, the ASCII status symbols above). No Nerd Font, image protocol, or particular terminal is required.
- **Monochrome and ASCII charts** cannot separate four overlaid lines by colour. In those modes the comparison chart switches to four stacked small charts sharing one y-scale, with the focused provider's taller. **[R]**
- **Alternative palettes:** `high-contrast` (bold primaries on black) and `mono`. Per-provider colour overrides in the configuration.

> **Not built:** the stacked small charts — the four series stay overlaid in one chart, and in the ASCII monochrome rendering each is plotted with its slot digit. Per-provider colour overrides have no effect (`providers.<key>.color` is accepted and ignored): a card's colour goes by its slot.

### 9.6 Detail views

`Enter` on a provider opens its detail view with tabs (`h`/`l` to switch):

| Tab | Content |
|---|---|
| Summary | Every stream of this provider across protocols and families with status, last, p50, p95, rates, n, coverage |
| Distribution | RTT histogram with fixed, labelled bin edges; window vs session; the full metric table from §5; all counters from §4.4 |
| DNS | Per-transport rows, fresh-connection phase breakdown, per-query scenario results, last transaction |
| Raw | Scrollable observation list: UTC time, seq, outcome, RTT, sched lag, flags (`late`, `dup`, `suspect`), destination, connection id; `/` filters, `Enter` shows every field |
| Route | §7 |
| Events | This provider's events and incidents with open/close times and recovery durations |
| Settings | Effective settings for this provider, capability results, backend names and versions, destination epochs |

A global **Settings/Capabilities** screen (`C`) shows the effective configuration with its sources, the platform capability probe results, and collector health (scheduling lag, loop lag, queue depth, drops).

> **As built:** the detail view has five tabs — Streams (the Summary tab above), Distribution, DNS, Raw, Events. Route is a screen of its own, opened with `t`, not a tab.
>
> **Not built:** the Settings tab and the global Settings/Capabilities screen. `pathwatch config show --effective` prints the effective configuration with its sources, `pathwatch doctor` the capability results, and the summary of `pathwatch sample` the collector's health counters. The Raw tab has no filter and no every-field view of one observation.

Design sketches of the DNS tab and the Route view, with illustrative data:

```text
╭─ 2 Google · details ──────────────────────────────────── Summary  Distribution  [DNS]  Raw  Route  Events  Settings ─╮
│ stream          destination               status       last   p50   p95  no-resp  err-resp   n  notes                │
│ DNS/UDP v4      8.8.8.8:53                ● OK         21.3  20.8  27.4   0.0 %    0.0 %    60  cache-influenced     │
│ DNS/TCP v4      8.8.8.8:53                ● OK         43.9  42.6  55.0   0.0 %    0.0 %    12  connect + query each │
│ DoT v4 reused   8.8.8.8:853 dns.google    ● OK         22.0  21.4  29.9   0.0 %    0.0 %    60  conn #2, 04:10 old   │
│ DoT v4 fresh    8.8.8.8:853 dns.google    ● OK         66.1    ·     ·      —        —       3  tcp 20 tls 24 q 22   │
│ DoH v4 reused   8.8.8.8:443 dns.google    ● OK         23.5  22.9  31.2   0.0 %    0.0 %    60  h2 · conn #1, 5 m old│
│ DNS/UDP v6      2001:4860:4860::8888      ! LOCAL ERR    —     —     —      —        —       0  no v6 route; not loss│
│                                                                                                                      │
│ Query scenarios — round-robin, identical order for every provider · RD=1 · EDNS 1232 · no ECS · no retries           │
│   . NS                  20 sent   20 NOERROR                     matches expectation (noerror)                       │
│   example.com. A        20 sent   20 NOERROR + answer            matches expectation (answer)                        │
│   wikipedia.org. AAAA   20 sent   19 NOERROR + answer, 1 SERVFAIL   error response 14:01:07, 48.2 ms — not a timeout │
│                                                                                                                      │
│ Last transaction  14:03:20.412Z  DNS/UDP v4  id 0x9c41  example.com. A  →  NOERROR, 1 answer, 56 B, 20.6 ms, TC=0    │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 h/l tab · j/k row · ⏎ inspect transaction · r raw observations · Esc back                           [ILLUSTRATIVE DATA]
```

```text
╭─ Route · 4 Arelion · ICMP v4 → 198.51.100.7 ───────────────── trace #3 · round 7 of 10 · 42 of 300 probes · running ─╮
│ ttl  address          name (reverse DNS, async)        sent  repl  no-reply    last    avg   best  worst   note      │
│  1   192.0.2.1        gateway                             7     7      0 %      1.2    1.3    1.0    2.1             │
│  2   —                —                                   7     0    100 %       —      —      —      —    a         │
│  3   198.51.100.1     (lookup pending)                    7     7      0 %      8.9    9.4    8.1   12.0             │
│  4   198.51.100.14    core1.example.net                   7     4     43 %     10.2   31.7    9.8   96.4   b         │
│  5   203.0.113.2      edge3.example.net                   7     7      0 %     11.0   11.3   10.6   12.4             │
│      203.0.113.6      edge4.example.net                   ·     ·       ·        ·      ·      ·      ·    c         │
│  6   198.51.100.7     target                              7     7      0 %     30.8   31.0   29.9   33.5             │
│                                                                                                                      │
│ a  Hop 2 never answers but hops 3–6 do: this router does not send ICMP Time Exceeded. Not evidence of loss.          │
│ b  Hop 4 answers 4 of 7 while later hops answer 7 of 7: consistent with ICMP rate limiting, not forwarding loss.     │
│ c  Two addresses seen at TTL 5 (4 and 3 rounds): per-flow load balancing (ECMP) or a route change. Hop RTTs are not  │
│    additive — each is a separate round trip with its own return path; do not subtract adjacent rows.                 │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 t start/stop · j/k hop · ⏎ hop probes · n names · Esc back   ICMP trace only; DNS may take another path  [ILLUSTRATIVE]
```

### 9.7 Truthful charts

| Rule | Behaviour |
|---|---|
| Missing is not zero | A probe without an RTT draws nothing in the latency chart. The line is broken there. No interpolation across a hole, no carry-forward of the previous value |
| Distinct absences | Timeout, unreachable, error response, local error, pending, unsupported, and gap each have their own timeline glyph and are never merged into one "missing" look |
| Common scale by default | All four series share the y-axis. `z` cycles common → fit-to-visible-data → log; any non-default scale shows a `ZOOM`/`LOG` tag in the chart title, and `independent` scales (detail view only) label each axis |
| Downsampling keeps the truth | When a column covers several probes, the column draws the **min–max span** of its RTTs (so a single spike survives) with the median emphasised; the timeline cell shows the worst outcome with priority no-reply > unreachable > error > local error > gap > replied; counts are kept per column and shown on inspection |
| Gap boundaries are exact | A column that is partly gap is drawn as gap only if no probe was sent in it; otherwise it draws its probes. Gap start/end times come from the gap record, not from column edges |
| Quantiles are recomputed | Window statistics always come from raw samples (§5.1), never from per-column or per-bucket summaries |
| Like with like | The comparison chart holds one protocol and one family. Switching mode replaces the series; it never overlays ICMP and DNS as if equivalent. The detail view can show phases of one transaction stacked, each labelled |
| Coverage is visible | Each card shows coverage; the timeline shows where the missing coverage is |
| Epoch changes | A destination change draws a vertical marker on that series and a `◆` in its timeline row |

> **As built:** the timeline has cells for replied, some unanswered, none answered, error response, local error, skipped, and gap. Timeouts and unreachables share the "unanswered" cells; pending probes have no cell, and an unsupported or unconfigured row says so in words.
>
> **Not built:** the scale cycle (`z`), with its fit, log and independent scales — the common scale of §9.3 is the only one. The epoch marker and the `◆` timeline cell (§2).

### 9.8 Motion

| Trigger | Effect | Duration |
|---|---|---|
| Probe sent | the card's status dot dims one step | until reply or deadline |
| Reply received | dot returns to full brightness; the last-RTT digits brighten | 150 ms |
| Value changes by more than 20 % of p50 | the changed number is emphasised (bold) | 300 ms |
| Incident opens or closes | the card border uses the badge colour | 2 frames on, then steady badge only |
| New column enters the chart | the plot scrolls by one cell | one frame |

No spinners, no idle animation, no flashing. With `--reduced-motion` (or `reduced_motion = true`) every effect above is replaced by its end state, and the frame rate drops to on-change rendering capped at 4 fps.

> **Not built:** every effect in the table. There is no animation: the screen is redrawn when its data changes, at most ten times a second. `--reduced-motion` limits redraws for new data to four a second; keys are still answered at once.

---

## 10. Interaction

### 10.1 Keyboard map

The same key means the same thing on every screen. Keys that would be destructive or surprising are capitals.

| Key | Action | Where |
|---|---|---|
| `q`, `Ctrl-C` | quit cleanly (restores the terminal) | everywhere |
| `?` | help overlay listing every binding for the current screen | everywhere |
| `Tab` / `Shift-Tab` | move focus to the next / previous panel | everywhere |
| `1` `2` `3` `4` | focus that provider (`g`, `d` focus the gateway / local-DNS controls when enabled) | everywhere |
| `h` `l`, `←` `→` | previous / next provider in the overview; previous / next tab in details; move the cursor when cursor mode is on | — |
| `j` `k`, `↓` `↑` | move the selection in the focused list or table | lists |
| `Ctrl-d` / `Ctrl-u`, `G` / `gg` | half-page down / up, bottom / top | lists |
| `Enter` | open details for the focused item | everywhere |
| `Esc` | go back one level; leaves cursor mode and filters first | everywhere |
| `m` / `M` | next / previous **measurement mode**: ICMP → DNS (UDP) → encrypted DNS (DoH/DoT) | overview, chart |
| `f` | toggle address family IPv4 ↔ IPv6 | overview, chart |
| `+` (`=`) / `-` | longer / shorter time window: 1 m · 5 m · 15 m · 1 h · 6 h · 24 h | everywhere |
| `Space` | **freeze / unfreeze the view** (collection keeps running) | everywhere |
| `c` | cursor mode on the chart: a crosshair, moved with `h`/`l` (`H`/`L` by 10 columns), showing exact values and counts for every series in that column | chart |
| `x` / `X` | hide or show the focused series / show only the focused series (press again to restore all) | chart |
| `z` | cycle y-scale: common · fit · log | chart |
| `a` | overlay all four series ↔ focused only | compact layout |
| `/` | filter the focused list (outcome, text); `Esc` clears | lists |
| `e` | events screen | everywhere |
| `r` | raw observations for the focused stream | everywhere |
| `t` | route view for the focused provider; `Enter` there starts a trace, `t` stops it | everywhere |
| `C` | settings and capabilities | everywhere |
| `P` | **stop / resume collection** (asks for confirmation; creates a history gap) | everywhere |
| `R` | reload configuration | everywhere |
| `Ctrl-L` | full redraw | everywhere |
| `Ctrl-Z` | suspend to the shell (terminal restored; collection gap on resume) | everywhere |

Bindings are configurable; the help overlay and footer are generated from the active map, so they cannot drift.

> **Not built:** these keys are not bound — `x` / `X` (hide, solo), `z` (scale cycle), `/` (filter), `e` (events screen; events are a tab of the detail view), `r`, `C`, `g` / `d` (control focus), `Ctrl-d` / `Ctrl-u`. Key bindings are not configurable: `[ui.keys]` is accepted and has no effect.
>
> **As built:** top of a list is a single `g`, not `gg`; `PageUp` / `PageDown` scroll ten rows. `Tab` / `Shift-Tab` act as `l` / `h` — there is no separate panel focus. The modes cycle ICMP → DNS/UDP → DoH → DoT. A second `?` turns the help overlay into the glossary.

### 10.2 Freeze is not stop

- **Freeze** (`Space`) pins the displayed window to the moment of freezing. The header and footer show `FROZEN 00:37 · collection continues` in the warning style, with the count of new observations since. Unfreezing jumps back to live. Nothing is lost.
- **Stop collection** (`P`) is a separate, confirmed action. Probes stop, the header shows `PAUSED`, and the interval is recorded as a `gap(stopped)`.

> **As built:** the header shows `STOPPED`, and the cards go `STALE` (§9.4).

### 10.3 Inspection

- **Selected sample:** in cursor mode, `Enter` opens the list of probes in the column under the crosshair, and `Enter` again shows one observation with every field of the observation schema ([architecture.md §6](architecture.md#6-observation-schema)).
- **Timeline cells** are inspectable the same way.
- **Raw values** are always one step from any aggregate: every statistic's detail row shows its n and the window bounds.
- **Mouse** is optional and **off by default**, because capturing the mouse disables the terminal's native text selection. With `--mouse`: click a card or legend entry to focus/toggle, wheel to scroll lists or change the window over the chart, click the chart to place the cursor. Every mouse action has a keyboard equivalent.

> **Not built:** inspection of a selected sample or a timeline cell. The cursor shows the values of its column in the footer; single observations are listed in the Raw tab.

---

## 11. CLI contract

```text
pathwatch                         interactive dashboard (same as `pathwatch run`)
pathwatch run        [opts]       interactive dashboard; requires a TTY on stdout
pathwatch sample     [opts]       headless collection; observations as JSONL on stdout or to a file
pathwatch doctor     [--json]     local capability diagnosis; opens sockets, sends nothing unless --probe
pathwatch config check            validate the configuration; no network activity
pathwatch config show [--effective] [--format toml|json] [--show-secrets]
pathwatch config path             print the configuration file path in use
pathwatch config example          print a commented example configuration
pathwatch export     <file>       convert a recording: --format csv|jsonl|summary, --redact (default on)
pathwatch replay     <file>       play a recording through the dashboard: --speed 1x|10x|max
pathwatch demo       [scenario]   dashboard on built-in synthetic scenarios; no network activity
pathwatch --version               version, build features, and measurement backends
```

Common options: `--config <path>`, `--mode icmp|dns|doh|dot`, `--family 4|6`, `--window <dur>`, `--color auto|always|never`, `--ascii`, `--reduced-motion`, `--mouse`, `--record <path>`, `--log-file <path>`, `--log-level`.
`sample` options: `--duration <dur>` or `--count <n>`, `--format jsonl|summary`, `--output <path>`, and `--assert '<expr>'`.

> **As built:**
> - `run` has no `--mode`, `--family` or `--window`, and `general.default_mode`, `default_family` and `default_window` have no effect: the dashboard opens on ICMP, IPv4 and five minutes. `sample` does take `--mode`, `--family` and `--window`.
> - `--record` takes a directory; a session is written as JSONL segments under it.
> - `replay --speed` takes a number, a multiple of real time (there is no `max`); without it the whole session is shown at once.
> - `export` has `--redact-local` (also remove the host's own addresses, interface names, gateway and resolver) and `--no-redact` (leave the records as recorded) in place of `--redact`.
> - Added beyond the design: `pathwatch glossary`, `pathwatch man`, `pathwatch config reference`, `sample --format plain`, and `--theme`. `sample --assert` is built.
>
> **Not built:** `--log-file` and `--log-level`, and any log file: `[logging]` is accepted and has no effect. What a session has to say is in its events and, with `--record`, in its recording.

| Topic | Behaviour |
|---|---|
| TTY detection | `run`, `replay`, `demo` require a terminal on stdout and stdin. Without one they exit with code 2 and a one-line message suggesting `sample`. They never write escape sequences into a pipe |
| stdout / stderr | stdout carries only the data asked for (JSONL, CSV, summary, config). Diagnostics and logs go to stderr or the log file. In interactive mode logs go to the log file only, never onto the screen |
| Plain text | `sample --format summary` prints a fixed-width table per stream with the §5 metrics; colour only if stdout is a TTY and colour is enabled |
| Piping | `sample` flushes one JSON object per line; safe for `jq`, `tee`, `head` |
| Broken pipe | `EPIPE` on stdout ends collection quietly: flush nothing further, exit 0 |
| `SIGINT` | interactive: clean quit, exit 0. `sample`: finalise pending probes as `cancelled`, print the summary if requested, exit 130 |
| `SIGTERM`, `SIGHUP` (terminal closed) | orderly shutdown: stop probes, flush recorder, restore terminal; exit 143 / 129. `SIGHUP` with a controlling terminal still present reloads configuration |
| `SIGTSTP` / `SIGCONT` | restore the terminal before stopping; on continue, re-enter the alternate screen, redraw, record a `gap(process_stopped)` |
| `SIGWINCH` | re-layout on the next frame; resize events are coalesced; layout class changes preserve focus and window |
| Panic / fatal error | terminal is restored first (raw mode off, alternate screen left, cursor shown, mouse capture off), then the error is printed to stderr |
| Terminal disconnect | write errors on the terminal trigger orderly shutdown; a recording in progress is closed cleanly |

**Exit codes.** A successful exit says the *process* worked. It says nothing about the network.

| Code | Meaning |
|---|---|
| 0 | ran and ended normally — regardless of what the network did |
| 1 | unexpected internal error |
| 2 | usage error (bad flags, no TTY for an interactive command) |
| 3 | configuration invalid |
| 4 | no stream could be started (every configured probe is unsupported or denied on this host) |
| 5 | I/O error on a requested output (recording, export) |
| 10 | a `sample --assert` condition was false — the opt-in way to turn network state into an exit status |
| 129 / 130 / 143 | ended by `SIGHUP` / `SIGINT` / `SIGTERM` in headless mode |

> **As built:** exit status 129 is never produced. In `sample` a hang-up reloads the configuration; in the dashboard it reloads while the terminal is still there and shuts down in order once it is gone. A panic exits with status 101, after the terminal has been restored.

---

## 12. Configuration and reliability

### 12.1 Format and location

**TOML.** **[R]** It is comment-friendly, has unambiguous types for durations-as-strings and arrays of tables, is the native format of the Rust ecosystem, and is diff-friendly for a file that is edited by hand. YAML's implicit typing and JSON's lack of comments both make worse hand-edited config.

Path: `$XDG_CONFIG_HOME/pathwatch/config.toml`, defaulting to `~/.config/pathwatch/config.toml` on both macOS and Linux. `--config` and `PATHWATCH_CONFIG` override.

A complete, commented example file is [pathwatch.example.toml](../../crates/pathwatch-core/src/pathwatch.example.toml); `pathwatch config example` prints it. It ships with NextDNS and Arelion **disabled with explicit placeholders**; the application starts with those two cards in `NEEDS CONFIG`.

It covers: providers and targets, per-protocol settings, family selection, source binding, intervals and deadlines, history, incident thresholds, theme and glyph mode, key bindings, recording, redaction, and logging. Every setting, its default, and whether a reload applies it is in [configuration.md](../configuration.md).

> **As built:** some settings are accepted and change nothing — among them source binding, key bindings and logging. Each is marked in [configuration.md](../configuration.md), and setting one to something other than its default prints a warning.

### 12.2 Precedence and validation

1. Built-in defaults
2. Configuration file
3. Environment (`PATHWATCH_CONFIG`, `NO_COLOR`, `PATHWATCH_NEXTDNS_PROFILE` and other `*_env` secrets the file points at)
4. Command-line flags

- Unknown keys are errors (typos must not silently become defaults).
- Cross-field checks: deadline < interval for non-overlapping transports; aggregate traffic within the global budget (§13); every stream's protocol is one its target role allows; placeholders (`"<…>"`) are rejected in enabled targets with a message naming the key.
- `config show --effective` prints every setting with its **source** (default / file / env / flag) and the resulting stream list. Secrets are masked unless `--show-secrets`.
- A config file readable by group or others while containing a secret produces a warning.

### 12.3 Changing configuration

`R` in the dashboard, or `SIGHUP`, reloads. The new file is parsed and validated in full before anything changes. On failure the running configuration is kept untouched and an event shows the error. On success the config **revision** increments, a `config_reloaded` event is recorded, unchanged streams keep their history, and changed streams are cancelled and restarted as new streams. Every observation carries the revision it was taken under.

> **As built:** not every setting can be applied to a running session; [configuration.md](../configuration.md) says which. A reload that contains any change needing a restart is refused whole, and nothing is changed.

### 12.4 Degraded operation

| Situation | Behaviour |
|---|---|
| Unprivileged ICMP not permitted on this host | ICMP streams show `UNSUPPORTED` with the exact remedy the OS documents (displayed, never executed); DNS streams run normally; exit code is 0 unless *nothing* can run |
| IPv6 has no route or address | v6 streams show `LOCAL ERR (no IPv6 route)`; not counted as loss; re-checked on network change |
| Interface or default route changes, VPN up/down | pending probes cancelled, short gap, new source context event, long-lived DoT/DoH connections re-established and marked `after_reconnect` |
| Bootstrap resolution fails | the stream shows `LOCAL ERR (bootstrap)` with the resolver error; last known address is **not** silently reused past its refresh deadline unless `bootstrap.on_failure = "keep"` |
| Permission denied on a socket option (interface binding) | that stream is `UNSUPPORTED (binding)`; the application does not fall back to an unbound socket, because that would measure a different path |
| Invalid configuration at start | exit 3 with every problem listed, not just the first |
| Wall-clock step | `clock_step` event; durations unaffected |
| Target flapping | `flapping` incident (§8) |
| Terminal disconnect | orderly shutdown (§11) |
| Disk full or write error while recording | recording stops, an event and a header notice appear, measurement continues; see [architecture.md §7](architecture.md#7-persistence-recording-and-replay) |

The application never changes sysctls, file capabilities, setuid bits, firewall rules, DNS settings, or routes. It does not need to run as root, and `doctor` warns if it is.

> **Not built:** source and interface binding. The configuration rejects any `[source]` value other than empty, so every socket follows the operating system's routing. Raw-socket ICMP is not built either: ICMP uses the unprivileged datagram socket only (`icmp.backend = "raw"` is refused), so running as root does not make ICMP available where the host has it switched off.

### 12.5 Untrusted text

Reverse-DNS names, hostnames from configuration, TXT/CNAME data, TLS certificate names, HTTP headers, and any subprocess output are untrusted. Before display or logging they are length-limited and stripped of C0/C1 control characters, escape sequences, and bidirectional override characters; non-printable bytes are shown as `\xNN`. Parsers have explicit size limits (DNS message ≤ 65,535 bytes, HTTP response body ≤ 64 KiB, hop name ≤ 255 bytes). Logs are written to a file, never to the rendered screen, and pass through the same redaction as exports.

> **As built:** there is no log file (§11). Text read back from a recording is treated as untrusted as well and cleaned the same way, so a recording from someone else can be replayed and exported safely. Recordings are created readable by their owner only, and retention (`recording.max_total`) deletes only the program's own segments.

---

## 13. Defaults and traffic budget

| Setting | Default | Justification |
|---|---|---|
| ICMP interval | 1 s per stream | Matches `ping`; fine enough to see sub-minute events, coarse enough to be a polite neighbour |
| ICMP payload | 56 bytes (64-byte ICMP message) | Same as `ping`, so numbers are comparable |
| ICMP deadline | 2000 ms | Leaves room for slow paths without holding results back long; replies after it are recorded as late |
| DNS interval | 5 s per stream | A resolver health signal, not a load test |
| DNS deadline | 3000 ms | Below the interval, so a stream never overlaps itself |
| Fresh DoT/DoH connection | every 5 min | Enough to track handshake cost without churning connections |
| Retries | 0 everywhere | First-attempt truth |
| Render rate | up to 15 fps while something is animating; on-change (≤ 4 fps) when idle, over SSH (`SSH_CONNECTION` set), or with reduced motion | Collection cadence is independent of this |
| Tracing | off until requested | §7 |
| Window | 5 min | |
| History | compact samples 24 h per stream; full observation records 1 h per stream | [architecture.md §4.4](architecture.md#44-history-and-memory-bounds) |

> **As built:** the screen is redrawn when its data changes, at a fixed ten times a second at most, with no slower rate over SSH; with reduced motion, four times a second for new data. `ui.max_fps` and `ui.idle_fps` have no effect.

**Aggregate traffic with everything enabled** — four providers, both families, both controls, no retries. Computed from packet sizes; byte counts are IP-layer and exclude link framing. DoT/DoH figures are estimates to confirm **[T]**.

| Source | Streams | Rate | Outbound size | Outbound load |
|---|---|---|---|---|
| ICMP echo | 4 × 2 + gateway v4 = 9 | 9 packets/s | 84 B (IPv4), 104 B (IPv6) | ≈ 0.84 kB/s |
| DNS/UDP | 4 × 2 + local DNS = 9 | 1.8 queries/s | 56–70 B (IPv4), 76–90 B (IPv6) — measured for the default queries | ≈ 0.13 kB/s |
| DoT or DoH, reused | up to 8 | 1.6 requests/s | ≈ 150–350 B | ≈ 0.25–0.55 kB/s **[T]** |
| DoT/DoH fresh handshakes | up to 8 | one per stream per 5 min | ≈ 3–6 kB per handshake, both directions **[T]** | ≈ 0.1–0.2 kB/s average |
| DNS/TCP (off by default) | up to 9 | one per stream per 60 s | ≈ 6–8 small segments | negligible |
| **Steady state** | | **≈ 12–13 transactions/s** | | **≈ 1.4–1.8 kB/s out** (≈ 11–14 kbit/s), similar inbound |
| Route trace (on demand) | 1 | ≤ 30 probes/s, ≤ 300 probes per trace | 84 B | ≤ 2.5 kB/s for ≤ 10 s bursts |

Per destination this is one echo request per second per family — 86,400 per day — the same as leaving `ping` running.

**Global limits** (enforced by the scheduler, visible in the Capabilities screen): 25 probe transactions per second sustained with a burst of 40; at most 128 probes in flight; at most 16 concurrent DNS transactions; one trace. A configuration whose computed steady-state rate exceeds the sustained limit fails validation; at run time a slot that would exceed the limit becomes `skipped(rate_limit)` and is counted as a collector condition, never as network loss.

> **As built:** DNS transactions are not limited separately (`limits.max_concurrent_dns` has no effect), and with no Capabilities screen (§9.6) the limits are shown by `pathwatch config show --effective`.

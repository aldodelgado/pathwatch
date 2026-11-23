# 03 · Implementation plan

**Status:** draft for review · 2026-11-21. **No development has started.** Nothing in this plan has been scaffolded, built, installed, or run. Every test below is described, not executed. Claim labels **[V] [R] [A] [T]** are defined in [01](01-design-spec.md).

---

## 1. Shape of the plan

| # | Milestone | Outcome | Depends on | Effort (focused days) |
|---|---|---|---|---|
| 0 | Research and design package | these documents, reviewed | — | done, pending your review |
| 1 | Measurement foundation | a headless collector that produces trustworthy, structured observations for ICMP, DNS/UDP, and your NextDNS transport | your explicit go-ahead; Q1–Q4 are answered ([04 §3.1](04-references-and-open-questions.md#31-answers-received-on-2026-11-21)); the Arelion addresses, NextDNS profile id, and Linux release versions are needed by task 1.12 | 9–14 |
| 2 | Polished dashboard | the overview, details, three layouts, keyboard interaction | 1 | 8–12 |
| 3 | Diagnostic expansion | the remaining encrypted transport, route inspection, controls, correlation, recording/replay, reload | 2 | 9–14 |
| 4 | Hardening and distribution | accessibility, soak and performance validation, documentation, release artefacts | 3 | 6–10 |

Effort assumes one experienced Rust developer working with an AI coding agent, counted in focused days, excluding review turnaround. Ranges reflect the uncertainty named in each milestone's risks; they are estimates, not commitments.

**Order adjustment.** The request suggested encrypted DNS in milestone 3. NextDNS's hostname-steered network is reachable only over DoH, DoT, or DoQ **[V]**, and plain DNS to NextDNS needs a linked IP or per-profile address. A NextDNS card without an encrypted transport would therefore be either empty or measuring a path you may not use. So **DoH — the transport you confirmed for NextDNS on 2026-11-21 — moves into milestone 1**. DoT follows in milestone 3.

**First useful release = milestones 1 + 2:** ICMP, DNS/UDP with TCP fallback, one encrypted transport, the full overview, detail views, three layouts, demo mode, headless JSONL sampling. Restart to change configuration.

**Later:** route inspection, second encrypted transport, gateway and local-DNS controls, incident correlation, recording/export/replay, live reload, mouse, packaged releases.

---

## 2. Milestone 1 — Measurement foundation

Specific enough to start on approval. Task ids are stable references for review.

| Id | Task | Deliverable | Notes |
|---|---|---|---|
| 1.0 | Workspace scaffold | three-crate workspace ([02 §4.1](02-architecture.md#41-modules-and-data-flow)), lockfile, CI on macOS arm64 and Linux x86_64: format, lint with warnings denied, tests, licence/advisory check, single-Crossterm-major check | first commit; nothing else lands without CI |
| 1.1 | Model and schema | id types, `Observation`, outcome and error enums, `Event`, `Gap`; JSONL serialisation; schema v1 golden files | [02 §6](02-architecture.md#6-observation-schema) |
| 1.2 | Configuration | TOML types with unknown-key rejection; cross-field validation reporting every problem; effective settings with provenance; placeholder rejection; secret masking; `config check`, `config show`, `config path`, `config example` | the example file in `planning/examples/` becomes a test fixture |
| 1.3 | Clocks and gaps | `Clock` abstraction (monotonic, wall, suspend-inclusive); platform clock pair; gap detector; wall-clock step detection | [01 §4.5](01-design-spec.md#45-observation-gaps) |
| 1.4 | Capability detection and `doctor` | per-family socket probes, sysctl and group read-out on Linux, binding checks, backend selection, human and JSON output | sends nothing unless `--probe` |
| 1.5 | Scheduler | slot grid, phase stagger, explicit skip accounting, token bucket, in-flight cap, no-overlap rule | [02 §4.3](02-architecture.md#43-scheduling) |
| 1.6 | ICMP engine | echo build/parse and checksum; one socket and receiver per family; pending table keyed by probe id; deadline queue; duplicate, late, and unmatched handling; Linux error queue and TTL control messages; macOS IP-header parsing and strict filtering | the highest-risk task; begins with a two-platform spike |
| 1.7 | DNS over UDP with TCP fallback | codec via `hickory-proto`; connected UDP socket per query; four-layer result; scenario expectations; classification table; truncation fallback as a linked attempt | [01 §6](01-design-spec.md#6-dns-measurement) |
| 1.8 | DoH (your NextDNS transport) | bootstrap resolution and destination epochs; own dial → TLS with separate server name → HTTP/2 connection; reused and fresh cohorts; reconnect tagging; redaction of the profile id | also serves the Cloudflare and Google DoH streams |
| 1.9 | Processor | lifecycle state machine, compact and full rings, window statistics, nearest-rank percentiles with minimum-n rules, RTT variation with adjacency rules, counter identity | [01 §4–5](01-design-spec.md#4-measurement-semantics) |
| 1.10 | Headless `sample` | JSONL and summary formats, duration/count limits, signal handling, broken-pipe behaviour, exit codes | [01 §11](01-design-spec.md#11-cli-contract) |
| 1.11 | Scripted source and fixtures | a deterministic observation source driven by scenario scripts and a seeded generator; the scenario set in §4.3 | becomes `replay`/`demo` later; used by every test from here on |
| 1.12 | Target validation with you | with your consent and on your network: run `doctor`, then a short `sample` against your actual NextDNS endpoint and your chosen Arelion target; record what each answers in this package | the first time anything touches the network; closes the "outstanding reachability/capability" items in [01 §3](01-design-spec.md#3-providers-targets-and-capabilities) |

**Acceptance criteria**

1. With a scripted source and a paused clock, the same scenario produces byte-identical JSONL and identical statistics on every run and on both platforms.
2. For every stream and window in every fixture, `sent = pending + replied + timed_out + unreachable + service_error + protocol_error + cancelled` and `slots = sent + local_error + skipped`.
3. A reply 1 µs before the deadline is `success`; at or after the deadline it is `timeout` with `late_rtt`; a second reply is a `duplicate`; replies arriving in reverse order are matched to the right probes; a sequence crossing 65535 → 0 matches correctly.
4. No fixture produces an RTT of 0 or of exactly the deadline for a probe without a reply; `rtt_us` is absent in those records.
5. A simulated 10-minute suspend produces one gap record, zero probes sent or "missed" in the gap, no burst afterwards, and unchanged no-reply rates.
6. With unprivileged ICMP denied (simulated and, on a real Linux host with the sysctl off, actual), ICMP streams report `unsupported`, DNS streams run, the process exits 0, and no rate includes those streams.
7. `SERVFAIL`, `REFUSED`, `NXDOMAIN`, empty answers, and blocked answers from the local fixture server are classified per the table in [01 §6.3](01-design-spec.md#63-classification); none is a timeout.
8. On loopback (`127.0.0.1`, `::1`) on both platforms, ICMP echo and DNS/UDP run unprivileged and report plausible sub-millisecond times; an unroutable family reports `local_error`.
9. The encrypted transport connects to a local TLS fixture by IP with a separate server name, fails with `protocol_error (certificate)` on a name mismatch, and reports connect, handshake, and request phases separately; reused and fresh statistics never mix.
10. A stream whose target never answers does not delay any other stream's sends (scheduling lag of the others stays within budget in the fixture).
11. Configuration with a placeholder in an enabled target, a deadline ≥ interval, an unknown key, or a rate above the global limit is rejected with a message naming the key; `config show --effective` masks the profile id.
12. Measured aggregate traffic in a 60-second loopback run is within ±10 % of the computed budget in [01 §13](01-design-spec.md#13-defaults-and-traffic-budget).

**Risks**

| Risk | Likelihood | Effect | Response |
|---|---|---|---|
| macOS datagram ICMP delivers a shape the plan did not anticipate (IPv6 in particular was traced less thoroughly in source) | medium | rework in 1.6 | two-day spike first; packet-byte fixtures from real hosts |
| Your Ubuntu hosts have unprivileged ICMP disabled (Ubuntu does not enable it **[V]**; Debian 13 does; Debian 12 unconfirmed) | high on Ubuntu | ICMP cards `UNSUPPORTED` there | `doctor` explains; you decide whether to change the sysctl; raw backend remains opt-in |
| DoH phase timing is distorted by certificate verification inside the handshake | medium | handshake numbers larger than expected on macOS | record verification as part of the handshake and say so; evaluate a webpki-only verifier **[T]** |
| Your "Yes" to Q1 meant something other than DoH | low | 1.8 retargets (DoT, or plain DNS with linked IP) | recorded reading is in 04 §3.1 for you to correct |
| Hand-written `unsafe` control-message parsing has a bug | low–medium | wrong TTL or missed errors | confined module, fuzz-style tests on parsers, Miri where applicable |

---

## 3. Milestones 2–4

### Milestone 2 — Polished dashboard

| Id | Task |
|---|---|
| 2.1 | Terminal lifecycle: init/restore on every exit path, panic hook order, resize, suspend/resume, TTY checks |
| 2.2 | Capability and theme layer: colour depth detection, the palette table at every depth, monochrome owned by the application, Unicode/ASCII glyph sets, reduced motion |
| 2.3 | Layout engine: three size classes evaluated per axis; "terminal too small" screen |
| 2.4 | Custom widgets: latency plot (braille, gaps, min–max columns, focus, cursor, epoch markers; small multiples in monochrome/ASCII), outcome timeline, histogram, provider card, compact provider table |
| 2.5 | Overview screen: header, cards, chart, timeline, events, footer |
| 2.6 | Detail screens: Summary, Distribution, DNS, Raw, Events, Settings; global Capabilities screen |
| 2.7 | Input: key map from configuration, generated help and footer, focus model, mode/family/window switching, freeze, cursor and sample inspection, series hide/solo, scale cycling, list filtering |
| 2.8 | Stream status and incidents: the four single-stream rules with hysteresis and cooldown; status vocabulary; `stale` detection |
| 2.9 | Snapshot pipeline: view spec → pure snapshot; render-on-change with animation frames |
| 2.10 | `demo` command on the milestone-1 scenarios, for design review without network activity |
| 2.11 | UI tests: buffer unit tests per widget; text snapshots at 160 × 45, 120 × 35, 80 × 24 in each colour and glyph mode; style assertions on cells; pseudo-terminal tests for cleanup |

**Acceptance criteria**

1. Feeding the same observations at render rates of 1, 15, and 60 fps and with rendering disabled yields identical snapshot statistics.
2. In every layout, a reviewer shown a screenshot can name the active protocol and family, the failing target, and any stale value within five seconds; verified by you on the demo scenarios.
3. `pending`, `timeout`, `local_error`, `unsupported`, `needs_config`, and gap are visibly different in the cards and timeline in truecolor, 256-colour, 16-colour, monochrome, and ASCII snapshots.
4. Unconfigured NextDNS and Arelion cards show `NEEDS CONFIG`, display no percentage, and the process sends nothing to any address not in the configuration (asserted by the fixture transport).
5. The chart never draws a segment across a probe without an RTT or across a gap (asserted on buffer cells for the gap fixtures); a single-sample spike survives downsampling at every width.
6. After quit, `SIGINT`, `SIGTERM`, a forced panic, and terminal close, the pseudo-terminal shows raw mode off, the main screen restored, and the cursor visible.
7. Resizing through all three size classes and below the minimum never panics, never clips content silently, and preserves focus, window, and freeze state.
8. `Space` freezes the view while the collector's counters keep increasing, and the screen says so; `P` requires confirmation and records a gap.
9. With `--reduced-motion`, consecutive frames differ only when data differ.

**Risks:** braille and block glyph rendering varies by font and emulator (mitigation: ASCII mode, your terminals named in Q6, manual check list); Ratatui 0.31 lands mid-milestone (mitigation: stay on the lockfile, upgrade as a separate task); visual polish is open-ended (mitigation: the sketches in [01 §9](01-design-spec.md#9-visual-design) are the scope; changes go through you).

### Milestone 3 — Diagnostic expansion

| Id | Task |
|---|---|
| 3.1 | Second encrypted transport (DoT if DoH shipped in milestone 1), and the optional DNS/TCP stream |
| 3.2 | Route inspection: own ICMP tracer (option A), Route view, annotations, budget enforcement, asynchronous reverse DNS with sanitising |
| 3.3 | `trippy-core` spike (time-boxed to two days): adapter feasibility, bounded-rounds cancellation, Linux privilege behaviour → keep or drop |
| 3.4 | Source context and network-change handling: default interface and gateway discovery, change events, pending-probe cancellation, reconnects |
| 3.5 | Controls: gateway ICMP and local-DNS rows; system resolver discovery including the loopback-stub case |
| 3.6 | Incident correlation across streams with observation-cited wording |
| 3.7 | Recording, `export`, `replay`: segment format, rotation, caps, disk-full behaviour, redaction, recovery of truncated files |
| 3.8 | Configuration reload with revisions (`R`, `SIGHUP`) |
| 3.9 | Optional mouse support |

**Acceptance criteria**

1. A trace never exceeds its probe or rate cap (asserted by counting in the fixture transport), stops within one round of `t`, and only one trace runs at a time.
2. In the trace fixtures — silent hop, rate-limited hop, persistent loss from hop *k*, two addresses at one TTL — each annotation appears exactly where [01 §7](01-design-spec.md#7-route-diagnostics) specifies and no annotation asserts a cause.
3. Replaying a recording of a scripted session reproduces the live session's final snapshot statistics exactly; replay incidents never appear in a live session's event list.
4. A simulated disk-full stops recording, raises the notice, and loses no measurement; a recording truncated mid-line replays up to the last complete record with a warning.
5. An export with redaction on contains no occurrence of the profile id in any field, URL, or server name (asserted by search over the output).
6. A reload with an invalid file changes nothing and shows the error; a valid reload increments the revision, preserves history of unchanged streams, and tags subsequent observations with the new revision.
7. An interface change in the fixture cancels pending probes, records one gap and one source-context event, and produces no incident from the cancelled probes.
8. A correlated-failure fixture yields one grouped event listing affected and unaffected comparable streams and the control results; its text contains counts and no attribution.

**Risks:** `trippy-core`'s unstable API and Linux privilege requirement (mitigation: optional, behind a feature, default is our own tracer); network-change notification crate maturity (mitigation: periodic re-snapshot fallback); recording volume (mitigation: caps by default, compression later).

### Milestone 4 — Hardening and distribution

| Id | Task |
|---|---|
| 4.1 | Accessibility pass: high-contrast and monochrome themes, ASCII mode, reduced motion, colour-independent status, a plain-text `sample --format summary` usable with a screen reader, help text for every metric |
| 4.2 | Performance validation against §5 on both reference environments; fix or re-baseline with evidence |
| 4.3 | 24-hour soak with bounded-history assertions; 7-day soak as supplementary evidence. *Since the growth budget was restated (§5), showing it needs the soak to run past hour 25 — thirty hours, or the 7-day one.* |
| 4.4 | Documentation: README, measurement glossary generated from the metric dictionary, capability and privilege guide, configuration reference, man page |
| 4.5 | Release pipeline: tagged builds, artefacts per target, SHA-256 checksums, release notes; evaluate `dist` against a hand-written workflow |
| 4.6 | Installation: `cargo install`, tarballs from releases, a Homebrew tap; install test on a clean machine per target |
| 4.7 | `sample --assert` for scripted health checks (exit code 10) |

**Acceptance criteria:** every budget in §5 met or explicitly re-baselined with the measurement attached; resident memory growing by no more than 2 MB after hour 25 (restated on 2026-11-22; it read "after hour 24 within 2 MB of hour 1"); each published artefact installs and starts on a clean host of its target and its checksum verifies; the capability guide's statements match `doctor` output on each tested host.

**Risks:** static musl builds with the default TLS crypto provider are unverified (mitigation: glibc builds are the supported artefacts; musl is best-effort until proven); unsigned macOS binaries downloaded through a browser are quarantined by the OS **[A]** (mitigation: Homebrew and `cargo install` paths; signing is a later decision).

---

## 4. Verification strategy

### 4.1 Deterministic core tests

Run on every change, no network, paused clock, scripted transport.

| Area | Cases |
|---|---|
| Statistics | nearest-rank percentiles at n = 1, 4, 5, 19, 20, 99, 100; minimum-n gating; mean/min/max; sample standard deviation; property test: percentiles equal those of a naive sort for random inputs |
| Windows and cohorts | probe sent 1 µs inside vs outside the window edge; reply arriving after its send has left the window; pending probes excluded; epoch change mid-window; window longer than the session (coverage < 100 %) |
| Deadline edges | reply at deadline − 1 µs, at the deadline, at deadline + 1 µs; late reply within and beyond the late window |
| Matching | duplicate replies; out-of-order completion; sequence wraparound; foreign packets; wrong nonce; wrong source address |
| Outcomes | every outcome in [01 §4.3](01-design-spec.md#43-probe-outcomes) reachable from a fixture; counter identity holds after each |
| RTT variation | adjacency broken by timeout, skip, gap, and epoch change |
| Retries | retries off: transaction = attempt; retries on: attempts linked, first-attempt rates unchanged |
| DNS | every RCODE class; truncation with and without fallback; ID mismatch; question mismatch; malformed message; blocked answers with and without a `blocked` expectation; EDE codes |
| Scheduler | stagger phases; late timer → explicit skips; no burst after a pause; token bucket and in-flight cap; no-overlap rule |
| Gaps | suspend/resume; process stop; collection stop; collector stall; wall-clock step without a gap |
| Incidents | open/close thresholds, minimum samples, hysteresis, cooldown, flapping; gaps do not open incidents |
| Backpressure | full observation channel → skipped slots and counter, no lost results for sent probes; full recorder queue → counted drop and gap record |

### 4.2 UI and terminal tests

- Widget unit tests directly against the cell buffer, including style assertions (snapshots capture text only **[V]**).
- Text snapshots of whole screens for each scenario at the three sizes × {truecolor, 256, 16, monochrome} × {Unicode, ASCII} × {motion, reduced motion}.
- Pseudo-terminal tests: start, resize sequence, key sequences, `SIGINT`/`SIGTERM`/`SIGTSTP`+`SIGCONT`, forced panic, closed terminal; assert final terminal modes.
- A blocked-terminal test (the reader stops reading) asserts that collection continues and scheduling lag stays in budget.
- A slow-target test asserts input stays responsive while one stream times out continuously.

### 4.3 Replay scenarios

Each is a script for the deterministic source, used by core tests, UI snapshots, and `demo`.

| Scenario | What it must show |
|---|---|
| `healthy` | four streams, stable latency, 100 % coverage |
| `isolated-failure` | one target stops replying for 40 s; the other three unaffected |
| `correlated-spike` | all four rise together for 15 s; gateway control flat |
| `correlated-outage` | all four stop; gateway still replies; DNS partially answers |
| `dns-only` | ICMP healthy; DNS returns `SERVFAIL`, then timeouts, then recovers |
| `missing-permission` | ICMP unsupported on the host; DNS runs |
| `needs-config` | providers 3 and 4 unconfigured |
| `address-change` | a hostname target moves to a new address; epoch marker; statistics restart |
| `collector-overload` | observation channel saturates; skipped slots and backpressure notice; no network loss shown |
| `sleep-resume` | a 12-minute suspend; one gap; no burst; no incident |
| `late-dup-reorder` | late replies, duplicates, reversed arrival order, sequence wraparound |
| `flapping` | repeated short failures collapse into one flapping incident |
| `ipv6-missing` | v6 streams report local errors while v4 is healthy |

### 4.4 Integration tests on real sockets

Controlled, no Internet: loopback and a local fixture resolver (UDP, TCP, TLS, HTTP/2) started by the test.

- macOS arm64 and Linux x86_64 in CI; Linux with unprivileged ICMP both enabled and disabled where the runner permits **[T]**.
- IPv4 and IPv6 loopback echo; an address with no route for `local_error`; a closed UDP port for `unreachable`.
- Interface binding accepted and refused.
- TLS fixture with a correct certificate, a wrong name, and an expired certificate.

### 4.5 Soak and live comparison

- **Soak:** 24 hours on the standard workload with self-metrics recorded every 10 s; assertions on resident memory, ring occupancy, scheduling lag, and file-descriptor count. A max-speed replay loop exercises a week of observations in minutes for memory-bound checks.
- **Live Internet comparison is supplementary evidence only**, never a CI assertion: run `pathwatch sample` beside `ping` and `dig` against the same addresses for ten minutes and compare medians and no-reply counts by eye; differences are investigated, not asserted.

### 4.6 Acceptance criteria required by the brief

| Required demonstration | Where it is proven |
|---|---|
| Identical observations yield identical statistics regardless of render rate | milestone 2 criterion 1; milestone 3 criterion 3 |
| Pending, timeout, local error, unsupported, and missing stay distinguishable | milestone 1 criteria 2, 4, 6; milestone 2 criterion 3 |
| Denominators and windows match the documented definitions | milestone 1 criteria 2, 3; §4.1 window cases |
| Active protocol, failing target, and stale data identifiable at a glance | milestone 2 criterion 2 |
| Memory bounded; traffic within the configured budget | milestone 1 criterion 12; milestone 4 soak |
| Unsupported capabilities neither crash nor become false outage reports | milestone 1 criterion 6; milestone 2 criterion 4 |

---

## 5. Performance budgets

**These are proposals to validate, not results.** The ones that need no network were measured on the first reference environment on 2026-11-22, on loopback, in runs of up to five minutes: see [milestone-4-status.md](milestone-4-status.md#task-42--performance-validation-the-part-that-needs-no-network). Two were restated that day, on your decision, and are marked below. Nothing has been measured on the real workload or on the second environment.

**Reference environments**

| | Environment | To record at measurement time |
|---|---|---|
| R1 | the development Mac: Apple silicon (arm64), macOS 26.7 | exact model, core count, power state, terminal emulator and version |
| R2 | one of your Linux hosts — Ubuntu or Debian, x86_64 assumed, kernel ≥ 6.1; which machine and release is still open | distribution, kernel, CPU, terminal, whether over SSH |

**Workloads**

| | Definition |
|---|---|
| W1 | the example configuration with all four providers enabled, both families, ICMP + DNS/UDP + DoH, both controls (≈ 26 streams); standard 120 × 35 layout; live view, 5-minute window; 1 hour |
| W2 | W1 at 160 × 45 with a route trace running |
| W3 | W1 frozen and idle (no visible change) |

| Budget | Proposed limit | Method |
|---|---|---|
| CPU, W1 | mean ≤ 3 % of one core. *Restated from 2 %: on loopback it measured 1.9 % to 2.5 %, collection alone being 0.8 % and ten frames a second most of the rest. To be confirmed on the real workload.* | process CPU time deltas sampled every 10 s by the application's self-metrics, cross-checked with `ps`/`pidstat` |
| CPU, W3 | mean ≤ 1 % of one core | same |
| Resident memory | ≤ 64 MB at any time in 24 h of W1; ≤ 2 MB growth after hour 25. *Restated from "hour 1 to hour 24": the compact samples are kept for 24 hours, so the program fills about 1.4 MB of them an hour until then, and none after. The limit is there to catch a leak, and a leak shows once the history is full.* | self-metrics plus `ps` |
| Input latency, application side | p95 ≤ 50 ms from key event read to frame flushed | internal timestamps |
| Input latency, end to end | p95 < 100 ms from key written to the pseudo-terminal to the changed output read back | pseudo-terminal harness; the emulator's own drawing time is outside this and is assessed separately, by hand, on the terminals you name |
| Probe scheduling lag | p99 ≤ 5 ms, maximum ≤ 50 ms outside gaps, over 1 h of W1 | `sched_lag_us` from the observations |
| Receive loop lag | p99 ≤ 2 ms | heartbeat lateness |
| Terminal output | mean ≤ 20 kB/s in W1; ≤ 2 kB/s in W3 | bytes written to the pseudo-terminal |
| Start-up | first frame ≤ 300 ms | pseudo-terminal harness |
| Shutdown | terminal restored ≤ 100 ms after the quit key; exit ≤ 3 s | pseudo-terminal harness |
| Traffic | within ±10 % of [01 §13](01-design-spec.md#13-defaults-and-traffic-budget) | the application's own send counters; loopback capture in integration tests |
| Long session | no unbounded structure: ring occupancy, channel depth, and file descriptors flat after warm-up | 24-hour soak |

---

## 6. Risk register

| # | Risk | Mitigation | Owner of the decision |
|---|---|---|---|
| 1 | Readers take the no-reply rate to a router or anycast resolver as "packet loss on my line" | naming ("no-reply"), help text, trace annotations, control rows; Google and Cloudflare both document ICMP rate limiting on their side **[V]** | design |
| 2 | Arelion target is ambiguous; its listed resolvers may refuse non-customers; a router target is rate-limited | card stays `NEEDS CONFIG` until you choose; capability test in task 1.12; DNS refusal shown as an error response, not an outage | you (Q2) |
| 3 | NextDNS path assumptions wrong; profile id leaks into a shared artefact | Q1 before milestone 1; redaction on by default; search-based test on exports | you / design |
| 4 | Unprivileged ICMP unavailable on some Linux hosts | degraded mode, `doctor`, opt-in raw backend, nothing changed automatically | you, per host |
| 5 | Pre-1.0 dependencies change under us (Ratatui 0.31, Crossterm, hickory, trippy) | lockfile, thin internal interfaces, upgrades as isolated tasks, CI duplicate-version check | implementation |
| 6 | User-space timestamps overstate RTT under load | `sched_lag`, loop-lag flagging, separate collector thread; kernel timestamps as a later, labelled backend | design |
| 7 | Certificate verification time inflates TLS handshake measurements | documented in the metric; evaluate an alternative verifier | implementation |
| 8 | Scope growth in visual polish or diagnostics | the sketches and tables in 01 are the scope; deferred list in §7 | you |
| 9 | Timer behaviour on a laptop on battery differs from the budget | measure on R1 in both power states; re-baseline with evidence | implementation |
| 10 | Recording fills a disk | caps, free-space floor, stop-don't-crash | design |
| 11 | Sustained probing is unwelcome to a target | 1 probe/s per family per target, far below documented resolver limits (Google documents 1,500 queries/s per address **[V]**); global cap; router targets flagged | design |

---

## 7. Deferred work

Not in any milestone above; listed so their absence is a decision, not an omission.

- Web dashboard, cloud account, distributed collectors, external notifications, automatic remediation, speed or throughput tests, a general plugin system, Windows.
- DoQ and DoH3.
- Kernel receive timestamps as a selectable timestamp source.
- UDP/TCP and flow-controlled (Paris/Dublin) traces beyond what an optional `trippy-core` adapter gives.
- Compression of recording segments; a database for long-range queries.
- A controlled cold-cache DNS benchmark. It needs a zone you control with unique per-query labels and known TTLs, an authoritative server whose logs confirm each recursion, and its own result type; ordinary monitoring cannot provide it.
- Screen-reader integration beyond plain-text output (nothing in the chosen UI library supports it today **[V]**).

---

## 8. Platform support and packaging

**Proposed support — nothing here has been built or verified.**

| Tier | Target | Commitment |
|---|---|---|
| A — tested every release | `aarch64-apple-darwin` (your Mac), `x86_64-unknown-linux-gnu` on Ubuntu and Debian, with unprivileged ICMP both on and off | CI tests, integration tests, soak |
| B — built and smoke-tested | `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin` (a Tier 2 Rust target now) | CI build, start-up test, `doctor` |
| Best effort | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | attempted; published only if a clean-host test passes |

Constraints to respect:

- **No promise of universally static binaries.** glibc builds link dynamically against glibc; the minimum glibc version depends on how they are built and will be stated per artefact after it is tested. Fully static musl builds depend on the TLS crypto provider building and linking for musl, which its documentation lists as supported but which this project has not verified.
- Release builds need a C compiler for the default TLS crypto provider **[V]**.
- Linux ICMP behaviour depends on host configuration, not on the binary.
- Artefacts: compressed tarball per target containing the binary, licence texts, and the example configuration; a `SHA256SUMS` file; optionally build provenance attestation.
- Installation paths in order of preference: Homebrew tap (macOS and Linux), tarball from the release page, `cargo install` from source.
- Minimum Rust version for building: 1.88 **[V]**, set by dependencies.

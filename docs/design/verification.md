# pathwatch — verification

This document says how pathwatch is verified: what the tests are and what they may touch, the scripted scenarios, the performance budgets and what has been measured against them, and which platforms are supported at what level.

It describes the test suite as it stands in the repository. What the program is meant to do is in [design.md](design.md); how it is built is in [architecture.md](architecture.md). What is not yet built, and what is built but not yet verified, is listed in [ROADMAP.md](../../ROADMAP.md).

Section numbers are stable, because source comments cite them. A comment that cites §4.3 or §5 of the implementation plan refers to the section of the same number here: §4.3 is the list of scripted scenarios, §5 the performance budgets.

---

## 1. What "verified" means here

A statement about pathwatch's behaviour has one of four kinds of evidence behind it, and this document says which.

| Evidence | What it is | What it is worth |
|---|---|---|
| **Asserted** | a test that runs on every change, locally and in CI, and fails if the statement stops being true | the statement holds on the platforms the tests run on (§7) |
| **Measured** | a reading taken on request by one of the ignored measurement tests (§5) | a figure for one machine, one workload and one run length; not a guarantee |
| **Observed** | something seen in a short session against real providers (§4.4) | one observation from one network; never asserted, never repeated automatically |
| **Not verified** | no test, no measurement, no observation | stated as such wherever it matters |

Tests are built on three rules.

1. **Nothing leaves the machine.** Tests use loopback addresses, fixture servers started by the test on `127.0.0.1`, and scripted data. No test sends a packet to any provider or resolves a name through the host's resolver, and no test result is a statement about any network or provider.
2. **Deterministic where possible.** Everything that can be tested with a scripted transport and a paused clock is tested that way. The same scenario and seed produce the same bytes on every run.
3. **A test that cannot run says so.** Where the host forbids something a test needs, the test prints why and stops; it does not pass silently where the host is known to permit it (§3).

## 2. Properties the tests hold

These are the properties the design depends on, and where each is asserted. File paths are relative to `crates/`.

| Property | Asserted by |
|---|---|
| The same observations give the same statistics however often they are read or drawn | `statistics_do_not_depend_on_how_often_they_are_read` (`pathwatch-core/tests/scenarios.rs`); `snapshots_are_a_pure_function_of_data_view_and_time` (`pathwatch-core/src/snapshot.rs`); `rendering_is_a_pure_function_of_the_snapshot` (`pathwatch-tui/tests/render.rs`); `replaying_a_recording_reproduces_the_live_sessions_statistics_exactly` (`pathwatch-core/tests/recording.rs`) |
| The same scenario produces byte-identical records | `every_scenario_is_deterministic_and_internally_consistent` (`scenarios.rs`); `identical_scenarios_produce_identical_records_and_statistics` (`pathwatch-core/tests/collector_scripted.rs`) |
| Counter identities: `sent = pending + replied + timed_out + unreachable + service_error + protocol_error + cancelled` and `slots = sent + local_error + skipped` | every scenario, every stream, at windows of 10 s, 60 s, 300 s and 3600 s (`scenarios.rs`) |
| A probe without a reply carries no round-trip time: not zero, not the deadline, and the field is absent from the serialised line | `scenarios.rs`; `timeout_carries_no_round_trip_time` (`pathwatch-core/src/probe/icmp_core.rs`) |
| Pending, timeout, local error, unsupported and unconfigured stay distinct, in the data and on screen | `unsupported_and_unconfigured_streams_have_no_outcomes_at_all` (`scenarios.rs`); `local_send_failures_are_not_network_loss`, `unavailable_icmp_is_reported_as_unsupported_and_sends_nothing` (`collector_scripted.rs`); `inactive_cards_never_show_a_percentage_and_statuses_stay_distinct` (`render.rs`); `every_status_has_a_distinct_word_and_an_ascii_symbol` (`pathwatch-tui/src/theme.rs`) |
| Denominators, windows and minimum-sample rules match their definitions | the unit tests in `pathwatch-core/src/stats.rs` |
| An error response from a resolver is never a timeout | `dns_udp_classifies_every_kind_of_answer` (`pathwatch-core/tests/real_sockets.rs`); `every_rcode_class_is_an_error_response_not_a_timeout` (`pathwatch-core/src/probe/dns.rs`) |
| A capability the host lacks neither crashes the program nor reads as an outage | `collector_scripted.rs` with a simulated refusal; the CI job with unprivileged ICMP refused (§3) |
| Traffic sent is what the configuration implies | exact byte counts for ICMP in `loopback_icmp_runs_unprivileged_on_both_families` (`real_sockets.rs`); `default_budget_matches_the_documented_packet_sizes` (`pathwatch-core/src/traffic.rs`); measured in §5 |
| The terminal is left as it was found on every way out | `pathwatch/tests/terminal.rs` (§4.2) |

One property the design asks for is not asserted by any test: that a reader can name the active protocol, the failing target and any stale value at a glance. The reference screens in `pathwatch-tui/tests/golden/` fix what is drawn; whether it reads well is a judgement made by eye on the demo scenarios.

## 3. Running the tests

```sh
cargo test --workspace
```

This runs every check. It needs no network, no privilege and no configuration file. A Unix host is required for the pseudo-terminal tests (`crates/pathwatch/tests/terminal.rs` is compiled only on Unix).

| Variable | Effect |
|---|---|
| `PATHWATCH_REQUIRE_ICMP=1` | A test that needs a live ICMP session skips itself, with a message, where the host forbids unprivileged ICMP. With this variable set, that skip is a failure. It is honoured by the receive-buffer test in `real_sockets.rs` and by five tests in `terminal.rs`. Set it on a host known to permit unprivileged ICMP, so that those tests cannot be skipped by accident; CI sets it where ICMP is allowed |
| `UPDATE_GOLDEN=1` | Rewrites the reference files instead of comparing against them: the screens in `crates/pathwatch-tui/tests/golden/` and the schema lines in `crates/pathwatch-core/tests/golden/schema_v1.jsonl`. Review the diff before committing it |
| `UPDATE_DOCS=1` | Rewrites `docs/pathwatch.1` and `docs/glossary.md` from what the program prints. Review the diff before committing it |

macOS always permits unprivileged ICMP. Linux decides by `net.ipv4.ping_group_range`; where the range excludes the user, ICMP streams read `UNSUPPORTED`, and the tests check that they do, that nothing is sent, and that everything else still runs.

**Ten tests are marked `#[ignore]`.** They are measurements, not checks: they take minutes, want a quiet machine, and mean something only in a release build (each refuses to run in a debug build). `cargo test` does not run them. To run them:

```sh
cargo test --release -p pathwatch --test terminal perf -- --ignored --nocapture --test-threads=1
cargo test --release -p pathwatch-core --test perf -- --ignored --nocapture
cargo test --release -p pathwatch-tui --test render perf -- --ignored --nocapture
```

| Test target | Ignored tests |
|---|---|
| `pathwatch`, `terminal` | `perf_environment`, `perf_start_up_and_shutdown`, `perf_key_to_screen`, `perf_steady_then_frozen`, `perf_profile_of_the_live_dashboard` (macOS only), `perf_scheduling_and_traffic` |
| `pathwatch-core`, `perf` | `perf_memory_of_the_history`, `perf_snapshot_cost_against_history`, `perf_snapshot_cost_with_every_dns_stream_failing` |
| `pathwatch-tui`, `render` | `perf_drawing_a_frame` |

A single one is run by name, for example `cargo test --release -p pathwatch --test terminal perf_key_to_screen -- --ignored --nocapture`. Each prints `PERF |` rows and fails where a budget it measures is exceeded. They use loopback only. §5 says what they have shown.

**Continuous integration** (`.github/workflows/ci.yml`) runs on every push and pull request, on three configurations: macOS; Ubuntu with unprivileged ICMP allowed; Ubuntu with it refused. The Linux setting is made on the runner by the workflow, so the outcome does not depend on the runner image. Each configuration runs:

- `cargo fmt --all -- --check`;
- `cargo clippy --workspace --all-targets --locked -- -D warnings`;
- `cargo test --workspace --locked --no-fail-fast`, with `PATHWATCH_REQUIRE_ICMP=1` where ICMP is allowed;
- `pathwatch doctor`, to show what the runner permits;
- a check that the dependency tree holds one major version of Crossterm.

A separate job checks licences and security advisories with `cargo deny` against the policy in `deny.toml`. CI builds with the stable toolchain only (§7).

---

## 4. Verification strategy

### 4.1 Unit and scripted tests

These run with no I/O, or with a scripted transport and a paused clock.

**Unit tests** sit beside the code they test.

| Area | Module (under `crates/`) | Cases |
|---|---|---|
| Statistics | `pathwatch-core/src/stats.rs` | nearest-rank percentiles at small n and against a naive sort; minimum-sample gating; the no-reply denominator; timeouts contributing no round-trip time; RTT variation needing adjacent successes; unanswered runs; coverage of a young session |
| ICMP matching | `pathwatch-core/src/probe/icmp_core.rs` | deadline edges; duplicates; out-of-order replies; sequence wraparound; foreign packets; the identifier checked only where the kernel preserves it; unreachable attributed by destination and sequence; the late window bounding memory |
| ICMP packets | `pathwatch-core/src/probe/icmp_packet.rs` | checksum and size; replies with and without an IP header; reply TTL; quoted packets in unreachable and time-exceeded messages; short and foreign packets rejected |
| DNS codec and classification | `pathwatch-core/src/probe/dns.rs` | query shape; every RCODE class; blocked answers; mismatched and malformed responses; truncation |
| Scheduler | `pathwatch-core/src/schedule.rs` | slots on time; a late timer skipping explicitly; no burst after a pause; stagger; the token bucket |
| Clocks and gaps | `pathwatch-core/src/clock.rs` | suspend detection; a wall-clock step as an event and not a gap; collector stall |
| Incidents | `pathwatch-core/src/incident.rs`, `correlate.rs` | open and close thresholds; hysteresis; cooldown; flapping collapsed into one event; grouping of incidents that open together, with controls cited and never counted |
| History and snapshots | `pathwatch-core/src/engine.rs`, `snapshot.rs` | fast lookups against a walk of the whole history; kept records against the observations they came from; scale, gaps, downsampling, freezing, histogram bins |
| Configuration | `pathwatch-core/src/config.rs` | the built-in defaults; unknown keys and placeholders rejected; every problem reported; provenance; profile ids masked; static addresses; the stream plan and its stagger; settings that are not built rejected, and settings without effect warned about |
| Text from outside | `pathwatch-core/src/sanitize.rs`, `recorder.rs` | escape sequences and bidirectional overrides stripped; a secret removed wherever it stands |
| Host network | `pathwatch-core/src/localnet.rs`, `netwatch.rs` | parsing the resolver configuration and routing-table output; classifying a resolver as stub, local or remote; nothing is sent |
| Dashboard | `pathwatch-tui/src/app.rs`, `theme.rs`, `widgets.rs` | key handling; the help pages; reduced-motion pacing; colour depth; clipping and labels |
| Command line | `pathwatch/src/assert.rs`, `recording.rs`, `man.rs`, `main.rs`, `dashboard.rs` | `--assert` parsing and judging; a recorder whose disk fills; manual-page escaping; address redaction; the choice of palette |

There is no property-testing or fuzzing harness. The percentile comparison against a naive sort uses generated inputs inside an ordinary unit test.

**Scripted integration tests** are in `crates/pathwatch-core/tests/`.

- `collector_scripted.rs` runs the real collector against a scripted ICMP transport, a clock the test advances, and a host network the test can change. It covers stagger and the absence of bursts; a silent target not delaying the others; stop and resume as one gap; a host suspend as one gap with no burst and no loss; local send failures kept apart from network loss; ICMP unreachable as its own outcome; late and duplicate replies; backpressure skipping slots without losing a sent probe's result; unavailable ICMP reported as unsupported; an ICMP transport opened only when a stream needs it; a network change cancelling probes in flight and recording one gap and one event; the gateway control following the host; and configuration reload.
- `scenarios.rs` holds every scripted scenario (§4.3) to the properties in §2 and to what each scenario is there to show, and guards the JSONL schema against `tests/golden/schema_v1.jsonl`.
- `trace.rs` runs the route tracer against a scripted path with a paused clock: the probe and rate caps are never exceeded; a trace stops within a round of being asked; each annotation (a hop that never answers, a rate-limited hop, no reply from a hop to the destination, two addresses at one TTL, a slow hop) appears where the design says and nowhere else; no annotation claims a cause; only one trace runs at a time.
- `recording.rs` writes recordings to temporary directories from scripted data and reads them back: round trip; rotation by size and by age; the size cap; a disk that fills (storage that fails on demand, not a full disk); a truncated file; unknown lines; file permissions; text cleaned as it is read; a profile id never reaching the disk; replay reproducing a live session's statistics exactly.

**Rendering tests** are in `crates/pathwatch-tui/tests/render.rs`. A scripted scenario, a terminal size and a colour and glyph mode go in; a cell buffer comes out. No terminal is involved.

- Twenty-one reference screens in `tests/golden/` are compared as text. They cover the three layouts (160 × 45, 120 × 35, 80 × 24), each details tab, the cursor, help, the glossary page, the route view, the controls line and the mouse hint.
- Every scenario is drawn at nine sizes, in four colour and glyph modes and every measurement mode, and checked not to panic or overflow; below 80 × 24 the screen must ask for a larger window and draw nothing else.
- Every details tab is drawn for every scenario at three sizes in two modes, with lists scrolled to their end.
- ASCII mode is checked to emit no non-ASCII character.
- Style is asserted on cells where text cannot show it: the chart draws nothing in a provider's colour where there was no reply, and the high-contrast palette leaves no cell to the terminal's colours and dims none.
- Mouse targets are checked against what their keys do.

Not covered: the 256-colour mode has no whole-screen test, and the 16-colour mode is only checked not to panic or overflow.

### 4.2 Real sockets on loopback, pseudo-terminal tests, documentation tests

**Real sockets** (`crates/pathwatch-core/tests/real_sockets.rs`). The collector runs on the system clock against this host only. Fixture servers are started by the test on `127.0.0.1`: a resolver answering over UDP and TCP on one port, an HTTP/2-over-TLS server with a self-signed certificate for `doh.test`, and a DNS-over-TLS server with a certificate for `dot.test`. The client is given a configuration that trusts the fixture's certificate; the operating system's trust store is not exercised by any test.

- ICMP echo to `127.0.0.1` and `::1`, unprivileged: replies arrive, the round trip ends at the kernel's receive stamp, the reply TTL is captured, and traffic counts are exact to the byte. A family with no usable loopback address, or a host that forbids unprivileged ICMP, is reported and that part is skipped.
- A burst of replies left unread is all still there to read (the receive buffer).
- DNS over UDP: every kind of answer the fixture can give — answer, `SERVFAIL`, `REFUSED`, `NXDOMAIN`, empty, blocked, truncated with fallback to TCP, silence, a wrong id, garbage — is classified as the design specifies.
- DNS over TCP as its own stream; a refused TCP connection and a closed UDP port are `unreachable`, not timeouts.
- DoH: connect, handshake and request timed separately; connection reuse tracked; a certificate for another name rejected; static addresses bypassing the resolver while keeping the server name; a network change replacing the connection.
- DoT: queries on one TLS connection; a closed connection replaced; a certificate for another name rejected.
- Ending a session does not wait for a query in flight, over UDP or TCP.

Not covered: an expired certificate; a connect that never completes (loopback accepts or refuses at once); the Linux error queue delivering an unreachable or time-exceeded message from a router. Source and interface binding is not in the build, so it has no test.

**Pseudo-terminal tests** (`crates/pathwatch/tests/terminal.rs`, Unix only). The built program runs in a real pseudo-terminal under a stand-in shell, and the test reads what it writes. `demo` plays scripted data; live sessions probe loopback addresses and closed loopback ports only.

- The terminal is restored — alternate screen left, cursor shown, canonical mode, mouse reporting off — after quitting, `Ctrl-C`, `SIGTERM`, `SIGHUP`, a forced panic, and `Ctrl-Z` followed by resume.
- A terminal closed under a live session ends the session in order: the recording is closed with the session's last record.
- A terminal that is too small gets a request for a larger one; no terminal at all gets a refusal with no escape sequence written.
- Stop and resume, reload on a key and on hang-up, freeze, the route trace (to loopback), the controls line, the mouse, high contrast and reduced motion are driven by keys and checked on the screen.
- Recording, export in every format and replay run end to end, including a search of the output for a profile id and a recording carrying hostile text.
- `sample --assert` is checked for its exit statuses, `sample --format plain` and `glossary` for their form.

Five of these need a live ICMP session and skip themselves where the host forbids one (§3). A live resize of the terminal is not tested; resizing is covered only by drawing at many sizes in `render.rs`.

**Documentation tests** (`crates/pathwatch/tests/docs.rs`) hold the files in `docs/` against the program. Nothing is sent.

- `docs/pathwatch.1` and `docs/glossary.md` are what `pathwatch man` and `pathwatch glossary --markdown` print, compared whole.
- `docs/configuration.md` is checked setting by setting: every setting is a row, every default shown is the default in force, and "no effect" is said of exactly the settings that have none. The built-in example file is held to the same list.
- `docs/capabilities.md` is checked against `pathwatch doctor` on the host running the tests: every line `doctor` prints is one the guide explains, and ICMP is available exactly when the guide's rule says so.
- `README.md` links to files that exist and names commands the program has.

These check facts — keys, defaults, options, lines — not whether an explanation is clear.

### 4.3 Scripted scenarios

Each scenario is a script for the deterministic observation source in `crates/pathwatch-core/src/script.rs`: 180 seconds of observations generated from a seed. The same scripts are used by the core tests, the rendering tests and reference screens, and `pathwatch demo`. `pathwatch demo --list` prints this list; `pathwatch demo <name>` plays one with no network activity.

| Scenario | What it shows | What `scenarios.rs` asserts of it, beyond the general properties |
|---|---|---|
| `healthy` | four streams, stable latency, full coverage | no timeouts, local errors or skipped slots; a no-reply rate of 0; coverage of 180 s in a 300 s window |
| `isolated-failure` | one target stops replying for 40 s; the other three are unaffected | 40 timeouts in one run on that stream, then recovery; none on the others |
| `correlated-spike` | all four rise together for 15 s | no probe unanswered; the spike shows in the maximum and does not move the median |
| `correlated-outage` | all four ICMP targets stop; DNS partially answers | the general properties only; its grouped event and controls line are reference screens |
| `dns-only` | ICMP healthy; DNS returns `SERVFAIL`, then times out, then recovers | error responses counted apart from unanswered queries and kept out of latency statistics |
| `missing-permission` | ICMP unsupported on the host; DNS runs | the ICMP streams have no slots, no probes and no rate |
| `needs-config` | providers 3 and 4 unconfigured; nothing is sent to them | those streams have no slots, no probes and no rate |
| `address-change` | a hostname target moves to a new address; statistics restart | a second destination epoch; samples from the old address are not pooled with the new |
| `collector-overload` | the processor falls behind: skipped slots, no network loss | 20 skipped slots per stream, a no-reply rate of 0, reduced coverage |
| `sleep-resume` | a 12-minute host suspend: one gap, no burst, no loss | one gap of 720 s; no timeouts; 180 probes sent, not 900; RTT variation not paired across the gap |
| `late-dup-reorder` | late replies, duplicates, and a 16-bit sequence wraparound | adjacency held across the boundary; late replies leave their probes as timeouts; duplicates counted and nothing else changed. Also the source of the schema reference lines |
| `flapping` | repeated short failures on one target | 30 timeouts in runs of 6. The collapse into one flapping event is asserted in `incident.rs` and shown in a reference screen |
| `ipv6-missing` | IPv6 has no route: local errors while IPv4 is healthy | 180 local errors, nothing sent, no rate on IPv6; IPv4 unaffected |

A test asserts that there are exactly thirteen, that each is identical for the same seed and different for another, and that the properties in §2 hold for each.

### 4.4 Live comparisons

A comparison against the real Internet is supplementary evidence only. It is never a CI assertion and no test performs one. The method is to run `pathwatch sample` beside the system `ping` (and `dig`) against the same addresses for a few minutes and compare medians and no-reply counts by eye; a difference is investigated, not asserted.

What has been done, all from one network, on macOS, over IPv4:

- **Two one-minute runs beside the system `ping`**, ICMP to two public resolver addresses at one probe a second. In the second, with a release build, pathwatch's medians read 7.6 ms and 6.4 ms against `ping`'s 6.4 ms and 5.2 ms. Of that difference about 0.8 ms was the time a reply waited in the kernel for the process to read it (median 0.77 ms and 0.84 ms). Across both minutes pathwatch had 3 of 247 probes unanswered and `ping` 2 of 240; two of the three coincided with a disturbance `ping` also saw.
- As a result, an ICMP round trip now ends at the kernel's receive stamp, and the wait is recorded beside it. On loopback the median then read 0.1 ms against `ping`'s 0.19 ms average, over 155 probes. **The comparison has not been repeated on a real path since that change.**
- **Short sessions of 20 seconds to 3 minutes** have exercised, once each: DoH against two providers with the operating system's trust store; DoT (60 s); DNS over TCP (60 s); one route trace; recording followed by export and replay; the gateway and system-resolver controls.

None of the following is on record: a comparison with `dig`; any comparison for the DNS transports; anything over IPv6 on a real path (the network used had no IPv6 route); any live run on Linux; any run against a NextDNS profile endpoint or an Arelion target; any session longer than three minutes.

---

## 5. Performance budgets

The budgets are limits the program is meant to stay within. Some have been measured; none has been validated on the workload it is defined for.

**The limits of the evidence.** Every measurement below was taken on loopback, on one Apple-silicon Mac (arm64), in runs of up to five minutes. None has been taken on the real one-hour workload, on a second machine, or over a 24-hour or longer run. Loopback answers ICMP in about a tenth of a millisecond and refuses DNS and DoH at once, so no TLS handshake, no DNS answer and no real round trip is in any figure; the real workload may cost more or less. The dashboard ran in a pseudo-terminal read by the test, so no terminal emulator's drawing time is in any figure. The machine was on mains power and was not idle: CPU figures moved by half a percentage point between runs.

**Workloads**

| | Definition | Measured? |
|---|---|---|
| W1 | all four providers enabled, both address families, ICMP, DNS over UDP and DoH, both controls; the standard 120 × 35 layout; live view with a 5-minute window; for 1 hour | in shape and rate only: 34 streams and 12.4 transactions a second, every destination on loopback, for up to 5 minutes |
| W2 | W1 at 160 × 45 with a route trace running | not measured |
| W3 | W1 frozen and idle | on loopback, for up to 5 minutes |

The history and snapshot measurements (`pathwatch-core`, `perf`) send nothing at all: they build a simulated history for a 26-stream model of W1.

**Reference environments.** The first is macOS on Apple silicon. The second is a Linux host (Ubuntu or Debian, x86_64); nothing has been measured there.

**Budgets and measurements**

| Budget | Limit | Measured | How | State |
|---|---|---|---|---|
| CPU, W1 | mean ≤ 3 % of one core | 1.9 % to 2.5 % in five runs of 40 s to 5 min | process CPU time before and after the run, read by the test from the system | within the restated limit, on loopback |
| CPU, W3 | mean ≤ 1 % of one core | 0.87 % to 0.93 % in four runs | same | within, on loopback |
| Resident memory | ≤ 64 MB at any time in 24 h of W1 | 13.3 MB at the start, 14.2 MB after ten minutes, 15.9 MB at most. The history alone is 41.9 MB after a simulated day, counted by allocation; that projects to about 55 MB | resident size read with `ps`; a counting allocator in `perf_memory_of_the_history` | within for ten minutes; a day is a projection, not a measurement |
| Memory growth | ≤ 2 MB after hour 25 | not measured | needs a run past hour 25 | not measured |
| Input latency, application side | p95 ≤ 50 ms from key event read to frame flushed | not timed separately; it is inside the figure below | — | bounded by the end-to-end figure |
| Input latency, end to end | p95 < 100 ms from a key written to the pseudo-terminal to the changed output read back | median 0.9 ms, p95 1.1 ms, maximum 1.6 ms, of 200 keys | pseudo-terminal harness | within; no terminal emulator in the figure |
| Probe scheduling lag | p99 ≤ 5 ms, maximum ≤ 50 ms outside gaps, over 1 h of W1 | p99 3.3 to 3.6 ms, maximum 7.9 to 20.8 ms, of 1,493 to 3,731 probes | `sched_lag_us` in the observations | within, in runs of up to 5 min |
| Receive-loop lag | p99 ≤ 2 ms, by heartbeat lateness | the heartbeat was more than 2 ms late in 26 % to 35 % of checks, at worst 6 to 19 ms | a timer checked twenty times a second, counted in every summary | **not met as written; open — see below** |
| Terminal output | mean ≤ 20 kB/s in W1; ≤ 2 kB/s in W3 | 1.1 to 1.4 kB/s; 0.08 kB/s | bytes written to the pseudo-terminal | within |
| Start-up | first whole frame ≤ 300 ms | at most 31 ms (`demo`) and 35 ms (live, on loopback), over four runs of 15 starts | pseudo-terminal harness | within |
| Shutdown | terminal restored ≤ 100 ms after the quit key; process gone ≤ 3 s | restored in at most 5 ms; gone in a median of 1 to 3 ms, with one exit in fifteen taking about a second | pseudo-terminal harness | within; the slow exits are unexplained |
| Traffic | within ±10 % of the traffic budget the configuration implies (see [design.md](design.md)) | at most +0.7 % | the program's own send counters against the computed budget | within, for ICMP and DNS over UDP; DoH has no computed budget |
| Long session | no unbounded structure: ring occupancy, channel depth and file descriptors flat after warm-up | not measured | a 24-hour run | not measured |

**Two budgets were restated after measurement.**

- *Live CPU: from 2 % to 3 % of one core.* On loopback this workload measured 1.9 % to 2.5 %. Collection alone costs about 0.8 %; drawing ten frames a second accounts for most of the rest, spread across many short wake-ups. With `--reduced-motion` (four frames a second) it measured 1.55 %. The 3 % limit is still to be confirmed on the real workload, where TLS adds cost and answered DNS removes some.
- *Memory growth: counted after hour 25, where it was counted from hour 1 to hour 24.* The compact samples are kept for 24 hours by design, so the program fills about 1.4 MB of them an hour until then and none after. The limit exists to catch a leak, and a leak shows once the history is full. A 24-hour run cannot show it; the run has to go past hour 25.

**One budget is not met as written: receive-loop lag.** The budget measures the receive loop by how late a heartbeat timer fires, and asks for a 99th percentile of 2 ms. The heartbeat was more than 2 ms late in roughly a quarter to a third of checks. Most of that is how late macOS wakes a sleeping thread, not anything the receive loop does. The loop was therefore assessed by a different measure: how long a reply waited between the kernel's receive stamp and the program's read, which was 0.42 ms at the 99th percentile and 2.38 ms at most, of 1,080 replies. `perf_scheduling_and_traffic` judges that wait against 2 ms and reports the timer figure without judging it. Whether the budget should be redefined on the reply wait, or the timer lateness reduced, is open. Timer lateness does affect when a probe is sent; that is the scheduling-lag budget, which holds on loopback.

**Other things the measurements showed**

- *One exit in fifteen took about a second*, in two separate runs. It is within the 3-second limit and its cause has not been found.
- *A snapshot costs 0.1 ms after ten minutes of history and 0.7 ms after a day* for the 5-minute view (26 streams, simulated history); `perf_snapshot_cost_against_history` holds the 5-minute view to a mean of 1 ms. With a 24-hour window on screen after a day, a snapshot costs 8 ms, about 8 % of a core while that window is shown.
- *Drawing one frame costs about 0.05 ms*; `perf_drawing_a_frame` holds it to 0.5 ms.
- *A 60-second loopback run at the default intervals* matched the traffic budget exactly for ICMP on both families (84.0 and 104.0 B/s) and for DNS over UDP on IPv6; DNS over UDP on IPv4 read +7.2 %, an edge effect of a 60-second run holding thirteen slots of a 5-second stream.
- Scheduling lag has also been read in short live sessions: median 1.7 ms and worst 3.3 ms in one 62-second release-build run. That is an observation, not a measurement against the budget.

**Repeating the measurements.** The commands are in §3. `PATHWATCH_PERF_SECONDS` sets how long each steady measurement runs (default 120; the figures above used up to 300). `PATHWATCH_PERF_ARGS` adds options to the dashboard under measurement, to see what one of them costs. The same commands are what to run on a Linux host for the second environment.

---

## 6. Risks still open

- **Timestamps differ by transport.** An ICMP round trip ends at the kernel's receive stamp. DNS over UDP, TCP, TLS and HTTPS end at the program's own read, so they include the wait for the process to be woken (about 0.8 ms in the one live comparison). The two are never drawn on one line; kernel stamps for the DNS transports are not built.
- **Timer wake-ups are late on macOS** (§5). Behaviour on battery power has not been measured.
- **A full ICMP receive buffer is silent.** macOS hands every ICMP socket a copy of every ICMP message arriving at the host. The buffer is enlarged and a test holds a burst of 60, but if it overflows nothing tells the program, and a dropped reply reads as a timeout.
- **The Linux ICMP error path is unexercised end to end.** No test makes a router send an unreachable or time-exceeded message to a Linux host, and a trace on Linux has only been to loopback.
- **Unprivileged ICMP is a host setting on Linux.** Where `net.ipv4.ping_group_range` excludes the user, ICMP streams are `UNSUPPORTED`; there is no raw-socket mode, and running as root does not change it. Which distributions enable it by default has not been confirmed.
- **Certificate verification is inside the TLS handshake time.** Handshake figures include it, and it may differ by platform; this has not been measured.
- **Simulated, not real:** a host suspend, a change of network while a session runs, and a full disk are tested with a clock, a network and storage the test controls. None is recorded as tested on a real host.
- **Long sessions.** Nothing has run for longer than minutes (§5). Recording volume (about 320 MB a day) is an extrapolation from three minutes, and replay loads a whole recording into memory.
- **IPv6** has been exercised only on loopback and through the "no route" path. IPv6 route tracing is untested, even on loopback.
- **Not tried by a person:** the plain-text output with a screen reader, the high-contrast palette in a real terminal, and the mouse with a real mouse.
- **Pre-1.0 dependencies** (Ratatui, Crossterm, hickory-proto) can change their interfaces. The lockfile is committed, CI builds with `--locked`, and CI refuses two major versions of Crossterm.

## 7. Platform support

pathwatch is a Unix program. The tiers are the intended level of support; the last column is what has actually been done.

| Tier | Target | Intended commitment | Verification state |
|---|---|---|---|
| A — tested on every change | `aarch64-apple-darwin` | CI tests, integration tests, long runs | tested locally on macOS 26.7 and in CI on macOS runners. No long run (§5) |
| A — tested on every change | `x86_64-unknown-linux-gnu` on Ubuntu and Debian, with unprivileged ICMP both allowed and refused | CI tests, integration tests, long runs | tested in CI on x86_64 Ubuntu runners, with ICMP allowed and refused. Never run on an Ubuntu or Debian installation outside a container or a CI runner. No performance measurement and no long run |
| B — built and smoke-tested | `aarch64-unknown-linux-gnu` | CI build, start-up test, `doctor` | the full test suite was run in an aarch64 Linux container (kernel 7.0) during development, as root and as an unprivileged user, with the ICMP range open and at `1 0`. Not in CI |
| B — built and smoke-tested | `x86_64-apple-darwin` | CI build, start-up test, `doctor` | not built or tested |
| Best effort | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` | attempted | not built or tested |
| Not supported | Windows | — | not supported: the platform layer covers macOS and Linux only |

- **Minimum supported Rust version: 1.88**, set by dependencies and declared as `rust-version` in the workspace `Cargo.toml`. CI builds with the stable toolchain only and does not yet build on 1.88, so the declared minimum is not verified.
- **IPv6** has been exercised only on loopback and through the "no route" path, on every platform.
- **Linux ICMP behaviour depends on the host's configuration, not on the binary** (§6). `pathwatch doctor` reports what a host permits and sends nothing.
- **Release builds need a C compiler** for the default TLS cryptography provider.
- **No promise of static binaries.** glibc builds link dynamically against glibc. A fully static musl build depends on the TLS cryptography provider building and linking for musl, which has not been tried.
- **No release artefact has been built or installed on a clean host.** Packaging is in [ROADMAP.md](../../ROADMAP.md).

## 8. Deferred work

Not verified because not built. [ROADMAP.md](../../ROADMAP.md) is the authoritative list.

- Validation of the budgets in §5 on the real workload and on a Linux host; a 24-hour run, and one past hour 25.
- Release artefacts, checksums and installation tests.
- DoQ and DoH3; UDP and TCP route tracing; kernel receive timestamps for the DNS transports.
- Compression of recordings; a controlled cold-cache DNS benchmark.
- Screen-reader support in the dashboard itself, beyond the plain-text output.
- Windows.

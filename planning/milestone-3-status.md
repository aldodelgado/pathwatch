# Milestone 3 — status report (tasks 3.7, 3.6, 3.5, 3.4, 3.8, 3.1, 3.2, 3.9, the 3.3 evaluation, and the DNS/TCP stream)

**Date:** 2026-11-21 · started on your go-ahead.

**In one line:** eight of milestone 3's nine tasks are built (the ninth is an evaluation, below) and tested on macOS and Linux — recording, export and replay (3.7), grouping of incidents that open together across streams (3.6), the gateway and local-DNS controls (3.5), source context with network-change handling (3.4), configuration reload (3.8), DNS over TLS and the optional DNS/TCP stream (3.1), route tracing (3.2), and optional mouse support (3.9). Network-change handling and reload have only been exercised on a simulated network and on loopback; DNS over TLS has had one 60-second live check; route tracing has had one live trace, to 1.1.1.1 over IPv4 on macOS; the mouse has been driven by tests in a pseudo-terminal, never by a hand on a real terminal; the DNS/TCP stream has had one 60-second live check. When I first asked whether to start milestone 3 I described it as "logging to disk and replay"; the plan's milestone 3 is much larger than that. The ninth, the `trippy-core` evaluation (3.3), is done: it recommended not adopting it, and you decided to drop it. Nothing in milestone 3 is left unstarted.

Nothing was committed. Building and testing it reached nothing beyond this machine's loopback address; afterwards, with your consent, one 3-minute live session against Cloudflare and Google was recorded — see [below](#first-recorded-live-session-3-minutes).

## Try it

```bash
cargo run -p pathwatch -- demo
```

The demo is unchanged. To record a real session you would run `pathwatch run --record ~/pathwatch-recordings` (this probes Cloudflare and Google), quit, and then:

```bash
pathwatch replay ~/pathwatch-recordings/<session id>
```

```bash
pathwatch export ~/pathwatch-recordings/<session id> --format summary
```

## What exists

| Part | Behaviour |
|---|---|
| Recording | `run --record [DIR]`, `sample --record [DIR]`, or `[recording] enabled = true`. Off by default: nothing is written unless you ask |
| Layout | `DIR/<session id>/000001.jsonl`, `000002.jsonl`, … — JSON lines, usable with `jq`, `grep`, `tail -f` |
| Self-describing segments | every segment starts with a header (schema, version, host, session, segment number, providers, effective configuration with secrets masked) followed by the session's stream list, so any one segment can be read or replayed alone |
| Rotation | at `rotate_size` (64 MiB) or `rotate_age` (24 h), whichever comes first; synced to disk on rotation |
| Size cap | `max_total` (1 GiB) across the whole recording directory; oldest segments deleted first, never the one being written; empty session directories removed |
| Disk full / write error | recording stops, the header shows `REC STOPPED: <reason> — measurement continues`, an event is added, and the exit status is 5. No probe or statistic is affected |
| Low disk | checked every five seconds; recording stops below `min_free_disk` (200 MiB) |
| Slow disk | records go to the recorder thread through a bounded queue. If it fills, lines are dropped and counted (shown in the header and on exit) — the processor never waits for the disk |
| Cannot start | a recording that was asked for and cannot be created is an error before anything is sent (exit 5) |
| Redaction | profile ids are removed **as lines are written**, by replacing the id wherever it occurs in the serialised line. They are never on disk |
| Reading | every complete, readable line is used. A truncated last line, unknown lines, and lines from a newer schema are skipped, each with one warning |
| `replay` | plays a recording through the same processor and dashboard as a live session. Header reads `REPLAY` / `REPLAY ENDED` and `nothing is being sent` throughout. Without `--speed` the whole session is shown at once; `--speed N` plays it from the start (`--at S` to skip ahead) |
| `export` | `--format jsonl` (the records), `csv` (one row per observation), `summary` (per-stream totals from the same statistics engine). `--redact-local` also removes this host's addresses |
| Replaying `sample` output | the JSONL that `sample` prints has no segment header and is still accepted |

Two wording fixes came out of this: a demo or replay no longer calls itself `LIVE` (it reads `PLAYING` or `ENDED`), and a frozen demo no longer says "collection continues".

## Test evidence

240 tests pass on macOS (arm64) and in the Linux container; lint and formatting are clean on macOS.

- **Recorder (10, new):** round trip; rotation by size and by age; the size cap; a full disk; low disk; a truncated file; unknown and newer-schema lines; redaction; replay fidelity.
- **Recorder thread (2, new):** a disk that fills stops it with one notice and the sender never blocks, even when sending four times the queue's capacity; flag and configuration precedence.
- **End to end (3, new, in a pseudo-terminal where needed):** record a loopback session with `sample --record` and export it in all three formats; refuse a recording that cannot be started; record with `run --record`, then replay the result and use its detail tabs.
- **Also new:** a guard that the footer's last key is never cut off, and validation of the recording settings.

## Acceptance criteria that concern this task ([03 §3](03-implementation-plan.md#3-milestones-24))

| # | Criterion | Status |
|---|---|---|
| 3 | Replaying a recording of a scripted session reproduces the live session's final statistics exactly; replay incidents never appear in a live session | **Met** for the first half, on seven scenarios: every stream's statistics over three windows and since start are equal, as are the cards, outcome cells, and events. The second half holds by construction — a replay is a separate process with its own processor and cannot write to a live session — and has no test of its own |
| 4 | A simulated disk-full stops recording, raises the notice, and loses no measurement; a truncated recording replays to its last complete record with a warning | **Met**, with storage that fails on demand. A really full disk was not tried |
| 5 | An export with redaction contains no occurrence of the profile id anywhere | **Met**, by search over the recording file and all three exports of a session whose DoH target URL carried a profile id |

## First recorded live session (3 minutes)

Run on 2026-11-21 with your consent: `pathwatch run --record` in a pseudo-terminal, release build, built-in configuration (Cloudflare DoH pinned), then `export` and `replay` of the result. Exit 0, terminal restored, recording location printed after the dashboard closed.

**The recording.** 1,053 records in one 666 kB segment: 1,011 observations, 38 connection records, 2 resolutions, 1 event, plus the header and the opening record. The header says `redacted: true`. A JSONL export is byte-identical to the file; the CSV has 1,011 rows.

**Volume.** 3.7 kB per second, about 640 bytes per observation — **roughly 320 MB a day** uncompressed at the built-in rates. That is inside the design's 0.3–0.5 GB estimate, which was an assumption until now. It is three minutes extrapolated, on a network where half the observations are short IPv6 local errors, so treat it as a first reading.

**Replay against live.** The live Distribution tab two seconds before quitting and the replay's agree on every figure they share: min 5.9, p50 6.8, mean 8.1, p95 7.8, p99 69.4, max 117.9, sd 9.8 ms, no-reply 1.7 %. The replay shows 177 round trips to the live screen's 175 because it includes the last two seconds. Statuses, the outcome timeline, and the event list match. This is a comparison of screens by eye, not the exact equality the scripted test asserts.

**What the three minutes showed** (per-stream totals from `export --format summary`):

| Stream | Sent | Answered | p50 | p95 | Max |
|---|---|---|---|---|---|
| ICMP v4, Cloudflare | 180 | 177 | 6.8 ms | 7.8 ms | 117.9 ms |
| ICMP v4, Google | 180 | 180 | 5.4 ms | 7.8 ms | 17.9 ms |
| DNS/UDP v4, Cloudflare | 36 | 36 | 9.1 ms | 34.8 ms | 37.7 ms |
| DNS/UDP v4, Google | 36 | 35 | 22.7 ms | 49.5 ms | 52.3 ms |
| DoH v4, Cloudflare (pinned) | 36 | 36 | 7.3 ms | 19.0 ms | 19.5 ms |
| DoH v4, Google | 36 | 36 | 23.1 ms | 53.2 ms | 56.9 ms |

Cloudflare's three unanswered echoes were single probes, not a run, all in the second half of the session; they tripped the "no-reply rate 5 % over the last minute" rule, so its card read `LOSSY` at the end while Google's read `OK`. Three isolated probes in three minutes say very little about either provider. This was also the first live use of the built-in Cloudflare DoH pin, and it worked.

**Found by this run:**

1. **The recording had no closing record** (fixed, with a test). Quitting the dashboard stopped reading from the collector before the collector had finished shutting down, so the session's last record — its counters and traffic totals — and any probe cancelled in flight never reached the file. `sample --record` was not affected. The recording made in this run is the one from before the fix.
2. **One slow reply flattened the chart** (fixed the same day, on your go-ahead). A single 117.9 ms echo set the common scale to 200 ms, which pressed three minutes of 5–8 ms data into the bottom row. The scale now covers the highest round trip of 95 % of the time columns; a peak up to twice that is simply included; anything higher is drawn to the top row with a `▲` in its series' colour, and the title says `▲ off scale, max 117.9`. The data is untouched: the cursor and the Distribution tab still give the value exactly, and a change that lasts more than a twentieth of the window sets the scale as before. Replaying this run's recording now gives a 20 ms scale with four marked columns. The histogram had the same weakness — 168 of 175 samples in one 10 ms bin — and got the same treatment: its bins cover the bulk of the data (to the 95th percentile, or everything when the tail reaches no more than twice as far), and whatever lies above is one open last row, `≥ 7.8 … 8 up to 117.9`. On this recording that gives ten 0.2 ms bins instead of one useful 10 ms bin. Every sample is still in exactly one row.

## Deviations and limits

- **Redaction happens at write time, not at export.** The design redacted recordings on export. Writing them clean is safer — an export cannot leak what is not there, whoever runs it — but it means `export --no-redact` cannot bring an id back. With `[redaction] profile_ids = false` the recording is written as is and its header says so.
- **Recording does not resume by itself, and there is no key to resume it.** After a stop you restart to record again.
- **Replay loads the whole recording into memory.** Fine for hours; a multi-day recording at every-stream rates would be several hundred megabytes. The design's volume estimate (0.3–0.5 GB a day) is still an assumption — nothing long has been recorded.
- **Replay uses the default incident thresholds**, not the ones in the recorded configuration.
- **The schema gained a `segment` record without a version bump.** Nothing outside this repository reads schema 1, and readers skip kinds they do not know.
- **Not built:** `fsync = "interval"`, hostname redaction, and local-address redaction as a setting (use `export --redact-local`). `config check` now rejects these rather than accepting them silently.
- **One real recording exists**, of three minutes. Rotation, the size cap, and anything about long sessions are tested on scripted data only.

## Task 3.6 — incident correlation across streams

Started on your go-ahead, 2026-11-21. No network activity: scripted data, and a replay of the existing recording.

**What it does.** Incidents that open within 10 seconds of each other on comparable streams — same protocol, same address family, collecting on their regular cadence — become one event instead of several. The event says how many of the comparable streams are in it, gives each one's own counts, and then says which comparable streams were *not* affected, with their counts. For example, from a scripted fixture:

> 3 of 4 ICMP v4 streams began going unanswered within 1 s of each other: cloudflare (3 of 60 unanswered), google (3 of 60 unanswered), nextdns (3 of 60 unanswered). Still answering: arelion (0 of 60 unanswered). No gateway or local-DNS control is configured.

- **Counts, never a cause.** The wording is assembled from each stream's own observations in the minute up to its incident. A test rejects words that would claim one ("outage", "ISP", "because", "packet loss", and others).
- **Kinds are not mixed.** Unanswered probes, a latency shift, error responses, and refused sends are grouped separately, each in its own words.
- **One stream is never a group**, however many rules it trips; nor are two streams that fail half a minute apart.
- **A stream already in trouble** before the group formed is named as "already affected", not as unaffected.
- **Pure function of the data.** It is computed from the incident list and statistics as of the displayed time, so a frozen view, a replay, and a live session group identically, and a group appears only once its second member has opened.
- **On screen.** The overview shows the grouped line (cut with `…`); the Events tab shows it whole, for every provider it names.

**On the real recording.** Replaying the 3-minute session, nine events become four: the six IPv6 "sends are failing" incidents at start-up are one line, `6 of 6 IPv6 streams had sends refused by this host within 4 s of each other: …`.

**Acceptance criterion 8** — *a correlated-failure fixture yields one grouped event listing affected and unaffected comparable streams and the control results; its text contains counts and no attribution* — is **met**, the control results since task 3.5 (below). Without controls the event says "No gateway or local-DNS control is configured" rather than leaving it unsaid.

**Deviations and limits**

- **Refused sends are grouped by address family across protocols**, not per protocol as the design says. A host with no IPv6 route cannot send any protocol over it; one line says that better than three.
- **Recoveries are not grouped.** Each stream's "replies resumed" is still its own event.
- **The 10-second window is fixed**, not a configuration setting.
- **Groups are not written to recordings.** Incidents are derived from observations on replay, and so are groups.
- **"Fresh connection" DoH streams are never grouped**: they probe every five minutes and say nothing about a ten-second window.
- **Not seen on real data as a real event.** The only grouping on real data so far is the IPv6 start-up case; no correlated network event has occurred in a recorded session.

## Task 3.5 — gateway and local-DNS controls

Started on your go-ahead, 2026-11-21. Built and tested against loopback addresses and scripted data, then run live once with your consent — see [the live run](#live-run-with-the-controls-on-3-minutes) below.

**What they are.** Two optional streams that are not providers: ICMP echo to the default gateway, and the standard DNS/UDP queries to the first resolver the system lists. They answer one question during an incident — was the local segment still fine? — and are off by default:

```toml
[controls.gateway]
enabled = true
address = "auto"      # or a literal IP

[controls.local_dns]
enabled = true
address = "auto"
```

**Traffic when enabled:** one echo a second to the gateway and one DNS query every five seconds to the resolver, on the same schedule and limits as provider streams.

| Part | Behaviour |
|---|---|
| Discovery | read from this host's own state, never by sending or by timing a lookup. Gateway: `/proc/net/route` and `/proc/net/ipv6_route` on Linux (lowest-metric default route that has a gateway); on macOS the system's `/sbin/route -n get default`, by absolute path with fixed arguments, used only if its output parses as an address. Resolvers: `nameserver` lines of `/etc/resolv.conf`, in order |
| `doctor` | now prints the gateway (IPv4, IPv6) and resolvers it found, or why not. It sends nothing to find them |
| Not found | the control is present, reads `UNSUPPORTED` with the reason (`no default gateway found: …`), and sends nothing |
| Never a card | a control has no provider and no slot. It is one quiet line between the cards and the chart, at the fullest level of detail that fits whole: addresses at 120 columns, figures only at 80. It is not in the chart or the timeline |
| Never compared | a gateway is ICMP v4 like the providers, but is not counted among "3 of 4 ICMP v4 streams" |
| Cited in grouped events | `Controls in the same minute: gateway 60 of 60 answered; local DNS 12 of 12 answered.` Counts only, as for everything else in that sentence |
| Where the resolver is | decided from its address. On a loopback address (`127.0.0.53`, a local forwarder): `Local DNS (stub)` — it measures the stub, its cache included, plus whatever it forwards to. On a private or link-local network: `Local DNS`, no caveat. On a public address: **`System DNS (remote)`**, cited in events as `system DNS (a remote resolver, not on the local network)` — it measures that resolver over the internet and says nothing about the local segment. `doctor` says the same for each resolver it lists |
| Silent gateway | a gateway that has never answered an echo carries a caveat wherever it is cited — some never do, and then it says nothing either way |
| Reserved names | `gateway` and `local-dns` cannot be used as provider keys |

**On this machine and in the Linux container,** discovery (which sends nothing) finds an IPv4 gateway, no IPv6 default route, and the resolvers in `/etc/resolv.conf` — three here, one in the container.

**Deviations and limits**

- **macOS discovery runs a system tool.** There is no routing table in the filesystem there; the proper alternative is a routing socket, which is more `unsafe` code than this was worth today. It is the one place pathwatch starts another program.
- **IPv6 gateways are not probed.** They are almost always link-local addresses, which need an interface scope the ICMP socket does not carry. The control reads `UNSUPPORTED` and says exactly that.
- **One resolver, not all of them.** The first address of each family in `/etc/resolv.conf`. On macOS that file reflects the primary resolver set; per-domain and VPN resolvers known to the system are not consulted.
- **A control that could not run at start stays off.** One that was running follows the host to a new gateway or resolver when the network changes (task 3.4, below); one that found no gateway at start is not started later.
- **Controls have no details tab.** The line on the overview and their citations in events are all there is. They are in recordings and exports like any stream.
- **`doctor --json` does not include the discovered addresses.**

### Live run with the controls on (3 minutes)

Run on 2026-11-21 with your consent: `pathwatch run --record`, release build, the built-in configuration plus both controls on `auto`. Exit 0, terminal restored, 1,272 records (815 kB) recorded. Addresses on your network are left out of this document on purpose.

| Stream | Sent | Answered | p50 | p95 | Max |
|---|---|---|---|---|---|
| Gateway (ICMP v4) | 180 | 180 | 0.6 ms | 0.8 ms | 2.9 ms |
| Local DNS (DNS/UDP v4) | 36 | 36, all `NOERROR` | 35.3 ms | 37.7 ms | 65.2 ms |
| ICMP v4, Cloudflare | 181 | 181 | 6.9 ms | 7.7 ms | 59.4 ms |
| ICMP v4, Google | 180 | 180 | 5.4 ms | 6.3 ms | 8.9 ms |

- **Your gateway answers echo**, in well under a millisecond, so it is a usable control.
- **Your system resolver is not on your local network, and is not a stub.** `/etc/resolv.conf` lists a public resolver address in NextDNS's published range, so the "Local DNS" control here measures that resolver over the internet: slower than Cloudflare's or Google's from this network (p50 35 ms against 9.5 and 22.9), and not evidence about your local segment. It is, in effect, the only measurement of NextDNS the tool has so far. This also fits the Cloudflare DoH hostname being rewritten for this machine, found in milestone 1.
- **No incident occurred**, so no grouped event cited the controls on real data; that wording is still verified only on scripted data.
- **The recording now ends properly**: its last record is the session's closing one, with counters — the fix made after the first recorded session, seen here on real data for the first time.

**Found by this run, all fixed and tested:**

1. **Modestly slower replies were marked off scale** beside one far outlier (a 59 ms echo): the scale went to 8 ms and echoes of 9–16 ms were clipped with it. Values within twice the bulk are now shown whatever else is present; only what lies beyond is marked.
2. **Axis ticks were labelled with rounded numbers that were not their values** (`6` for a tick at 5.7 ms) once the controls line took a row from the chart. Ticks now show their real value. They are exact but no longer round; placing ticks on round values is a polish left undone.
3. **`config check` reported a rate without the controls.** The reported steady-state rate now includes them.
4. **`export --redact-local` left the gateway's and the resolver's addresses in the export** — in observations, in the stream list, and in the recorded configuration. They are now removed wherever they stand, and nothing else is touched (`10.0.0.1` is replaced; the same digits inside `10.0.0.194` are not). On this run's recording: 361 and 73 occurrences before, none after. **A recording itself still contains them** — only an export made with `--redact-local` does not. If that resolver address is specific to your NextDNS profile, treat the recording as you would the profile id.

5. **"Local DNS" was the wrong name for your resolver** (fixed on your go-ahead). The row now reads `System DNS (remote)` when the resolver is a public address, and events cite it as a remote resolver; see the table above. Replaying this run's recording shows it.
6. **In a replay, the controls came back as a fifth and a sixth card** — found by replaying this run's recording to check item 5. A recording's stream list includes the controls, and the replay took every unlisted provider for a card. Controls are now never providers in a replay either, with a test for recordings with and without a header.

The Linux test run failed twice today at the link step. The cause is now known: the linker was killed (`signal 9`) in the 4 GB container when several test programs linked at once. Running with two parallel jobs avoids it; nothing in the code was involved.

## Task 3.4 — source context and network-change handling

Started on your instruction, 2026-11-21. Nothing was sent to find or test any of it: the kernel is asked which source address it would use, and the routing table is read. **No real network change has happened under it** — I cannot change your network, and did not.

**Source context.** Which local address and interface this host sends from, per address family, and through which gateway. It is recorded as an event when a session starts (`source context 1: IPv4 … on en0, gateway …; IPv6 no route`), every observation carries the id of the context it was sent in along with the address and interface, the dashboard header shows the current one (`via … on en0`), and `doctor` prints it. A tunnel-like interface name (`utun`, `tun`, `wg`, `ppp`, …) is flagged as "looks like a tunnel" — a hint from the name, never acted on.

**When it changes.** Every two seconds the collector asks again. If the source address or interface of either family differs:

| What happens | Why |
|---|---|
| ICMP probes in flight are cancelled; a DNS or DoH transaction in flight that then fails to get a reply is cancelled too | a reply may be on a path that no longer exists; it is not "unanswered" |
| One observation gap is recorded, cause `network_change`, from the last look at the old network to the first at the new | that interval is the network being rearranged, not measurement |
| One event says what changed, in words — only the families that did | `source context 2: IPv4 192.0.2.10 on en0, gateway 192.0.2.1 → 198.51.100.7 on utun3 (looks like a tunnel), no gateway found.` |
| No incident opens from it | cancelled probes are not unanswered, and a gap restarts run counters |
| Later observations carry the new context id, address, and interface | so a recording shows which network each probe used |
| The long-lived DoH connection is closed (`the network changed`) and a new one opened; its first request is marked `after_reconnect` and stays out of reused-connection statistics | a connection made on the old network is bound to an address the host may no longer have |
| Controls on `auto` move to the new gateway and resolver as a new destination epoch; with no network they report "no default gateway found" as a local error and send nothing | the two gateways' statistics are never pooled |

**Acceptance criterion 7** — *an interface change in the fixture cancels pending probes, records one gap and one source-context event, and produces no incident from the cancelled probes* — is **met**, on a simulated network under the scripted collector, and the DoH reconnect on real loopback sockets.

**Deviations and limits**

- **Polling, not notification.** The design named a notification crate with periodic re-reading as the fallback; this is the fallback only, every two seconds, with no new dependency. A change is noticed up to two seconds late, and the gap covers that interval.
- **A change that keeps the source address is noticed only through the gateway**, which is re-read every 30 seconds, and only when a control is on `auto`. Without controls, a new gateway behind the same address goes unnoticed.
- **The "failed-send pattern" trigger in the design is not built.** Refused sends still open their own incident, as before.
- **A change is one event however many things moved.** Wi-Fi dropping and a VPN coming up two seconds apart are two gaps and two events.
- **`export --redact-local` now also removes the host's own source addresses from the source-context events**, not only from observations.
- **Not seen on a real network.** What the operating system reports while a network is actually changing — brief "no route" states, address flapping — is unknown to me. The first real change may well produce more events than one.

## Task 3.8 — configuration reload

Started on your go-ahead, 2026-11-21. Tested on scripted data and loopback only; no provider was contacted.

**How to use it.** Edit the configuration file, then press `R` in the dashboard, or send the process `SIGHUP` (`kill -HUP <pid>`; this is also how a headless `pathwatch sample` is told). What happened is in the Events list, or on standard error when headless.

**What a reload does.** The file is parsed and validated in full before anything changes.

| Case | Result |
|---|---|
| The file does not parse or does not validate | Nothing changes. One event: `configuration not reloaded, and nothing was changed: …` with the first problems |
| It changes a setting a running session cannot take | Nothing changes — not even the parts that could have been applied. The event names the settings: `… changes limits.burst, which a running session cannot take: restart to apply it` |
| There is no configuration file | Nothing changes; the event says the session runs on the built-in configuration and where a file would go |
| It is valid and applicable | A new **revision**. One `config` record lists every stream of the revision, one event says what changed in counts (`configuration revision 2: 1 started, 0 stopped, 1 restarted, 14 unchanged`), and every observation from then on is tagged with the new revision |

**What happens to each stream.**

- **Unchanged** — not touched: same schedule, same connection, same sequence numbers, one unbroken history across the reload.
- **Changed** (another address, another interval) — stopped and started again under the same name as a new destination epoch. Its sequence numbers carry on rising and its statistics start afresh: the old and the new definition are never pooled.
- **Added** — started; in the dashboard its card goes from `NEEDS CONFIG` or `DISABLED` to collecting. This is the case reload exists for: adding your NextDNS or Arelion targets without ending a session.
- **Removed or switched off** — stopped. What it measured stays in the session.

**What can be reloaded:** providers and their targets, the controls, the address families, probe intervals and deadlines, and which protocols run. **Everything else needs a restart**, and a reload that touches it is refused whole: limits, DNS queries, ICMP payload, bootstrap, history, recording, redaction, incident thresholds, and the interface settings.

**Recordings and replay.** The `config` record is in the recording, and a replay follows it: the cards and streams it shows change at the moment of the recorded reload.

**Acceptance criterion 6** — *a reload with an invalid file changes nothing and shows the error; a valid reload increments the revision, preserves history of unchanged streams, and tags subsequent observations with the new revision* — is **met**: in the scripted collector, and end to end through the `R` key, `SIGHUP`, and headless `sample` on loopback.

**Found while building it**

- **A closed terminal killed the dashboard before it could close its recording.** With the terminal gone, the first attempt to print to it — the terminal library reporting that it could not restore the terminal — aborted the program. A session being recorded lost its closing record. This predates today: closing the terminal was listed as verified in milestone 2 on the strength of a `SIGHUP` sent while the terminal was still there, which is not the same thing. A hang-up from a closed terminal now ends the session in order, and a test closes a real pseudo-terminal under it to prove it.
- **`SIGHUP` now means two things**, as the design intended: with the terminal still there it reloads; with the terminal gone, or in a demo or replay, it ends the session.

**Deviations and limits**

- **Probes in flight at a reload finish normally**, under the revision they were sent in. The design had them cancelled as "reconfigured"; their results are valid, so they are kept.
- **The revision is not shown in the dashboard header.** It is in the Events list and on every record.
- **A stream that is otherwise unchanged keeps its place in the schedule** even if the reload added streams beside it; the even spacing of probes within an interval is only computed for the streams a reload starts.
- **A control added by a reload does not follow later network changes**; one present from the start does.
- **A protocol this host could not run at start still cannot.** If unprivileged ICMP was unavailable when the session began, a reload does not look again.
- **The NextDNS profile id still comes from the environment at start.** A reload re-reads the file, not the environment of a running process, so `PATHWATCH_NEXTDNS_PROFILE` must be set before `pathwatch` starts.
- **The schema gained a `config` record without a version bump**, like `segment` before it.
- **Not tried on real targets.** The reload that matters most — adding your own endpoints to a running session — has only been done with loopback addresses.

## Task 3.1 — DNS over TLS

Started after your review of the dashboard ("It's fine, carry on"), 2026-11-21, as the next task that sends no new kind of traffic by itself. Built and tested against a TLS server on loopback only. **No provider's DoT service has been contacted**, and the built-in DoT targets remain switched off.

**To use it,** enable a DoT target — the built-in configuration has one for Cloudflare and one for Google, both `enabled = false`:

```toml
[[providers.cloudflare.targets]]
id        = "dot"
hostname  = "one.one.one.one"     # the TLS server name; the certificate is verified against it
port      = 853
protocols = ["dot"]
# addresses = ["1.1.1.1"]         # dial this instead of resolving the hostname
```

A reload picks it up. `m` now cycles ICMP → DNS/UDP → DoH → DoT, and `pathwatch sample --mode dot` restricts a headless run to it.

| Part | Behaviour |
|---|---|
| Reused connection | one long-lived TLS connection per stream; the standard queries every five seconds, each timed from the write to the last byte of its response |
| Fresh cohort | a separate stream every five minutes: TCP connect, a full TLS handshake, and one query — three phases and a total — on a connection that is then closed |
| Framing and IDs | DNS messages with a two-byte length on the TLS stream; a random DNS ID per query, as on UDP (DoH uses 0) |
| TLS | the address is dialled directly and the configured hostname is the server name; verification is never disabled; `dot` is offered as the application protocol and a server that selects none is accepted |
| A server that hangs up | DoT servers close idle connections. Before each query the socket is checked for the server's close, so the query goes out on a new connection — marked `after_reconnect` and kept out of reused-connection statistics — rather than into a dead one |
| No answer | a timeout; the connection is replaced, because a late answer on it would be read as the reply to the next query |
| Shared with DoH | everything up to the end of the TLS handshake, the connection records, static addresses, the certificate errors, the reaction to a network change |
| Traffic accounting | its own kind (`dot`), counted as TCP payload bytes, TLS included |

### Live DoT check (60 seconds)

Run on 2026-11-21 with your consent: `pathwatch sample --mode dot --family 4`, release build, with a temporary configuration enabling one DoT target per provider — Cloudflare dialled at `1.1.1.1` with `one.one.one.one` as server name, Google at whatever `dns.google` resolved to. Exit 0.

| Stream | Sent | Answered | Round trips | Connection |
|---|---|---|---|---|
| Cloudflare DoT, reused | 13 | 13, all `NOERROR` | 5.2–8.6 ms, median 5.7 ms | one, TLS 1.3, for the whole minute |
| Cloudflare DoT, fresh | 1 | 1 | 54.1 ms: connect 32.3, handshake 15.2, query 6.6 | full handshake, then closed |
| Google DoT, reused | 12 | 12, all `NOERROR` | 5.0–33.7 ms, median 7.0 ms | one, TLS 1.3, to `8.8.8.8` |
| Google DoT, fresh | 0 | — | — | not due: see below |

- **Both certificates verified** against the configured names, at the addresses dialled.
- **Neither server selects an application protocol.** The client offers `dot` and both answer with none, which the design accepts; a client that insisted on `dot` would have failed against both.
- **Neither closed its connection in a minute at one query every five seconds.** How long either tolerates an idle connection is still unknown; the hang-up handling is proved only against the fixture.
- **Google's fresh-connection probe was not sent.** Fresh probes are spread across their five-minute interval so that they do not all fire together, and the second one's turn comes at 2.5 minutes. In a 60-second run that is expected, and worth knowing when reading a short sample.
- **DoT is at least as quick as DoH here** (Cloudflare 5.7 ms against 7.3–7.5 ms for DoH in the earlier runs; Google 7.0 ms against 22–23 ms) — one minute, a dozen queries each: an observation, not a finding.

One fix came of it: the summary's traffic heading still named only DoH and DNS/TCP as counted in TCP payload bytes.

**Not built in this pass:** the optional DNS/TCP stream. It was built afterwards — see [below](#task-31-rest--the-dnstcp-stream).

**Limits**

- **One minute of real use so far.** How long Cloudflare's and Google's servers tolerate an idle connection is unknown; the hang-up handling has only met the fixture.
- **No padding** (RFC 7830/8467). The design left it for later; queries are sent at their natural length.
- **One query at a time per connection**; nothing is pipelined.
- **The cards have no DoT sparkline history of their own before DoT is enabled**: a provider with no DoT target reads `UNSUPPORTED (no DoT target)` in that mode, which is accurate and looks sparse.

## Task 3.2 — route tracing

Started on your go-ahead, 2026-11-21, built and tested offline first as agreed. Tested against a scripted path with a paused clock, and end to end against this machine's loopback address. One live trace was then run on your yes (below). Each further live trace needs its own yes.

**How to use it.** In a live session, focus a provider and press `t`. The Route view opens and shows what a trace would send — nothing has been sent yet. `Enter` starts it; `t` stops it; `n` hides or shows names; `Esc` leaves, which also stops it.

**The budget** (from `[trace]`): ICMP echo with TTL 1 to 30, 10 rounds a second apart, at most 300 probes in all and 30 a second. One trace at a time. The view shows probes used against the cap throughout.

| Part | Behaviour |
|---|---|
| Its own socket | each trace opens an ICMP socket of its own, so the measurement sockets never carry a trace's TTL and the latency series never contains a trace probe |
| Finding the destination | once the destination answers at some TTL, later rounds stop there; the first round may overshoot by a probe or two, which are not shown as hops |
| Per hop | TTL, every address that answered with how often, replies of probes sent, last, average, best and worst round trip |
| Only its own answers | matched by sequence number, and by echo identifier and a nonce where available: echoes for the measurement streams, or for another program's ping, are not counted |
| Caps | enforced by the state machine itself: asked twice at the same instant it waits; at the probe cap it stops with `stopped at the probe cap` |
| Reverse DNS | each new address is looked up on another thread, never on the path of a probe; names are untrusted and are cleaned before display |
| A trace that cannot start | says why in the view (for example, no permission to open the socket) |

**What the view says about its own numbers** — each from the counts, after at least three probes per hop, and none naming a cause:

| When | The row says |
|---|---|
| a hop never answers but later ones do | `does not send ICMP Time Exceeded — not evidence of loss` |
| a hop answers only some probes while a later hop answers more | `consistent with ICMP rate limiting at this hop, not forwarding loss` |
| no-reply starts at a hop and is at least as high at every hop through the destination | `no-reply begins at or before this hop and persists` — once, on the hop where it starts |
| several addresses answer at one TTL | `several addresses: per-flow load balancing (ECMP) or a route change` |
| a middle hop usually takes clearly longer than the destination (medians, at least three answers each) | `hop RTT reflects that router's own reply handling and return path` |

Under every trace: that it is an ICMP trace and other traffic may take another path; that rows are separate round trips and must not be subtracted from one another; and that an anycast destination's answering site can change.

**Acceptance criteria 1 and 2** — *a trace never exceeds its probe or rate cap (counted in the fixture transport), stops within one round of `t`, and only one runs at a time; in the fixtures (silent hop, rate-limited hop, persistent loss from a hop, two addresses at one TTL) each annotation appears exactly where specified and none asserts a cause* — are **met**, on the scripted path.

**Found while building it**

- **On Linux, every answer was discarded.** The kernel stamps an arriving reply, and on loopback it had stamped the reply before the tracer read its own clock for "sent" — so each answer appeared to precede its probe and was dropped. macOS did not show it. The send time is now taken before the send, as on the measurement path, and a round trip that comes out a hair below zero is zero, not a stranger.
- **On macOS, pathwatch could report timeouts the network never caused — in the measurement itself, not only in a trace.** macOS hands every ICMP socket a copy of every ICMP message arriving at the machine, other programs' included, and the socket's default receive buffer is 8 KiB: a few dozen messages. Once it is full the kernel drops what arrives, silently, and a dropped answer of ours became a timeout. It surfaced as a test that failed about one run in five: a loopback echo "lost" while a trace ran. The trace was not the cause. Sixteen loopback samplers started together each lost their second or third echo, every time, while `ping` beside them lost none (`ping` asks for a large buffer). The other ping and trace tools running on this machine at the time were adding to every socket's load — which is exactly how pathwatch will be run while it is being compared with them. The socket now asks for 256 KiB. After that: no timeouts with 12, 16, 24 or 32 samplers at once, four rounds each; a test that leaves 60 loopback answers unread and then finds all 60 (43 without the change); ten runs of the whole suite without a failure. **The earlier live runs on macOS were made before this fix**, so a timeout seen in them may have been this and not the network. Linux was not affected: there the kernel gives a socket only its own answers, and the default buffer is larger.

**Deviations and limits**

- **One live trace, IPv4, on macOS only.** See "Live check" below. A trace on Linux has only been to loopback, so time-exceeded messages arriving through the Linux error queue have still not been seen by a trace.
- **IPv6 tracing is untested**, even on loopback.
- **A trace is not recorded.** It is not in a recording, an export, or the event list; it exists in the Route view until the next one.
- **Reverse lookups ask your system resolver** about each hop's address while a trace runs — on your network, that is NextDNS. `n` only hides the names; `reverse_dns = false` under `[trace]` stops the lookups.
- **Leaving the view always stops the trace.** `stop_on_leave = false` is not built and is rejected by `config check`.
- **Dashboard only.** There is no `pathwatch trace` command for a headless trace.
- **A reply counts if it arrives within one round interval** (at least half a second, at most three); later ones are ignored rather than shown as late.
- **A receive buffer that overflows is still silent.** The larger buffer makes it far less likely; if it does happen, nothing tells pathwatch, and the result is still a timeout. `doctor` does not report the buffer size granted.
- **`trippy-core` was not evaluated** (task 3.3). This is the plan's own tracer, which was to be the default either way.

### Live check — one trace to 1.1.1.1 (2026-11-21, macOS)

Run on your yes, from the dashboard in a pseudo-terminal, with one ICMP stream to 1.1.1.1 as the live session and reverse lookups off. The hop addresses are your path and are not recorded here.

| Checked | Seen |
|---|---|
| `t` proposes and sends nothing | the proposal showed the budget and "Nothing has been sent." |
| The trace runs and stops by itself | 10 rounds, state `finished` |
| Budget | 90 probes of the 300 allowed: 9 hops × 10 rounds, with no overshoot past the destination in the first round |
| Real routers' time-exceeded messages are received and matched (macOS) | 6 intermediate hops answered 10 of 10 |
| Silent hops | 2 hops answered nothing; each carried the "does not send ICMP Time Exceeded — not evidence of loss" note |
| Destination | found at hop 9, 10 of 10 |
| The measurement beside it | 0 no-reply of 23 echoes; the trace did not disturb it |
| Leaving and quitting | `Esc` returned to the overview; exit status 0 |

**Found in the live trace**

- **The slow-hop note is set off by a single slow answer.** One hop answered in about 6 ms nine times and 87 ms once; its average came out near 20 ms against the destination's 7.5 ms, and it was given "hop RTT reflects that router's own reply handling and return path". The sentence is true of any hop, but that hop was not slower than the destination — one reply was. The rule compared averages. **Changed on your yes:** it now compares medians, and needs at least three answers from both the hop and the destination; a fixture with that hop's numbers (6 ms nine times, 87 ms once, destination 7 ms) carries no note, and the same hop slow more often than not still does. The average column in the view is unchanged. Not re-run live.

Not covered by this run: IPv6, Linux, the rate-limited, persistent-loss and several-address notes on a real path, stopping a live trace early with `t`, and reverse lookups.

## Task 3.9 — optional mouse support

Started on your go-ahead, 2026-11-21. No network traffic is involved. Tested by drawing frames and by driving the real program in a pseudo-terminal with the byte sequences a terminal sends for the mouse. **Nobody has used it with a real mouse in a real terminal yet** — that is yours to try: `pathwatch demo --mouse`.

**Off unless asked for**, as the design says, because a terminal that reports the mouse to a program stops selecting text with it. Ask with `--mouse` on `run`, `demo` or `replay`, or `mouse = true` under `[ui]` (which `run` reads). Without either, the terminal is never asked about the mouse at all.

| With the mouse | Does | The same from the keyboard |
|---|---|---|
| click a card, a row of the compact table, a name in the chart's legend, or a label in the outcomes panel | focuses that provider | `1`–`4` |
| click the chart or the outcomes | puts the cursor on that time column | `c`, then `h` / `l` |
| click "a: all four" (compact layout) | all four series ↔ the focused one | `a` |
| click a tab in details | opens it | `h` / `l` |
| wheel over the chart or the outcomes | away from you a shorter window, towards you a longer one | `-` / `+` |
| wheel over a list (DNS, Raw, Events tabs; the Route view) | scrolls three rows | `k` / `j` |

That is the design's list (focus, cursor, wheel) plus the tabs. Every action is one the keyboard already had: the tests apply each click and each turn of the wheel and compare the result with the keys', field for field, in all three layouts. Nothing can be started or stopped with the mouse — not a trace, not collection, not a reload.

| Part | Behaviour |
|---|---|
| What the terminal is asked for | button presses and the wheel only (mode 1000, SGR encoding 1006) — not pointer movement, which nothing here uses and which would redraw the dashboard for every cell crossed |
| Where a click lands | each frame reports where its own targets are, from the same arithmetic that drew them; a test checks every target has its provider's name, or its tab's label, written under it |
| Help panel | three lines on the mouse, shown only while it is captured; all 21 lines fit the smallest terminal (80 × 24) |
| While help is open or "Stop collection?" is waiting | the mouse does nothing: both are answered from the keyboard |
| Other buttons, releases, drags | ignored |
| Handing the mouse back | on quit, `Ctrl-C`, a termination signal, a hang-up, `Ctrl-Z` (taken again on `fg`), and a panic (before its message) — each tested; every test that ends the program now also checks the mouse was handed back |

**Deviations and limits**

- **Not tried by hand, and on no particular terminal.** The byte sequences are the standard ones (crossterm reads them), but how iTerm2, Terminal.app, GNOME Terminal, tmux or an SSH session behave has not been seen.
- **Selecting text while the mouse is captured** needs your terminal's own modifier — commonly Shift; Option in iTerm2 and Fn in Terminal.app, from memory and not checked here.
- **No sample inspection by click.** The design's "Enter on the cursor opens the probes in that column" (§10.3) is not built for the keyboard either, so there is nothing for a click to open.
- **No double-click, no drag, no right-click menu, no hover.** A click on the focused card does not open details; `Enter` does.
- **The wheel over the chart changes the window; it does not pan.** There is no panning from the keyboard either.
- **`[ui] mouse` is not reloadable**: it is read at start.

## Task 3.3 — the `trippy-core` evaluation

Started on your go-ahead, 2026-11-21. The plan asked three things of `trippy-core` 0.13.0 as an *optional* second trace backend — can its results be adapted, can a running trace be stopped, and what does Linux ask for in privileges — and then: keep or drop. The plan allowed two days; it took far less, because the answers were in its source and in a short program.

**Outcome: dropped, on your decision, 2026-11-21.** The recommendation was not to add the adapter: pathwatch's own tracer stays the only backend. On your Linux machines it would do nothing without a privilege pathwatch is not allowed to ask for by default, and on any system it cannot keep two promises the Route view makes: that a trace stops when told to, and that it never sends faster than its stated rate. Nothing in pathwatch was changed by this task.

**How it was evaluated.** `trippy-core` 0.13.0 (still the newest release) was fetched into a throwaway crate, `spikes/trippy-core/`, a workspace of its own, so that pathwatch's dependency tree and lockfile never contained it. That crate was deleted after the decision; what it showed is in the table. Its source was read for the three questions, and a small program ran traces to this machine's loopback address only — on macOS as an ordinary user, and in the Linux container as root with Docker's default capabilities, as root without `CAP_NET_RAW`, and as an ordinary user. **Nothing was traced along a real path with it.**

| Question | Answer | Evidence |
|---|---|---|
| Can its results be adapted to the Route view? | **Yes.** Each round hands back every probe with its TTL, the address that answered, and send and receive times; about forty lines fold that into hops | the program's `fold`; a 3-round loopback trace printed as hops on both systems |
| Can a running trace be stopped? | **No.** There is no stop call. With every handle to a running trace dropped, it carried on: 3 rounds done when dropped, 10 done 1.5 s later, and it ended only after all 15 it was built with | source: the loop ends on the round count alone; the program, on both systems |
| …is there a way round that? | **Partly.** One tracer per round, with a flag looked at between rounds, stopped about 135 ms after being asked, and building a tracer costs well under a millisecond. But each new tracer has forgotten how far away the destination is, and probes past it again | the program, on both systems |
| Can it be held to "at most 30 a second"? | **No.** It sends the next probe as soon as it has looked for a reply; nothing spaces them. Two probes left 0.2–0.4 ms apart; with nothing answering, 23 went out inside one 400 ms round. Only the *total* can be bounded, through the largest TTL and the round count | source: no pacing in the send loop; the program |
| How are round trips timed? | By the wall clock, read in the program on both sides. On loopback its median was 440–500 µs; pathwatch's own probe to the same address is about 35 µs by the kernel's stamp and about 440 µs by its own in-program clock, which it keeps only as a diagnostic | the program; an earlier loopback record |
| Linux, ordinary user, ICMP | **Does not work.** "Privileged" mode: `Operation not permitted`. "Unprivileged" mode: fails on every Linux run, root included, with `Protocol not available … set header included` | the program in the container |
| Linux, ordinary user, UDP | **Runs and sees nothing.** 23 probes sent, none answered, no error — indistinguishable from a path that drops everything | the program in the container |
| Linux, ordinary user, TCP | reached the loopback destination; whether it can see the hops in between without privileges was not testable on loopback, and the UDP result suggests not | the program; **not verified** |
| Linux with `CAP_NET_RAW` | ICMP, UDP and TCP all ran | the program, root in the container |
| macOS, ordinary user | "Unprivileged" ICMP, UDP and TCP all ran; "privileged" mode is refused | the program |
| What it costs to carry | 8 crates pathwatch does not have, and a second copy of `socket2` (0.5 beside our 0.6); licences MIT or Apache-2.0 throughout; its documentation says "the public API is not stable" | `cargo tree`, the two lockfiles |

**What adopting it would buy:** UDP and TCP traces, and with privileges the Paris and Dublin strategies — on macOS without root, on Linux only with `CAP_NET_RAW`. That is the open question Q7 in [the references](04-references-and-open-questions.md): whether an ICMP trace is enough for you. If it is not, an alternative that was **not evaluated here** is to teach pathwatch's own tracer UDP probes, which would keep its stop, its rate cap and its kernel timestamps.

**Not done in this evaluation:** a trace along a real path with `trippy-core` (it would send probes and was not asked for); its privileged multipath strategies; IPv6; its unreleased development branch.

**Cleaned up after the decision:** `spikes/trippy-core/` and the Docker volume `trippy-spike-target` were deleted. Cargo's download cache in your home directory still holds the crates that were fetched; that is cargo's own cache and was left alone.

## Task 3.1 (rest) — the DNS/TCP stream

Started on your "Yes, start it", 2026-11-21, and finished the next day; built and tested offline first as agreed. It was built and tested against a resolver fixture on loopback only; afterwards, with your consent, one 60-second live check ran against Cloudflare and Google — see [below](#live-dnstcp-check-60-seconds). The stream is off in the built-in configuration.

**To use it,** two settings, because the built-in targets do not list the protocol:

```toml
[dns.tcp]
enabled  = true
interval = "60s"                  # the default; must be longer than dns.deadline

[[providers.cloudflare.targets]]
id        = "resolver-v4"
address   = "1.1.1.1"
protocols = ["icmp", "dns-udp", "dns-tcp"]
```

A reload picks both up. `pathwatch sample --mode dns-tcp` restricts a headless run to it. The stream appears on the provider's Streams, DNS, Raw and Events tabs as `dns-tcp v4`; there is no card mode for it, and `m` still cycles ICMP → DNS/UDP → DoH → DoT.

| Part | Behaviour |
|---|---|
| Connection | a new TCP connection for every probe, one query with its two-byte length, the response, and the connection closed. Nothing is reused and nothing is retried |
| What is timed | the headline time runs from the start of the connection to the last byte of the answer — connect and query together — because that is what a client that has to use TCP waits for. Connect and query are each recorded as phases beside it. This holds for error responses too |
| Where the parts are shown | each record carries them, so they are in recordings and exports; on the dashboard, the Raw tab's note for the probe reads `connect 20.1 + query 22.3 ms`. The same note now appears for fresh DoT and DoH probes, with the handshake between the two (`connect … + tls … + query … ms`); until now their parts were in the records only |
| Queries | the same scenarios, in the same rotation, as every other DNS stream; a random DNS ID per query |
| Port and address | port 53 at the target's literal address. A target given only as a hostname is refused when the configuration is read |
| Refused | `unreachable (connection refused)`: attempted, no round-trip time, never a timeout |
| No connection before the deadline | a timeout, saying that the connect did not complete, with no connect phase |
| Connected, then no answer | a timeout with its connect time kept, which is how the two are told apart |
| Closed without an answer | `protocol_error`, not a timeout: something did come back |
| Marked truncated over TCP | there is nothing to fall back to, so the answer is judged as it stands and flagged `truncated`. This also applies to the automatic retry after a truncated UDP answer, which used to accept such an answer unjudged |
| Incidents | the same rules as any DNS stream. At one probe a minute, "three in a row unanswered" takes three minutes to say; the rate and latency-shift rules need more samples than a minute provides and so do not fire |
| Grouping with other streams | a stream that probes less often than the 10-second grouping window is not grouped with others. Before this, two once-a-minute streams failing together would have been reported as one event on "1 of 1 unanswered" each. Set to probe every 10 seconds or faster, DNS/TCP streams are compared with each other like any other protocol |
| Traffic accounting | its own kind (`dns-tcp`), counted as TCP payload bytes at the moment the query is written; a refused connection writes no query and is not counted |
| Budget | in the configured rate at one per interval; its size cannot be computed from the configuration, as for DoT and DoH |

**Corrected on the way**

- **`pathwatch doctor` and `pathwatch --version` listed things as "not in this build" that have been built since milestone 1** — DoT, route tracing, the controls, recording and replay, reload, the dashboard. Both lists now name what is in fact absent: source binding, raw-socket ICMP, retries, DoH by GET or through a proxy, session resumption for fresh probes, an explicit bootstrap resolver, UDP or TCP route tracing.
- **`dns.dot.fresh_interval` was not checked** against the 8-second cap on a fresh-connection probe; only the DoH one was. Both are now.
- **A stream-detail note** now reads "A fresh or dns-tcp stream connects anew for each probe; its time includes that." One line of one reference screen changed.

**Tested (6 new):** against the loopback fixture — an answer, with the total equal to the time from connection start and at least connect plus query; SERVFAIL and a blocked answer as error responses timed the same way; a connection left open and unanswered; a connection closed without an answer; an answer marked truncated over TCP; a wrong ID and garbage; one record per slot; the traffic count; and, separately, a refused connection. The plan: off by default, present and silent when listed but not enabled, collecting on its own interval when enabled, in the budget, and the three validation errors. Grouping: two once-a-minute streams are not grouped and the same two at five seconds are — and the test fails with the rule removed. On the dashboard: a row with three samples shows a last value and counts, no median and no percentage; and the Raw tab's note lists the parts for a probe that made its own connection and nothing for one that did not.

### Live DNS/TCP check (60 seconds)

Run on 2026-11-22 with your consent: `pathwatch sample --mode dns-tcp --family 4 --duration 60s`, release build, macOS, with a temporary configuration holding only two targets — `1.1.1.1` and `8.8.8.8`, each with `protocols = ["dns-tcp"]` — and `dns.tcp.interval = "10s"` so that a minute held more than one probe. Nothing else was sent. Exit 0.

| Stream | Sent | Answered | Total | Connect | Query |
|---|---|---|---|---|---|
| Cloudflare `1.1.1.1:53` | 7 | 7, all `NOERROR`, all as expected | 13.8–25.6 ms, median 15.9 | 6.1–17.9 ms, median 7.6 | 7.7–9.7 ms, median 8.3 |
| Google `8.8.8.8:53` | 6 | 6, all `NOERROR`, all as expected | 28.7–50.3 ms, median 32.6 | 6.9–8.4 ms, median 7.1 | 21.7–41.9 ms, median 25.4 |

- **Both resolvers accept DNS over TCP on port 53 from this network.** No connection was refused, left unanswered, or closed early.
- **The total is the sum of its parts**: on every probe the total exceeded connect plus query by at most 4 µs.
- **Traffic matched the queries to the byte**: 494 bytes written for 13 probes, which is the three standard queries in rotation, each with its two-byte length.
- **Connecting took about 7 ms to either.** Cloudflare's first probe took 17.9 ms to connect and was the slowest of its seven.
- **Google took three to five times as long to answer the query as to accept the connection**; Cloudflare's two parts were about equal. Six and seven probes in one minute: an observation, not a finding.
- **The summary printed "2225 unmatched packets ignored".** No ICMP stream ran. The program opens its ICMP sockets whether or not one will, and on macOS such a socket is handed every echo reply arriving at the host — here, those of the other ping tools running on this machine. The count is true and has nothing to do with DNS/TCP; it reads as if it did.
- **The coverage column read 23 % and 20 %**: seven and six probes against the thirty a five-minute window would hold. Expected for a one-minute run.

Medians are the program's own (nearest rank: of six values, the third).

**Fixed afterwards, on your "Yes, fix it" (2026-11-22): ICMP sockets are opened only when a stream needs one.** The collector used to open one per address family at the start of every session. It now opens a family's socket when the first ICMP stream of that family is about to start — at the outset, or when a reload adds one — and not at all in a session that never has one. The same loopback-only run with one DNS stream and no ICMP stream, six seconds long, counted 229 unmatched packets before the change and 0 after, with the other ping tools on this machine still running. A new test checks that nothing is opened while no stream asks, that a reload switching ICMP on opens IPv4 once and never IPv6 for an IPv4-only plan, and that the streams then run; it fails if the socket is opened at the start.

What this does not change: a socket, once opened, stays open for the rest of the session even if a later reload removes every ICMP stream; a session that does have ICMP streams still reads and ignores other programs' replies on macOS, which is where the count comes from; `pathwatch doctor` and the start-up capability check still open a socket for a moment and close it, sending nothing; a route trace still opens its own.

**A false alarm on Linux, and what it was.** The first two Linux runs for this task failed one test that has nothing to do with DNS — the one that closes the terminal under a live session and waits for the dashboard to end. The dashboard had ended. I had started the container with `cargo` as its first process, where every earlier run had a shell there; an orphaned process that exits is collected by the first process, a shell does that and `cargo` does not, and an exited process nobody has collected still answers the test's "are you there?". The test now reads the process's state on Linux and treats exited-but-uncollected as ended; it passes started either way. The program was not changed for this.

**Limits**

- **One minute of real use, over IPv4, to two resolvers.** How NextDNS or an Arelion resolver answer on TCP port 53 from your network is unknown, as is anything over IPv6 or on Linux against a real resolver. No failure of any kind has been seen outside the fixture.
- **A connect that never completes is untested.** Loopback either accepts or refuses at once; that path is shared with DoH and DoT, where it is equally untested by a fixture.
- **The local-DNS control stays UDP only.** The design's traffic table counted a ninth DNS/TCP stream for it; I did not add it, since many home routers answer TCP port 53 poorly and the control exists to be a steady reference.
- **No row note such as "connect + query each"** as in the design sketch: the Streams tab has no notes column. The sentence under the table says it instead, and the Raw tab shows the parts probe by probe. There is no median of the connect time alone on screen.
- **One probe a minute is sparse.** A new stream reads `WARMING UP` for its first twenty probes — twenty minutes — and its percentiles appear after five and twenty answers.

## The rest of milestone 3

Nothing is left unstarted. What remains is use on a real network, listed next.

## What I would do next, in order

1. ~~Your review of the dashboard's look and keys~~ — done on 2026-11-21: accepted as it is.
2. **A real network change while a session runs** — only you can do this: start `pathwatch run`, switch Wi-Fi off and on (or bring a VPN up), and look at the Events tab. That is the test task 3.4 has not had.
3. ~~Your decision on `trippy-core`~~ — done on 2026-11-21: dropped.
4. ~~The optional DNS/TCP stream~~ — built, and checked live for 60 seconds against Cloudflare and Google on 2026-11-22.

# Milestone 2 — status report

**Date:** 2026-11-21 · two slices so far, each started on your explicit go-ahead.

**In one line:** the dashboard is usable end to end — overview, three layouts, five detail tabs, a chart cursor, stop/resume, suspend/resume, a demo mode — and its terminal handling is tested in a real pseudo-terminal on macOS and Linux. It has had **two 30-second live runs** against Cloudflare and Google: the first found six display bugs, the second showed them fixed on real data and found two smaller ones, also fixed. It still needs your eyes on the look and the keys.

Nothing was committed.

## Try it

```bash
cargo run -p pathwatch -- demo
```

That plays the `isolated-failure` scenario with no network activity. `pathwatch demo --list` shows all 13 scenarios; `--speed 4` plays faster; `--at 30` starts earlier. Worth looking at: `isolated-failure`, `correlated-outage`, `dns-only` (press `m`, then `Enter` and `l` twice for the DNS tab), `sleep-resume` (press `+` twice), `needs-config`, `ipv6-missing` (press `f`), `flapping` (`4`, `Enter`, `h` for its events).

`pathwatch run` is the live dashboard. **With the built-in configuration it probes Cloudflare and Google**, at about 5.6 probe transactions a second. It has been run twice for 30 seconds, each time with your consent — see [the live runs](#first-live-run-30-seconds-built-in-configuration) below.

## Keys

| Key | Does |
|---|---|
| `1`–`4` | focus that provider |
| `h` `l` / arrows / `Tab` | previous / next provider; in details, previous / next tab |
| `Enter` / `Esc` | details for the focused provider / back |
| `j` `k` `g` `G`, `PgUp` `PgDn` | scroll a list in details |
| `m` `M`, `f`, `+` `-` | measurement mode, address family, time window |
| `Space` | freeze the view — collection continues, and the header says so |
| `c` | chart cursor; `h` `l` move it, `H` `L` by ten columns; exact values in the footer |
| `P` | stop collection (asks first); `P` again resumes |
| `R` | reload the configuration file; unchanged streams keep running (milestone 3) |
| `t` | route trace to the focused provider: shows what it would send; `Enter` starts, `t` stops (milestone 3) |
| `a` | compact layout: focused series ↔ all four |
| `Ctrl-Z`, `Ctrl-L` | suspend to the shell; redraw |
| `?`, `q` / `Ctrl-C` | help (generated from the same table the keys run from); quit |

## What exists

| Plan task | State |
|---|---|
| 2.1 Terminal lifecycle | **Done.** Alternate screen and raw mode entered and left on quit, `Ctrl-C`, `SIGTERM`, `SIGHUP`, and a panic; `Ctrl-Z` hands the terminal back and `fg` takes it again; refuses to start without a terminal or with `TERM=dumb`; resize redraws |
| 2.2 Theme and glyphs | **Done except motion.** Colour depth detected, palette mapped by table at each depth, monochrome owned by the application, Unicode and ASCII glyph sets. Nothing animates, so reduced-motion has nothing to reduce |
| 2.3 Layout | **Done.** Large (≥ 160 × 45), standard (≥ 120 × 35), compact (≥ 80 × 24), and a one-line request for a larger window below that |
| 2.4 Widgets | **Mostly.** Latency plot (its scale set by the bulk of the data, with off-scale values marked — added after the first recorded live session), outcome timeline on the same axis, cards with sparkline, compact table, histogram, chart cursor. Not built: epoch markers on the chart, small multiples for monochrome (series are marked with their slot digit instead) |
| 2.5 Overview | **Done** |
| 2.6 Details | **Mostly.** Tabs: Streams, Distribution, DNS, Raw, Events. Not built: a Settings tab and a Capabilities screen (`pathwatch doctor` and `config show` cover them from the shell) |
| 2.7 Input | **Mostly.** Everything in the table above. Not built: hide/solo a series, scale cycling, filtering lists, a traceroute screen, reload |
| 2.8 Status and incidents | **Done for single streams.** Five rules — no-reply run, elevated no-reply rate, latency shift, error responses, local errors — each with hysteresis, a cooldown against immediate reopening, and flapping collapsed into one event. Thresholds come from `[incidents]` in the configuration. Not built: cross-provider correlation ("all four at once") |
| 2.9 Snapshot pipeline | **Done** |
| 2.10 Demo | **Done** on all 13 scenarios |
| 2.11 UI tests | **Done for what is built.** See below |

### The detail tabs

- **Streams** — every stream of the provider across protocols and families, with its counters. Below 150 columns the counters move to a second line instead of being cut off.
- **Distribution** — a histogram of round-trip times in the window with fixed, labelled bins (1–2–5 steps, at most twelve rows) over the bulk of the data, and one open last row for a long tail, beside the statistics for the window and since the session started. Unanswered probes have no time and are in no bin; the tab says so.
- **DNS** — per stream, what each configured query returned, grouped by response code or by what happened instead (timeout, unreachable), plus the latest transaction.
- **Raw** — the last 200 observations, newest first: send time, sequence, outcome, round trip, receive delay, scheduling lag, notes.
- **Events** — incidents, gaps, and collector events for this provider and the session.

### Stop is not freeze

`Space` pins the view; probes keep going and the header counts what has arrived since. `P` stops sending: it asks first, the header then reads `STOPPED … nothing is being sent`, and when you resume the whole interval is recorded as one observation gap with a `collection stopped` / `collection resumed` pair of events. Probes already in flight when you stop still get their verdict. Slots inside a stop do not exist: they are not skipped, not timed out, and in no rate.

One limitation: the gap is drawn on the timeline when collection resumes, not while it is stopped. During the stop those columns are blank and the cards go `STALE`.

## Test evidence

141 tests pass on macOS (arm64) and in the Linux container; lint and formatting are clean on macOS.

- **Core (111):** unit tests (81), the scripted collector (11, two new for stop/resume), real loopback sockets (6), scenarios (13).
- **Dashboard (24):** key handling (3), theme and widgets (7), whole-screen rendering (14). Golden screens now cover every tab, the cursor, and help. Every scenario is drawn at nine sizes in four colour/glyph modes and three measurement modes; every tab at three sizes in two modes with the list scrolled to its end. ASCII mode is checked to emit no non-ASCII character on any of them.
- **Pseudo-terminal (6), now in the suite:** quit after using every screen; `Ctrl-C`, `SIGTERM`, `SIGHUP`; a forced panic; `Ctrl-Z` then continue; a too-small terminal and no terminal at all; stop and resume on a live loopback session. Each checks the exit status and that the alternate screen was left, the cursor shown, and the terminal back in canonical mode. The suspend test runs the dashboard under a stand-in shell, because a process with no job-control parent has its `SIGTSTP` discarded by the kernel.

## Bugs found and fixed in this slice

1. **Suspending left the cursor hidden.** After `Ctrl-Z` the shell got a terminal with no cursor. Found by the pseudo-terminal test.
2. **`Ctrl-L` and resume could kill the dashboard.** The library's redraw call asks the terminal for its cursor position and waits; a terminal that does not answer produced "terminal error" and an exit. The dashboard now redraws without asking anything, and a test asserts it never sends that query.
3. **The Streams tab was clipped at 120 columns** — the counters ran off the right edge. This was in the first slice and I had reported the screen as done.
4. **A scrolled list could get stuck at the bottom** (`G` then `k` did nothing). The screen now reports its scroll range and the position is kept inside it.

The seven from the first slice (frozen view showing live state, time axis for young sessions, dotted lines, a quantisation hole, lost sentences on unconfigured cards, ASCII leaks, tag collisions) are unchanged and still covered.

## First live run (30 seconds, built-in configuration)

Run once on 2026-11-21 with your consent, in a pseudo-terminal at 120 × 35, release build, no configuration file, `PATHWATCH_NEXTDNS_PROFILE` unset. Keys pressed during it: mode, details and three tabs, a second provider, family, cursor, quit. It exited 0 with the terminal restored. The dashboard writes no log yet, so everything below is read off captured screens — nothing here can be re-derived from records.

**What it showed about the network** (30 seconds; not a measurement campaign):

| Stream | Seen |
|---|---|
| ICMP v4, Cloudflare `1.1.1.1` | 29 of 29 answered; p50 6.9 ms, p95 18.9 ms |
| ICMP v4, Google `8.8.8.8` | 28 of 29 answered; p50 5.4 ms, p95 18.0 ms. One echo near the end went unanswered — with one run and no log I cannot say whether that was loss on the path |
| DNS/UDP v4 | every query answered `NOERROR` on both; Cloudflare about 9–10 ms, Google 22–66 ms |
| DoH v4, Google | answered, 23–38 ms on a reused connection |
| DoH v4, Cloudflare | every transaction failed certificate verification at `108.61.72.30` — the redirect by your system resolver found in milestone 1. The built-in configuration has no static address for it |
| Everything IPv6 | refused by this host (no route, no address): local errors, nothing sent, not counted as loss |

Both ICMP series began with a first round trip several times the later ones (Cloudflare's was 26.9 ms) before settling; that is typical of a first packet and is why the first seconds read high.

**What it showed about the dashboard** — six bugs, none visible on scripted data:

1. **A stream in known trouble read `WARMING UP`.** Cloudflare DoH failed 4 of 4 and its incident was open, yet the card said `WARMING UP`, because "too few samples to judge" was checked first. An open incident now outranks it.
2. **The "sends are failing" incident closed itself.** On IPv6 the first probe left, every later send was refused, and when that first probe timed out two seconds later it was taken as proof that sends work again. Only a send made *after* the latest failure counts now. This was the reason the IPv6 ICMP cards also read `WARMING UP`.
3. **Events about probes that were never sent had no time** (`--:--:--`). They are now dated from the session clock.
4. **Long event text ran into the panel border.** The overview now ends a cut line with `…`; the Events tab wraps it in full.
5. **`NEEDS CONFIG` ran into the mode label** in DNS/UDP mode (`NEEDS CONFIGDNS/UDP · v4`). The label is shortened or dropped when it does not fit.
6. **Failed DoH connections were missing from the DNS tab** ("nothing finalised yet" beside four failures), and a card with only error responses showed no error count. The failure is now listed against the query its slot was for, with the reason, and the card shows `err 4 / 4`.

Each has a regression test built from what the run showed.

## Second live run (30 seconds, Cloudflare DoH pinned)

Run on 2026-11-21 with your consent, the same way, with one change: a temporary configuration that repeats the built-in Cloudflare targets and adds `addresses = ["1.1.1.1", "2606:4700:4700::1111"]` to its DoH target. Exit 0, terminal restored.

| Stream | Seen |
|---|---|
| ICMP v4, Cloudflare | 30 of 30 answered; p50 7.1 ms, p95 8.6 ms |
| ICMP v4, Google | 29 of 29 answered; p50 5.4 ms, p95 5.8 ms |
| DNS/UDP v4 | every query `NOERROR` on both |
| DoH v4, Cloudflare, pinned | **works**: certificate verified for `cloudflare-dns.com` at `1.1.1.1`; 7.7–8.8 ms on the reused connection, 64.0 ms for the one fresh connection (connect and TLS included) |
| DoH v4, Google | answered; 37.0 ms at the one moment captured |
| Everything IPv6 | refused by this host, as before |

So the pin is what Cloudflare DoH needs on this network, and `1.1.1.1` does serve that hostname. On your decision the same day, **the pin is now the built-in default**: a plain `pathwatch run` dials `1.1.1.1` and `2606:4700:4700::1111` for Cloudflare DoH and never resolves the hostname. The cost is that this stream will not follow Cloudflare if the service moves; removing `addresses` and setting `follow = true` in a configuration file restores resolution. Google's DoH still resolves `dns.google`. The default has been checked offline (`config check` and a test) but has not itself been run against Cloudflare — the second run used the same addresses from a temporary file.

**The six fixes, on real data:** the IPv6 cards and streams read `LOCAL ERR` and stayed that way; every event carries a time; cut event lines end in `…` and the Events tab shows them whole; `NEEDS CONFIG` no longer touches the mode label; the DNS tab lists IPv6 failures as `local error (no route)` against each query. Fix 1 (an erroring stream reading `ERRORS`) had nothing to show, because with the pin nothing errored — it is covered by its test only.

**Two more found, fixed, and tested:** a DoH URL as a card's target ran into the other-family marker (now cut with `…`), and the event list's stream column was one character too narrow for `cloudflare dns-udp v6`.

## Acceptance criteria ([03 §3](03-implementation-plan.md#3-milestones-24))

| # | Criterion | Status |
|---|---|---|
| 1 | Same observations → same statistics at any render rate | **Met** |
| 2 | A reviewer can name the protocol, failing target, and stale data within five seconds | **Accepted by you** on 2026-11-21, after running the demo: "It's fine, carry on." No changes to the look or the keys were asked for |
| 3 | Pending, timeout, local error, unsupported, needs-config, and gap are visibly different in every colour mode | **Partly.** Distinct glyphs in Unicode and ASCII and distinct statuses in truecolor and ASCII-monochrome are tested; 256- and 16-colour screens are only checked not to panic |
| 4 | Unconfigured cards show `NEEDS CONFIG`, no percentage, and nothing is sent | **Met** |
| 5 | No segment across a missing sample or a gap; a single spike survives downsampling | **Met.** The cursor reads `no reply`, never 0, on such a column |
| 6 | Terminal restored after quit, `SIGINT`, `SIGTERM`, forced panic, terminal close | **Met** for quit, `Ctrl-C`, `SIGTERM`, and a forced panic, on both platforms. **Correction:** this row first claimed terminal close on the strength of a `SIGHUP` sent with the terminal still open. A really closed terminal aborted the program until it was fixed and tested during milestone 3 (see [milestone-3-status.md](milestone-3-status.md), task 3.8) |
| 7 | Resizing never panics or clips silently and preserves focus, window, and freeze | **Mostly.** No panic at any size; view state does not depend on size. A live resize is still only exercised by hand |
| 8 | `Space` freezes while collection continues, and says so; stop needs confirmation | **Met** |
| 9 | With reduced motion, frames differ only when data differ | **Trivially true**: nothing animates |

## Not used yet

In `[ui]`: `color` and `glyphs` are applied; `mouse` is applied since milestone 3 (task 3.9); `theme` and `reduced_motion` since milestone 4 (task 4.1); `max_fps`, `idle_fps`, `mask_secrets`, and `[ui.keys]` are accepted but have no effect.

## What I would do next, in order

1. **Your review of the look and the keys**, on `pathwatch demo`.
2. A longer live session of your own (`pathwatch run`), now that all three protocols work for both providers on IPv4 by default.
3. Plan task 1.12, which needs your Arelion router address and resolver city, and the NextDNS profile in `PATHWATCH_NEXTDNS_PROFILE`.
4. Then milestone 3 (logging to disk and replay), or the smaller milestone-2 leftovers above if you want them first.

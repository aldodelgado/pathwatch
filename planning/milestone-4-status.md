# Milestone 4 — status report (tasks 4.7, 4.1, 4.4 and the offline half of 4.2)

**Date:** 2026-11-22 · started on your "Start milestone 4".

**In one line:** three of milestone 4's seven tasks are built and tested on macOS and Linux — `sample --assert` (4.7), the accessibility pass (4.1) and the documentation (4.4). All were built and tested on loopback and on synthetic data only; nothing was sent to any provider. The performance budgets that need no network have been measured on this Mac (task 4.2, in part): three defects were found and fixed, memory was brought within its budget, and two budgets — live CPU, and memory growth over the first day — were restated on your decision. The rest — the live half of 4.2, the soak, the release pipeline and installation — each needs a decision, a long live run, or one of your Linux machines — see [the rest of milestone 4](#the-rest-of-milestone-4).

Nothing was committed.

## Try it

```sh
pathwatch demo --theme high-contrast          # no network activity
pathwatch demo --reduced-motion               # no network activity
pathwatch glossary                            # every word and number, explained; no network activity
# In the dashboard: ? for the keys, ? again for what the words and numbers mean.

# These two send probes to your configured targets (Cloudflare and Google by default):
pathwatch sample --duration 30s --format plain
pathwatch sample --duration 30s --quiet --assert 'no-reply <= 1%' --assert 'cloudflare: p50 < 40ms'; echo "exit $?"
```

## Task 4.7 — `sample --assert`

The opt-in way to turn what was measured into an exit status, for a scripted health check. Without it a zero exit still says only that the program ran.

**An assertion** is `[<streams>:] <metric> <comparison> <value>`, and `--assert` can be given more than once.

| Part | What it can be |
|---|---|
| Streams | a pattern over stream ids, where `*` matches anything: `cloudflare`, `*/icmp/4`, `*/dns-udp/*`, `*/fresh`. A pattern that stops at the end of a part of the id covers everything below it. Left out, it means every stream that ran |
| Metric, as a count | `sent`, `replied`, `no-reply`, `error-responses`, `local-errors`, `skipped` |
| Metric, as a share | `no-reply` and `error-responses`, when the value ends in `%` |
| Metric, as a time | `min`, `mean`, `p50`, `p95`, `p99`, `max`, with the value in `ms` |
| Comparison | `<`, `<=`, `>`, `>=`, `==` |

It is checked against every stream it selects, when the sample ends, over the same window as the summary (`--window`, five minutes unless set).

| Situation | What happens |
|---|---|
| Every check holds | exit 0; each is listed on standard error with what was measured (not listed with `--quiet`) |
| A check is false | exit 10; it is listed with what was measured and what was asserted, with a count of how many checks did not hold |
| A check cannot be evaluated | exit 10 as well, listed as `NO DATA` with the reason. A check that could not be made does not pass |
| No finalised probe on the stream | `no-reply`, `error-responses` and every time are `NO DATA`. `sent`, `replied`, `local-errors` and `skipped` are always known |
| Too few samples | the same gates as the summary and the dashboard: a percentage needs 20 finalised probes, `mean` and `p50` 5 answers, `p95` 20, `p99` 100. Below that it is `NO DATA`; a count can be asserted instead |
| The pattern selects no stream, or only streams that will not run | refused with exit 2 before anything is sent, listing the streams and why they will not run |
| It cannot be parsed, or a time is given without `ms`, or a count with `%` | refused with exit 2 before anything is sent |
| Neither `--duration` nor `--count` is given | refused with exit 2: a check has to end by itself to have a result |
| The sample is interrupted, or its output is closed early | the assertions are not checked, and it says so. The exit status is the signal's (130, 143) or 5 — never 0 |
| A pattern also selects streams that did not run | the ones that ran are checked, and a note names the ones that were not |

**Tested (7 new):** parsing, with eleven kinds of mistake each answered with what to write instead; patterns; counts, shares and times judged against statistics built for the purpose; every way a check can lack data. Through the command itself, against a loopback port where nothing listens: a true check exits 0, a false one 10 with the summary still written, a check without data 10, five kinds of unusable assertion are refused with exit 2 and not one record written, and an interrupted sample exits 130 saying its assertions were not checked.

**Limits**

- **Never run against a real provider.** The checks themselves are arithmetic on the same statistics the summary prints.
- **One comparison per assertion.** There is no "and" or "or"; several `--assert` options are an "and".
- **Streams added by a configuration reload during the sample are not checked**, as they are not in the summary either.
- **`==` on a time or a share compares exactly.** It is there for counts.
- **A short sample of DNS streams cannot assert a percentage.** At one query every five seconds, twenty finalised probes take 100 seconds. That is the same rule that makes the dashboard show "2 / 12" instead of a percentage, applied where it is less convenient.

## Task 4.1 — accessibility pass

| Plan item | State |
|---|---|
| High-contrast theme | **Built.** `--theme high-contrast`, or `theme = "high-contrast"` under `[ui]`. Providers are bright yellow, cyan, green and magenta in bold; text is white on black, set explicitly in every cell so it does not depend on the terminal's own colours; nothing is dimmed; warning and failure badges are filled grounds (black on yellow, white on red), which no provider's colour is. It uses only the sixteen colours every colour terminal has |
| Monochrome theme | Existed (`--color never`, `NO_COLOR`). `theme = "mono"` now selects it too. Where colour has been refused, high contrast does not override it |
| ASCII mode | Existed (`--ascii`). The new help page is held to it: its outcome marks are the ones ASCII mode draws |
| Reduced motion | **Built.** `--reduced-motion`, or `reduced_motion = true`. Frames drawn only because the data changed are held at least a quarter of a second apart, so numbers and the chart change at most four times a second instead of ten. A key is answered at once. Nothing in the dashboard animates, so there was nothing else to reduce |
| Colour-independent status | Existed: a symbol and a word. Now also checked in high contrast, on every screen |
| A plain-text summary usable with a screen reader | **Built as a new format,** `sample --format plain`: the same numbers as the summary, as sentences. No table to keep one's place in, no symbol standing for a word, nothing outside ASCII, no colour. A withheld number says how many samples it has and how many it needs |
| Help text for every metric | **Built.** In the dashboard, `?` shows the keys and `?` again shows one line for each of 38 words, numbers and marks; `j` `k` `g` `G` scroll it where it does not fit. `pathwatch glossary` prints the full explanations as plain text, `--markdown` as a document |

**One table behind all the explanations.** The help page, `pathwatch glossary`, and the documentation still to be written (task 4.4) read the same table. The numbers quoted in it — "needs 20 answers", "5 probes in a row" — are taken from the constants the statistics and the incident rules use, and a test holds the status words against the ones the dashboard can print.

**Changed on the way**

- **The help key now turns pages**: closed, keys, meanings, closed. `Esc` closes it from either page. Two reference screens changed by the new hint in the panel's title and the wording of the `?` line.
- **`[ui] theme` is checked** when the configuration is read; any value used to be accepted and ignored.

**Tested (13 new):** the glossary table (every term fits the help panel, the status words match, the thresholds quoted are the ones in force, both renderings carry every term); the help page on a tall terminal, scrolled through at 80 × 24, and in ASCII; the help key's cycle and what the list keys scroll; high contrast on three scenarios, three sizes and five screens — the same characters as the default palette, no cell left to the terminal's colours, none dimmed, only the sixteen colours; the palette's own rules; how the option is resolved against a refusal of colour; the pacing rule; the plain format and the glossary command through the program; and the dashboard started in a pseudo-terminal with both options, checking the colour codes it actually writes and that a key is answered promptly.

**Limits**

- **Not tried with a screen reader.** The plain format and the glossary were written for one and checked for form — ASCII, sentences, no table — not listened to.
- **The dashboard itself remains unusable with a screen reader.** It redraws cells of a grid; the plan defers anything more ([03 §7](03-implementation-plan.md#7-deferred-work)). `sample --format plain` and `pathwatch glossary` are the accessible way in.
- **High contrast has not been looked at in a real terminal.** What the sixteen colours look like is the terminal's choice; contrast ratios were not measured.
- **The four-a-second limit is proved as a rule, not by counting frames**: the rule is unit-tested, and the pseudo-terminal test checks that the screen still moves and that keys are not held back.
- **Still accepted and without effect in `[ui]`:** `max_fps`, `idle_fps`, `mask_secrets`, and `[ui.keys]`. Per-provider colour overrides are not built. (Task 4.4, below, found more of these elsewhere and made setting one print a warning.)

## Task 4.4 — documentation

Started on your "Yes, continue", 2026-11-22. Nothing was sent to any provider; the Linux checks ran in containers on this Mac.

| Plan item | What exists | How it is kept true |
|---|---|---|
| README | [`README.md`](../README.md), rewritten for someone who has not read the plan: what it is, building it, first steps, what each command sends, how to read the screen, configuration, exit codes, accessibility, what is not built | a test checks that every file it links to exists, that every command it names is one the program has, and that the default traffic it states is the default plan's |
| Measurement glossary, generated from the metric dictionary | [`docs/glossary.md`](../docs/glossary.md) | it is the output of `pathwatch glossary --markdown`, compared whole |
| Capability and privilege guide | [`docs/capabilities.md`](../docs/capabilities.md): what each probe needs, Linux's `ping_group_range` and its remedy, what `doctor` prints line by line, what is sent and where, what is kept | checked against `doctor` on the host running the tests: every line `doctor` prints is one the guide explains, and ICMP is available exactly when the guide's rule says so |
| Configuration reference | [`docs/configuration.md`](../docs/configuration.md), also printed by `pathwatch config reference` | checked setting by setting: every setting the program has is a row and no row is of a setting it lacks, every default shown is the default in force, the "reload" column matches what a reload applies, and "no effect" is said of exactly the settings that have none |
| Man page | [`docs/pathwatch.1`](../docs/pathwatch.1), printed by `pathwatch man` | generated from the same definitions that parse the command line, so it cannot list an option that does not exist; compared whole. No dependency was added for it |

The checks were themselves checked: a wrong default, a deleted row, a wrong "reload" entry and a false description in the reference each failed the test.

**Found while writing it, and what was done**

- **Thirteen settings are accepted and change nothing**, plus a provider's `color`. Reading the code for each setting showed it: `general.default_mode`, `default_family` and `default_window`; `limits.max_concurrent_dns`; `bootstrap.servers`; `ui.max_fps`, `idle_fps`, `mask_secrets` and the whole `[ui.keys]` table; and the whole `[logging]` table — no log file is written at all. Earlier notes named four of them. They are now listed in one place in the program; the reference marks each; **setting one to something other than its default prints a warning** instead of being silently ignored. None was made to work: that is a decision for each, not documentation.
- **The built-in example file described things that are not built.** Its first lines said "Nothing reads this file yet", though it has been the built-in configuration since milestone 1 and is what `pathwatch config example` prints; it promised an environment layer of settings that does not exist, that keys can be remapped, that a refused interface binding makes streams `UNSUPPORTED`, and offered `raw`, `get`, `env` and `allow-resumption` as values the validator refuses. The comments now say what is true, and a test holds the "no effect" ones against the program's list. No value changed.
- **`doctor` had no line for DNS over TCP or DNS over TLS**, and its `--json` option no help text. Both added.
- **Running as root does not make ICMP available on Linux.** With the range at `1 0`, `doctor` as root still reports `UNSUPPORTED`: the build has no raw-socket mode. The guide says so; the example file's old comment about a `raw` backend that "needs privilege" suggested otherwise.

**Checked on:** macOS as an ordinary user; Linux containers as root and as user 1000, each with the group range open and with it at `1 0` (set for that container only). The guide lists them, and what was not checked.

**Tested (8 new):** the six document checks above, the warning for a changed no-effect setting, and the escaping of text in the manual page.

**Limits**

- **Not checked on your Ubuntu or Debian machines**, on x86_64, or on Linux outside a container — the plan's acceptance criterion says "each tested host", and those are the hosts that matter. Run `pathwatch doctor` there and compare with the guide.
- **Which distributions enable unprivileged ICMP is still unconfirmed**, and the guide says so.
- **The manual page has no date** and `mandoc -Tlint` warns of that and of long source lines; it reports nothing else.
- **The prose was written by me and read by nobody else.** The tests hold facts — keys, defaults, options, lines — not whether an explanation is clear.
- **The planning documents were not rewritten.** Where the design still describes something as planned that the build does differently, the status reports and `docs/` are the current account.

## Task 4.2 — performance validation, the part that needs no network

Started on your "Yes, measure them", 2026-11-22. Every destination was this machine's loopback; nothing was sent to any provider.

**This is not the plan's validation.** The plan's main workload (W1) is an hour of live probing of all four providers, and its second environment is one of your Linux machines. What was measured here is W1's shape and rate — 34 streams, 12.4 transactions a second: four providers, both families, ICMP, DNS over UDP and DoH, both controls — with every destination on loopback, on this Mac only, in runs of five minutes. Loopback answers ICMP in a tenth of a millisecond and refuses DNS and DoH at once, so no TLS handshake and no real answer is in any figure.

**Environment (R1):** Mac mini (Mac16,10), Apple M4, 10 cores (4 performance, 6 efficiency), macOS 26.7.1, on mains power, release build. The dashboard ran in a pseudo-terminal read by the test, so no terminal emulator's drawing time is in any number. The machine was not idle: its load average was between 1.8 and 3.7 throughout, and other ping tools were running.

### Results, after the fixes below

| Budget | Limit | Measured | |
|---|---|---|---|
| Start-up: first whole frame | ≤ 300 ms | at most 31 ms (demo) and 35 ms (live), over four runs of 15 starts | within |
| Shutdown: terminal restored | ≤ 100 ms | at most 5 ms | within |
| Shutdown: process gone | ≤ 3 s | median 1–3 ms; one exit in 15 took about a second, twice, unexplained | within |
| Key written to the terminal → changed output read back | 95th percentile < 100 ms | median 0.9, 95th percentile 1.1, maximum 1.6 ms of 200 keys | within |
| Of which the application's share | 95th percentile ≤ 50 ms | not timed separately; it is inside the 1.1 ms above | within |
| CPU, live view | mean ≤ 3 % of one core, restated from 2 % | 1.9 % to 2.5 % in five runs of 40 s to 5 min | within the restated limit |
| CPU, frozen and idle | mean ≤ 1 % of one core | 0.87 % to 0.93 % in four runs | within |
| Terminal output, live | mean ≤ 20 kB/s | 1.1 to 1.4 kB/s | within |
| Terminal output, frozen | ≤ 2 kB/s | 0.08 kB/s | within |
| Probe scheduling lag | 99th percentile ≤ 5 ms, maximum ≤ 50 ms | 99th percentile 3.3 to 3.6 ms, maximum 7.9 to 20.8 ms, of 1,493 to 3,731 probes | within |
| Receive loop lag | 99th percentile ≤ 2 ms | replies waited 0.42 ms at the 99th percentile, 2.38 ms at most, of 1,080 — see below | within, **by a different measure** |
| Traffic against the configured budget | within ±10 % | at most +0.7 % | within |
| Resident memory | ≤ 64 MB at any time in 24 h | 13.3 MB at the start and 14.2 MB after ten minutes (15.9 MB at most), once records were made smaller; the history itself comes to 42 MB after a day — see below | within for ten minutes; **about 55 MB projected for a day** |
| Growth after hour 25 | ≤ 2 MB, restated from "hour 1 to hour 24" | not measured: it needs the soak, run past hour 25 | not measured |

### Three defects found by measuring, and fixed

1. **A snapshot's cost grew with the length of the session.** Every snapshot — ten a second — walked each stream's whole history several times and sorted all of it for the since-start statistics. One snapshot of the 26-stream workload cost 0.9 ms after ten minutes of history, 5.2 ms after an hour, 11.5 ms after six, and **32 ms after a day: about a third of a processor core, for as long as the dashboard stayed open.** It showed first as the frozen view using more CPU than the live one, simply because it was measured later.
   Now a window's samples are found by position; a few facts each status needs (has anything been sent, has anything been answered, have twenty been finalised) are kept as results arrive; and computed statistics are remembered until the samples behind them change. The same snapshot costs **0.1 ms after ten minutes and 0.7 ms after a day.** The DNS tab's tally no longer formats every record of the last hour for every snapshot.
   *How it is known to be the same answer:* two tests compare the new lookups with a plain walk of the whole history on every scripted scenario — gaps, a host suspend, late and duplicate replies, a sequence wraparound, a change of destination — at 41 moments each, live and in the past, over six window lengths, and again after every single message: 31,464 comparisons and more, all equal. The reference screens did not change.
2. **A frozen view was redrawn ten times a second** though it has only a clock and a count to show. It is now redrawn once a second, and the thread that makes snapshots is woken at once when the view changes or the session ends — so a key is not held back (tested), and leaving no longer waits for a tick: the process is gone 1 to 3 ms after `q` instead of 105 ms.
3. **Ending a session waited for any DNS query still in flight** — up to its deadline: 3 seconds by default, 8 for a fresh-connection probe. So leaving was slowest exactly when a resolver had stopped answering, and could exceed the 3-second budget. Loopback could not show this, since it refuses at once; a test with a resolver that never answers did: 5.0 s to end with a 5 s deadline. In-flight DNS, TCP, DoT and DoH transactions are now given up at the end of the session and recorded as cancelled, as ICMP probes already were. The test now ends in 0.6 s.

### Two budgets restated on your decision, and one brought within its limit

- **Live CPU sits at the 2 % limit, and over it on a busy machine.** Collection alone costs about 0.8 % for this workload; drawing ten frames a second costs most of the rest, though building a snapshot (0.1 ms) and drawing a frame (0.05 ms) are small: it is spread across many short wake-ups, not in one place I could find with a profiler. With `--reduced-motion` (four frames a second) it measured 1.55 %. On Apple silicon the figure also depends on which kind of core the system runs the process on. I recommended keeping ten frames a second and setting this budget to 3 %. **Restated to 3 % on your "Yes, restate them", 2026-11-22**, in the plan and in the measurement test, to be confirmed on the real workload, where TLS adds cost and healthy DNS removes some.
- **Memory: over its budget when measured, and now within it — on your "Yes, reduce it".** Ten minutes of the live run could not show the problem. Counting every allocation while a day of history was built for the 26-stream workload showed it: **64.0 MB for the history alone from the first hour on**, of the 64 MB the whole program is allowed. 33 MB of that is the compact samples for 24 hours, set aside at the start; 31 MB was an hour of full records, each 672 bytes before the strings it points to. (I first reported "117 MB after a day". That was the resident size of the test process, its own scratch memory included, and it overstated the history by almost half. The exact count replaces it.)
  A full observation is mostly things no view reads back: which session and stream it belongs to, the backend's name, the details of the connection. The history now keeps what the views do read — 144 bytes instead of 672 — and holds once what repeats from record to record: where it was sent from, what was asked. **The history is 41.9 MB after a day instead of 64.0**, flat from the first hour to the twenty-fourth, and the live run grew by 0.9 MB in ten minutes where it had grown by 6. Recordings on disk are written from the full observation and are unchanged.
  *How it is known that nothing was lost:* a test compares every kept record with the observation it came from, field by field, and checks that one source and one copy of each query stand behind all of a stream's records; the Raw tab, the DNS tab and the summary read the kept record, and their reference screens and tests did not change.
  With about 13 MB in use before any history exists, that projects to **about 55 MB after a day** — within 64, with 9 MB to spare for what the projection cannot see, such as memory the allocator holds on to. Only the soak (task 4.3) measures that.
- **"No more than 2 MB of growth from hour 1 to hour 24" cannot hold as written.** The compact samples are kept for 24 hours, by design and by default, so the program fills about 1.4 MB of them every hour until the twenty-fourth and none after. The budget was meant to catch a leak. I recommended restating it as no more than 2 MB of growth after hour 25, which is what bounded history promises. **Restated on your "Yes, restate them", 2026-11-22**, in the plan's budgets and in milestone 4's acceptance criteria. A 24-hour soak cannot show it: the soak has to run past hour 25.

### One budget measured differently from the plan

The plan measures "receive loop lag" by the lateness of a heartbeat timer. That check is now counted (twenty times a second, and how often it is more than 2 ms late) and printed in every summary: it was more than 2 ms late in **26 % to 35 % of checks** on this Mac, with a worst case of 6 to 19 ms. That is mostly how late macOS wakes a sleeping thread, not anything the receive loop does: replies themselves waited 0.42 ms at the 99th percentile between the kernel's stamp and this program's read. The measurement test judges the receive loop by that wait and reports the timer figure without judging it. What timer lateness does affect is when a probe is sent, and that has its own budget, which holds.

### How to repeat it

```sh
cargo test --release -p pathwatch --test terminal perf -- --ignored --nocapture --test-threads=1
cargo test --release -p pathwatch-core --test perf -- --ignored --nocapture
cargo test --release -p pathwatch-tui --test render perf -- --ignored --nocapture
```

Loopback only, about five minutes at the default length (`PATHWATCH_PERF_SECONDS=300` for the lengths used here). Each prints `PERF |` rows and fails where a budget it measures is exceeded. This is what to run on your Linux machine for the second environment.

**Tested (6 new, besides the measurements):** the two comparisons of fast lookups against a full walk; a frozen view changing a few times in 2.6 seconds, not twenty-six, and still answering a key, unfreezing and quitting at once; ending a session with a UDP query and with a TCP query in flight; every kept record against its full observation.

**Limits**

- **Loopback is not a network.** No TLS handshake, no DNS answer, no real round trip is in any figure; the real workload may cost more or less.
- **Five minutes, not an hour; one machine, not two.** Nothing was measured on Linux. The container on this Mac is not the plan's second environment.
- **The machine was busy.** CPU figures moved by half a percentage point between runs.
- **No terminal emulator.** How long your terminal takes to draw is not in the key-to-screen figure.
- **A 24-hour window is still costly to look at after a day**: 8 ms a snapshot, down from 56, which is about 8 % of a core while that window is on screen.
- **One exit in fifteen took about a second**, in two separate runs, and I did not find why. It is within the 3-second budget.
- **The memory figure for a day is a count of allocations in a test, plus a projection.** What the running program's resident size is after 24 hours is for the soak (task 4.3) to measure.
- **The compact samples were not made smaller.** At 32 bytes each they are now four fifths of the history. They could be 24, saving about 8 MB; that was not needed to meet the budget and was not done.

## Test evidence

274 tests pass on macOS (arm64) and in the Linux container; lint and formatting are clean on macOS.

## The rest of milestone 4

| Id | Task | What it needs |
|---|---|---|
| 4.2 (rest) | Performance validation on the real workload and the second environment | The offline half is done, above. Still needed: an hour of live probing of all four providers (your NextDNS and Arelion details, and your yes); and the measurement commands run on one of your Linux machines |
| 4.3 | 24-hour soak, with a 7-day one as supplementary evidence | A day of live probing on a machine left running: your yes, and your choice of machine. A fast replay loop for the memory bounds can be done here, offline Since the growth budget was restated, the soak must run past hour 25 to show it: thirty hours, not twenty-four |
| 4.5 | Release pipeline: tagged builds, artefacts, checksums | A git repository and where it is hosted — the workspace is not a repository, and I have not made it one. The final name |
| 4.6 | Installation: `cargo install`, tarballs, a Homebrew tap | The same, plus the clean machines to test on |

What I would do next: stop for your decisions — the live hour, the soak, and where the code is to live.

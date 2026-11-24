# pathwatch

[![ci](https://github.com/aldodelgado/pathwatch/actions/workflows/ci.yml/badge.svg)](https://github.com/aldodelgado/pathwatch/actions/workflows/ci.yml)

A terminal instrument that continuously measures up to four DNS and network
providers — Cloudflare and Google out of the box, NextDNS and Twelve99/Arelion
once you give it their endpoints — and shows, with stated definitions, which
one is misbehaving, on which protocol and address family, and since when.

It replaces keeping `ping`, `prettyping`, `gping` and a route tracer open side
by side: one dashboard, Vim-style keys, macOS and Linux, no root.

<!-- The screen below is crates/pathwatch-tui/tests/golden/standard-120x35-isolated-failure.txt:
     what `pathwatch demo isolated-failure` draws in a 120×35 terminal, without its colours. -->
```text
 pathwatch ▸ DEMO   mode ICMP echo   family IPv4   via 192.0.2.10   up 01:30
 window 1 m · PLAYING · ICMP: unprivileged datagram socket           [DEMO: synthetic data · scenario isolated-failure]
╭─ 1 Cloudflare ─────────────╮╭─ 2 Google ─────────────────╮╭─ 3 NextDNS ────────────────╮╭─ 4 Arelion ────────────────╮
│ ● OK             ICMP · v4 ││ ● OK             ICMP · v4 ││ ● OK             ICMP · v4 ││ ✖ NO REPLY       ICMP · v4 │
│ 1.1.1.1                    ││ 8.8.8.8                    ││ 203.0.113.28               ││ 198.51.100.7               │
│  12.0 ms    last 1.0 s ago ││  18.2 ms    last 0.7 s ago ││  22.8 ms    last 0.5 s ago ││  31.7 ms     last 30 s ago │
│ p50 12.1  p95 12.5  Δ 0.3  ││ p50 18.2  p95 18.8  Δ 0.5  ││ p50 24.1  p95 25.0  Δ 0.7  ││ p50 30.8  p95 32.7  Δ 1.3  │
│ no-reply 0.0 %      0 / 60 ││ no-reply 0.0 %      0 / 60 ││ no-reply 0.0 %      0 / 60 ││ no-reply 48.3 %    28 / 58 │
│ n 60 · coverage 100 %      ││ n 60 · coverage 100 %      ││ n 60 · coverage 100 %      ││ n 30 · coverage 100 %      │
│ ▂▇▁▅▇▅▄▆▄▅▆▅▅▆▅██▅▄▇▄▇▆▇▆▄ ││ ▂▆▄▂▁▅▄▃▃▄▄▄▃▁▃▆▄▇▃█▃▅▅▅▁▄ ││ ▅▄▄▄▅▆█▂▆▆▆▆▇▅▂▁▅▅▆▁▆▃▃▅▅▄ ││ ▆▁▅▄▆▁▅▄██▇▇▆××××××××××××× │
╰────────────────────────────╯╰────────────────────────────╯╰────────────────────────────╯╰────────────────────────────╯
╭─ Latency · ICMP echo RTT · IPv4 · common scale, ms ───────────────────────── Cloudflare · Google · NextDNS · Arelion ╮
│  40 ┤                                                                                                                │
│     │ ⢠⠤⢤ ⢀⣀⣠⢤⣀⣀⣀⣀ ⢠⠤⠤⢤⣀⣀ ⢀⣀⣀  ⢠⠤⢤⣀⣠⠤⢤⣀⣠⠤⠤⢤⣀⣀   ⢠⢤ ⢠⠤⠤⠤⢤⣀⣠⠤                                                       AR │
│  30 ┤⠉⠉ ⠈⠉⠉      ⠈⠉⠉    ⠈⠉⠉ ⠈⠉⠉⢉⣀⣀          ⠈⠉⠉⠉⠉⠈⠉⠉                                                                 │
│     │⠉⠙⠒⠚⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠙⠒⠚⠉⠉⠉⠙⠒⠒⠚ ⠘⠒⠚⠉⠉⠉⠙⠒⠒⠚⠉⠉⠉⠙⠒⠚⠉⠉⠉⠉⠙⠒⠚⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠙⠒⠒⠒⠒⠚⠉⠉⠉⠉⠉⠉⠙⠒⠚⠉⠙⠒⠒⠒⠒⠚⠉⠉⠉⠙⠒⠒⠚⠉⠉⠉⠙⠒⠚⠉⠙⠚⠉⠙⠒⠚⠉⠙⠒⠒⠒ ND │
│  20 ┤⠒⠒⠒⠒⠒⠚⠉⠙⠒⠒⠒⠒⠒⠒⠲⠤⠴⠒⠒⠒⠒⠲⠤⠴⠒⠒⠒⠒⠒⠒⠒⠒⠲⠤⠴⠒⠒⠒⠒⠒⠒⠒⠒⠲⠴⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠚⠉⠙⠒⠒⠒⠒⠒⠚⠙⠒⠒⠒⠒⠒⠚⠉⠙⠚⠉⠙⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠲⠤⠴⠒⠒⠒⠒⠒⠒⠒ GO │
│     │⠤⠤⠤⠤⠤⠴⠒⠲⠤⠤⠤⠤⠤⠤⠤⠤⠴⠒⠒⠲⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠴⠒⠲⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠴⠲⠤⠤⠤⠤⠤⠤⠤⠤⠴⠒⠲⠤⠤⠤⠤⠤⠴⠲⠤⠴⠒⠲⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠴⠒⠲⠴⠒⠲⠤⠤⠤⠴⠒⠲⠤⠤⠤⠤⠴⠒⠲⠤⠴⠲⠤⠴⠒⠲⠤⠤⠤⠤⠤ CF │
│  10 ┤                                                                                                                │
│     │                                                                                                                │
│   0 └────────────────────────────────────────────────────────────────────────────────────────────────────────────    │
│      -1m                     -45s                       -30s                       -15s                       now    │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Outcomes · same time axis as the chart ─────────────────────────────────────────────────────────────────────────────╮
│ 1 CF ············································································································    │
│ 2 GO ············································································································    │
│ 3 ND ············································································································    │
│ 4 AR ······················································███████████████████████████████████████████████████       │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭─ Events ────────────────────────────────────────────────────────────────────────────────────── newest first · 2 of 2 ╮
│ 00:01:04  ▲ arelion icmp v4          no-reply run opened: 5 consecutive probes unanswered                            │
│ 00:01:02  ▲ arelion icmp v4          no-reply rate 5 % over the last minute (3 of 60 probes)                         │
│                                                                                                                      │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
 1 Cloudflare │ 1 m │ PLAYING │ 1-4 · Enter · m mode · f family · +/- window · Space freeze · c cursor · ? help · q
```

That is `pathwatch demo isolated-failure`: synthetic data, nothing sent. The
fourth provider stopped answering thirty seconds ago; the other three did not,
so the fault is on its side of the path, not yours.

**Status.** Version 0.1.0, built from source; nothing is packaged or released
yet. The measurement engine, the dashboard, recording and replay, route tracing
and the documentation are built and tested. It has been run against Cloudflare
and Google over IPv4, briefly; NextDNS, Arelion, IPv6 and long sessions have
not been verified. [`ROADMAP.md`](ROADMAP.md) says exactly what has been
checked, what has not, and what is designed but not built.

## Build

```bash
cargo build --release
```

Needs Rust 1.88 or newer and a C compiler (for the TLS crypto provider). The
binary is `target/release/pathwatch`. To put it on your `PATH`, where the
commands below expect it:

```bash
cargo install --path crates/pathwatch --locked
```

## First steps

```bash
pathwatch demo
```

The dashboard on synthetic data. Nothing is sent; `pathwatch demo --list`
names the scenarios. Press `?` for the keys, `?` again for what every word and
number means, `q` to quit.

```bash
pathwatch doctor
```

What this host permits. It sends nothing.

```bash
pathwatch run
```

The live dashboard. **This sends probes.** With the built-in configuration: to
Cloudflare and Google, over IPv4 and IPv6 — an ICMP echo request a second and
a DNS query over UDP and over HTTPS every five seconds to each, and a fresh
HTTPS connection every five minutes: about six transactions a second in all.
NextDNS and Arelion show `NEEDS CONFIG` until you configure them.

Probes go only to the addresses in your configuration. A target given as a
hostname is looked up through the system's resolver, and so are the names of
route hops when you start a trace. [`docs/capabilities.md`](docs/capabilities.md)
lists every kind of traffic and where it goes.

## Commands

| Command | What it does | Sends |
|---|---|---|
| `pathwatch run` | the live dashboard | probes to your configured targets |
| `pathwatch sample` | the same collection without a screen: JSON records, a summary table, or plain sentences | probes to your configured targets |
| `pathwatch demo [scenario]` | the dashboard on synthetic scenarios | nothing |
| `pathwatch replay <recording>` | a recording played through the dashboard | nothing |
| `pathwatch export <recording>` | a recording as JSONL, CSV or a summary | nothing |
| `pathwatch doctor` | what this host permits | nothing, unless `--probe`: three echo requests to each loopback address |
| `pathwatch config check` | validate the configuration and list every problem | nothing |
| `pathwatch config show`, `path`, `example`, `reference` | the settings in force, the file in use, a commented example, an explanation of every setting | nothing |
| `pathwatch glossary` | what every word and number on screen means | nothing |
| `pathwatch man` | the manual page | nothing |

`pathwatch <command> --help` lists the options of each. The same is in the
[manual page](docs/pathwatch.1): `man docs/pathwatch.1`.

## Reading it

Each provider has a card, and each card a status: a symbol and a word, never
colour alone. `OK` means answering with nothing open against it; `NO REPLY`,
`LOSSY`, `SLOW` and `ERRORS` each have a stated threshold; `NEEDS CONFIG`,
`UNSUPPORTED` and `DISABLED` mean nothing is being sent, and are never counted
as loss. A dash in place of a number means too few samples to show one.

Three things the instrument is careful about, because the tools it replaces
are not:

- **"No reply" is not "packet loss".** It is the share of probes that got no
  answer. The request may have been lost, or the reply, or the target may limit
  how often it answers. The screen says no-reply, and never names a cause.
- **A probe that could not be sent is not a probe that went unanswered.** No
  route, no permission, a slot skipped under load, time spent asleep: each is
  its own outcome and none counts against the target.
- **Different things are not drawn on one line.** ICMP round trips, DNS over
  UDP, a fresh TLS connection: separate streams, separate statistics.

The full definitions are in the [glossary](docs/glossary.md).

## Configuration

It runs with no configuration file. To change something, put only what you
change in `~/.config/pathwatch/config.toml`:

```toml
[controls.gateway]
enabled = true          # one echo request a second to your default gateway
```

- `pathwatch config example` prints every setting with its default, commented.
- [`docs/configuration.md`](docs/configuration.md) — or `pathwatch config reference` — explains each one, and says
  which a reload applies (`R` in the dashboard) and which need a restart.
- `pathwatch config check` validates a file and sends nothing.

**NextDNS.** Enable the provider and give your profile id through the
environment, not the file:

```bash
export PATHWATCH_NEXTDNS_PROFILE=<your profile id>
```

The id is masked on screen and removed from recordings as they are written.

**Arelion.** Its two targets — a router you see in your own traces, and one of
its listed resolvers — hold placeholders until you fill them in.

## What it needs from the host

No root, no setuid, no capability, and it never changes a system setting. On
macOS nothing needs configuring. On Linux, ICMP needs
`net.ipv4.ping_group_range` to include one of your groups; where it does not,
the ICMP streams read `UNSUPPORTED` with the remedy, and DNS over UDP, TCP, TLS
and HTTPS still run. See [`docs/capabilities.md`](docs/capabilities.md).

## Scripted checks

A zero exit status means the program ran, not that the network is healthy —
unless you ask:

```bash
pathwatch sample --mode icmp --family 4 --duration 60s --quiet --assert 'no-reply <= 1%' --assert 'cloudflare: p50 < 40ms'
```

exits 10 if a check is false, or could not be made for lack of data. A check
is made against every stream it selects that ran, and a stream with too few
samples does not pass: so sample one kind of stream, as here, or name the
streams in the check (`'*/icmp/4: no-reply <= 1%'`), and sample for long
enough.

The other codes: 2 usage, 3 invalid configuration, 4 no stream could start, 5
an error reading or writing a file that was asked for, and from `sample` 130
and 143 for an interrupt and a termination. The dashboard exits 0 when you
leave it; 1 means it lost its terminal, and 101 an internal error.

## Recording and replay

`pathwatch run --record` and `pathwatch sample --record` write the session as
JSON lines under `~/.local/state/pathwatch/recordings/`. `pathwatch replay`
plays one back through the dashboard, sending nothing; `pathwatch export`
converts one, and `--redact-local` removes your own addresses, interface names
and gateway from a copy you mean to share.

A recording is created readable by you alone. It can be replayed safely
whoever made it: text in a recording is treated as untrusted, and control
characters in it are shown as text, never sent to your terminal.

## Accessibility

- `--theme high-contrast`: bright bold colours, white on black, nothing dimmed.
- `--color never` or `NO_COLOR`: no colour; status is still a symbol and a word.
- `--ascii`: no character outside ASCII.
- `--reduced-motion`: the screen is redrawn for new data at most four times a second.
- `pathwatch sample --format plain` and `pathwatch glossary`: sentences, for a
  screen reader. The dashboard itself is a grid of cells and is not usable with one.

## Documentation

| | |
|---|---|
| [`docs/glossary.md`](docs/glossary.md) | every word and number, defined |
| [`docs/configuration.md`](docs/configuration.md) | every setting |
| [`docs/capabilities.md`](docs/capabilities.md) | what the host must permit, what is sent and where, what is kept |
| [`docs/pathwatch.1`](docs/pathwatch.1) | the manual page |
| [`docs/design/`](docs/design/README.md) | for contributors: the design, the architecture, how it is verified, and the sources behind it |
| [`ROADMAP.md`](ROADMAP.md) | what has been verified, what has not, and what is not built |

The glossary and the manual page are printed by the program itself. The tests
check the other two against it — every setting and its default, every line
`doctor` prints — so those facts cannot fall behind; their prose is kept by hand.

## What is not built

Binding to an interface or source address; raw-socket ICMP; retries; DoH by GET
or through a proxy; DoQ and DoH3; an explicit bootstrap resolver; TLS session
resumption for fresh-connection probes; UDP and TCP route tracing; a gateway
control over IPv6 where the gateway is a link-local address; key remapping; a
log file.

A setting for something that is not built is handled one of two ways, and the
configuration reference says which: it is refused when the configuration is
checked, or it is accepted, changes nothing, and prints a warning.
`pathwatch doctor` lists what this build leaves out.

## Development

```bash
cargo test --workspace
```

The tests use loopback and local fixture servers only; nothing leaves the
machine. Some drive the dashboard in a pseudo-terminal, so they need a Unix
host; no visible terminal is required. After an intended change to a screen or
to a printed document, `UPDATE_GOLDEN=1` and `UPDATE_DOCS=1` rewrite the
reference files; review the difference before keeping it.

[`CONTRIBUTING.md`](CONTRIBUTING.md) has the rest: what CI runs, what a change
needs, and where things are. To report a security problem, see
[`SECURITY.md`](SECURITY.md).

## Licence

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or
the [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in pathwatch by you, as defined in the Apache-2.0
licence, shall be dual licensed as above, without any additional terms or
conditions.

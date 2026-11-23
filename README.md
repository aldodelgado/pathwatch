# pathwatch (placeholder name)

A terminal instrument that continuously measures up to four DNS and network
providers — Cloudflare and Google out of the box, NextDNS and Twelve99/Arelion
once you give it their endpoints — and shows, with stated definitions, which
one is misbehaving, on which protocol and address family, and since when.

It replaces keeping `ping`, `prettyping`, `gping` and a route tracer open side
by side: one dashboard, Vim-style keys, macOS and Linux, no root.

**Status.** Built from source only; nothing is packaged or released yet. The
measurement engine, the dashboard, recording and replay, route tracing and the
documentation are built and tested. What has and has not been verified, task by
task, is in [`planning/`](planning/README.md).

## Build

```bash
cargo build --release
```

Needs Rust 1.88 or newer and a C compiler (for the TLS crypto provider). The
binary is `target/release/pathwatch`.

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
a DNS query over UDP and over HTTPS every five seconds to each, about six
transactions a second in all. NextDNS and Arelion show `NEEDS CONFIG` until you
configure them. Nothing is ever sent to an address you did not configure.

## Commands

| Command | What it does | Sends |
|---|---|---|
| `pathwatch run` | the live dashboard | probes to your configured targets |
| `pathwatch sample` | the same collection without a screen: JSON records, a summary table, or plain sentences | probes to your configured targets |
| `pathwatch demo [scenario]` | the dashboard on synthetic scenarios | nothing |
| `pathwatch replay <recording>` | a recording played through the dashboard | nothing |
| `pathwatch export <recording>` | a recording as JSONL, CSV or a summary | nothing |
| `pathwatch doctor` | what this host permits | nothing, unless `--probe`: three echo requests to loopback |
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
pathwatch sample --duration 60s --quiet --assert 'no-reply <= 1%' --assert 'cloudflare: p50 < 40ms'
```

exits 10 if a check is false, or could not be made for lack of data. The other
codes: 2 usage, 3 invalid configuration, 4 no stream could start, 5 an output
error, 130 and 143 for an interrupt and a termination.

## Recording and replay

`pathwatch run --record` and `pathwatch sample --record` write the session as
JSON lines under `~/.local/state/pathwatch/recordings/`. `pathwatch replay`
plays one back through the dashboard, sending nothing; `pathwatch export`
converts one, and `--redact-local` removes your own addresses from a copy you
mean to share.

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
| [`planning/`](planning/README.md) | the design, the architecture, the plan, and a status report per milestone |

The glossary and the manual page are printed by the program itself, and the
other two are checked against it by the tests, so none of them can fall behind.

## What is not built

Binding to an interface or source address; raw-socket ICMP; retries; DoH by GET
or through a proxy; DoQ and DoH3; UDP and TCP route tracing; key remapping; a
log file. Some settings for these exist and are accepted without effect; the
configuration reference marks each one, and setting it prints a warning.

## Development

```bash
cargo test --workspace
```

The tests use loopback and local fixture servers only; nothing leaves the
machine. Some drive the dashboard in a pseudo-terminal, so they need a Unix
host; no visible terminal is required. After an intended change to a screen or
to a printed document, `UPDATE_GOLDEN=1` and `UPDATE_DOCS=1` rewrite the
reference files; review the difference before keeping it.

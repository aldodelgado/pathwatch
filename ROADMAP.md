# Roadmap and status

What pathwatch does today, what has and has not been checked, and what is
designed but not built. The design itself is in
[`docs/design/`](docs/design/README.md).

## Where it stands

pathwatch is at version 0.1.0 and has no release: it is built from source.
The measurement engine, the dashboard, recording and replay, route tracing,
scripted checks and the documentation are built and tested. It has been used
against real providers only briefly, from one network, over IPv4.

Read the next two sections before relying on it.

## What has been verified

| What | How |
|---|---|
| The whole test suite: scripted sessions on a virtual clock, real sockets on loopback, the dashboard in a pseudo-terminal, golden screens, documentation checked against the program | macOS on Apple silicon, locally and in CI; Linux in CI on x86_64 Ubuntu runners, with unprivileged ICMP allowed and refused; Linux on aarch64 in a container |
| ICMP echo, DNS over UDP and DNS over HTTPS to Cloudflare and Google | short live sessions (30 seconds to 3 minutes) over IPv4, including two one-minute runs beside the system `ping` |
| DNS over TLS and DNS over TCP | one 60-second live check each, against Cloudflare and Google, over IPv4 |
| Route tracing | scripted paths and loopback; one live ICMP trace over IPv4, on macOS |
| The gateway and local-DNS controls | one 3-minute live session, on macOS |
| The performance budgets that need no network | measured on loopback on one Mac, in runs of up to five minutes — see [`docs/design/verification.md`](docs/design/verification.md) §5 |

## What has not been verified

- **NextDNS and Twelve99/Arelion.** Two of the four providers the program is
  built around have never been measured. They need an endpoint that only a
  user can supply, and ship switched off (`NEEDS CONFIG`).
- **IPv6 to a real destination.** Every IPv6 test so far is loopback or the
  "no route" path.
- **Linux outside a container or a CI runner.** No Ubuntu or Debian
  installation, and no run with unprivileged ICMP on a real x86_64 host. On
  stock Ubuntu, ICMP reads `UNSUPPORTED` until an administrator widens
  `net.ipv4.ping_group_range`; DNS measurement works without it.
- **Long runs.** No hour-long live session, no 24-hour soak. Memory over a day
  is projected from short runs, not measured.
- **A real change of network, and a real sleep and wake.** Both are tested on
  a simulated network and a simulated clock only.
- **Correlated incidents on real data.** The rules are tested on scripted
  sessions.
- **A build on Rust 1.88**, the declared minimum. CI builds on stable.
- **Terminals.** The mouse, the high-contrast theme and the layout breakpoints
  have not been tried across terminal emulators, tmux or SSH. The plain-text
  summary has not been tried with a screen reader.
- **Other platforms.** Intel Macs and musl Linux are untested. Windows is not
  supported.

## Designed, not built

Measurement:

- Binding to an interface or a source address.
- Raw-socket ICMP, for hosts that refuse unprivileged ICMP.
- Retries.
- DoH by GET or through a proxy; DoQ and DoH3.
- An explicit bootstrap resolver; TLS session resumption for fresh-connection
  probes.
- UDP, TCP and Paris route tracing; a trace that keeps running after its view
  is left; traces in recordings and exports.
- A gateway control over IPv6 where the gateway is a link-local address.
- A per-target probe interval.

Dashboard:

- A Settings tab and a Capabilities screen.
- Inspecting a single sample or timeline cell.
- Hiding or soloing a series, cycling the scale, filtering lists.
- Configurable keys.
- Epoch markers on the chart; small multiples for monochrome terminals.

Command line and files:

- A log file (`--log-file`, `--log-level`, `[logging]`).
- `--mode`, `--family` and `--window` on `run`.
- A converter between recording schema versions. The schema is version 1.

Settings that are accepted and change nothing are marked in
[`docs/configuration.md`](docs/configuration.md); each will be built or
removed.

## Known limitations

- **Large or hostile recordings.** `replay` and `export` read a whole
  recording into memory and do not cap the length of a line. A very large or
  deliberately malformed file can use a great deal of memory, and numbers in
  it are not range-checked.
- **The NextDNS profile id** is removed from recordings as they are written
  and masked on screen. It is not removed from the JSON records `sample`
  writes to standard output or to `--output`.
- **`[ui]` settings** apply to `run`. `demo` and `replay` take their look from
  command-line options only.
- **`config show --effective`** does not list the control streams;
  `config check` does.
- **The lists of what is not built** printed by `doctor` and `--version` are
  shorter than the one above.

## Next

1. A tagged `v0.1.0` with a changelog, and an installation path beyond
   `cargo install --path`.
2. Limits on what `replay` and `export` will read; one scrubber for the
   profile id in front of every output.
3. A CI job on the minimum Rust version.
4. First runs against NextDNS and Arelion, on IPv6, and on a Linux host; then
   the hour-long run and the 24-hour soak.
5. A decision on each setting that has no effect.

## Open decisions

- Whether the Arelion card should probe a backbone router at all by default,
  and at what rate.
- Which route-tracing methods beyond ICMP are worth building.
- Packaging: Homebrew, release binaries, signing on macOS, musl builds.

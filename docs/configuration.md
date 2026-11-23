# Configuration reference

Every setting `pathwatch` reads, its default, and what it does. `pathwatch config reference` prints this page; a test holds every key, default and "reload" entry below against the program, so it cannot fall behind it.

## Where the configuration comes from

Later sources override earlier ones:

1. The built-in defaults — `pathwatch config example` prints them as a commented file.
2. A configuration file: the one given with `--config`, else the one named by `PATHWATCH_CONFIG`, else `~/.config/pathwatch/config.toml` if it exists (`$XDG_CONFIG_HOME/pathwatch/config.toml` where that variable is set). `pathwatch config path` says which is in use.
3. Command-line options, for the settings that have one.

A file only needs the settings it changes. A table such as `[icmp]` is merged key by key; a list such as a provider's `targets` or `[[dns.query]]` replaces the built-in list whole.

Rules that hold throughout:

- **An unknown key is an error**, not something ignored. `pathwatch config check` lists every problem at once and sends nothing.
- **Durations are strings** with a unit: `"250ms"`, `"1s"`, `"5m"`, `"24h"`. Sizes likewise: `"64MiB"`, `"1GiB"`.
- **A value written `"<like this>"` is a placeholder.** An enabled target that still holds one is an error, which is why NextDNS and Arelion ship switched off.
- **"Reload"** says whether a change is applied to a running session — `R` in the dashboard, or `SIGHUP` to `pathwatch sample`. A reload containing any change that needs a restart is refused whole, and nothing is changed.
- **Some settings are accepted and change nothing in this build.** Each is marked below, and setting one to something other than its default prints a warning.

`pathwatch config show --effective` prints every setting in force with where it came from, and the streams that would run.

## Top level and `[general]`

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `config_version` | `1` | The version of this format. Only `1` is read. | no |
| `general.families` | `["ipv4", "ipv6"]` | Which address families get streams. A stream is always one family; the two are never pooled. | yes |
| `general.default_mode` | `"icmp"` | The measurement the overview would open on. **No effect in this build:** the dashboard opens on ICMP. | no |
| `general.default_family` | `"ipv4"` | The family the overview would open on. **No effect in this build:** it opens on IPv4. | no |
| `general.default_window` | `"5m"` | The statistics window the dashboard would open on. **No effect in this build:** it opens on five minutes; `+` and `-` change it. | no |

## `[source]`

Which interface and address probes leave from. Empty means "as the operating system routes it", so an active VPN is used, not bypassed.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `source.interface` | `""` | Bind to this interface. Only the empty value is accepted in this build: binding is not built. | no |
| `source.address_v4` | `""` | Bind IPv4 probes to this address. Only empty in this build. | no |
| `source.address_v6` | `""` | Bind IPv6 probes to this address. Only empty in this build. | no |

## `[limits]`

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `limits.max_transactions_per_second` | `25` | The most probe transactions per second, all streams together. A configuration whose streams need more is an error; at run time a slot over the limit is skipped, and counted as skipped. | no |
| `limits.burst` | `40` | How many transactions may go out at once before the limit above bites. | no |
| `limits.max_in_flight` | `128` | How many ICMP probes may wait to be sent at once. Beyond it a slot is skipped, never delayed. | no |
| `limits.max_concurrent_dns` | `16` | **No effect in this build.** DNS streams are not limited separately. | no |

## `[icmp]`

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `icmp.enabled` | `true` | Whether targets that list `icmp` are probed. | yes |
| `icmp.interval` | `"1s"` | Time between echo requests on one stream. | yes |
| `icmp.deadline` | `"2s"` | How long a reply is waited for. One that comes later is recorded as late, and the probe stays timed out. | yes |
| `icmp.payload_bytes` | `56` | Payload size; 56 makes the 64-byte message `ping` sends. | no |
| `icmp.late_window` | `"30s"` | How long after its deadline a reply is still matched to its probe and noted as late. | no |
| `icmp.backend` | `"auto"` | `auto` or `datagram`: the unprivileged datagram socket, the only one built. `raw` is refused. | no |
| `icmp.retries` | `0` | Only `0`: a probe is sent once and its result stands. | no |

## `[dns]`

Common to DNS over UDP, TCP, TLS and HTTPS.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `dns.interval` | `"5s"` | Time between queries on one stream. Must be longer than the deadline. | yes |
| `dns.deadline` | `"3s"` | How long an answer is waited for. | yes |
| `dns.retries` | `0` | Only `0`. | no |
| `dns.edns_udp_size` | `1232` | The UDP payload size offered in EDNS(0). At least 512. | no |
| `dns.recursion_desired` | `true` | The RD bit of every query. | no |
| `dns.tcp_fallback` | `true` | After a UDP answer marked truncated, ask again over TCP. Both attempts are recorded; the stream's result is the UDP one. | no |
| `dns.query[].name` | — | A name to ask for, ending in a dot; `"."` is the root. The built-in list asks for `.`, `example.com.` and `wikipedia.org.`. Queries are sent in turn, in the same order to every resolver. | no |
| `dns.query[].type` | — | The record type: `A`, `AAAA`, `NS`, `TXT`, `MX`, `CNAME`, `SOA` or `PTR`. | no |
| `dns.query[].expect` | — | What counts as the expected answer: `noerror`, `answer` (at least one record of the type), `nxdomain`, `blocked`, or `any`. Anything else that comes back is an error response, not a timeout. | no |
| `dns.udp.enabled` | `true` | Whether targets that list `dns-udp` are probed. | yes |
| `dns.tcp.enabled` | `false` | Whether targets that list `dns-tcp` are probed: a new TCP connection per query. No built-in target lists it. | yes |
| `dns.tcp.interval` | `"60s"` | Time between DNS-over-TCP probes on one stream. Must be longer than `dns.deadline`. | yes |
| `dns.dot.enabled` | `true` | Whether targets that list `dot` are probed. The built-in DoT targets are themselves switched off. | yes |
| `dns.dot.fresh_interval` | `"5m"` | Time between fresh-connection probes: TCP connect, full TLS handshake, one query. Longer than 8 s. | yes |
| `dns.dot.fresh_handshake` | `"full"` | Only `full`: a fresh probe never resumes a TLS session. | yes |
| `dns.doh.enabled` | `true` | Whether targets that list `doh` are probed. | yes |
| `dns.doh.method` | `"post"` | Only `post`. | yes |
| `dns.doh.fresh_interval` | `"5m"` | As for DoT. | yes |
| `dns.doh.fresh_handshake` | `"full"` | Only `full`. | yes |
| `dns.doh.proxy` | `"direct"` | Only `direct`: DoH connects to the resolver itself. If `HTTPS_PROXY` or `ALL_PROXY` is set, the session says so in its events and does not use it. | yes |

## `[bootstrap]`

How a target given by hostname gets its address. Never on the timing path.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `bootstrap.resolver` | `"system"` | Only `system`: the operating system's resolver. A target's own `addresses` bypasses it. | no |
| `bootstrap.servers` | `[]` | **No effect in this build.** | no |
| `bootstrap.refresh` | `"5m"` | How often a followed hostname is resolved again. | no |
| `bootstrap.on_failure` | `"fail"` | When a hostname stops resolving: `fail` reports local errors until it resolves; `keep` goes on with the last address. | no |

## `[providers.<key>]`

One table per provider; the built-in ones are `cloudflare`, `google`, `nextdns` and `arelion`. `gateway` and `local-dns` are reserved.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `providers.<key>.name` | required | The name on the card. | yes |
| `providers.<key>.slot` | required | Which of the four cards, `1` to `4`; each used once. It also decides the card's colour. | yes |
| `providers.<key>.enabled` | required | Whether any of its targets is probed. | yes |
| `providers.<key>.color` | required | A colour. **No effect in this build:** a card's colour goes by its slot. | yes |
| `providers.<key>.profile_id_env` | none | The environment variable holding a profile id, substituted for `{profile_id}` in this provider's targets. The id is masked on screen and removed from recordings as they are written. | yes |
| `providers.<key>.targets[].id` | required | A name for the target, unique within the provider. It is the second part of a stream's id. | yes |
| `providers.<key>.targets[].enabled` | `true` | Whether this target is probed. | yes |
| `providers.<key>.targets[].role` | none | `router` marks a hop, not a service: only `icmp` is accepted for it. | yes |
| `providers.<key>.targets[].address` | none | A literal IP address. Give exactly one of `address`, `url`, `hostname`. Needed for `icmp`, `dns-udp` and `dns-tcp`. | yes |
| `providers.<key>.targets[].url` | none | An `https://` URL. Needed for `doh`. | yes |
| `providers.<key>.targets[].hostname` | none | A hostname: the TLS server name, and what the certificate is verified against. Needed for `dot`. | yes |
| `providers.<key>.targets[].port` | none | The port for `dot`; 853 when left out. | yes |
| `providers.<key>.targets[].protocols` | required | What to send to it: any of `icmp`, `dns-udp`, `dns-tcp`, `dot`, `doh`. Each becomes a stream per address family. | yes |
| `providers.<key>.targets[].follow` | by kind | Whether to keep resolving the hostname and move to its new address. On for a URL or hostname, off for a literal address or static `addresses`. | yes |
| `providers.<key>.targets[].addresses` | `[]` | Static addresses for a URL or hostname target: dialled directly, one per family, and the hostname is never resolved. The hostname is still the TLS server name. | yes |

## `[controls]`

Two optional reference streams on a line of their own under the cards: your gateway, and the resolver your system uses.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `controls.gateway.enabled` | `false` | Send an echo request to the default gateway at `icmp.interval`. | yes |
| `controls.gateway.address` | `"auto"` | `auto`, the default gateway of each family, or a literal IP. `pathwatch doctor` shows what `auto` finds. | yes |
| `controls.local_dns.enabled` | `false` | Send the DNS queries over UDP to the system's resolver at `dns.interval`. | yes |
| `controls.local_dns.address` | `"auto"` | `auto`, the first resolver the system lists for each family, or a literal IP. | yes |

## `[trace]`

Route tracing, which runs only when asked for with `t` and Enter.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `trace.protocol` | `"icmp"` | Only `icmp`. | no |
| `trace.max_hops` | `30` | The largest TTL tried, at most 64. | no |
| `trace.rounds` | `10` | How many times each hop is probed. | no |
| `trace.round_interval` | `"1s"` | Time between rounds. | no |
| `trace.max_probes` | `300` | The most probes one trace may send in all. | no |
| `trace.max_rate` | `30` | The most probes per second. | no |
| `trace.reverse_dns` | `true` | Ask the system resolver for the name of each hop found. Never on the probe path; `n` hides the names. | no |
| `trace.stop_on_leave` | `true` | Only `true`: leaving the Route view stops the trace. | no |

## `[history]`

What is kept in memory. Both are bounded.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `history.samples` | `"24h"` | How far back each stream's compact samples go; the longest window that can be shown. | no |
| `history.records` | `"1h"` | How far back each stream's full records go; what the Raw tab can list. | no |

## `[incidents]`

When a stream's status changes. Every rule opens on one condition and closes on a stricter one.

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `incidents.cooldown` | `"60s"` | After a rule closes, how long before it may open again. | no |
| `incidents.no_reply_run.open_after_consecutive` | `5` | `NO REPLY` after this many ICMP probes in a row unanswered. | no |
| `incidents.no_reply_run.close_after_consecutive` | `10` | It ends after this many answers in a row. | no |
| `incidents.elevated_no_reply.window` | `"60s"` | The period `LOSSY` looks at. | no |
| `incidents.elevated_no_reply.open_at` | `0.05` | `LOSSY` at this share unanswered, as a fraction. | no |
| `incidents.elevated_no_reply.close_at` | `0.01` | It ends at or below this share, | no |
| `incidents.elevated_no_reply.close_hold` | `"30s"` | held for this long. | no |
| `incidents.elevated_no_reply.min_finalised` | `30` | The fewest finalised probes in the period for the share to be judged. | no |
| `incidents.latency_shift.recent` | `"60s"` | The period whose median `SLOW` compares, | no |
| `incidents.latency_shift.baseline` | `"15m"` | against the median of this long before it. | no |
| `incidents.latency_shift.open_ratio` | `1.5` | `SLOW` when the recent median is more than this many times the baseline, | no |
| `incidents.latency_shift.open_min_delta` | `"10ms"` | and at least this much above it. | no |
| `incidents.latency_shift.close_ratio` | `1.2` | It ends when the recent median has stayed below this many times the baseline for one `recent` period. | no |
| `incidents.latency_shift.min_recent` | `30` | The fewest recent answers, | no |
| `incidents.latency_shift.min_baseline` | `300` | and baseline answers, for the comparison to be made. | no |
| `incidents.dns.open_after_consecutive_no_response` | `3` | `NO REPLY` on a DNS stream after this many queries in a row unanswered. | no |
| `incidents.dns.close_after_consecutive_success` | `3` | It ends after this many answers in a row. | no |
| `incidents.dns.error_responses_in_last_10` | `3` | `ERRORS` when this many of the last ten DNS transactions were error responses. | no |

## `[ui]`

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `ui.color` | `"auto"` | `auto` follows the terminal and `NO_COLOR`; or `truecolor`, `256`, `16`, `never`. `--color` overrides it. | no |
| `ui.glyphs` | `"unicode"` | `unicode`, or `ascii` for a screen with no character outside ASCII. `--ascii` turns it on. | no |
| `ui.theme` | `"default"` | `default`; `high-contrast` — bright bold colours, white on black, nothing dimmed; or `mono`. `--theme` overrides it. Where colour has been refused, high contrast does not bring it back. | no |
| `ui.max_fps` | `15` | **No effect in this build.** The screen is redrawn when the data changes, ten times a second at most. | no |
| `ui.idle_fps` | `4` | **No effect in this build.** | no |
| `ui.reduced_motion` | `false` | Redraw for new data at most four times a second. Keys are still answered at once. `--reduced-motion` turns it on. | no |
| `ui.mouse` | `false` | Capture the mouse: click to focus or place the cursor, wheel to scroll or change the window. Off because it takes over the terminal's text selection. `--mouse` turns it on. | no |
| `ui.mask_secrets` | `true` | **No effect in this build.** Profile ids are always masked on screen. | no |
| `ui.keys` | a table | **No effect in this build.** Keys cannot be remapped; `?` in the dashboard lists them. | no |

## `[recording]` and `[redaction]`

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `recording.enabled` | `false` | Record every session of `run` and `sample`. `--record` does it for one run. | no |
| `recording.directory` | `"~/.local/state/pathwatch/recordings"` | Where recordings go, one directory per session. | no |
| `recording.rotate_size` | `"64MiB"` | Start a new segment file at this size. | no |
| `recording.rotate_age` | `"24h"` | Or at this age. | no |
| `recording.max_total` | `"1GiB"` | The most disk all recordings may use; the oldest segments are deleted first. | no |
| `recording.min_free_disk` | `"200MiB"` | Below this much free space recording stops and measurement continues. | no |
| `recording.fsync` | `"rotate"` | `rotate` syncs a segment to disk when it is closed; `never` leaves it to the system. Lines are flushed after every batch either way. | no |
| `redaction.profile_ids` | `true` | Remove profile ids from a recording as it is written, so they never reach the disk. | no |
| `redaction.local_addresses` | `false` | Only `false`. `pathwatch export --redact-local` removes your own addresses from an export. | no |
| `redaction.hostnames` | `false` | Only `false`. | no |

## `[logging]`

| Setting | Default | Meaning | Reload |
|---|---|---|---|
| `logging.level` | `"info"` | **No effect in this build.** No log file is written; what a session has to say is in its events and, with `--record`, in its recording. | no |
| `logging.file` | `"~/.local/state/pathwatch/pathwatch.log"` | **No effect in this build.** | no |
| `logging.max_size` | `"10MiB"` | **No effect in this build.** | no |
| `logging.max_files` | `3` | **No effect in this build.** | no |

## Environment

| Variable | Use |
|---|---|
| `PATHWATCH_CONFIG` | The configuration file, when `--config` is not given. |
| `PATHWATCH_NEXTDNS_PROFILE` | Your NextDNS profile id, read through the built-in `profile_id_env`. Keeping it here keeps it out of the file. |
| `NO_COLOR` | Non-empty: no colour. Bold and reverse are kept. |
| `COLORTERM`, `TERM` | How many colours the terminal has, when `ui.color` is `auto`. |
| `XDG_CONFIG_HOME`, `HOME` | Where the default configuration file and the recording directory are looked for. |
| `HTTPS_PROXY`, `ALL_PROXY` | Noticed and reported; never used. |

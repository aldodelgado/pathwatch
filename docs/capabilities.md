# What pathwatch needs from the host

`pathwatch` runs as an ordinary user. It needs no root, no setuid bit and no capability, and it never changes a system setting — not a sysctl, not a firewall rule, not DNS, not a route. Where the host does not permit a kind of probe, that stream is shown as `UNSUPPORTED` with the reason; it is never shown as the target failing, and nothing is counted as unanswered.

`pathwatch doctor` reports what this host permits. It opens sockets and reads system state, and sends nothing. The statements on this page were checked against its output on the hosts listed [at the end](#where-this-was-checked); a test keeps the two in step on whatever host the tests run on.

## What each kind of probe needs

| Probe | What it uses | macOS | Linux |
|---|---|---|---|
| ICMP echo | an unprivileged ICMP datagram socket, one per address family, opened when the first ICMP stream starts | available to every user | available when `net.ipv4.ping_group_range` includes one of your groups — see below |
| DNS over UDP, TCP, TLS and HTTPS | ordinary UDP and TCP sockets | nothing special | nothing special |
| Route trace | an ICMP datagram socket of its own, with the TTL set | as ICMP echo | as ICMP echo |
| The source context of every session, and the gateway and local-DNS controls | the routing table and `/etc/resolv.conf`, read only. On macOS the routing table is read by running `/sbin/route -n get default`; nothing else is ever started | nothing special | nothing special |
| Recording | permission to write in the recording directory | — | — |

## Linux: unprivileged ICMP

Linux lets a process send echo requests without privilege only if one of its group ids lies within the range in `net.ipv4.ping_group_range`. That one setting governs IPv4 and IPv6 alike. The kernel's own default is `1 0`, an empty range: off.

Where it is off, `doctor` says so —

```text
ICMP IPv4       UNSUPPORTED — unprivileged ICMP is disabled on this host for your groups (net.ipv4.ping_group_range = "1 0")
```

— followed by the remedy, and the dashboard shows the ICMP streams as `UNSUPPORTED`. Everything else runs: DNS over UDP, TCP, TLS and HTTPS do not depend on it.

To see for yourself:

```sh
sysctl net.ipv4.ping_group_range
id -G
```

**The remedy is an administrator's to apply; pathwatch does not apply it and does not ask to.** It is the setting systemd's own defaults use, in a file under `/etc/sysctl.d/`:

```text
net.ipv4.ping_group_range = 0 2147483647
```

Inside a container whose group mapping is narrow, a range covering only your own group is enough, and `doctor` prints it with your group id filled in.

**Running as root does not help.** This build opens only the unprivileged datagram socket; it has no raw-socket mode to fall back to, with or without `CAP_NET_RAW`. As root with the range at `1 0`, ICMP is still `UNSUPPORTED`, because group 0 is not in that range either. `doctor` warns when it is run as root: the program is designed not to need it.

**Which distributions have it on** was not confirmed from their packages. The design expects it off on Ubuntu until enabled and on in Debian 13; Debian 12 is unknown. `doctor` on the machine itself is the answer.

## macOS

ICMP datagram sockets are open to every user, so ICMP works with nothing to configure.

One thing is particular to it: an ICMP socket on macOS is handed a copy of every echo reply arriving at the host, including those answering other programs' pings. `pathwatch` ignores the ones that are not its own and counts them as "unmatched packets ignored" in a sample's summary. That number says how busy the other tools are; it is not a fault and not loss.

## What `doctor` prints

| Line | Meaning |
|---|---|
| `host` | Operating system, processor architecture, kernel release. |
| `warning` | Shown only when run as root: the program does not need it. |
| `ICMP IPv4`, `ICMP IPv6` | Whether the unprivileged ICMP socket for that family could be opened, and if not, why, with the remedy underneath. |
| `source IPv4`, `source IPv6` | The address and interface the system would send from for that family, or `no route`. Found by asking the routing table; nothing is sent. |
| `gateway IPv4`, `gateway IPv6` | The default gateway the `[controls.gateway]` setting `auto` would use. |
| `system DNS` | The resolvers `/etc/resolv.conf` lists, each marked as on the local network or remote. The first of each family is what `[controls.local_dns]` `auto` would use. A remote one means that control measures a path across the Internet, not your router. |
| `ping groups` | Linux only: the range in force and your group ids. |
| `echo identifier` | Whether the kernel rewrites the identifier of echo requests (Linux) or leaves it (macOS). It decides how replies are told apart, and nothing you need act on. |
| `ICMP errors` | Where "destination unreachable" and "time exceeded" messages arrive: the socket's error queue (Linux) or the socket itself (macOS). |
| `sleep detection` | The pair of clocks compared to notice that the host was asleep, so the time asleep is recorded as a gap and not as unanswered probes. |
| `DNS over UDP`, `DNS over TCP`, `DNS over TLS`, `DNS over HTTPS` | Always available: they need no privilege. |
| `not in build` | What this version does not do at all. |
| `loopback` | With `--probe` only: the result of three echo requests to this host's own loopback addresses — the one thing `doctor` can send, and it does not leave the machine. |

`pathwatch doctor --json` gives the capability report in a form a script can read.

## What it sends, and where

Only to addresses named in the configuration, and to the system's resolver for names:

| Traffic | To |
|---|---|
| ICMP echo requests | each target that lists `icmp`; the gateway, if that control is on; the target of a route trace you start, with a rising TTL |
| DNS queries over UDP port 53 | each target that lists `dns-udp`; the system resolver, if that control is on |
| DNS over TCP port 53 | a target after a truncated UDP answer; each target that lists `dns-tcp`, if enabled |
| DNS over TLS, port 853 unless set | each enabled target that lists `dot` |
| DNS over HTTPS, port 443 unless the URL says otherwise | each target that lists `doh` |
| Name lookups through the operating system | hostnames of targets that have no static `addresses`; the names of route hops, unless `trace.reverse_dns` is off |

A firewall that blocks any of these makes that stream time out or report unreachable. That is a true report of what happened to the probe, and the program cannot tell a firewall from a silent target.

**VPNs and proxies.** Sockets follow the system's routing table, so an active VPN is used, not bypassed. An HTTP proxy named in `HTTPS_PROXY` or `ALL_PROXY` is not used: a query through a proxy would time the proxy. The session notes that the variable is set and that it is connecting directly.

**TLS.** Certificates are verified against the operating system's trust store and the configured hostname. Verification cannot be switched off.

## What it keeps

Nothing on disk unless you ask for a recording (`--record`, or `[recording] enabled`). A recording holds every observation, including destination addresses and your source address; a NextDNS profile id is removed from it as it is written. It is created readable by you alone, in directories made the same way; a directory that already exists keeps the permissions it has. `pathwatch export --redact-local` removes your own addresses, your interface names and your gateway from a copy you mean to share.

Only this program's own segments count against `recording.max_total`, and only they are deleted for it: files named `NNNNNN.jsonl` that open with a segment header, in a directory directly under the recording directory. Anything else there is left alone.

A recording from someone else can be replayed and exported safely. Text in a recording is treated like text off the network: control characters and direction overrides in it are shown as text (`\x1b`), never sent to the terminal.

## Where this was checked

| Host | As | ICMP | Result |
|---|---|---|---|
| macOS on Apple silicon, kernel 25.6 | an ordinary user | on | both families available |
| Linux on aarch64, kernel 7.0, in a container | root, range `0 2147483647` | on | available; the root warning shown |
| the same | user 1000, range `0 2147483647` | on | available |
| the same | user 1000, range `1 0` | off | `UNSUPPORTED` with the remedy naming group 1000; DNS lines unchanged |
| the same | root, range `1 0` | off | `UNSUPPORTED` — root does not help |

Not checked: any x86_64 host, and a Linux host outside a container — an Ubuntu or Debian installation in particular. Containers started by Docker here had the range already open, which is Docker's doing, not the distribution's.

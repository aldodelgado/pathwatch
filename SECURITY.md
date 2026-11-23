# Security policy

## Reporting a vulnerability

Please report it privately, not in a public issue:

1. Open the repository's **Security** tab.
2. Choose **Report a vulnerability**.

That opens a private advisory that only the maintainers can read. Say what you
found, how to reproduce it, and which version or commit you used
(`pathwatch --version`). You will get an answer there.

Please give the maintainers a reasonable time to fix a problem before
describing it in public.

## Supported versions

pathwatch has no release yet. Fixes are made on the `main` branch.

## What counts

pathwatch runs without privilege, reads text from the network and from files,
and writes to a terminal and to disk. Reports in these areas are especially
welcome:

- **Text reaching the terminal unescaped** — from a DNS answer, a reverse-DNS
  name, an error string, a configuration value, or a recording someone else
  made.
- **A crash or unbounded resource use** caused by a network reply or by a
  recording passed to `pathwatch replay` or `pathwatch export`.
- **TLS verification being skipped or weakened** for DNS over TLS or HTTPS.
- **A secret reaching a file or the screen** — a NextDNS profile id in a
  recording, an export or an error message.
- **A file written, read or deleted outside what was asked for** — in the
  recording directory or anywhere else.
- **Traffic sent anywhere the documentation does not name.**
  [`docs/capabilities.md`](docs/capabilities.md) lists everything the program
  sends and where.

## What is by design

- A recording holds your source address, your interface name, the
  destinations probed and the configuration in force. It is created readable
  by its owner only. `pathwatch export --redact-local` removes the local
  details from a copy meant for sharing.
- The program sends probes to the targets in its configuration, and looks
  hostnames up through the system's resolver. With the built-in
  configuration those targets are Cloudflare's and Google's public resolvers.

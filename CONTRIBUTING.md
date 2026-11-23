# Contributing to pathwatch

Thank you for looking. This file says how to build and test the project, what
a change needs before it can be merged, and where things are.

## Where things are

| | |
|---|---|
| `crates/pathwatch-core` | measurement: probes, scheduling, statistics, incidents, recording. No terminal code |
| `crates/pathwatch-tui` | the dashboard. Draws snapshots; does no network work |
| `crates/pathwatch` | the command line |
| [`docs/`](docs/) | what a user reads: glossary, configuration reference, capability guide, manual page |
| [`docs/design/`](docs/design/README.md) | what a contributor reads: the design, the architecture, how it is verified |
| [`ROADMAP.md`](ROADMAP.md) | what is not built and what has not been verified |

## Building and testing

Rust 1.88 or newer, a C compiler, and a Unix host (macOS or Linux).

```bash
cargo test --workspace
```

- **Nothing leaves your machine.** The tests use loopback, local fixture
  servers and scripted data. A test that sends anything anywhere else is a
  bug in the test.
- **Some tests need a live ICMP session.** macOS always permits one. Linux
  permits one only if `net.ipv4.ping_group_range` includes one of your
  groups; where it does not, those tests print why and do not run. To make a
  skipped test a failure instead — on a host you know permits ICMP — set
  `PATHWATCH_REQUIRE_ICMP=1`.
- **Some tests drive the dashboard in a pseudo-terminal.** No visible
  terminal is needed.
- **Ten tests are ignored on purpose.** They are measurements, not checks;
  [`docs/design/verification.md`](docs/design/verification.md) says how to
  run them.

Before opening a pull request, run what CI runs:

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

```bash
cargo test --workspace --locked
```

CI runs these on macOS, and on Ubuntu with unprivileged ICMP both allowed and
refused.

## Reference files

Two kinds of file are compared with what the program produces:

- **Screens** in `crates/pathwatch-tui/tests/golden/`. After an intended
  change to a screen, `UPDATE_GOLDEN=1 cargo test -p pathwatch-tui` rewrites
  them.
- **Printed documents**: `docs/pathwatch.1` and `docs/glossary.md` are the
  program's own output. After a change to a command, an option or a glossary
  entry, `UPDATE_DOCS=1 cargo test -p pathwatch --test docs` rewrites them.

Read the difference before you commit it. A reference file that changed for a
reason you cannot name is a bug you have just recorded.

## What a change needs

- **A test that fails without it**, for a fix or a new behaviour.
- **Documentation in the same change** when a command, an option, a setting,
  an exit status or a screen changes. The tests check `docs/configuration.md`
  and `docs/capabilities.md` against the program and will tell you.
- **Honest wording on screen.** The program never calls an unanswered probe
  "packet loss", never counts a probe it could not send against the target,
  and shows a dash where there are too few samples. New output keeps to that;
  the [glossary](docs/glossary.md) has the vocabulary.
- **No new privilege.** No root, no setuid, no capability, no changed system
  setting.
- **Untrusted text stays text.** Anything that came off the network or out of
  a file is cleaned (`pathwatch_core::sanitize`) before it is stored or shown.
- **`unsafe` with its reason.** Each block carries a `// SAFETY:` comment
  saying why it is sound.

Small, single-purpose pull requests are reviewed fastest.

## Reporting problems

- A bug or an idea: open an issue. For a bug, the output of `pathwatch doctor`
  and `pathwatch --version` answers most first questions. Before attaching a
  recording, run it through `pathwatch export --redact-local`.
- A security problem: do not open an issue. See [`SECURITY.md`](SECURITY.md).

## Licence

pathwatch is licensed under either of the [Apache License, Version
2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in pathwatch by you, as defined in the Apache-2.0
licence, shall be dual licensed as above, without any additional terms or
conditions.

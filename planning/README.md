# Planning package — four-provider network monitor

**Status:** planning approved; **milestone 1 (headless measurement) built and milestone 2 (dashboard) started on 2026-11-21** — see [milestone-1-status.md](milestone-1-status.md) for what was delivered, the test evidence, and what is still open. No probe has been sent to any provider, nothing has been committed, and no system setting was changed.

`pathwatch` is a placeholder name for the executable.

## Documents

| Read | Document | Contains |
|---|---|---|
| 1 | [01 · Design specification](01-design-spec.md) | goals and scope, measurement identity, provider/target matrix, measurement semantics, metric dictionary, DNS and route diagnostics, incidents, layouts with screen sketches, interaction, CLI contract, configuration, defaults and traffic budget |
| 2 | [02 · Architecture decision](02-architecture.md) | stack comparison and weighted matrix, dependency set, ICMP and trace backend decisions, timing boundaries, modules and data flow, concurrency model, platform capability matrix, observation schema, persistence and replay, trade-offs |
| 3 | [03 · Implementation plan](03-implementation-plan.md) | milestones with tasks and acceptance criteria, verification strategy, replay scenarios, performance budgets, risk register, deferred work, platform support |
| 4 | [04 · References, assumptions, open questions](04-references-and-open-questions.md) | every source with the claims it supports, what could not be confirmed, assumptions, and the ten decisions that need your input |
| 5 | [Milestone 1 status](milestone-1-status.md) | delivered tasks, acceptance criteria met and not met, open items, deviations, observations |
| 6 | [Milestone 2 status](milestone-2-status.md) | the dashboard: keys, what exists, test evidence, bugs found, what is not built |
| 7 | [Milestone 3 status](milestone-3-status.md) | recording, export and replay (3.7), incident correlation (3.6), the gateway and local-DNS controls (3.5), network-change handling (3.4), configuration reload (3.8), DNS over TLS (3.1), route tracing (3.2), optional mouse support (3.9), and the `trippy-core` evaluation (3.3, dropped): what exists, test evidence, and what of milestone 3 is not started |
| 8 | [Milestone 4 status](milestone-4-status.md) | `sample --assert` (4.7), the accessibility pass (4.1), the documentation (4.4), and the performance budgets that need no network (4.2, in part): results, three defects fixed, memory brought within its budget, two budgets restated; what the remaining tasks need |
| — | [../docs/](../docs/) | the user documentation: glossary, configuration reference, capability guide, manual page — each checked against the program by the tests |
| — | [examples/pathwatch.example.toml](examples/pathwatch.example.toml) | a syntactically valid illustrative configuration with NextDNS and Arelion disabled behind explicit placeholders |

## In one paragraph

A Rust terminal application (Ratatui, Tokio) with a hand-written unprivileged ICMP engine and explicit DNS clients, organised so that the collector, the statistics processor, and the renderer run on separate threads and the renderer can never influence a measurement. Four provider cards are always present; an unconfigured or unsupported target says so in words and is never shown as loss. Every number on screen is defined in a metric dictionary with its formula, window, denominator, and minimum sample count. Go with Bubble Tea v2 is the credible alternative.

## How to read the labels

**[V]** verified against a primary source on 2026-11-21 · **[R]** engineering recommendation · **[A]** assumption · **[T]** to be validated during implementation. All screen sketches use illustrative data.

## What happens next

Milestone 1 is built apart from the first run against your NextDNS and Arelion endpoints, which needs those details from you. Milestone 2 is mostly built: the dashboard runs (`pathwatch demo`) with detail tabs, a chart cursor, stop/resume, and suspend/resume, and its terminal handling is tested on macOS and Linux. It is waiting for your review of the look and the keys, and has had two 30-second live runs against Cloudflare and Google — see [milestone-2-status.md](milestone-2-status.md) for what is and is not built. From milestone 3, recording, export and replay (task 3.7) , incident correlation across streams (task 3.6), and the gateway and local-DNS controls (task 3.5, not yet run against your gateway or resolver) and source context with network-change handling (task 3.4, tested on a simulated network only) , and configuration reload (task 3.8, tested on loopback only) , and DNS over TLS (task 3.1, tested against a local fixture only) , and route tracing (task 3.2, one live trace) , and optional mouse support (task 3.9, never tried by hand) are built and tested; the `trippy-core` evaluation (task 3.3) is done and it was dropped on your decision; the optional DNS/TCP stream (the rest of task 3.1) is built and has had one 60-second live check — see [milestone-3-status.md](milestone-3-status.md). Milestone 4 is started: `sample --assert` (task 4.7), the accessibility pass (task 4.1) and the documentation (task 4.4) are built and tested, and the performance budgets that need no network have been measured on this Mac (three defects found and fixed; memory brought within its budget; the live CPU and memory-growth budgets restated on your decision); the live half of performance validation, the soak, the release pipeline and installation (4.2, 4.3, 4.5, 4.6) each need a decision or a long live run — see [milestone-4-status.md](milestone-4-status.md).

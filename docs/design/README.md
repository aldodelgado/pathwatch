# Design documents

For contributors. What a user needs is one level up, in [`docs/`](../).

| Document | Contains |
|---|---|
| [Design](design.md) | what the program measures and how it says so: measurement identity, the provider and target matrix, outcome semantics, the metric dictionary, DNS and route diagnostics, incidents, the screens, the keys, the command-line contract, configuration, defaults and the traffic budget |
| [Architecture](architecture.md) | how it is built: why Rust, the dependencies, the ICMP engine, where a measurement starts and ends, the modules, the threads and channels, what each platform permits, the record schema, recording and replay |
| [Verification](verification.md) | how it is checked: the tests and what they may touch, the scripted scenarios, the performance budgets and what has been measured, platform support |
| [References and assumptions](references.md) | the primary sources behind the design, the claim each supports, and what could not be confirmed |

Where the implementation differs from the design, or stops short of it, the
section says so. [`ROADMAP.md`](../../ROADMAP.md) lists what is not built and
what has not been verified.

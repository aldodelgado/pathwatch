## What this changes

<!-- One or two sentences. Link the issue if there is one. -->

## How it was checked

<!-- The test that fails without this change, or what you ran and saw. -->

## Checklist

- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked` pass
- [ ] A test covers the change
- [ ] Documentation is updated if a command, option, setting, exit status or screen changed
- [ ] Reference files (`UPDATE_GOLDEN`, `UPDATE_DOCS`) were regenerated only where intended, and the difference was read
- [ ] Nothing in the tests sends traffic beyond loopback

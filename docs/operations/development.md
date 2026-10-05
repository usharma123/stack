# Development and validation

Install Rust and Node.js 22.14 or newer. CI uses Rust 1.93.1. Service tests also need mise; the Docker suite needs Docker.

```sh
cargo build --locked --release
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
node --test tests/*.test.mjs
```

## Examples and service tests

```sh
cargo test                       # unit + integration tests (real git repos in temp dirs)
cargo run -- -C examples/app inspect
tests/e2e/run.sh                 # Docker: real mise + Pitchfork + OCI registry, all scenarios
tests/e2e/native.sh target/release/stack   # same scenarios on this macOS/Linux host (needs mise)
python3 eval/harness/pilot.py --stack target/release/stack   # scripted concurrency pilot
```

`eval/` holds the competitor evaluation (Flox, devbox, devenv, mise) that shaped this design:
[eval/REPORT.md](../../eval/REPORT.md).

## Architecture and releases

Read the [design](../DESIGN.md) for the composition and session model, and the [release guide](../RELEASING.md) for packaging and publishing.

[All docs](../README.md)

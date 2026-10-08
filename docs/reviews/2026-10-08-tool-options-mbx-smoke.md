# Tool options and the rust-mbx bundle: real-tool receipts

Route 2 of [the jdx expansion design](../design/jdx-expansion.md), run on 2026-10-08 on macOS
arm64 with mise 2026.10.3 (`/opt/homebrew/bin/mise`), Mr Boxington 1.22.0 and Rust 1.93.1
already in mise's tool store, and a debug build of this branch. Each run used its own
`STACK_CACHE_DIR` and `STACK_STATE_DIR` under `/tmp`. Fake-provider tests are in
`tests/compile.rs` and `tests/runtime.rs`; these receipts are what only the real tools show.

| # | Command | Result | Boundary |
|---|---|---|---|
| 1 | `stack compile --json` in a project using `examples/bundles/rust-mbx` plus `jq = { version = "1.7" }` and an `[env]` value `{{ exec(command='touch <tmp>/env-ran') }}` | exit 0; rust `1.93` → `1.93.1` with `options = { mr_boxington = true }`, mbx `1.22.0`; generated config has `[tools.rust] version = "1.93.1", mr_boxington = true` | `env-ran` absent (no project template evaluated during resolution); `<cache>/resolve/` empty afterwards; `~/.local/state/mise/trusted-configs` count unchanged (31 before and after) |
| 2 | `stack install --json` in a crate and in a `git worktree` of it, each with the rust-mbx bundle | `ok: true` in both | `mise version` gate passed (2026.10.3 ≥ 2026.9.2) |
| 3 | `stack exec -- sh -c 'command -v cargo; cargo --version'` | `~/.local/share/mise/command-wrappers/bin/cargo`, `cargo 1.93.1` | the wrapper mise publishes is first on the planned `PATH`; nothing installed by Stack |
| 4 | `MBX_CACHE_DIR=<tmp>/mbx-cache stack exec -- cargo build` (first checkout) | built; `mbx[cache]: 0 hits ... 435.7 KiB stored locally`; `target -> <tmp>/mbx-cache/targets/v1/<hash>` | mbx store redirected through the caller's environment; user mbx settings untouched; no `mbx setup` |
| 5 | `stack --json run build` (`run = "cargo build"`) in the worktree | `ok: true`, exit 0; `mbx[cache]: 1 hits, 0 misses`; `target` linked into the same store | `run` goes through `mise run` unchanged |
| 6 | `mbx stats` (same `MBX_CACHE_DIR`) | `builds 2`, `cache hits 1`, `managed targets 2`, `live workspaces 2` | |
| 7 | `stack --json compile` with `fnox = { version = "1.39", identity = "https://github.com/jdx/fnox/.github/workflows/release.yml@refs/tags/v1.39.0" }` | `fnox` → `1.39.0`, options recorded | `mise registry fnox` reported `packslip:github.com/jdx/fnox` first, so the registry name is eligible |
| 8 | same with `fnox = { version = "latest", identity = "bogus" }` | `resolve_failed`: "bundle does not verify: ... identity mismatch: expected bogus" | the options reach `mise latest` in the scratch root |
| 9 | same with a well-formed but wrong `pubkey` and `version = "1.39"` | resolves `1.39.0` | for a prefix request mise lists releases without the signed list, so a wrong key is caught at install, not at resolution (not exercised) |
| 10 | `stack --json compile` with `jq = { version = "1.7", identity = "x" }` | `invalid_tool`: "mise's registry installs it through aqua:jqlang/jq" | |
| 11 | `stack --json doctor` in the crate | `mise_release` ok: "mise 2026.10.3; needs 2026.9.2 (tools.rust (bundle:rust-mbx) sets mr_boxington)" | |
| 12 | `STACK_TEST_MISE=$(which mise) STACK_TEST_BINARY=target/debug/stack node --test tests/provider-boundary.test.mjs` | pass | project, parent, global and inherited aliases still cannot reinterpret locked tools |
| 13 | `tests/e2e/native.sh target/debug/stack 1-independent-checkouts 6-configuration-generation` | both passed | real mise, Pitchfork, Postgres and Redis in an isolated HOME and mise data directory; every version request resolved through the new scratch roots |

Observed side effect: mise records every configuration it loads under its state directory's
`tracked-configs` as a symlink. Each resolution adds one pointing at a removed scratch file.
Trust records are not written: the scratch root is trusted for the one command through
`MISE_TRUSTED_CONFIG_PATHS` (with `MISE_YES=1` alone mise writes a persistent trust record).

Not run here: a `mise` older than 2026.9.2 (covered with a fake `mise version`), a packslip
tool off github.com and gitlab.com, and installation with an explicit `pubkey`.

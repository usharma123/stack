# Agent skills: real-tool receipts

Route 4 (phases A and B) of [the jdx expansion design](../design/jdx-expansion.md), run on
2026-10-08 on macOS arm64 with mise 2026.10.3 (`/opt/homebrew/bin/mise`), fnox 1.39.0,
Mr Boxington 1.22.0, Pitchfork 2.29.0, jq 1.7.1 and Postgres 17.11 already in mise's tool
store, and a debug build of this branch. Each run used its own `STACK_CACHE_DIR` and
`STACK_STATE_DIR` under `/tmp`; runs that trust or install also set `MISE_STATE_DIR` there, so
no trust or tracked-config record reached the user's mise state. Nothing was installed or
downloaded: every pinned release was already present. Fake-provider tests are in
`tests/skills.rs` and `src/skills.rs`; these receipts are what only the real tools show.

| # | Command | Result | Boundary |
|---|---|---|---|
| 1 | `stack --json compile` in a project with `fnox = "1.39.0"`, `mbx = "1.22.0"`, `jq = "1.7.1"`, a `postgres` 17.11 service, `[skills] dir = ".claude/skills"` and an `[env]` value `{{ exec(command='touch <tmp>/env-ran') }}` | exit 0; `skills`: fnox 1.39.0 `available` (`fnox`), mbx 1.22.0 `available` (`mbx`), jq 1.7.1 `no_skill`, postgres 17.11 `no_skill`; no warnings | `env-ran` absent |
| 2 | `rm -rf .config`, then `stack --json inspect --all-skills` | exit 0; the same `skills`, entrypoints under `~/.local/share/mise/installs/<tool>/<version>/.mise-packslip/repo/skills/<name>/SKILL.md`; `provider_skills`: pitchfork `available` | project tree identical before and after (`find` diff), `.config` not recreated; `<cache>/skills` empty afterwards; `env-ran` absent; `~/.local/state/mise/trusted-configs` count unchanged (33) |
| 3 | `stack mcp`: `initialize`, then `stack_skill {tool: fnox, name: fnox}`, `{pitchfork, pitchfork}`, `{jq, jq}`, `stack_inspect {all_skills: true}` | instructions contain the skills sentence; fnox returns 4139 bytes (the file is 4139 bytes) at version 1.39.0; pitchfork `skill_not_found` ("belongs to a tool stack adds for its provider"); jq `skill_not_found` | text returned verbatim, not executed |
| 4 | `stack --json install` (same project) | exit 0; `skills` step `ok`, linked `fnox`, `mbx`; `.claude/skills/{fnox,mbx}` link to the release skill directories; `.stack-skills.json` records both | `pitchfork` not linked although its skill is installed and the stack has a service; no `mise skills sync` call |
| 5 | replace `.claude/skills/mbx` with a real directory, add a foreign link `mine -> /tmp`, `stack --json install` | exit 0; `unchanged: [fnox]`, `kept: [{mbx, "a real directory stack did not create"}]` | real directory and foreign link untouched; mbx dropped from the record |
| 6 | remove `fnox` from `[tools]`, `stack compile`, `stack --json install` | exit 0; `pruned: [fnox]`; `kept: [mbx ...]` | only stack's recorded link removed; the empty record file removed |
| 7 | tools-only project (`fnox = "1.39"`, `mbx`), `stack --json up --ttl 5m --timeout 2m`, `stack --json down` | `up` ok, `skills` step `ok`, linked `fnox`, `mbx`; `down` ok | |
| 8 | `stack --json compile` with `fnox = { version = "1.39.0", identity = "{{ exec(command='touch <tmp>/opt-ran') }}" }` | `invalid_tool`: option `identity` must not contain template syntax | `opt-ran` absent |
| 9 | `stack --json inspect` with a `mise` wrapper first on `PATH` that fails `mise skills` the way mise 2026.9.1 does (exit 1, "no tasks defined") and passes everything else to mise 2026.10.3 | exit 0; every entry `unavailable`; warning `skills_unavailable: ... failed (exit 1) ...` | mise's stderr not forwarded |

Observed before the fix in row 8: with mise 2026.10.3, a tools-only scratch configuration
`fnox = { version = "1.39.0", identity = "{{ exec(command='touch …') }}" }` ran the command
during both `mise skills ls --json` and `mise ls --json`. mise renders tool versions and option
strings as templates whenever it loads a configuration, so a templated value in a tool entry
would have run during resolution (`compile`) and skills discovery (`inspect`). Stack now
rejects template syntax in versions and option strings at parse time, and discovery refuses to
write a templated lock entry.

Research (GPT-6.1-Sol, high; T3 task `jdx-skills-research-mise-skills-r1`), with source
references at mise v2026.10.3 and runs against the installed mise and a scratch copy of 2026.9.1:

- `mise skills ls --json` is `[{ name, tool, version, path }]`, `path` the directory holding
  `SKILL.md`; several skills of one release are several rows; only configured, installed
  releases are listed. Names are not restricted to `[a-z0-9_-]` and paths may be links,
  including ones resolving outside the install directory: stack's name and containment checks
  are needed, not redundant.
- `mise ls --json` is keyed by the configured tool name (`fnox`, `packslip:github.com/jdx/fnox`,
  `postgres`) and also lists installed releases that are not configured; a configured release
  that is missing has `installed: false`.
- `skills` arrived in mise 2026.9.2. Older mise treats `skills` as a task name and fails with
  "no tasks defined" in a root without tasks, which the scratch root always is.
- Neither listing installs anything; both may use the network to resolve versions and write
  mise's own state and caches (tracked configs among them). Both evaluate `[env]` and, as
  confirmed in row 8, templates in tool values.
- mise compares its ceiling path by equality with the resolved working directory; scratch roots
  are now created under the resolved cache path so the ceiling matches under `/tmp` on macOS.

Not run here: `stack up` with services and `[skills]` against real Pitchfork (the shared
`skills` step ran under `install` with a service, row 4, and under a tools-only `up`, row 7);
a real mise older than 2026.9.2 driven through stack (row 9 simulates its failure; the research
ran 2026.9.1 directly); Linux; a tool installed through an explicit `packslip:` name.

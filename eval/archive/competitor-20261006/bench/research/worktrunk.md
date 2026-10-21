# Worktrunk research for the real application benchmark

Research date: 2026-10-06. Implementation owner: Opus 5.5. Worktrunk is an adjacent Git worktree manager. Compare its supported worktree workflow with explicit Python/uv/Compose providers; an absent package solver is `unsupported` for Worktrunk itself, not a failed application task. This note contains no service execution or timing results.

## Version and inspected evidence

The latest official release observed through GitHub's release API is [v0.80.0, published 2026-09-27](https://github.com/max-sixty/worktrunk/releases/tag/v0.80.0), commit `b49ca7eea9b03145791a5b94eccaf9c59412ed37`. The shallow main clone initially resolved to `dea4029c593a89a39fb141da9d0849e4d10dae19`; it was then checked out at the release tag. Source links below use the release commit. Current website documentation can describe later main changes even while its footer says v0.80.0, so release source and release CLI take precedence.

No `wt` was installed on PATH. A private release binary at `/tmp/stack-bench-sources/worktrunk-bin/worktrunk-aarch64-apple-darwin/wt` reported `wt v0.80.0`. Read-only `switch --help`, `remove --help`, `list -h`, and `hook --help` confirmed the documented flags. Download archive SHA256 `8a2bb053c4bc80dea7d9ce6c221ff038d10a3d2dca2dc8f60d1b1a094fa783a9` matched the release checksum; extracted `wt` SHA256 is `0708ca37fc39f9fa48edc1af500a2ff3664ec0155f63909425b02994f29f0fd1`. No existing containers or services were changed. There is no Worktrunk-specific local image evidence; record any eventual adapter image and external provider versions separately.

Release assets cover macOS ARM64/AMD64, Linux ARM64/AMD64 musl, and Windows AMD64. This POSIX recipe targets macOS with Docker Desktop or Linux with Docker Engine and Compose. Linux containers need an available Docker daemon/socket to run this recipe; a plain Worktrunk container alone cannot manage the service containers. Prefer host execution with a private pinned Worktrunk binary. Source builds require Rust 1.98 according to [Cargo.toml](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/Cargo.toml#L57). Installation and platform guidance are in the [official repository](https://github.com/max-sixty/worktrunk#install).

## What the source implements

| Requirement | Worktrunk native behavior | Additional provider or script |
| --- | --- | --- |
| Two working checkouts | `wt switch --create`, branch/path lookup and configurable worktree paths | Run-scoped names; linked worktrees share Git objects and config |
| Install dependencies | Blocking `pre-start` hook in the newly created worktree | Exact Python provider, uv, `uv.lock`, frozen installation |
| Command entry | `wt -C PATH -y ALIAS` or `wt switch --no-cd --execute PROGRAM` | Provider environment and application endpoints |
| Database/cache lifecycle | Hooks and arbitrary shell aliases | Compose service definitions, volumes, healthchecks, start/stop |
| Readiness | Foreground hook/alias can wait for an external command | Compose health wait plus application connection/identity check |
| Local dev-server cleanup | `wt step tether` supervises a local command/process group | Docker containers require explicit Docker/Compose teardown |
| Cleanup | Blocking `pre-remove`, worktree removal, optional branch retention | Compose down and owned-volume removal |
| Reproduction | Committed files appear in every worktree; `copy-ignored` supports ignored files and COW copies | Runtime/image/dependency pins; exclude data/endpoints from cache copying |
| Status | `wt list --format=json`, hook logs, branch variables | Service status and endpoint ownership checks |

Implementation points, read rather than integration-tested:

- [Creation and hooks](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/worktree/switch.rs#L1300) create the worktree before blocking `pre-start`; [execution ordering](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/worktree/switch.rs#L1922) spawns background hooks before `--execute`. A successful switch does not wait for background service setup.
- [Hook implementation](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/hooks.rs#L1) loads project commands from the invoking worktree, even when execution happens in a different worktree. Planned lifecycle hooks preserve the approved command selection. [Hook configuration](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/config/hooks.rs#L27) explicitly distinguishes blocking setup from background setup.
- [Aliases](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/alias.rs#L1) use foreground shell pipelines, template expansion and command approval. Pipeline failure handling is in [command_executor.rs](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/command_executor.rs#L465). Templates shell-escape forwarded arguments; use `{{ args }}` rather than constructing an unquoted argument string.
- [`--execute`](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/worktree/switch.rs#L1968) expands individual argv elements, and the [output handler](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/output/handlers.rs#L1142) runs the command in the selected directory. `--no-cd` affects the parent shell, not the child cwd. Shell syntax requires explicit `sh -c`.
- [Removal](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/remove.rs#L254) runs `pre-remove` before deletion. Default directory deletion is background; `--foreground` blocks. `post-remove` is background even when directory deletion is foreground, so place required service cleanup in `pre-remove`.
- [`tether`](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/step/tether.rs#L1) polls worktree existence every 250 ms and terminates the local process group on Unix. It cannot reap containers detached through Docker's daemon. Its child-exit path records success/failure in a trace but returns `Ok(())` after the final sweep; do not treat tether's exit alone as proof that the application command succeeded.
- [`copy-ignored`](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/commands/step/copy_ignored.rs#L28) copies ignored files selected by `.worktreeinclude`, with optional COW support. With no include file it copies all ignored files, so explicitly select caches and exclude service data, old endpoint files and path-sensitive virtualenvs.
- [`hash_port`](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/src/config/expansion.rs#L505) hashes into only 10,000 ports. It is deterministic, not a reservation or collision guard. The [official database-per-worktree recipe](https://github.com/max-sixty/worktrunk/blob/b49ca7eea9b03145791a5b94eccaf9c59412ed37/docs/src/content/docs/tips-patterns.md#L232) explicitly invokes Docker from hooks; it is evidence of supported glue, not native database management.

## Concrete recipe for the canonical fixture

Commit the common `bench/fixtures/app` contents at the root of a disposable benchmark Git repository, together with the following files. Set `.gitignore` to exclude `.venv/`. Keep `pyproject.toml` and `uv.lock` unchanged. The fixture currently requires Python `>=3.13,<3.14`; provide the same exact Python 3.13 patch/build used for all adapters, through an absolute `RWB_PYTHON` path. Worktrunk does not choose that runtime. Record and require its exact `RWB_PYTHON_VERSION`, uv version/binary hash, Worktrunk version, Compose version, and image digests. No Python download or dependency resolution happens silently.

`.config/wt.toml`:

```toml
[pre-start]
deps = "sh scripts/wt-app.sh setup"
[pre-remove]
services = "sh scripts/wt-app.sh purge"
[aliases]
up = "sh scripts/wt-app.sh up"
stop-services = "sh scripts/wt-app.sh stop"
start-services = "sh scripts/wt-app.sh start"
app = "DATABASE_URL={{ vars.database_url }} REDIS_URL={{ vars.redis_url }} .venv/bin/python -m rwbapp {{ args }}"
test = "DATABASE_URL={{ vars.database_url }} REDIS_URL={{ vars.redis_url }} .venv/bin/python -m pytest -q {{ args }}"
cmd = "env DATABASE_URL={{ vars.database_url }} REDIS_URL={{ vars.redis_url }} {{ args }}"
```

`compose.yaml`, using the verified service image pins from [Compose research](compose.md):

```yaml
services:
  postgres:
    image: postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
    environment: {POSTGRES_USER: bench, POSTGRES_PASSWORD: bench, POSTGRES_DB: bench}
    ports: [{target: 5432, host_ip: 127.0.0.1}]
    volumes: [pgdata:/var/lib/postgresql/data]
    healthcheck:
      test: [CMD-SHELL, "PGPASSWORD=$$POSTGRES_PASSWORD psql -h 127.0.0.1 -U $$POSTGRES_USER -d $$POSTGRES_DB -Atc 'select 1' | grep -qx 1"]
      interval: 1s
      timeout: 3s
      retries: 60
  redis:
    image: redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0
    command: [redis-server, --appendonly, "yes", --appendfsync, always]
    ports: [{target: 6379, host_ip: 127.0.0.1}]
    volumes: [redisdata:/data]
    healthcheck:
      test: [CMD-SHELL, "redis-cli ping | grep -qx PONG"]
      interval: 1s
      timeout: 3s
      retries: 60
volumes:
  pgdata: {}
  redisdata: {}
```

`scripts/wt-app.sh`. This is benchmark-owned glue, not a shipped Worktrunk command:

```sh
#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
: "${RWB_RUN_ID:?set a unique lowercase alphanumeric run id}"
: "${RWB_PYTHON:?absolute path to the shared pinned Python 3.13}"
: "${RWB_PYTHON_VERSION:?exact runtime version, e.g. the frozen provider version}"
: "${WT:?absolute path to the pinned Worktrunk release binary}"
: "${RWB_WT_CONFIG:?absolute path to the private Worktrunk user config}"
case "$RWB_RUN_ID" in ''|*[!a-z0-9]*) exit 2;; esac
test "$("$RWB_PYTHON" -c 'import platform; print(platform.python_version())')" = "$RWB_PYTHON_VERSION"
suffix=$("$RWB_PYTHON" -c 'import hashlib,os; print(hashlib.sha256(os.path.realpath(".").encode()).hexdigest()[:16])')
project="rwb-$RWB_RUN_ID-$suffix"
dc() { docker compose --project-name "$project" --file compose.yaml "$@"; }
endpoints() {
  pg=$(dc port postgres 5432)
  rd=$(dc port redis 6379)
  test -n "$pg" && test -n "$rd"
  export DATABASE_URL="postgresql://bench:bench@$pg/bench"
  export REDIS_URL="redis://$rd/0"
  "$WT" --config "$RWB_WT_CONFIG" config state vars set "database_url=$DATABASE_URL"
  "$WT" --config "$RWB_WT_CONFIG" config state vars set "redis_url=$REDIS_URL"
  "$WT" --config "$RWB_WT_CONFIG" config state vars set "compose_project=$project"
}
action=$1
shift
case "$action" in
  setup) uv sync --frozen --no-python-downloads --python "$RWB_PYTHON" ;;
  up) dc up -d --wait --wait-timeout 90; endpoints; .venv/bin/python -m rwbapp wait --timeout 30 ;;
  stop) dc stop --timeout 15 ;;
  start) dc start --wait --wait-timeout 90; endpoints; .venv/bin/python -m rwbapp wait --timeout 30 ;;
  purge) dc down --volumes --remove-orphans --timeout 15 ;;
  *) exit 2 ;;
esac
```

Dynamic published ports avoid relying on `hash_port` collision probability. Compose allocates the ports and scopes networks/volumes by the explicit project name. The hash derives from the full checkout path and includes the run id, so identical branch names in two separate clones still get different projects. Startup stores endpoints in native per-branch variables. Repeated application aliases read those variables directly, avoiding two Compose lookups and runtime revalidation on every command. Save project names and label/volume receipts before teardown. Use one private, fixed Compose CLI for every invocation; pin it on PATH rather than upgrading the user's plugin.

Noninteractive commands, after the fixture repository and providers are prepared:

```sh
# ROOT is the disposable fixture repository. Export all RWB_* values above.
# WT is the private pinned release binary. This config file is outside ROOT.
export WT RWB_RUN_ID RWB_PYTHON RWB_PYTHON_VERSION RWB_WT_CONFIG
printf 'worktree-path = "{{ repo_path }}.{{ branch | sanitize }}"\n' > "$RWB_WT_CONFIG"
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT" -y switch --create a --base HEAD --no-cd --format=json
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y up
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app migrate
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app mark --checkout a
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app crud --checkout a
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app cache --checkout a
RWB_CHECKOUT=a "$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y test
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT" -y switch --create b --base HEAD --no-cd --format=json
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.b" -y up
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.b" -y app migrate
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.b" -y app mark --checkout b
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.b" -y app check --checkout b --forbid a
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app check --checkout a --forbid b
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y cmd true
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app persist --checkout a
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y stop-services
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.b" -y app identity
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y start-services
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT.a" -y app persisted --checkout a
"$WT" --config "$RWB_WT_CONFIG" -C "$ROOT" -y remove a b --foreground --no-delete-branch
```

These commands and snippets are proposed implementation input, not a completed runtime result. `-y` grants approval for the benchmark's inspected project commands during each invocation. An external private `--config` avoids ambient user hooks/aliases. No shell integration installation, interactive picker, inherited activated shell, or global Git configuration is necessary.

Static verification completed: TOML and YAML snippets parsed, shell snippets passed `sh -n`, and the release CLI's `config alias show app` parsed the supplied project alias in an owned temporary Git repository. No hook, application command, or service ran during that check.

## Fair tasks and exact checks

- Measure worktree creation plus frozen dependency installation, service startup/readiness, integration workload, repeated aliases, and final removal separately. Do not mix `wt switch --execute` and `wt -C PATH app` into one warm-command sample. Aliases are the reusable command path; `switch --execute` additionally performs worktree selection and switch hooks.
- Run the normal two-worktree scenario first. If the requirement is two independently cloned repositories rather than two working directories, repeat with separate fixture clones and the same script; disclose their shared or independent Git/provider cache state. Worktrunk's normal worktrees share Git metadata, which is expected behavior.
- Capture `app identity` before and after startup. Require different PostgreSQL `system_identifier`, Redis `run_id`, owned container IDs and volume names, and each checkout's marker isolation. Equal in-container data paths/internal ports are valid because the containers are distinct. Container/project labels must map the endpoints to the intended checkout.
- Prove A stops by saved endpoint refusal and Docker status. Reading endpoints with `compose port` after stopping can fail or change, so preserve them before stop. Check B's identity and data while A is stopped. Run `persist` before stop, then `persisted` after restart with the same volumes. Require `pg_keeper == true` and `redis_durable == true` in the output, not merely exit zero, because the fixture only raises for the missing PostgreSQL keeper. Then rerun cache behavior. Redis `run_id` may change on restart; that alone is not lost persistent data.
- Create fresh checkout C from the committed fixture/config/lock, run the blocking setup hook, and verify unchanged `uv.lock` hash, exact Python patch, dependency versions and config hashes. Regenerate only endpoint/project identity. Worktrunk has no native dependency lock, so label lock enforcement `external uv` and recipe distribution `Git`.
- Check cleanup after `wt remove --foreground`: removed worktree paths, owned project containers/networks/volumes absent, B remains functional when only A is removed. Blocking directory removal alone does not prove Docker cleanup. Preserve hook stderr and Docker receipts, and never hide cleanup failure with `|| true`.
- Classify bad package/version handling as provider validation, not Worktrunk package resolution. Test malformed `.config/wt.toml` and a failing `pre-start` command as Worktrunk failures. Creation occurs before `pre-start`, so inspect and explicitly clean a leftover worktree after a setup failure. Test a fixed-port collision only in a separately labeled static-port variant; the normal dynamic-port recipe has no fixed port to occupy.

The new runner must not import older `eval/` outcomes as current results. No existing Worktrunk adapter was found there. Report Worktrunk-native capabilities, composed-stack success, external setup burden, and unexecuted cells separately; it is useful for developer workflow comparison without inventing native service features.

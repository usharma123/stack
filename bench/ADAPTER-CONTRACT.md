# Adapter contract (stable v1)

Status: stable as of 2026-10-06, core owner Opus implementation agent. Changes to
`rwb/adapters/base.py`, `rwb/scenario.py`, `rwb/verify.py`, `rwb/transport.py`, the
registry, or `fixtures/app` go through the core owner. If an adapter needs a new hook, note
it in your handoff file; do not edit core modules.

## Adding a competitor

1. Write `bench/rwb/adapters/<module>.py` with one `Adapter` subclass.
2. Put every checked-in file it copies into checkouts under `bench/adapters/<name>/`.
   They are hashed into `meta.json` per run. Benchmark glue, such as service scripts and
   holders, lives there too and must be declared `scripted`.
3. The registry entry already exists for announced adapters (`rwb/adapters/registry.py`).
   The class names are fixed there: `DevcontainersAdapter`, `DevpodAdapter`, `DdevAdapter`,
   `LandoAdapter`, `ProcessComposeAdapter`, `ServicesFlakeAdapter`, `PkgxAdapter`,
   `DnvrAdapter`, `GuixAdapter`, `WorkzAdapter`, `WorktrunkAdapter`, `GitGroveAdapter`,
   `IsolaAdapter`, `BerthAdapter`, `BranchboxAdapter`, `TiltAdapter`, `OrganistAdapter`,
   `VagrantAdapter`. The roster is frozen (`SCOPE.md`). Shared helpers for a family go in
   that family's own module (`worktree_common.py`, `agent_env_common.py`). Ask the core
   owner for new names.
4. Check offline with `python3 -m unittest discover -s bench/tests -v`. The contract test
   runs every importable registered adapter through the full scenario with fakes.
   Use `python3 bench/run.py --tool <name> --dry-run` to print every planned body.
5. Real runs are serialized by the parent. Do not start timed runs yourself.

## What the adapter returns

Adapters do not execute anything. Each method returns a **bash body**, and the scenario
sends it through the transport. Bodies must be non-interactive, propagate failure with
`set -e` or `&&` and never pipe a failing command into `tail`, `grep` or `|| true` to hide
it. Quote checkout paths with `q()` (`shlex.quote`).

| Member | Required | Meaning |
|---|---|---|
| `name`, `title` | yes | registry key, display name |
| `transport` | | `"docker"` (default): every checkout of this tool shares one disposable container from `image`, so A/B port conflicts are real. `"host"`: bodies run on the host in a run-owned temp dir (`self.root`), for tools that create their own containers |
| `image` | docker | local image, e.g. `ev-nix`; the container runs `sleep infinity` under the image's entrypoint as user `agent` |
| `features` | yes | every key of `base.FEATURES` -> `native` / `scripted` / `unsupported`. Missing keys raise `TypeError` |
| `isolation_boundary` | | `service-instance` (default), `container`, or `database`; see below |
| `variants` | | e.g. `("configured", "worktree")`; selected by `--tool name:variant`, available as `self.variant` |
| `not_applicable` | | `{check id: reason}`; these checks become `not_applicable` |
| `config_files` | | paths under `bench/adapters/<name>/` copied into every checkout (may contain `/`) |
| `shared_files` | | files from `bench/adapters/_shared/` copied into every checkout (shared scripted glue) |
| `start_waits_ready` | | `True` if `start()` blocks until the tool's readiness probes pass; the first identity is then checked without retries |
| `lock_files` | | checkout-relative lock paths; hashed after A's setup and compared in checkout C |
| `app_python` | | interpreter expression inside `enter`; default `"${UV_PROJECT_ENVIRONMENT:-.venv}/bin/python"` |
| `pins` | | dict recorded into `meta.json` (pinned versions, binary hashes) |
| `timeouts` | | `setup`, `start`, `step` and optional `ready` seconds |
| `provision()` | | `[(label, body, user)]`, run before anything else. Use it to install a pinned CLI and check its hash; failure marks `provision` failed |
| `versions()` | yes | body printing tool versions (+ hashes) |
| `mounts()` | | `[(host path, container path)]` read-only; default mounts `bench/` at `/rwb/src` |
| `local_env(co)` | | shell lines writing machine-local, uncommitted per-checkout settings (ports from `co.pg_port`/`co.redis_port`) |
| `prepare(co, lock_from)` | | default copies fixture + config + local env + committed lock from A, and writes the source token. Override for git worktrees and similar setups, but keep the token line |
| `setup(co)` | yes | resolve/lock + install tools (cold in A, warm caches in B, lock copied from A) |
| `deps(co)` | | app dependencies; default `uv sync --frozen` in `enter` |
| `enter(co, body)` | yes | run `body` inside the tool's environment for checkout `co` |
| `start(co)` / `ready(co)` / `stop(co)` / `status(co)` | services | `ready` = native readiness; return `None` to have the app's retrying `wait` used (scripted readiness) |
| `frozen_setup(co)` | | install from the committed lock without changing it; `None` = unsupported |
| `break_config(co)` | | shell lines making checkout D request a nonexistent version |
| `planned_pg_port(co)` | | body printing the port co *will* use, for tools that assign ports |
| `stopped_probe(co, identity)` | | exits 0 once co's services are gone; default checks that A's URL ports refuse. Override for container or shared-server tools |
| `service_processes()` | | lists this tool's leftover service processes (empty = clean) |
| `instance_identity(co)` | | optional JSON receipt (container IDs, volume names) that must differ between A and B |
| `app_source_path(co)` | | where the app process sees co's code (container mount path); `None` skips the path check |
| `host_env(workdir)`, `cleanup_host()`, `host_resources()` | host | env for host commands; remove / list every resource named with `self.run_id` (e.g. `rwb-<run id>-a`) |
| `cleanup(co)` | | final scoped teardown, default `stop` |

`Checkout` fields: `name` (`a`..`e`), `path`, `pg_port`, `redis_port`, `token`, `instance`.

## How the scenario uses them

Checkout A is cold and generates the lock. Checkout B is a teammate's checkout with A's
lock committed and warm caches. C is a frozen copy for lock reproduction, D has a bad
config, and E has an occupied port. The step list, check ids and outcome rules are in
`IMPLEMENTATION.md` and `rwb/scenario.py`. Native `ready()` claims usable services, so the
first app `identity` must succeed with no retries. With scripted readiness, the app's
`wait` retries connection errors only.

## Correctness gates every adapter faces

- **Source identity.** The harness writes a random per-checkout token into
  `rwbapp/SOURCE_TOKEN`. The running app must report checkout `co`'s token, and its module
  must lie under `app_source_path(co)`. This catches tools that run the root checkout's
  code for a worktree. Configure correct build contexts and mounts in the adapter; never
  change the fixture.
- **URL truth.** The server ports the app reached must equal the ports in
  `DATABASE_URL`/`REDIS_URL`. Tool-exported `PGDATA`/`REDISDATA` must equal the
  server's directories.
- **Isolation** depends on the declared boundary. In every boundary, the unprefixed table
  `checkouts` and Redis key `rwb:checkout` must hold only the checkout's own marker.
  - `service-instance`: different PG cluster (system_identifier, or data dir+port+start)
    and different Redis run_id.
  - `container`: same, and paths/ports may repeat. `instance_identity` receipts must differ.
  - `database`: a shared cluster and server are legitimate. The (cluster, database) pair
    and the (run_id, db index) pair must differ. A restart then needs only data reuse,
    not new processes.
- **Persistence:** Postgres rows must survive stop/start. Redis gets an explicit `SAVE`
  before stop, and the durability policy (`appendonly`/`appendfsync`/`save`/`dir`) is
  reported. Pass/fail comes from the JSON booleans `pg_keeper` and `redis_durable` in the
  `persisted` receipt, not from the exit code. `persisted` exits 0 even when Redis lost
  data.
- **Exit codes are never enough:** app checks parse the one-line JSON receipt
  (`ok`, `result`). Repeated `read` samples also check the returned item.
- Missing Stack-only features (for example `wrong_instance_guard`) are recorded as
  `unsupported`, never as `fail`.

## Safety

Use only resources whose names contain `self.run_id`. Never prune, never stop anything
by global process name, and never touch existing containers, including long-running
`ev-*` ones. Docker adapters may do anything inside their own container. Host adapters
must make `cleanup_host()` complete and `host_resources()` truthful.

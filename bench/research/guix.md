# GNU Guix

Researched 2026-10-06. Guix belongs in the package manager comparison. `guix shell` supplies packages and runs commands; checkout service lifecycle needs an explicit script or a separate supervisor. Linux ARM64 is supported. Native macOS is unsupported. The current local setup has no Guix installation and fails the default-container namespace preflight, so record an environment blocker until a dedicated Linux environment is provisioned. No Guix package build, application run, service launch, or timing was executed for this note.

## Version and source identity

- Latest stable tag observed in the official repository is `v1.5.0`, released 2026-01-23. Annotated tag object `749a73cacad30fd9e149d9086c7e4e4a0b86834b` points to commit `230aa373f315f247852ee07dff34146e9b480aec`. The official announcement lists `guix-binary-1.5.0.aarch64-linux.tar.xz`. [Release announcement](https://lists.nongnu.org/archive/html/guix-devel/2026-01/msg00125.html).
- Official clone URL `https://git.guix.gnu.org/guix.git` redirects to `https://codeberg.org/guix/guix.git`. Shallow source checkout at `/tmp/stack-bench-sources/guix` is master commit `71d010188f039817c465985e46e185445fda6946`, dated 2026-10-06. This is a development revision, not another stable release.
- No local `guix` executable or Guix Docker image was present. Existing `ev-base:latest` is Linux ARM64 image `sha256:748dca6569d1619558c68bcdda89b1770896995c3a286a26f72de572d55b7a2b`. Do not report an available local Guix version.

## What the implementation does

`time-machine.scm` resolves its channel list, opens the store, obtains a cached channel instance, then executes that instance's `bin/guix`. Authentication and certificate verification default to enabled. A pinned channel therefore fixes package definitions and their transitive inputs, rather than only the top-level version strings. First invocation can fetch/build the Guix instance; subsequent invocations reuse it. [Implementation](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/guix/scripts/time-machine.scm#L245).

`shell.scm` caches the environment profile. The cache key includes channel commits, system, graft choice and manifest device/inode; modification time decides whether to rebuild. The implementation expressly warns that external state read by a manifest can invalidate caching assumptions. Keep this benchmark manifest static. `--rebuild-cache` rebuilds the environment cache, not an empty package store. [Caching implementation](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/guix/scripts/shell.scm#L242).

`environment.scm` builds the profile before launching the child command and returns its exit status. Plain shells do not introduce network, PID or data-directory isolation. `--pure` removes inherited environment variables except preserved and essential variables. `--container` adds Linux namespaces; without `--network`, that container has only its own loopback. These are environment isolation features, not PostgreSQL/Redis readiness or lifecycle management. [Launch implementation](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/guix/scripts/environment.scm#L1267), [shell manual](https://guix.gnu.org/manual/en/html_node/Invoking-guix-shell.html).

Guix System and Guix Home with Shepherd are separate supported service approaches. Selecting those would require a separate recipe and provisioning boundary. Do not credit their services to a plain `guix shell` manifest. [Guix Home Shepherd integration](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/gnu/home/services/shepherd.scm).

## Pinned recipe for a provisioned Linux runner

Use the development commit inspected here because its package definitions satisfy Python 3.13. Save both files in each independent checkout. Exact versions at this commit are Python 3.13.13, PostgreSQL 16.14, Redis 7.2.6 and uv 0.10.12. These patch versions differ from other providers unless those providers are aligned. Redis 7.2.6 also differs from a Redis 8 workload; do not silently substitute Valkey or label the server Redis 8. [Python definition](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/gnu/packages/python.scm#L1126), [PostgreSQL and Redis definitions](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/gnu/packages/databases.scm#L1657), [uv definition](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/gnu/packages/build-tools.scm#L1760).

`channels.scm`:

```scheme
(use-modules (guix channels))
(list (channel
        (inherit %default-guix-channel)
        (url "https://git.guix.gnu.org/guix.git")
        (commit "71d010188f039817c465985e46e185445fda6946")))
```

Inheriting the default channel retains its official authentication introduction. Do not use `--disable-authentication` to shorten setup. [Default channel definition](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/guix/channels.scm#L212).

`manifest.scm`:

```scheme
(use-modules (guix profiles) (gnu packages))
(specifications->manifest
 '("python@3.13.13" "postgresql@16.14" "redis@7.2.6"
   "uv@0.10.12" "bash" "coreutils" "nss-certs"))
```

Use explicit `-q`, channel file and manifest on every invocation. Do not rely on shell startup files or auto-discovery. The following fresh-shell commands are a recipe, not executed evidence:

```sh
guix time-machine -q -C channels.scm -- describe -f channels
guix time-machine -q -C channels.scm -- shell -q --pure -m manifest.scm -- \
  bash --noprofile --norc -c \
  'python3 --version; postgres --version; redis-server --version; uv --version'
guix time-machine -q -C channels.scm -- shell -q --pure -m manifest.scm -- \
  bash --noprofile --norc -c \
  'uv sync --frozen --python "$(command -v python3)" --no-python-downloads'
```

Resolve `python3` inside the Guix environment, as the quoted Bash command does. The canonical app has its own `uv.lock`; Guix's manifest does not lock PyPI dependencies. Guix packaged psycopg/redis Python dependencies differ from the fixture's pins, so use the same frozen uv lock as other script-based providers. Keep dependency installation in its own measured task.

## Service-script workflow

Run as an unprivileged benchmark user because PostgreSQL refuses root. Put data under each checkout's absolute `.guix-bench` directory. Assign explicit distinct `PGPORT` and `REDIS_PORT` for A and B when sharing a Linux network namespace. For separate containers, use container/volume identity and markers rather than demanding different internal path strings.

All service commands below run inside the pinned shell. The adapter should use a checked-in Bash script to set paths and URLs before each subcommand; `--pure` otherwise removes caller-provided URLs. With `$state` set to the checkout's absolute `.guix-bench` path:

```sh
mkdir -p "$state/redis" "$state/socket"
test -e "$state/pg/PG_VERSION" || initdb -D "$state/pg" -U bench --auth=trust
pg_ctl -D "$state/pg" -l "$state/postgres.log" \
  -o "-h 127.0.0.1 -p $PGPORT -k $state/socket" -w start
redis-server --bind 127.0.0.1 --port "$REDIS_PORT" \
  --dir "$state/redis" --dbfilename dump.rdb --appendonly yes \
  --pidfile "$state/redis.pid" --logfile "$state/redis.log" --daemonize yes
export DATABASE_URL="postgresql://bench@127.0.0.1:$PGPORT/postgres"
export REDIS_URL="redis://127.0.0.1:$REDIS_PORT/0"
.venv/bin/python -m rwbapp wait --timeout 60
.venv/bin/python -m rwbapp migrate
.venv/bin/python -m pytest -q
```

Readiness must query the actual SQL/Redis endpoints and verify process ownership. Redis daemonization can return before a bind failure appears; require that the recorded Redis PID is live and equals `INFO server`'s `process_id`. PostgreSQL's `-w` is useful but still capture `data_directory` and `pg_control_system().system_identifier`. Preserve both logs on failure. Keep Unix socket paths short enough for the platform limit.

Repeated `guix time-machine ... shell ... -- bash ./services.sh exec ...` commands should reuse live services and the cached package profile. Starting a new shell is not restarting PostgreSQL or Redis. A stopped checkout retains `.guix-bench`; restarting skips `initdb`. Before stop, persist app markers and Redis data. After stop, verify both listeners/PIDs are absent. Restart, then run the fixture's `persisted` and isolation checks. Use `pg_ctl -D "$state/pg" -m fast -w stop` and `redis-cli -h 127.0.0.1 -p "$REDIS_PORT" shutdown save` only after matching the endpoint to this checkout's recorded identity. Stop A and verify B's CRUD/cache operations still work. Destructive teardown removes only the owned checkout state after processes stop.

Copy `channels.scm`, `manifest.scm`, service scripts, `pyproject.toml` and `uv.lock` for the reproducibility task. Exclude runtime PIDs, logs, `.venv` and database files. Verify channel SHA, interpreter path/version, package store paths and fixture dependency versions after recreation. Exact definitions do not guarantee old substitute availability or successful historical source builds; preserve provisioning failures. [Time-machine manual](https://guix.gnu.org/manual/en/html_node/Invoking-guix-time_002dmachine.html).

## Installation boundary and observed blocker

The build system accepts `aarch64-linux` and excludes Darwin from supported platforms. Use a dedicated Linux ARM64 VM/container on this macOS ARM64 host. [Supported systems implementation](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/m4/guix.m4#L87).

Binary installation unpacks `/gnu` and `/var/guix`, creates accounts and configures a running store daemon. The official installer requires root; it is not a user-directory executable drop-in. In a container without an init system, the installer prints a manual daemon command. Provision the container only, with its own store and daemon, then run the app as an unprivileged user. Guix must have authorized official substitute keys or it may build the whole closure from source. [Installer implementation](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/etc/guix-install.sh#L445), [binary installation manual](https://guix.gnu.org/manual/en/html_node/Binary-Installation.html).

Observed preflight, with `ev-base` also returning exit 127 and `guix: No such file or directory` for an executable check:

```text
$ docker run --rm --entrypoint /usr/bin/unshare ev-base:latest -Ur true
unshare: unshare failed: Operation not permitted
exit 1
```

This demonstrates unavailable user namespaces under the current default container policy. Guix's shell container path explicitly checks them. It does not prove that every plain-shell or root-daemon configuration fails. The daemon can run as root with build users, or as an unprivileged account using namespaces. Docker's seccomp policy can also prevent disabling ASLR. The manual documents `--allow-aslr`, and `--disable-chroot` as a discouraged fallback that weakens build isolation. Do not silently enable either flag or use a privileged container and claim the default sandboxed setup passed. [Namespace checks](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/guix/scripts/environment.scm#L1126), [daemon/manual source](https://codeberg.org/guix/guix/src/commit/71d010188f039817c465985e46e185445fda6946/doc/guix.texi#L1777).

`curl -IL --max-time 20` and a second attempt with 15 seconds to the official ARM64 binary URL timed out with exit 28. A HEAD request to `https://guix.gnu.org/guix-install.sh` returned HTTP 200. Browser fetches of guix.gnu.org documentation returned 403; the corresponding official manual source in the clone was read instead. Network download failure is distinct from unsupported architecture. No installer, daemon or host configuration change was attempted.

## Fair measurements and failure traps

Include first environment realization, frozen Python dependency install, migrations/tests, warm command entry, two-checkout isolation, stop-A/keep-B-running, restart persistence and recreate-from-config. Guix has no native shell service readiness, automatic checkout port allocation or app data lifecycle in this recipe; report those as user-scripted. Its channel-level reproducibility is native. Keep VM/container provisioning separate from steady-state application timings, and disclose cache/store state.

Do not call `guix pull` inside warm tasks, use the unversioned `python` or `postgresql` aliases, equate `--pure` with sandboxing, leave user channels implicit, treat `--rebuild-cache` as a cold install, or kill by process name. Do not import old `eval` results: the old comparison explicitly listed Guix as unbenchmarked. The recipe above remains unexecuted until provisioning succeeds.

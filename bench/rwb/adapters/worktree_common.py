"""Shared host-transport base for the Git worktree managers (workz, Worktrunk, GitGrove).

None of these tools installs Python or runs PostgreSQL/Redis. Each adapter therefore pairs
the tool's own worktree, port and hook features with the same explicit provider glue:

- a private, hash-checked CPython 3.13.16 (python-build-standalone) and uv 0.12.23, put on
  PATH so uv resolves the committed `.python-version` with downloads disabled;
- Docker Compose projects with digest-pinned PostgreSQL 17.6 / Redis 8.10.2 images and
  project-scoped named volumes, one project per worktree, every name containing the run id.

All of that glue is declared `scripted`. Tool binaries, HOME, Git config, Docker client
config and caches live under the run-owned directory; nothing is installed globally.

Host prerequisites (recorded in pins, never installed): macOS arm64, Docker Desktop with the
Compose plugin, Git, and Node.js for GitGrove.
"""
import json
import os
import shutil

from .base import Adapter, q

UV_VERSION = "0.12.23"
UV_URL = f"https://github.com/astral-sh/uv/releases/download/{UV_VERSION}/uv-aarch64-apple-darwin.tar.gz"
UV_SHA256 = "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544"
PYTHON_VERSION = "3.13.16"
PYTHON_URL = ("https://github.com/astral-sh/python-build-standalone/releases/download/20261003/"
              "cpython-3.13.16%2B20261003-aarch64-apple-darwin-install_only_stripped.tar.gz")
PYTHON_SHA256 = "9e01f63bbb08576cd9c8bc2d0564d098cb30c8453a0cd4bcf6aef458f6d2a147"
POSTGRES_IMAGE = "postgres:17.6-alpine@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94"
REDIS_IMAGE = "redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0"

# Host programs linked into the private PATH (resolved once, recorded in pins).
HOST_PROGRAMS = ("docker", "git", "docker-credential-desktop")

# Prints one JSON receipt for a Compose project: containers (id, service, image, state,
# health, exact host port bindings) and its named volumes. Exits 1 if postgres or redis is
# missing or not running, so a broken receipt can never compare equal to a good one.
RECEIPT_PY = r'''
import json, subprocess, sys
project = sys.argv[1]
def docker(*args):
    return subprocess.run(["docker", *args], check=True, capture_output=True, text=True).stdout
ids = docker("ps", "-a", "-q", "--no-trunc", "--filter", f"label=com.docker.compose.project={project}").split()
containers = []
for item in (json.loads(docker("inspect", *ids)) if ids else []):
    labels = item["Config"]["Labels"]
    if labels.get("com.docker.compose.project") != project:
        sys.exit(f"label mismatch for {item['Id']}")
    state = item["State"]
    containers.append(dict(
        service=labels.get("com.docker.compose.service"), id=item["Id"], image=item["Image"],
        state=state["Status"], health=(state.get("Health") or {}).get("Status"),
        ports={k: [f"{b['HostIp']}:{b['HostPort']}" for b in (v or [])]
               for k, v in sorted((item["NetworkSettings"]["Ports"] or {}).items())},
        volumes=sorted(m["Name"] for m in item["Mounts"] if m.get("Type") == "volume")))
containers.sort(key=lambda c: c["service"] or "")
volumes = sorted(docker("volume", "ls", "-q", "--filter", f"label=com.docker.compose.project={project}").split())
receipt = dict(project=project, containers=containers, volumes=volumes,
               bindings={c["service"]: c["ports"] for c in containers})
print(json.dumps(receipt, sort_keys=True))
running = {c["service"] for c in containers if c["state"] == "running"}
missing = {"postgres", "redis"} - running
if missing:
    sys.exit(f"project {project}: not running: {sorted(missing)}")
'''

# Port the benchmark predicts a probe-based allocator will hand out next: the first port at
# or after START that binds on 127.0.0.1 and is not published by a running container.
NEXT_FREE_PY = r'''
import re, socket, subprocess, sys
start = int(sys.argv[1])
published = set(int(p) for p in re.findall(r":(\d+)->", subprocess.run(
    ["docker", "ps", "--format", "{{.Ports}}"], check=True, capture_output=True, text=True).stdout))
for port in range(start, start + 200):
    if port in published:
        continue
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", port))
    except OSError:
        continue
    finally:
        s.close()
    print(port)
    break
else:
    sys.exit("no free port")
'''
# Port-mapping receipt for the core `port_map` hook: the app runs on the host and reaches
# each service through the Docker published port of THIS checkout's own container.
PORT_MAP_PY = r'''
import json, subprocess, sys
project = sys.argv[1]
def inspect(service):
    ids = subprocess.run(["docker", "ps", "-q", "--no-trunc",
                          "--filter", f"label=com.docker.compose.project={project}",
                          "--filter", f"label=com.docker.compose.service={service}"],
                         check=True, capture_output=True, text=True).stdout.split()
    if len(ids) != 1:
        sys.exit(f"{project}/{service}: expected one running container, found {len(ids)}")
    return json.loads(subprocess.run(["docker", "inspect", ids[0]], check=True,
                                     capture_output=True, text=True).stdout)[0]
out, evidence = {}, []
for key, service, target in (("pg", "postgres", 5432), ("redis", "redis", 6379)):
    item = inspect(service)
    bindings = (item["NetworkSettings"]["Ports"] or {}).get(f"{target}/tcp") or []
    hosts = sorted({(b["HostIp"], int(b["HostPort"])) for b in bindings})
    if len(hosts) != 1 or hosts[0][0] != "127.0.0.1":
        sys.exit(f"{project}/{service}: expected one 127.0.0.1 binding for {target}/tcp, got {bindings}")
    out[key] = dict(published=hosts[0][1], target=target)
    evidence.append(f"docker inspect {item['Id'][:12]} ({project}/{service}) {target}/tcp -> 127.0.0.1:{hosts[0][1]}")
out["evidence"] = "; ".join(evidence)
print(json.dumps(out, sort_keys=True))
'''


class WorktreeHostAdapter(Adapter):
    """Common host-side glue. Subclasses provide the tool's own worktree/lifecycle verbs."""
    transport = "host"
    isolation_boundary = "container"
    start_waits_ready = True        # `docker compose up --wait` blocks on healthchecks
    lock_files = ("uv.lock",)
    # Checkout D pins CPython 3.13.99 (break_config); uv / the image registry must name it.
    bad_config_pattern = r"3\.13\.99"
    setup_scope = ("tool-native worktree sync/config check plus scripted provider resolution "
                   "(pinned CPython found by uv, committed uv.lock checked); dependencies install "
                   "in the deps step, services start in the start step")
    cache_note = ("host Docker image cache and the run-private uv cache are not cleared; "
                  "service images may already be present on the host")
    timeouts = dict(setup=1800, start=300, step=300, ready=90)
    repo_name = "rwb"               # fixture repository directory under the run root
    # bench/adapters/<name>/<file> -> path committed in the fixture repository.
    repo_files = {}
    # Programs a subclass additionally needs from the host (e.g. node for GitGrove).
    extra_host_programs = ()

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.config_files = tuple(self.repo_files)
        self.docker_config = self.options.get("docker_config", os.path.expanduser("~/.docker"))
        self.host_programs = {}
        for program in HOST_PROGRAMS + self.extra_host_programs:
            found = self.options.get(f"host_{program}") or shutil.which(program)
            self.host_programs[program] = os.path.realpath(found) if found else None
        self.pins = dict(uv=UV_VERSION, uv_sha256=UV_SHA256, python=PYTHON_VERSION,
                         python_sha256=PYTHON_SHA256, postgres_image=POSTGRES_IMAGE,
                         redis_image=REDIS_IMAGE, host_programs=dict(self.host_programs),
                         docker_config_source=self.docker_config, platform="darwin-arm64 host")

    # ---- run-owned layout ------------------------------------------------------------
    def realroot(self):
        # macOS temp dirs live behind /var -> /private/var; the app reports resolved module
        # paths, so checkout paths must be resolved too (no false wrong-source verdicts).
        return os.path.realpath(self.workdir())

    def repo(self):
        return f"{self.realroot()}/{self.repo_name}"

    def tools(self):
        return f"{self.realroot()}/tools"

    def private_home(self):
        return f"{self.realroot()}/home"

    def branch(self, name):
        return f"rwb-{self.run_id}-{name}"

    def project(self, co):
        """Compose project for checkout co (always contains the run id)."""
        return self.branch(co.name)

    def owned_patterns(self):
        """Run tokens that every owned Docker resource name contains."""
        return sorted({self.run_id, self.run_id.replace("-", "_")})

    def host_env(self, workdir):
        home = self.private_home()
        path = [f"{self.tools()}/bin", f"{self.tools()}/python/bin", f"{self.tools()}/hostbin",
                "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        env = dict(HOME=home, PATH=":".join(path), DOCKER_CONFIG=f"{home}/.docker",
                   XDG_CONFIG_HOME=f"{home}/.config", XDG_CACHE_HOME=f"{home}/.cache",
                   XDG_DATA_HOME=f"{home}/.local/share", UV_CACHE_DIR=f"{home}/.cache/uv",
                   UV_PYTHON_DOWNLOADS="never", GIT_CONFIG_NOSYSTEM="1",
                   GIT_CONFIG_GLOBAL=f"{home}/.gitconfig", GIT_TERMINAL_PROMPT="0",
                   GIT_AUTHOR_NAME="rwb", GIT_AUTHOR_EMAIL="rwb@invalid",
                   GIT_COMMITTER_NAME="rwb", GIT_COMMITTER_EMAIL="rwb@invalid",
                   npm_config_cache=f"{home}/.npm", npm_config_update_notifier="false",
                   LANG="en_US.UTF-8", NO_COLOR="1", RWB_RUN_ID=self.run_id)
        for key in ("USER", "LOGNAME", "TMPDIR", "SHELL"):
            if os.environ.get(key):
                env[key] = os.environ[key]
        self.pins["host_env"] = dict(env)
        return env

    # ---- provisioning ----------------------------------------------------------------
    def fetch(self, url, sha256, dest):
        return "\n".join([
            f'curl -fsSL --retry 3 -o {q(dest)}.part {q(url)}',
            f'echo "{sha256}  {dest}.part" | shasum -a 256 -c -',
            f'mv {q(dest)}.part {q(dest)}'])

    def provision(self):
        tools, home = self.tools(), self.private_home()
        missing = [p for p, path in self.host_programs.items() if not path]
        links = [f"ln -sfn {q(path)} {q(tools)}/hostbin/{q(name)}" for name, path in self.host_programs.items() if path]
        base = "\n".join([
            "set -euo pipefail",
            'test "$(uname -s)/$(uname -m)" = Darwin/arm64 || { echo "pins are for macOS arm64" >&2; exit 1; }',
            *([f'echo "missing host programs: {" ".join(missing)}" >&2; exit 1'] if missing else []),
            f"mkdir -p {q(tools)}/bin {q(tools)}/hostbin {q(tools)}/dl {q(home)}/.docker {q(home)}/.config",
            *links,
            f": > {q(home)}/.gitconfig",
            self.fetch(UV_URL, UV_SHA256, f"{tools}/dl/uv.tar.gz"),
            f"tar -xzf {q(tools)}/dl/uv.tar.gz -C {q(tools)}/dl",
            f"install -m 755 {q(tools)}/dl/uv-aarch64-apple-darwin/uv {q(tools)}/bin/uv",
            self.fetch(PYTHON_URL, PYTHON_SHA256, f"{tools}/dl/python.tar.gz"),
            f"tar -xzf {q(tools)}/dl/python.tar.gz -C {q(tools)}",
            f'test "$({q(tools)}/python/bin/python3 -c "import platform; print(platform.python_version())")" = {PYTHON_VERSION}',
            f'{q(tools)}/bin/uv --version | grep -q "^uv {UV_VERSION} "',
            # Private Docker client config: same daemon context, credential helper and CLI
            # plugins as the user's, but nothing the run does writes into ~/.docker.
            f"if [ -d {q(self.docker_config)}/contexts ]; then cp -R {q(self.docker_config)}/contexts {q(home)}/.docker/; fi",
            f"{q(tools)}/python/bin/python3 -I - {q(self.docker_config)}/config.json {q(home)}/.docker/config.json "
            f"{q(self.docker_config)}/cli-plugins <<'PY'\n"
            "import json, os, sys\n"
            "src, dest, plugins = sys.argv[1:]\n"
            "cfg = json.load(open(src)) if os.path.exists(src) else {}\n"
            "out = {k: cfg[k] for k in ('credsStore', 'currentContext') if k in cfg}\n"
            "out['cliPluginsExtraDirs'] = [plugins]\n"
            "json.dump(out, open(dest, 'w'), indent=1)\nPY",
            "docker compose version",
            "docker info --format '{{.ServerVersion}} {{.OSType}}/{{.Architecture}}'",
        ])
        return [("provision-providers", base, None)] + self.provision_tool()

    def provision_tool(self):
        return []

    def common_versions(self):
        return ("uv --version; python3 -VV; shasum -a 256 \"$(command -v uv)\"; "
                "git --version; docker version --format '{{.Client.Version}} {{.Server.Version}}'; "
                "docker compose version")

    # ---- fixture repository (one per run; worktrees branch off its main) --------------
    def ensure_repo(self):
        repo = self.repo()
        lines = [f"if [ ! -d {q(repo)}/.git ]; then",
                 f"  mkdir -p {q(repo)}",
                 f"  cp -R {q(str(self.src_host()))}/fixtures/app/. {q(repo)}/",
                 f"  rm -rf {q(repo)}/.venv",
                 f"  find {q(repo)} -name __pycache__ -type d -prune -exec rm -rf {{}} +"]
        for src, dest in self.repo_files.items():
            if "/" in dest:
                lines.append(f"  mkdir -p {q(repo)}/{q(dest.rsplit('/', 1)[0])}")
            lines.append(f"  cp {q(str(self.config_dir()))}/{q(src)} {q(repo)}/{q(dest)}")
        lines += [f"  git -C {q(repo)} init -q -b main",
                  f"  git -C {q(repo)} add -A",
                  f"  git -C {q(repo)} commit -q -m 'rwb fixture {self.run_id}'",
                  "fi"]
        return "\n".join(lines)

    def src_host(self):
        from .base import BENCH
        return BENCH

    def token_and_lock(self, co, lock_from):
        lines = [f"test -d {q(co.path)}/rwbapp || {{ echo 'worktree missing at {co.path}' >&2; exit 1; }}",
                 f"printf '%s\\n' {q(co.token)} > {q(co.path)}/rwbapp/SOURCE_TOKEN"]
        if lock_from is not None:
            lines += [f"cp {q(lock_from.path)}/{q(rel)} {q(co.path)}/{q(rel)}" for rel in self.lock_files]
        return lines

    def break_config(self, co):
        # Provider pin: request a CPython patch release that does not exist.
        return f"printf '3.13.99\\n' > {q(co.path)}/.python-version"

    def uv_provider_check(self, co):
        """Scripted provider resolution: uv finds the pinned interpreter (no downloads) and
        the committed lock matches pyproject without being rewritten."""
        return "\n".join([
            f"cd {q(co.path)}",
            'want=$(cat .python-version)',
            'py=$(uv python find --no-python-downloads)',
            'got=$("$py" -c "import platform; print(platform.python_version())")',
            'test "$got" = "$want" || { echo "uv resolved Python $got, pinned $want" >&2; exit 1; }',
            "uv lock --locked"])

    # ---- Docker receipts and probes ----------------------------------------------------
    def compose_receipt(self, project):
        return f"python3 -I - {q(project)} <<'PY'\n{RECEIPT_PY.strip()}\nPY"

    def port_map(self, co):
        """Core hook: exact Docker host-port bindings of co's own postgres/redis containers."""
        return f"python3 -I - {q(self.project(co))} <<'PY'\n{PORT_MAP_PY.strip()}\nPY"

    def instance_identity(self, co):
        return self.compose_receipt(self.project(co))

    def no_running_containers(self, project):
        return (f'running=$(docker ps -q --filter label=com.docker.compose.project={q(project)}) && '
                f'test -z "$running" || {{ echo "containers of {project} still running: $running" >&2; exit 1; }}')

    def ports_refuse(self, *ports):
        return " && ".join(f"({self.port_closed_var(p)})" for p in ports)

    def port_closed_var(self, port):
        # Like Adapter.port_closed, but the port may be a shell expression.
        return (f'for i in $(seq 1 150); do (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null || exit 0; '
                f'sleep 0.2; done; echo "port {port} still accepting" >&2; exit 1')

    def owned_filter_awk(self, field):
        """awk condition: field contains one of this run's tokens."""
        return " || ".join(f"index(${field}, {json.dumps(p)})" for p in self.owned_patterns())

    def service_processes(self):
        # Leftover *running* service containers of this run (each line one container). A
        # failing Docker query prints a line, so it can never read as "clean".
        return ("out=$(docker ps --filter label=com.docker.compose.project "
                "--format '{{.ID}} {{.Label \"com.docker.compose.project\"}} {{.Names}} {{.Status}}') "
                "|| { echo 'docker ps failed'; exit 1; }; "
                f"printf '%s\\n' \"$out\" | awk '{self.owned_filter_awk(2)}'")

    def supervisor_processes(self):
        # Stopped-but-present containers of this run (observed, not counted as a leak).
        return ("out=$(docker ps -a --filter status=exited --filter label=com.docker.compose.project "
                "--format '{{.ID}} {{.Label \"com.docker.compose.project\"}} {{.Names}} {{.Status}}') "
                "|| { echo 'docker ps failed'; exit 1; }; "
                f"printf '%s\\n' \"$out\" | awk '{self.owned_filter_awk(2)}'")

    def host_resources(self):
        awk = self.owned_filter_awk(1)
        return "\n".join([
            "set -uo pipefail",
            "fail=0",
            f"c=$(docker ps -a --format '{{{{.Label \"com.docker.compose.project\"}}}} {{{{.Names}}}}') || fail=1",
            f"v=$(docker volume ls --format '{{{{.Name}}}}') || fail=1",
            f"n=$(docker network ls --format '{{{{.Name}}}}') || fail=1",
            f"i=$(docker image ls --format '{{{{.Repository}}}}:{{{{.Tag}}}}') || fail=1",
            '[ "$fail" = 0 ] || echo "docker query failed"',
            f"printf '%s\\n' \"$c\" | awk '{awk} {{print \"container \" $0}}'",
            f"printf '%s\\n' \"$v\" \"$n\" | awk '{awk} {{print \"volume-or-network \" $0}}'",
            f"printf '%s\\n' \"$i\" | awk '{awk} {{print \"image \" $0}}'",
        ])

    def cleanup_host(self):
        """Remove every Docker resource of this run, matched by exact Compose project label
        pattern (rwb[-_]<run id>[-_]<checkout>) or the run id in its name. Worktrees, repo,
        tools and HOME live under the run root, which the harness deletes afterwards."""
        run_forms = "|".join(self.owned_patterns())  # [a-z0-9_-] only
        project_re = f"^rwb[-_]({run_forms})[-_][a-e]$"
        awk = self.owned_filter_awk(1)
        return "\n".join([
            "set -euo pipefail",
            f"re={q(project_re)}",
            "projects=$( { docker ps -a --format '{{.Label \"com.docker.compose.project\"}}';"
            " docker volume ls --format '{{.Label \"com.docker.compose.project\"}}';"
            " docker network ls --format '{{.Label \"com.docker.compose.project\"}}'; } | sort -u)",
            'for p in $projects; do',
            '  printf "%s" "$p" | grep -Eq "$re" || continue',
            '  ids=$(docker ps -a -q --filter "label=com.docker.compose.project=$p")',
            '  [ -z "$ids" ] || docker rm -f $ids >/dev/null',
            '  nets=$(docker network ls -q --filter "label=com.docker.compose.project=$p")',
            '  [ -z "$nets" ] || docker network rm $nets >/dev/null',
            '  vols=$(docker volume ls -q --filter "label=com.docker.compose.project=$p")',
            '  [ -z "$vols" ] || docker volume rm $vols >/dev/null',
            "done",
            f"imgs=$(docker image ls --format '{{{{.Repository}}}}:{{{{.Tag}}}}' | awk '{awk}')",
            '[ -z "$imgs" ] || docker image rm $imgs >/dev/null',
        ])

    def compose_down_volumes(self, project):
        """Destroy one checkout's containers, network AND data volumes (cleanup only)."""
        return "\n".join([
            f'ids=$(docker ps -a -q --filter label=com.docker.compose.project={q(project)})',
            '[ -z "$ids" ] || docker rm -f $ids >/dev/null',
            f'nets=$(docker network ls -q --filter label=com.docker.compose.project={q(project)})',
            '[ -z "$nets" ] || docker network rm $nets >/dev/null',
            f'vols=$(docker volume ls -q --filter label=com.docker.compose.project={q(project)})',
            '[ -z "$vols" ] || docker volume rm $vols >/dev/null'])

    def verify_project_gone(self, project):
        return (f'left="$(docker ps -a -q --filter label=com.docker.compose.project={q(project)})'
                f'$(docker volume ls -q --filter label=com.docker.compose.project={q(project)})'
                f'$(docker network ls -q --filter label=com.docker.compose.project={q(project)})" && '
                f'test -z "$left" || {{ echo "resources of {project} remain: $left" >&2; exit 1; }}')

    # ---- app commands: no `cd`; the tool must place the process in co's worktree -----
    def app(self, co, args):
        return self.enter(co, f"RWB_CHECKOUT={co.name} {self.app_python} -m rwbapp {args}")

    def pytest(self, co):
        return self.enter(co, f"RWB_CHECKOUT={co.name} {self.app_python} -m pytest -q -p no:cacheprovider")

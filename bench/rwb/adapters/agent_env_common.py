"""Shared helpers for the agent-environment family: isola, Berth and BranchBox.

Everything here is benchmark-owned glue. It pins the canonical fixture runtime, builds
run-owned git repositories, scrubs host environments, and lists or removes only the
Compose resources whose project names carry this run's id.
"""
import os
from pathlib import Path
import re

from .base import q

# Canonical fixture runtime (bench/adapters/stack/stack.toml). The isola lane installs these
# exact versions with mise. Berth and BranchBox use the same versions as digest-pinned
# images (digests from research/berth.md). The checked-in Dockerfiles and compose files
# repeat these literals; tests/test_agent_env_adapters.py keeps them in sync.
CANONICAL = dict(python="3.13.16", uv="0.12.23", postgres="17.11", redis="8.10.2")
IMAGES = dict(
    python="python:3.13.16-slim-bookworm@sha256:a1165e272e578941b84abc79e4ab38a0305cd12803a5c4247979ac7655f4d641",
    uv="ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21",
    postgres="postgres:17.11-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24",
    redis="redis:8.10.2-alpine@sha256:3811787313eba226a2ef38658c6ccb91cd5e110edc89c37767de373120a0e5a0",
)

# Variables that would override a tool's generated Compose env file or point git at
# another repository. Removed from every host command.
SCRUBBED = ("BERTH_NAME", "COMPOSE_PROJECT_NAME", "COMPOSE_FILE", "COMPOSE_PROFILES", "PGPORT",
            "REDIS_PORT", "DATABASE_URL", "REDIS_URL", "RWB_WORKSPACE", "GIT_DIR", "GIT_WORK_TREE",
            "VIRTUAL_ENV", "UV_PROJECT_ENVIRONMENT", "PYTHONPATH")

GIT_ID = ("-c user.name='Stack benchmark' -c user.email=benchmark@example.invalid "
          "-c commit.gpgsign=false -c init.defaultBranch=main")


def compact(run_id):
    """Lowercase alphanumeric form of a run id, for names that reject punctuation."""
    value = re.sub(r"[^a-z0-9]", "", run_id.lower())
    if not value:
        raise ValueError(f"run id {run_id!r} has no usable characters")
    return value


def git(repo, *args):
    return f"git {GIT_ID} -C {q(repo)} " + " ".join(args)


def init_repo(repo, branch, message="Prepare shared application fixture"):
    """Shell lines committing everything already in repo on a fresh branch."""
    return [git(repo, "init", "-q", "-b", q(branch)),
            git(repo, "add", "-A"),
            git(repo, "commit", "-q", "-m", q(message))]


def host_env(workdir, path_dirs=()):
    """Environment for host-transport tools: private HOME and git config, the user's Docker
    client config (read-only use; Docker Desktop's context lives there) and toolchains."""
    real_home = Path(os.environ.get("HOME", str(Path.home())))
    env = {k: v for k, v in os.environ.items() if k not in SCRUBBED}
    home = Path(workdir) / "home"
    home.mkdir(parents=True, exist_ok=True)
    env.update(
        HOME=str(home),
        XDG_CONFIG_HOME=str(home / ".config"), XDG_CACHE_HOME=str(home / ".cache"),
        XDG_DATA_HOME=str(home / ".local/share"), XDG_STATE_HOME=str(home / ".local/state"),
        GIT_CONFIG_GLOBAL=str(home / ".gitconfig"), GIT_CONFIG_NOSYSTEM="1",
        DOCKER_CONFIG=os.environ.get("DOCKER_CONFIG", str(real_home / ".docker")),
        NO_COLOR="1", COMPOSE_ANSI="never", COMPOSE_PROGRESS="plain", BUILDKIT_PROGRESS="plain",
    )
    # rustup's proxies locate installed toolchains through RUSTUP_HOME (read-only use here).
    env.setdefault("RUSTUP_HOME", str(real_home / ".rustup"))
    env["PATH"] = os.pathsep.join([*map(str, path_dirs), env.get("PATH", "")])
    return env


def require_owned(project, run_id):
    if compact(run_id) not in re.sub(r"[^a-z0-9]", "", project):
        raise ValueError(f"refusing to manage Compose project {project!r}: not named for run {run_id}")
    return project


def compose_resources(project):
    """Body printing every container/network/volume labelled with this Compose project.
    A failing Docker query fails the body; it is never read as 'no leftovers'."""
    label = q(f"label=com.docker.compose.project={project}")
    return "\n".join([
        "set -euo pipefail",
        f"c=$(docker ps -aq --filter {label})",
        f"n=$(docker network ls -q --filter {label})",
        f"v=$(docker volume ls -q --filter {label})",
        f'for x in $c; do echo "container {project} $x"; done',
        f'for x in $n; do echo "network {project} $x"; done',
        f'for x in $v; do echo "volume {project} $x"; done',
    ])


def compose_remove(project, images=()):
    """Body removing this Compose project's labelled resources (and named images built for
    it). Only exact-label matches are touched."""
    label = q(f"label=com.docker.compose.project={project}")
    lines = [
        "set -euo pipefail",
        f"c=$(docker ps -aq --filter {label})",
        'if [ -n "$c" ]; then docker rm -f -v $c >/dev/null; fi',
        f"n=$(docker network ls -q --filter {label})",
        'if [ -n "$n" ]; then docker network rm $n >/dev/null; fi',
        f"v=$(docker volume ls -q --filter {label})",
        'if [ -n "$v" ]; then docker volume rm $v >/dev/null; fi',
    ]
    for image in images:
        lines.append(f"if docker image inspect {q(image)} >/dev/null 2>&1; then docker image rm {q(image)} >/dev/null; fi")
    return "\n".join(lines)


def images_left(images):
    return "\n".join(f"if docker image inspect {q(i)} >/dev/null 2>&1; then echo 'image {i}'; fi"
                     for i in images) or "true"


def project_stopped(project):
    """Body exiting 0 once no container of the project is running (state checked by Docker)."""
    label = q(f"label=com.docker.compose.project={project}")
    return ("set -euo pipefail\n"
            f"for i in $(seq 1 150); do r=$(docker ps -q --filter {label} --filter status=running); "
            f"[ -z \"$r\" ] && exit 0; sleep 0.2; done\n"
            f"echo 'project {project} still has running containers' >&2; exit 1")


# Instance receipt for Compose-based lanes. argv: project, expected host checkout path,
# container mount target. Prints one JSON object and fails if the app container does not
# bind-mount the expected checkout (the code the app runs must be this checkout's).
COMPOSE_RECEIPT_PY = r'''
import json, os, subprocess, sys
project, checkout, target = sys.argv[1:4]
digest = sys.argv[4] if len(sys.argv) > 4 else None
def docker(*a):
    return subprocess.run(["docker", *a], check=True, capture_output=True, text=True).stdout
ids = docker("ps", "-aq", "--filter", f"label=com.docker.compose.project={project}").split()
if not ids:
    sys.exit(f"no containers for project {project}")
info = json.loads(docker("inspect", *ids))
services, volumes, networks, mount = {}, set(), set(), None
for c in info:
    labels = c["Config"]["Labels"] or {}
    if labels.get("com.docker.compose.project") != project:
        sys.exit(f"container {c['Id']} is not in project {project}")
    name = labels.get("com.docker.compose.service")
    ports = {k: [f"{b['HostIp']}:{b['HostPort']}" for b in (v or [])]
             for k, v in (c["NetworkSettings"]["Ports"] or {}).items()}
    services[name] = dict(id=c["Id"], image=c["Image"], running=c["State"]["Running"], published=ports)
    for m in c["Mounts"]:
        if m["Type"] == "volume":
            volumes.add(m["Name"])
        if m.get("Destination") == target and m["Type"] == "bind":
            mount = m["Source"]
    networks.update((c["NetworkSettings"]["Networks"] or {}).keys())
real = lambda p: os.path.realpath(p) if p else p
if mount is None or not (real(mount) == real(checkout) or real(checkout).startswith(real(mount) + "/")):
    sys.exit(f"{target} is bound from {mount!r}, not from checkout {checkout}")
print(json.dumps(dict(project=project, services=services, volumes=sorted(volumes),
                      networks=sorted(networks), mount=mount, code_digest=digest), sort_keys=True))
'''


def compose_receipt(project, checkout, target, digest_var=None):
    extra = f' "${digest_var}"' if digest_var else ""
    return f"python3 -I -c {q(COMPOSE_RECEIPT_PY)} {q(project)} {q(checkout)} {q(target)}{extra}"


# Digest of the application code (package, migrations, lock, source token) under a root.
# Run on the host checkout and inside the container; the two must be equal.
CODE_DIGEST_PY = r'''
import hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
files = sorted(p for d in ("rwbapp", "migrations") for p in (root / d).rglob("*")
               if p.is_file() and "__pycache__" not in p.parts)
files += [root / "pyproject.toml", root / "uv.lock"]
h = hashlib.sha256()
for p in files:
    h.update(str(p.relative_to(root)).encode() + b"\0" + hashlib.sha256(p.read_bytes()).digest())
print(h.hexdigest())
'''


def code_digest(python, root):
    return f"{python} -I -c {q(CODE_DIGEST_PY)} {q(root)}"


class ContainerApp:
    """Mixin for lanes whose app runs inside a Compose app container. App commands run from
    the container's view of the checkout (`container_root(co)`), never the host path."""
    container_python = "/usr/local/bin/python3.13"

    def container_root(self, co):
        raise NotImplementedError

    def app_source_path(self, co):
        return self.container_root(co)

    def app(self, co, args):
        return self.enter(co, f"cd {q(self.container_root(co))} && RWB_CHECKOUT={co.name} {self.app_python} -m rwbapp {args}")

    def deps(self, co):
        return self.enter(co, f"cd {q(self.container_root(co))} && uv sync --frozen --python {self.container_python}")

    def pytest(self, co):
        return self.enter(co, f"cd {q(self.container_root(co))} && RWB_CHECKOUT={co.name} {self.app_python} -m pytest -q -p no:cacheprovider")

    def code_check(self, co, host_path):
        """Host checkout digest == digest of the tree the container imports from."""
        inner = self.enter(co, code_digest(self.container_python, self.container_root(co)))
        return (f'h=$({code_digest("python3", host_path)}) && c=$({inner}) && '
                f'if [ "$h" != "$c" ]; then echo "code digest host $h != container $c" >&2; exit 1; fi')

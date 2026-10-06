"""Docker receipts for the container adapters (Dev Containers, DevPod, DDEV, Lando).

Benchmark-owned glue (declared `scripted` where it decides an outcome). It only reads Docker
state, except `remove`, which deletes resources of the named Compose projects after checking
that every project name contains this run's ownership token. Run with `python3 -I`.

Selectors: a Compose project name, or `dir:<path>` for the project whose
`com.docker.compose.project.working_dir` label is that directory (DevPod names projects after
an internal workspace UID; the working directory is the checkout-owned key).

Subcommands (all print JSON or one line per resource; failures exit nonzero):
  identity SEL                 containers, image IDs, volumes, networks, port bindings
  project SEL                  print the resolved project name
  health SEL SERVICE...        every service running and healthy (no health check = running)
  stopped SEL [SECONDS]        poll until no container of the project is running
  running TOKEN SEL...         one line per running container (empty = nothing running)
  resources TOKEN SEL...       one line per owned container/volume/network/image
  remove TOKEN SEL...          remove those resources, then fail if any remain
  shared-snapshot FILE KIND:NAME...   record whether shared infra existed before the run
  shared-cleanup FILE          remove shared infra this run created and nothing still uses
"""
import json
import os
import subprocess
import sys
import time

PROJECT = "com.docker.compose.project"
SERVICE = "com.docker.compose.service"
WORKDIR = "com.docker.compose.project.working_dir"
DOCKER = os.environ.get("RWB_DOCKER", "docker")


class Fail(Exception):
    pass


def docker(*args, ok_codes=(0,)):
    proc = subprocess.run([DOCKER, *args], capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if proc.returncode not in ok_codes:
        raise Fail(f"docker {' '.join(args)} exited {proc.returncode}: {proc.stderr.strip()}")
    return proc


def lines(*args):
    return [line for line in docker(*args).stdout.splitlines() if line.strip()]


def inspect(kind, ids):
    if not ids:
        return []
    return json.loads(docker(kind, "inspect", *ids).stdout)


def _paths(path):
    return {os.path.normpath(path), os.path.realpath(path)}


def resolve(selector):
    """Project names for a selector. A plain name resolves to itself."""
    if not selector.startswith("dir:"):
        return [selector]
    wanted = _paths(selector[4:])
    found = set()
    for line in lines("ps", "-a", "--filter", f"label={WORKDIR}",
                      "--format", f'{{{{.Label "{PROJECT}"}}}}\t{{{{.Label "{WORKDIR}"}}}}'):
        project, _, workdir = line.partition("\t")
        if _paths(workdir) & wanted:
            found.add(project)
    return sorted(found)


def containers(project):
    ids = lines("ps", "-a", "-q", "--no-trunc", "--filter", f"label={PROJECT}={project}")
    return inspect("container", ids)


def summary(c):
    labels = c["Config"].get("Labels") or {}
    state = c["State"]
    return dict(
        id=c["Id"], name=c["Name"].lstrip("/"), service=labels.get(SERVICE),
        image=c["Config"].get("Image"), image_id=c.get("Image"),
        status=state.get("Status"), health=(state.get("Health") or {}).get("Status"),
        started=state.get("StartedAt"),
        volumes=sorted(m["Name"] for m in c.get("Mounts", []) if m.get("Type") == "volume"),
        binds=sorted(f'{m["Source"]}:{m["Destination"]}' for m in c.get("Mounts", []) if m.get("Type") == "bind"),
        networks=sorted((c.get("NetworkSettings") or {}).get("Networks") or {}),
        ports={k: v for k, v in ((c.get("NetworkSettings") or {}).get("Ports") or {}).items() if v},
    )


def cmd_identity(selector):
    projects = resolve(selector)
    if len(projects) != 1:
        raise Fail(f"{selector}: expected one Compose project, found {projects}")
    found = [summary(c) for c in containers(projects[0])]
    if not found:
        raise Fail(f"project {projects[0]} has no containers")
    services = {}
    for item in sorted(found, key=lambda x: (x["service"] or "", x["id"])):
        services.setdefault(item["service"] or item["name"], item)
    receipt = dict(project=projects[0], services=services,
                   volumes=sorted({v for x in found for v in x["volumes"]}),
                   networks=sorted({n for x in found for n in x["networks"]}),
                   containers=sorted(x["id"] for x in found))
    print(json.dumps(receipt, sort_keys=True))


def cmd_project(selector):
    projects = resolve(selector)
    if len(projects) != 1:
        raise Fail(f"{selector}: expected one Compose project, found {projects}")
    print(projects[0])


def cmd_health(selector, *services):
    projects = resolve(selector)
    if len(projects) != 1:
        raise Fail(f"{selector}: expected one Compose project, found {projects}")
    by_service = {}
    for c in containers(projects[0]):
        s = summary(c)
        by_service[s["service"]] = s
    report, bad = {}, []
    for name in services:
        s = by_service.get(name)
        report[name] = None if s is None else dict(status=s["status"], health=s["health"])
        if s is None or s["status"] != "running" or s["health"] not in (None, "healthy"):
            bad.append(name)
    print(json.dumps(dict(project=projects[0], services=report, unhealthy=bad), sort_keys=True))
    if bad:
        raise Fail(f"not running/healthy: {bad}")


def cmd_stopped(selector, seconds="30"):
    deadline = time.monotonic() + float(seconds)
    while True:
        projects = resolve(selector)
        states = {}
        for project in projects:
            for c in containers(project):
                s = summary(c)
                states[s["name"]] = s["status"]
        running = sorted(n for n, st in states.items() if st in ("running", "restarting", "paused"))
        if not running:
            print(json.dumps(dict(projects=projects, containers=states), sort_keys=True))
            return
        if time.monotonic() >= deadline:
            print(json.dumps(dict(projects=projects, containers=states), sort_keys=True))
            raise Fail(f"still running after {seconds}s: {running}")
        time.sleep(0.5)


def _owned(token, selectors):
    if len(token) < 6:
        raise Fail(f"ownership token {token!r} is too short")
    projects = []
    for selector in selectors:
        for project in resolve(selector):
            if token not in project:
                raise Fail(f"refusing project {project!r}: it does not contain run token {token!r}")
            projects.append(project)
        if not selector.startswith("dir:") and token not in selector:
            raise Fail(f"refusing selector {selector!r}: it does not contain run token {token!r}")
    return sorted(set(projects))


def owned_resources(token, selectors):
    """(kind, identifier, description) for every resource of the owned projects."""
    projects = _owned(token, selectors)
    found = []
    for project in projects:
        for c in containers(project):
            s = summary(c)
            found.append(("container", s["id"], f'{project} {s["service"]} {s["name"]} {s["status"]}'))
        for name in lines("volume", "ls", "-q", "--filter", f"label={PROJECT}={project}"):
            found.append(("volume", name, project))
        for line in lines("network", "ls", "--filter", f"label={PROJECT}={project}",
                          "--format", "{{.ID}}\t{{.Name}}"):
            nid, _, name = line.partition("\t")
            found.append(("network", nid, f"{project} {name}"))
    # Tool-created volumes/images that carry the project name but no Compose label (DDEV's
    # <name>-postgres volume, <image>-<name>-built images). Names contain the run token.
    names = [s for s in selectors if not s.startswith("dir:")] + projects
    seen = {f[1] for f in found}
    for name in lines("volume", "ls", "--format", "{{.Name}}"):
        if name not in seen and token in name and any(n in name for n in names):
            found.append(("volume", name, "named for project"))
            seen.add(name)
    for ref in lines("image", "ls", "--format", "{{.Repository}}:{{.Tag}}"):
        if token in ref and any(n.lower() in ref for n in names):
            found.append(("image", ref, "named for project"))
    return found


def cmd_running(token, *selectors):
    for project in _owned(token, selectors):
        for c in containers(project):
            s = summary(c)
            if s["status"] in ("running", "restarting"):
                print(f'{project} {s["service"]} {s["id"][:12]} {s["name"]}')


def cmd_resources(token, *selectors):
    for kind, ident, desc in owned_resources(token, selectors):
        print(f"{kind} {ident} {desc}")


def cmd_remove(token, *selectors):
    resources = owned_resources(token, selectors)
    order = ("container", "network", "volume", "image")
    for kind in order:
        for k, ident, _ in resources:
            if k != kind:
                continue
            if kind == "container":
                docker("rm", "-f", "-v", ident, ok_codes=(0, 1))
            elif kind == "network":
                docker("network", "rm", ident, ok_codes=(0, 1))
            elif kind == "volume":
                docker("volume", "rm", ident, ok_codes=(0, 1))
            else:
                docker("image", "rm", ident, ok_codes=(0, 1))
    left = owned_resources(token, selectors)
    for kind, ident, desc in left:
        print(f"remaining {kind} {ident} {desc}")
    if left:
        raise Fail(f"{len(left)} owned resources remain")


SHARED_KINDS = ("network", "volume")


def _shared_items(items):
    parsed = []
    for item in items:
        kind, _, name = item.partition(":")
        if kind not in SHARED_KINDS or not name:
            raise Fail(f"bad shared item {item!r}: expected network:NAME or volume:NAME")
        parsed.append((item, kind, name))
    return parsed


def _shared_count(kind, name):
    """Exact-name matches from a successful listing. A failed listing (daemon down, ...) raises:
    an inspect error is never read as "absent", so no ownership is inferred from a failure."""
    listed = lines(kind, "ls", "--filter", f"name={name}", "--format", "{{.Name}}")
    return sum(1 for line in listed if line.strip() == name)


def cmd_shared_snapshot(path, *items):
    """Every listing must succeed before anything is written; a failed query leaves an
    existing record untouched instead of recording pre-existing infra as run-created."""
    record = {}
    for item, kind, name in _shared_items(items):
        record[item] = dict(existed_before=_shared_count(kind, name) > 0)
    tmp = f"{path}.tmp"
    with open(tmp, "w") as handle:
        json.dump(record, handle, sort_keys=True)
    os.replace(tmp, path)
    print(json.dumps(record, sort_keys=True))


def cmd_shared_cleanup(path):
    """Shared infra (ddev_default, lando_bridge_network, ...) is removed only when this run
    created it and nothing is attached; anything that pre-existed is never touched. All
    discovery and use checks finish before the first removal, and any failure raises."""
    if not os.path.exists(path):
        print("{}")
        return
    with open(path) as handle:
        record = json.load(handle)
    if not isinstance(record, dict):
        raise Fail(f"{path}: expected a JSON object")
    parsed = _shared_items(sorted(record))
    for item, _, _ in parsed:
        if not isinstance(record[item], dict) or not isinstance(record[item].get("existed_before"), bool):
            raise Fail(f"{path}: {item} has no boolean existed_before")
    result, candidates = {}, []
    for item, kind, name in parsed:
        if record[item]["existed_before"]:
            result[item] = "kept (pre-existing)"
            continue
        count = _shared_count(kind, name)
        if count > 1:
            raise Fail(f"{item}: {count} {kind}s named {name!r}; refusing ambiguous cleanup")
        if count == 0:
            result[item] = "absent"
            continue
        candidates.append((item, kind, name))
    removals = []
    for item, kind, name in candidates:
        if kind == "network":
            attached = json.loads(docker("network", "inspect", name).stdout)[0].get("Containers") or {}
            if attached:
                result[item] = f"kept: {len(attached)} containers attached"
                continue
        else:
            users = lines("ps", "-aq", "--filter", f"volume={name}")
            if users:
                result[item] = f"kept: used by {len(users)} containers"
                continue
        removals.append((item, kind, name))
    for item, kind, name in removals:
        docker(kind, "rm", name)
        result[item] = "removed (created by this run)"
    print(json.dumps(result, sort_keys=True))


COMMANDS = {
    "identity": cmd_identity, "project": cmd_project, "health": cmd_health, "stopped": cmd_stopped,
    "running": cmd_running, "resources": cmd_resources, "remove": cmd_remove,
    "shared-snapshot": cmd_shared_snapshot, "shared-cleanup": cmd_shared_cleanup,
}


def main(argv):
    if not argv or argv[0] not in COMMANDS:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        COMMANDS[argv[0]](*argv[1:])
    except Fail as error:
        print(f"compose_receipt: {error}", file=sys.stderr)
        return 1
    except TypeError as error:
        print(f"compose_receipt: bad arguments: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

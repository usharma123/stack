"""Pure checks over the fixture app's JSON receipts. No I/O, so tests cover them directly."""
import posixpath
from urllib.parse import urlsplit


def url_port(url):
    try:
        return urlsplit(url).port
    except ValueError:
        return None


INFRA_EXITS = {124: "timed out", 126: "not executable", 127: "command not found"}

# Exact result shapes required from successful app commands (fail closed on anything else).
EXPECTED = {
    "migrate": lambda r: isinstance(r.get("applied"), list) and r.get("current") == ["0001", "0002"],
    "mark": lambda r: bool(r.get("checkout")),
    "crud": lambda r: r.get("steps") == ["create", "read", "update", "list", "delete"],
    "cache": lambda r: r.get("sequence") == ["miss", "hit", "miss", "hit"],
    "read": lambda r: isinstance(r.get("item"), dict) and str(r["item"].get("sku", "")).startswith("keeper-"),
    "persist": lambda r: r.get("saved") is True,
    "persisted": lambda r: isinstance(r.get("pg_keeper"), bool) and isinstance(r.get("redis_durable"), bool),
    "identity": lambda r: identity_complete(r),
    "wait": lambda r: identity_complete(r),
    "check": lambda r: identity_complete(r) and r.get("problems") == [],
}


def identity_complete(r):
    """Identity receipts must carry every field the isolation/source/URL checks rely on."""
    try:
        return (isinstance(r["pg"]["port"], int) and bool(r["pg"]["data_directory"])
                and "started" in r["pg"] and bool(r["redis"]["run_id"]) and isinstance(r["redis"]["port"], int)
                and bool(r["urls"]["database"]) and bool(r["urls"]["redis"])
                and bool(r["source"]["module"]) and bool(r["source"]["token"])
                and isinstance(r["pg_markers"], list) and isinstance(r.get("declared"), dict))
    except (KeyError, TypeError):
        return False


def refusal(code, timed_out):
    """Classify a nonzero exit: a real refusal by the tool, or an infrastructure fault
    (timeout, missing/unexecutable command) that must never count as a detected failure."""
    if timed_out or code in INFRA_EXITS:
        return "infra"
    return "refused" if code != 0 else "accepted"


def app_result(payload, command, code=0, timed_out=False):
    """Return the result object of a successful app command, or raise ValueError.

    Fails closed: the process must have exited 0 without timing out AND printed an ok
    receipt for this command whose result has the expected shape."""
    if timed_out:
        raise ValueError(f"{command} timed out")
    if not isinstance(payload, dict):
        raise ValueError(f"no JSON receipt from app (exit {code})")
    if payload.get("command") != command:
        raise ValueError(f"receipt is for {payload.get('command')!r}, expected {command!r}")
    if payload.get("ok") is not True:
        error = payload.get("error") or {}
        raise ValueError(f"{command} failed: {error.get('code')}: {error.get('message')}")
    if code != 0:
        raise ValueError(f"{command} printed ok but exited {code}")
    result = payload.get("result")
    if not isinstance(result, dict):
        raise ValueError(f"{command} receipt has no result object")
    if command in EXPECTED and not EXPECTED[command](result):
        raise ValueError(f"{command} result has unexpected content: {result!r}"[:300])
    return result


def identity_problems(identity, token=None, path=None):
    """The app reached servers on the ports its URLs name, tool-declared paths match, and
    the running code is this checkout's (source token, and module path when known)."""
    problems = []
    source = identity.get("source") or {}
    if token is not None and source.get("token") != token:
        problems.append(f"running code has source token {source.get('token')!r}, expected this checkout's {token!r}")
    if path is not None and source.get("module") and not _norm(source["module"]).startswith(_norm(path) + "/"):
        problems.append(f"running module {source.get('module')} is outside checkout {path}")
    urls = identity.get("urls", {})
    pg, redis = identity.get("pg", {}), identity.get("redis", {})
    if url_port(urls.get("database", "")) not in (None, pg.get("port")):
        problems.append(f"postgres answered on {pg.get('port')}, URL names {url_port(urls['database'])}")
    if url_port(urls.get("redis", "")) not in (None, redis.get("port")):
        problems.append(f"redis answered on {redis.get('port')}, URL names {url_port(urls['redis'])}")
    declared = identity.get("declared", {})
    if declared.get("PGDATA") and _norm(declared["PGDATA"]) != _norm(pg.get("data_directory")):
        problems.append(f"PGDATA {declared['PGDATA']} != server data_directory {pg.get('data_directory')}")
    for key in ("REDISDATA", "REDIS_DATA"):
        if declared.get(key) and _norm(declared[key]) != _norm(redis.get("dir")):
            problems.append(f"{key} {declared[key]} != redis dir {redis.get('dir')}")
    return problems


def _norm(path):
    return posixpath.normpath(path) if isinstance(path, str) and path else path


def _known(value):
    return isinstance(value, str) and value and not value.startswith("unavailable")


def pg_instance(identity):
    pg = identity["pg"]
    if _known(pg.get("system_identifier")):
        return ("sysid", pg["system_identifier"], pg.get("data_directory"))
    return ("addr", pg.get("data_directory"), pg.get("port"), pg.get("started"))


def same_pg_cluster(before, after):
    """Same cluster across a restart: same system identifier (or data directory)."""
    a, b = before["pg"], after["pg"]
    if _known(a.get("system_identifier")) and _known(b.get("system_identifier")):
        return a["system_identifier"] == b["system_identifier"] and a.get("data_directory") == b.get("data_directory")
    return a.get("data_directory") == b.get("data_directory")


# Isolation boundaries an adapter may declare. Each defines what must differ between two
# checkouts; in every case the app's conflicting, unprefixed markers (table `checkouts`,
# Redis key `rwb:checkout`) must not leak between checkouts (checked by `rwbapp check`).
BOUNDARIES = {
    "service-instance": "separate PostgreSQL clusters and Redis processes on one host",
    "container": "separate service containers/volumes (paths and ports may repeat inside them)",
    "database": "separate PostgreSQL databases and Redis logical DBs on shared servers",
}


def isolation_key(identity, boundary):
    """Typed receipt of what identifies one checkout's storage under a boundary."""
    pg, redis = identity["pg"], identity["redis"]
    if boundary == "database":
        return dict(pg=[*pg_instance(identity), pg.get("database")], redis=[redis.get("run_id"), redis.get("db")])
    if boundary in ("service-instance", "container"):
        return dict(pg=list(pg_instance(identity)), redis=[redis.get("run_id")])
    raise ValueError(f"unknown isolation boundary {boundary}")


def distinct_instances(first, second, boundary="service-instance", extra=None):
    """Two checkouts must reach different storage under their declared boundary.

    extra: optional (first, second) adapter instance receipts (container/volume IDs) that
    must also differ, used by the container boundary.
    """
    a, b = isolation_key(first, boundary), isolation_key(second, boundary)
    problems = []
    if a["pg"] == b["pg"]:
        problems.append(f"both checkouts reached the same PostgreSQL storage ({boundary})")
    if a["redis"] == b["redis"]:
        problems.append(f"both checkouts reached the same Redis storage ({boundary})")
    if extra is not None:
        if not all(isinstance(x, dict) and x for x in extra):
            problems.append("adapter instance receipt (containers/volumes) missing or invalid for a checkout")
        elif extra[0] == extra[1]:
            problems.append("adapter instance receipts (containers/volumes) are identical")
    return problems


def restart_changes(before, after, boundary="service-instance"):
    """After stop/start: same PG cluster, new postmaster; new Redis process.

    Under the database boundary the servers are shared, so a checkout stop need not restart
    them; only data reuse is required.
    """
    problems = []
    if boundary == "database":
        if not same_pg_cluster(before, after) or before["pg"].get("database") != after["pg"].get("database"):
            problems.append("PostgreSQL database changed across restart (data not reused)")
        return problems
    if not same_pg_cluster(before, after):
        problems.append("PostgreSQL cluster changed across restart (data not reused)")
    if before["pg"].get("started") == after["pg"].get("started"):
        problems.append("PostgreSQL postmaster start time unchanged: service did not restart")
    if before["redis"].get("run_id") == after["redis"].get("run_id"):
        problems.append("Redis run_id unchanged: service did not restart")
    return problems


def unchanged_instance(before, after):
    """A repeated start must not replace or duplicate running services."""
    problems = []
    if before["pg"].get("started") != after["pg"].get("started") or not same_pg_cluster(before, after):
        problems.append("PostgreSQL instance changed after repeated start")
    if before["redis"].get("run_id") != after["redis"].get("run_id"):
        problems.append("Redis instance changed after repeated start")
    return problems


def classify_conflict(start, ready, identity, squatted_ports):
    """Occupied-port startup. Good: refused, detected, or relocated to a working own instance.

    start/ready: (exit code, timed_out) or None for ready when no readiness step ran.
    Timeouts and missing commands are infrastructure faults: never a good outcome."""
    kind = refusal(*start)
    if kind == "infra":
        return f"start-infra-fault (exit {start[0]})", False
    if kind == "refused":
        return "refused-at-start", True
    if ready is not None:
        kind = refusal(*ready)
        if kind == "infra":
            return f"readiness-infra-fault (exit {ready[0]})", False
        if kind == "refused":
            return "detected-at-readiness", True
    if identity is None:
        return "reported-ready-but-unreachable", False
    if identity["pg"].get("port") in squatted_ports or identity["redis"].get("port") in squatted_ports:
        return "reached-squatter", False
    return "relocated", True

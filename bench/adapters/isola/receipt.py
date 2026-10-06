"""Benchmark-owned receipts for the isola lane. Run from a worktree with the toolchain on PATH.

  receipt.py identity BRANCH PROJECT   accessory resources == generated env file == owner marker
  receipt.py stopped BRANCH            keeper stopped, accessories still provisioned
  receipt.py destroy BRANCH PG_PORT REDIS_PORT   run `isola destroy`; its resources must be gone

Each mode prints one JSON object and exits nonzero on any mismatch. Reads only isola's
own JSON output, the env file isola generated and the run-owned shared servers.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
from urllib.parse import urlsplit


def run(*argv):
    return subprocess.run(argv, check=True, capture_output=True, text=True).stdout


def fail(message):
    print(json.dumps(dict(ok=False, error=message)))
    sys.exit(1)


def env_file(path=".env.isola"):
    values = {}
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def accessories(branch):
    rows = [r for r in json.loads(run("isola", "accessory", "ls", "--json")) if r["worktree"] == branch]
    return {r["accessory"]: r for r in rows}


def keeper(branch):
    rows = [r for r in json.loads(run("isola", "ls", "--json")) if r["worktree"] == branch]
    if len(rows) != 1:
        fail(f"expected one isola service for {branch}, got {rows}")
    return rows[0]


def alive(pid):
    if not pid:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    stat = Path(f"/proc/{pid}/stat")
    return not (stat.exists() and stat.read_text().rsplit(")", 1)[1].split()[0] == "Z")


def identity(branch, project):
    acc, env = accessories(branch), env_file()
    db, cache = acc.get("database"), acc.get("cache")
    if not (db and db["provisioned"] and cache and cache["provisioned"]):
        fail(f"accessories not provisioned for {branch}: {acc}")
    url_db = urlsplit(env["DATABASE_URL"]).path.lstrip("/")
    redis = urlsplit(env["REDIS_URL"])
    url_index = int(redis.path.lstrip("/") or 0)
    if url_db != db["resource"]["database"]:
        fail(f"env DATABASE_URL names {url_db}, isola provisioned {db['resource']['database']}")
    if url_index != cache["resource"]["db"]:
        fail(f"env REDIS_URL names db {url_index}, isola provisioned {cache['resource']['db']}")
    marker = run("redis-cli", "-h", redis.hostname, "-p", str(redis.port), "-n", str(url_index),
                 "--raw", "GET", "__isola_owner__").strip()
    expected = cache["resource"].get("owner") or f"{project}:{branch}"
    if marker != expected or not marker.startswith(project + ":"):
        fail(f"redis db {url_index} owner marker {marker!r}, expected {expected!r}")
    service = keeper(branch)
    print(json.dumps(dict(ok=True, branch=branch, database=url_db, redis_db=url_index, owner=marker,
                          keeper=dict(status=service["status"], pid=service["pid"])), sort_keys=True))


def stopped(branch):
    service = keeper(branch)
    if service["status"] == "running" or alive(service.get("pid")):
        fail(f"keeper for {branch} still running: {service}")
    acc = accessories(branch)
    if not all(acc.get(n, {}).get("provisioned") for n in ("database", "cache")):
        fail(f"isola down dropped accessories for {branch}: {acc}")
    print(json.dumps(dict(ok=True, branch=branch, keeper=service, accessories_retained=True), sort_keys=True))


def destroy(branch, pg_port, redis_port):
    """Native `isola destroy`, then prove the recorded database and logical DB are gone."""
    acc = accessories(branch)
    database = (acc.get("database") or {}).get("resource") or {}
    cache = (acc.get("cache") or {}).get("resource") or {}
    run("isola", "destroy")
    left = {}
    if database.get("database"):
        count = run("psql", "-h", "127.0.0.1", "-p", pg_port, "-U", "bench", "-d", "postgres", "-Atc",
                    "select count(*) from pg_database where datname = '%s'" % database["database"].replace("'", "''")).strip()
        if count != "0":
            left["database"] = database["database"]
    if "db" in cache:
        keys = run("redis-cli", "-h", "127.0.0.1", "-p", redis_port, "-n", str(cache["db"]), "DBSIZE").strip()
        if keys != "0":
            left["redis_db"] = cache["db"]
    if left:
        fail(f"isola destroy left {left} for {branch}")
    print(json.dumps(dict(ok=True, branch=branch, dropped=dict(database=database.get("database"),
                                                              redis_db=cache.get("db"))), sort_keys=True))


if __name__ == "__main__":
    mode, *args = sys.argv[1:]
    dict(identity=identity, stopped=stopped, destroy=destroy)[mode](*args)

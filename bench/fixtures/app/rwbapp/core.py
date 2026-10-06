"""Benchmark fixture app. Reads only DATABASE_URL and REDIS_URL; no fallback endpoints.

Every command prints exactly one JSON object on stdout. Exit codes:
0 ok, 1 a check failed, 2 configuration error, 3 a service was unreachable, 4 other error.
"""
import json
import os
from pathlib import Path
import re
import sys
import uuid

MIGRATIONS = Path(__file__).resolve().parent.parent / "migrations"
MIGRATION_NAME = re.compile(r"^(\d{4})_[a-z0-9_]+\.sql$")


class CheckFailed(Exception):
    pass


class ConfigError(Exception):
    pass


def migration_files(directory=MIGRATIONS):
    """Ordered (version, path). Rejects gaps, duplicates and stray names."""
    found = []
    for path in sorted(Path(directory).iterdir()):
        if path.suffix != ".sql":
            continue
        match = MIGRATION_NAME.match(path.name)
        if not match:
            raise ConfigError(f"bad migration file name: {path.name}")
        found.append((match.group(1), path))
    versions = [version for version, _ in found]
    if versions != [f"{i:04d}" for i in range(1, len(versions) + 1)]:
        raise ConfigError(f"migrations must be numbered 0001.. without gaps: {versions}")
    return found


def cache_key(checkout, sku):
    return f"rwb:{checkout}:item:{sku}"


def stats_key(checkout):
    return f"rwb:{checkout}:stats"


def env_url(name):
    value = os.environ.get(name, "")
    if not value:
        raise ConfigError(f"{name} is not set")
    return value


def connect_pg():
    import psycopg
    try:
        # Some tools initialize clusters as SQL_ASCII; request UTF8 so text decodes uniformly.
        return psycopg.connect(env_url("DATABASE_URL"), connect_timeout=5, autocommit=True,
                               client_encoding="UTF8")
    except psycopg.OperationalError as error:
        raise ConnectionError(f"postgres: {error}") from error


def connect_redis():
    import redis
    client = redis.Redis.from_url(env_url("REDIS_URL"), socket_timeout=5, socket_connect_timeout=5,
                                  decode_responses=True)
    try:
        client.ping()
    except redis.exceptions.ConnectionError as error:
        raise ConnectionError(f"redis: {error}") from error
    return client


def _optional(conn, sql):
    """Privileged lookups (pg_control_system) may be denied to non-superuser roles."""
    import psycopg
    try:
        with conn.transaction():
            return conn.execute(sql).fetchone()[0]
    except psycopg.Error as error:
        return f"unavailable: {type(error).__name__}"


def pg_identity(conn):
    row = conn.execute(
        "select current_setting('data_directory'), inet_server_port(), current_setting('server_version'),"
        " current_setting('cluster_name'), current_user, current_database(), pg_postmaster_start_time()::text,"
        " current_setting('server_encoding')"
    ).fetchone()
    return dict(data_directory=row[0], port=row[1], version=row[2], cluster_name=row[3], user=row[4],
                database=row[5], started=row[6], server_encoding=row[7],
                system_identifier=_optional(conn, "select system_identifier::text from pg_control_system()"))


def redis_identity(client):
    info = client.info("server")
    config = {}
    for key in ("dir", "appendonly", "appendfsync", "save"):
        try:
            config.update(client.config_get(key))
        except Exception as error:  # CONFIG may be disabled; identity then rests on run_id/port.
            config[key] = f"unavailable: {type(error).__name__}"
    db = client.connection_pool.connection_kwargs.get("db", 0)
    return dict(run_id=info.get("run_id"), port=info.get("tcp_port"), version=info.get("redis_version"), db=db,
                pid=info.get("process_id"), executable=info.get("executable"), dir=config.get("dir"),
                appendonly=config.get("appendonly"), appendfsync=config.get("appendfsync"),
                save=config.get("save"))


def source_identity():
    """Which checkout's CODE is running: a per-checkout token file shipped in the source
    tree (written by the harness at checkout preparation, never passed via environment)."""
    package = Path(__file__).resolve().parent
    token = package / "SOURCE_TOKEN"
    return dict(module=str(package), token=token.read_text().strip() if token.exists() else None)


def declared_env():
    """Tool-exported locations, when the tool exports them; the harness compares them."""
    names = ("PGDATA", "PGPORT", "REDISDATA", "REDIS_DATA", "REDIS_PORT")
    return {name: os.environ[name] for name in names if os.environ.get(name)}


def applied_migrations(conn):
    exists = conn.execute("select to_regclass('public.schema_migrations') is not null").fetchone()[0]
    if not exists:
        return []
    return [r[0] for r in conn.execute("select version from schema_migrations order by version")]


def cmd_identity(args):
    conn, client = connect_pg(), connect_redis()
    with conn:
        applied = applied_migrations(conn)
        markers = []
        if "0001" in applied:
            markers = [r[0] for r in conn.execute("select name from checkouts order by name")]
        return dict(pg=pg_identity(conn), redis=redis_identity(client), migrations=applied,
                    pg_markers=markers, redis_marker=client.get("rwb:checkout"),
                    python=dict(version=sys.version.split()[0], executable=sys.executable),
                    urls=dict(database=os.environ["DATABASE_URL"], redis=os.environ["REDIS_URL"]),
                    declared=declared_env(), source=source_identity())


def cmd_migrate(args):
    conn = connect_pg()
    applied_now = []
    with conn:
        conn.execute("create table if not exists schema_migrations"
                     " (version text primary key, applied_at timestamptz not null default now())")
        done = set(applied_migrations(conn))
        for version, path in migration_files():
            if version in done:
                continue
            with conn.transaction():
                conn.execute(path.read_text())
                conn.execute("insert into schema_migrations (version) values (%s)", (version,))
            applied_now.append(version)
        return dict(applied=applied_now, current=applied_migrations(conn))


def cmd_mark(args):
    conn, client = connect_pg(), connect_redis()
    with conn:
        conn.execute("insert into checkouts (name) values (%s) on conflict do nothing", (args.checkout,))
        conn.execute(
            "insert into items (checkout, sku, title, price_cents, tags) values (%s, %s, %s, 100, %s)"
            " on conflict (sku) do nothing",
            (args.checkout, f"keeper-{args.checkout}", f"keeper for {args.checkout}", ["keeper"]))
    client.set("rwb:checkout", args.checkout)
    return dict(checkout=args.checkout)


def cmd_crud(args):
    conn = connect_pg()
    sku = f"{args.checkout}-{uuid.uuid4().hex[:12]}"
    steps = []
    with conn:
        item_id = conn.execute(
            "insert into items (checkout, sku, title, price_cents, tags) values (%s, %s, 'widget', 250, %s)"
            " returning id", (args.checkout, sku, ["new"])).fetchone()[0]
        steps.append("create")
        row = conn.execute("select title, price_cents, tags from items where id = %s", (item_id,)).fetchone()
        if row != ("widget", 250, ["new"]):
            raise CheckFailed(f"read after create returned {row!r}")
        steps.append("read")
        conn.execute("update items set price_cents = 300, tags = array_append(tags, 'sale'), updated_at = now()"
                     " where id = %s", (item_id,))
        row = conn.execute("select price_cents, tags from items where id = %s", (item_id,)).fetchone()
        if row != (300, ["new", "sale"]):
            raise CheckFailed(f"read after update returned {row!r}")
        steps.append("update")
        listed = conn.execute("select count(*) from items where checkout = %s and sku = %s",
                              (args.checkout, sku)).fetchone()[0]
        if listed != 1:
            raise CheckFailed(f"list returned {listed} rows")
        steps.append("list")
        deleted = conn.execute("delete from items where id = %s", (item_id,)).rowcount
        gone = conn.execute("select count(*) from items where id = %s", (item_id,)).fetchone()[0]
        if deleted != 1 or gone != 0:
            raise CheckFailed(f"delete removed {deleted}, {gone} remain")
        steps.append("delete")
    return dict(sku=sku, steps=steps)


def cached_item(conn, client, checkout, sku):
    """Read-through cache. Returns (source, item)."""
    key = cache_key(checkout, sku)
    raw = client.get(key)
    if raw is not None:
        client.hincrby(stats_key(checkout), "hit", 1)
        return "hit", json.loads(raw)
    row = conn.execute("select sku, title, price_cents from items where checkout = %s and sku = %s",
                       (checkout, sku)).fetchone()
    if row is None:
        raise CheckFailed(f"item {sku} not found in {checkout}")
    item = dict(sku=row[0], title=row[1], price_cents=row[2])
    client.set(key, json.dumps(item), ex=600)
    client.hincrby(stats_key(checkout), "miss", 1)
    return "miss", item


def cmd_cache(args):
    conn, client = connect_pg(), connect_redis()
    sku = f"keeper-{args.checkout}"
    with conn:
        client.delete(cache_key(args.checkout, sku))
        sequence = []
        source, first = cached_item(conn, client, args.checkout, sku)
        sequence.append(source)
        source, second = cached_item(conn, client, args.checkout, sku)
        sequence.append(source)
        if first != second:
            raise CheckFailed("cache hit differs from database read")
        new_price = first["price_cents"] + 1
        conn.execute("update items set price_cents = %s where sku = %s", (new_price, sku))
        client.delete(cache_key(args.checkout, sku))  # invalidate on write
        source, third = cached_item(conn, client, args.checkout, sku)
        sequence.append(source)
        source, fourth = cached_item(conn, client, args.checkout, sku)
        sequence.append(source)
        if third["price_cents"] != new_price or fourth != third:
            raise CheckFailed(f"stale cache after invalidation: {third!r} {fourth!r}")
    if sequence != ["miss", "hit", "miss", "hit"]:
        raise CheckFailed(f"unexpected cache sequence {sequence}")
    return dict(sequence=sequence, price_cents=new_price)


def cmd_read(args):
    conn, client = connect_pg(), connect_redis()
    with conn:
        source, item = cached_item(conn, client, args.checkout, f"keeper-{args.checkout}")
    return dict(source=source, item=item)


def cmd_wait(args):
    """Scripted readiness: retry only connection failures until the deadline, then identify."""
    import time
    deadline = time.monotonic() + args.timeout
    attempts, last = 0, None
    while True:
        attempts += 1
        try:
            result = cmd_identity(args)
            result["attempts"] = attempts
            return result
        except ConnectionError as error:
            last = error
        if time.monotonic() >= deadline:
            raise ConnectionError(f"not ready after {args.timeout}s and {attempts} attempts: {last}")
        time.sleep(0.2)


def cmd_persist(args):
    """Orderly persistence point before a stop: Redis SAVE (works with or without AOF)."""
    client = connect_redis()
    client.set(f"rwb:{args.checkout}:durable", args.checkout)
    return dict(saved=bool(client.save()))


def cmd_persisted(args):
    """After restart: keeper row and Redis durable key from before the stop."""
    conn, client = connect_pg(), connect_redis()
    with conn:
        keeper = conn.execute("select count(*) from items where sku = %s",
                              (f"keeper-{args.checkout}",)).fetchone()[0]
    durable = client.get(f"rwb:{args.checkout}:durable")
    result = dict(pg_keeper=keeper == 1, redis_durable=durable == args.checkout)
    if not result["pg_keeper"]:
        raise CheckFailed("postgres rows did not survive restart", result)
    return result


def cmd_check(args):
    """Isolation check: this checkout's data only, the other checkout's data absent."""
    result = cmd_identity(args)
    problems = []
    if result["pg_markers"] != [args.checkout]:
        problems.append(f"postgres markers {result['pg_markers']} != [{args.checkout}]")
    if result["redis_marker"] != args.checkout:
        problems.append(f"redis marker {result['redis_marker']!r} != {args.checkout!r}")
    for other in args.forbid:
        if other in result["pg_markers"] or result["redis_marker"] == other:
            problems.append(f"saw forbidden checkout {other}")
    conn = connect_pg()
    with conn:
        keeper = conn.execute("select count(*) from items where sku = %s",
                              (f"keeper-{args.checkout}",)).fetchone()[0]
    if keeper != 1:
        problems.append(f"keeper row count {keeper}")
    result["problems"] = problems
    if problems:
        raise CheckFailed("; ".join(problems), result)
    return result


COMMANDS = dict(identity=cmd_identity, migrate=cmd_migrate, mark=cmd_mark, crud=cmd_crud,
                cache=cmd_cache, read=cmd_read, check=cmd_check, wait=cmd_wait, persist=cmd_persist,
                persisted=cmd_persisted)


def parse(argv):
    import argparse
    parser = argparse.ArgumentParser(prog="rwbapp")
    parser.add_argument("command", choices=sorted(COMMANDS))
    parser.add_argument("--checkout", default="")
    parser.add_argument("--forbid", action="append", default=[])
    parser.add_argument("--timeout", type=float, default=60.0)
    args = parser.parse_args(argv)
    if args.command not in ("identity", "migrate", "wait") and not re.fullmatch(r"[a-z0-9]+", args.checkout):
        parser.error("--checkout NAME ([a-z0-9]+) is required")
    return args


def main(argv=None):
    args = parse(sys.argv[1:] if argv is None else argv)
    payload = dict(command=args.command, checkout=args.checkout or None)
    try:
        payload.update(ok=True, result=COMMANDS[args.command](args))
        code = 0
    except CheckFailed as error:
        payload.update(ok=False, error=dict(code="check_failed", message=str(error.args[0])))
        if len(error.args) > 1:
            payload["result"] = error.args[1]
        code = 1
    except ConfigError as error:
        payload.update(ok=False, error=dict(code="config", message=str(error)))
        code = 2
    except ConnectionError as error:
        payload.update(ok=False, error=dict(code="unreachable", message=str(error)))
        code = 3
    except Exception as error:  # any other client/server error still yields one JSON object
        payload.update(ok=False, error=dict(code="error", message=f"{type(error).__name__}: {error}"))
        code = 4
    print(json.dumps(payload, sort_keys=True, default=_jsonable))
    return code


def _jsonable(value):
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return str(value)

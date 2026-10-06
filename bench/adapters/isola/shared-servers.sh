#!/usr/bin/env bash
# Benchmark-owned shared PostgreSQL cluster and Redis server for the isola lane (scripted).
# isola's native accessories need existing servers: it clones one database per worktree on
# a cluster and claims one logical Redis DB per worktree on a server. These servers are
# private to the run's own container and are never a user's existing services.
#
#   shared-servers.sh ensure|stop|status
#
# Needs: RWB_SHARED (run-owned absolute dir), RWB_RUN (run id), RWB_PG_PORT, RWB_REDIS_PORT,
# and initdb/pg_ctl/psql/redis-server/redis-cli on PATH (the benchmark toolchain).
set -euo pipefail
: "${RWB_SHARED:?}" "${RWB_RUN:?}" "${RWB_PG_PORT:?}" "${RWB_REDIS_PORT:?}"
pg=$RWB_SHARED/pg
owner=$RWB_SHARED/OWNER

listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

check_owner() {
  [ "$(cat "$owner" 2>/dev/null)" = "rwb-isola $RWB_RUN" ] || {
    echo "refusing: $RWB_SHARED is not owned by run $RWB_RUN" >&2; exit 1; }
}

redis_pid() {
  local pid
  pid=$(cat "$RWB_SHARED/redis.pid" 2>/dev/null) || return 1
  kill -0 "$pid" 2>/dev/null && echo "$pid"
}

pg_running() { [ -s "$pg/PG_VERSION" ] && pg_ctl -D "$pg" status >/dev/null 2>&1; }

ensure() {
  if [ ! -e "$owner" ]; then
    # Fresh run-owned directory only; the ports must be free (no foreign server reused).
    [ ! -e "$pg" ] || { echo "refusing: $pg exists without an owner receipt" >&2; exit 1; }
    for port in "$RWB_PG_PORT" "$RWB_REDIS_PORT"; do
      if listening "$port"; then echo "port $port already in use; not reusing a foreign server" >&2; exit 1; fi
    done
    mkdir -p "$RWB_SHARED/redis" "$RWB_SHARED/logs"
    printf 'rwb-isola %s\n' "$RWB_RUN" > "$owner"
  fi
  check_owner
  if [ ! -s "$pg/PG_VERSION" ]; then
    initdb -D "$pg" -U bench -A trust --no-locale --encoding=UTF8 >"$RWB_SHARED/logs/initdb.log" 2>&1
  fi
  if ! pg_running; then
    pg_ctl -D "$pg" -l "$RWB_SHARED/logs/postgres.log" -w -t 60 \
      -o "-h 127.0.0.1 -p $RWB_PG_PORT -k '' -c cluster_name=rwb-isola-shared" start >/dev/null
  fi
  if [ "$(psql -h 127.0.0.1 -p "$RWB_PG_PORT" -U bench -d postgres -Atc \
        "select count(*) from pg_database where datname = 'rwb_template'")" = 0 ]; then
    # Empty template: every worktree still runs its own migrations (comparable workload).
    psql -h 127.0.0.1 -p "$RWB_PG_PORT" -U bench -d postgres -qc 'create database rwb_template' >/dev/null
  fi
  if ! redis_pid >/dev/null; then
    redis-server --bind 127.0.0.1 --port "$RWB_REDIS_PORT" --databases 16 \
      --dir "$RWB_SHARED/redis" --appendonly yes --appendfsync always --save '' \
      --daemonize yes --pidfile "$RWB_SHARED/redis.pid" --logfile "$RWB_SHARED/logs/redis.log"
  fi
  for _ in $(seq 1 150); do
    if pg_isready -q -h 127.0.0.1 -p "$RWB_PG_PORT" &&
       [ "$(redis-cli -h 127.0.0.1 -p "$RWB_REDIS_PORT" ping 2>/dev/null)" = PONG ]; then
      return 0
    fi
    sleep 0.2
  done
  echo "shared servers not ready after 30s" >&2; exit 1
}

stop() {
  [ -e "$owner" ] || return 0          # never started in this run
  check_owner
  if pg_running; then pg_ctl -D "$pg" -m fast -w -t 60 stop >/dev/null; fi
  local pid
  if pid=$(redis_pid); then
    redis-cli -h 127.0.0.1 -p "$RWB_REDIS_PORT" shutdown save >/dev/null 2>&1 || true
    for _ in $(seq 1 100); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$pid" 2>/dev/null; then echo "redis $pid did not stop" >&2; exit 1; fi
  fi
}

status() {
  check_owner
  printf '{"postgres": %s, "redis": %s, "dir": "%s"}\n' \
    "$(pg_running && echo true || echo false)" "$(redis_pid >/dev/null && echo true || echo false)" "$RWB_SHARED"
}

case "${1:-}" in
  ensure) ensure ;;
  stop) stop ;;
  status) status ;;
  *) echo "usage: $0 ensure|stop|status" >&2; exit 2 ;;
esac

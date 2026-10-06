#!/usr/bin/env bash
# Benchmark-owned service glue for tools WITHOUT a native service manager (Nix, Pixi).
# This is user-authored scripting, reported as `scripted`, not a feature of those tools.
#
#   rwb-services.sh up|down|status   run inside the tool's environment (postgres, redis on PATH)
#   source rwb-env.sh                 exports DATABASE_URL/REDIS_URL/PGDATA/REDIS_DATA
#
# Ports come from the checkout's uncommitted bench.local.env. State is checkout-local in
# .rwb-state/. Redis uses AOF with appendfsync always (the common durability policy).
set -euo pipefail
cd "$(dirname "$0")"
root=$PWD
# shellcheck source=/dev/null
source "$root/rwb-env.sh"
log=$root/.rwb-state/logs
mkdir -p "$log" "$REDIS_DATA"

listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

redis_pid() {
  local pid
  pid=$(cat "$root/.rwb-state/redis.pid" 2>/dev/null) || return 1
  kill -0 "$pid" 2>/dev/null && echo "$pid"
}

up() {
  local pg_running=0
  pg_ctl -D "$PGDATA" status >/dev/null 2>&1 && pg_running=1
  if [ "$pg_running" = 0 ] && listening "$PGPORT"; then
    echo "port $PGPORT is already in use by another process" >&2; exit 1
  fi
  if ! redis_pid >/dev/null && listening "$REDIS_PORT"; then
    echo "port $REDIS_PORT is already in use by another process" >&2; exit 1
  fi
  if [ ! -s "$PGDATA/PG_VERSION" ]; then
    initdb -D "$PGDATA" -U postgres --auth=trust --encoding=UTF8 --locale=C >"$log/initdb.log" 2>&1
  fi
  if [ "$pg_running" = 0 ]; then
    pg_ctl -D "$PGDATA" -l "$log/postgres.log" -w -t 60 \
      -o "-p $PGPORT -c listen_addresses=127.0.0.1 -c unix_socket_directories= -c cluster_name=$RWB_INSTANCE" start
  fi
  if ! redis_pid >/dev/null; then
    redis-server --bind 127.0.0.1 --port "$REDIS_PORT" --dir "$REDIS_DATA" --daemonize yes \
      --pidfile "$root/.rwb-state/redis.pid" --logfile "$log/redis.log" \
      --appendonly yes --appendfsync always
  fi
  for _ in $(seq 1 150); do
    if pg_isready -q -h 127.0.0.1 -p "$PGPORT" && [ "$(redis-cli -p "$REDIS_PORT" ping 2>/dev/null)" = PONG ]; then
      exit 0
    fi
    sleep 0.2
  done
  echo "services not ready after 30s" >&2; exit 1
}

down() {
  if pg_ctl -D "$PGDATA" status >/dev/null 2>&1; then
    pg_ctl -D "$PGDATA" -m fast -w -t 60 stop
  fi
  local pid
  if pid=$(redis_pid); then
    # Only shut down the Redis whose pid file this checkout owns.
    test "$(redis-cli -p "$REDIS_PORT" info server | tr -d '\r' | sed -n 's/^process_id://p')" = "$pid"
    redis-cli -p "$REDIS_PORT" shutdown >/dev/null 2>&1 || true
    for _ in $(seq 1 150); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
    ! kill -0 "$pid" 2>/dev/null
  fi
}

status() {
  local pg=stopped redis=stopped
  pg_ctl -D "$PGDATA" status >/dev/null 2>&1 && pg=running
  redis_pid >/dev/null && redis=running
  printf '{"postgres":"%s","redis":"%s","pgport":%s,"redisport":%s}\n' "$pg" "$redis" "$PGPORT" "$REDIS_PORT"
  [ "$pg" = running ] && [ "$redis" = running ]
}

"${1:?usage: rwb-services.sh up|down|status}"

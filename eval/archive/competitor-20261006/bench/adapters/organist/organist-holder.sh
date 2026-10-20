#!/usr/bin/env bash
# Benchmark-owned (scripted) detached holder for Organist's blocking service runner.
# Organist's native command `nix run .#start-services -- start` is a foreground Honcho
# session; this keeps it alive between independent commands, records its PID, and stops
# it by signalling only that recorded Honcho process.
#
#   organist-holder.sh start|stop|status     (run from the checkout, outside `nix develop`)
set -euo pipefail
cd "$(dirname "$0")"
root=$PWD
# shellcheck source=/dev/null
source ./rwb-env.sh
state=$root/.rwb-state
pidfile=$state/honcho.pid
mkdir -p "$state/logs"

listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
# PostgreSQL's own status line (postmaster.pid line 8) reads "ready" once it accepts
# connections on its socket; a foreign listener on PGPORT cannot fake it.
pg_ready() { [ "$(sed -n 8p "$PGDATA/postmaster.pid" 2>/dev/null | tr -d ' ')" = ready ]; }

honcho_pid() {  # the recorded PID, only while it is still this checkout's Honcho
  local pid
  pid=$(cat "$pidfile" 2>/dev/null) || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  ps -o args= -p "$pid" | grep -q honcho || return 1
  [ "$(readlink "/proc/$pid/cwd")" = "$root" ] || return 1
  echo "$pid"
}

start() {
  if pid=$(honcho_pid); then
    echo "honcho already running (pid $pid)"; return 0
  fi
  # `nix run` and the generated wrapper exec in place, so $! becomes Honcho itself.
  setsid nix run --no-update-lock-file .#start-services -- start \
    >"$state/logs/honcho.log" 2>&1 </dev/null &
  local pid=$!
  echo "$pid" >"$pidfile"
  # Return when PostgreSQL reports ready and both ports accept, or fail when Honcho exits
  # (it stops all services when any one exits, e.g. on a port that is already taken). App
  # readiness is still checked separately by the app's own retrying `wait`.
  for _ in $(seq 1 1500); do
    if ! kill -0 "$pid" 2>/dev/null; then
      wait "$pid" && rc=0 || rc=$?
      echo "honcho exited during start (exit $rc); log:" >&2
      cat "$state/logs/honcho.log" >&2
      return 1
    fi
    if ps -o args= -p "$pid" | grep -q honcho && pg_ready && listening "$PGPORT" && listening "$REDIS_PORT"; then
      echo "honcho pid $pid; ports $PGPORT $REDIS_PORT accepting"; return 0
    fi
    sleep 0.2
  done
  echo "services did not open their ports within 300s" >&2
  return 1
}

stop() {
  local pid
  if ! pid=$(honcho_pid); then
    echo "honcho not running"; return 0
  fi
  # Honcho forwards SIGTERM to every service process group and waits (SIGKILL after 5s).
  kill -TERM "$pid"
  for _ in $(seq 1 300); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$pid" 2>/dev/null; then echo "honcho $pid still running after 30s" >&2; return 1; fi
  for port in "$PGPORT" "$REDIS_PORT"; do
    if listening "$port"; then echo "port $port still accepting after honcho exit" >&2; return 1; fi
  done
  rm -f "$pidfile"
  echo "honcho $pid stopped; ports closed"
}

status() {
  local pid=null honcho=stopped pg=closed redis=closed
  if p=$(honcho_pid); then pid=$p; honcho=running; fi
  listening "$PGPORT" && pg=open
  listening "$REDIS_PORT" && redis=open
  printf '{"honcho":"%s","pid":%s,"postgres_port":%s,"postgres":"%s","redis_port":%s,"redis":"%s"}\n' \
    "$honcho" "$pid" "$PGPORT" "$pg" "$REDIS_PORT" "$redis"
  [ "$honcho" = running ]
}

"${1:?usage: organist-holder.sh start|stop|status}"

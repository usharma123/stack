#!/usr/bin/env bash
# Benchmark-owned glue around the Process Compose CLI (reported as `scripted`).
#
#   rwb-pc.sh up       start a detached manager for this checkout (no-op if it already answers)
#   rwb-pc.sh ready    native `project is-ready --wait` under a 120 s outer deadline, then
#                      require postgres+redis Running/Ready in the JSON process list
#   rwb-pc.sh status   native JSON process list
#   rwb-pc.sh down     native `down`, then wait until the recorded service PIDs and the API are gone
#
# `up` must run inside the toolchain shell (postgres/redis on PATH); the manager inherits it.
# Each checkout owns one control socket (pc.local.env), never the default TCP port 8080.
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=/dev/null
source ./rwb-env.sh
# shellcheck source=/dev/null
source ./pc.local.env
: "${RWB_PC_SOCKET:?pc.local.env must set RWB_PC_SOCKET}"
mkdir -p .rwb-state/logs "$(dirname "$RWB_PC_SOCKET")"

pc() { process-compose --unix-socket "$RWB_PC_SOCKET" "$@"; }
alive() { pc process list -o json >/dev/null 2>&1; }

case "${1:-}" in
up)
  if alive; then
    echo "process-compose manager already answers on $RWB_PC_SOCKET; not starting another" >&2
    exit 0
  fi
  pc --log-file "$PWD/.rwb-state/logs/process-compose.log" \
    up -f process-compose.yaml --disable-dotenv --tui=false --detached postgres redis
  ;;
ready)
  # is-ready --wait has no deadline of its own and retries API errors forever.
  timeout 120 process-compose --unix-socket "$RWB_PC_SOCKET" project is-ready --wait
  pc process list -o json | jq -e '
    [.[] | select(.name == "postgres" or .name == "redis")
         | select(.is_running == true and .status == "Running" and .is_ready == "Ready")]
    | length == 2' >/dev/null
  ;;
status)
  pc process list -o json
  ;;
down)
  if ! alive; then
    echo "no manager answers on $RWB_PC_SOCKET" >&2
    exit 0
  fi
  pids=$(pc process list -o json | jq -r '.[] | select(.name == "postgres" or .name == "redis") | select(.pid > 0) | .pid')
  # `down` returns before teardown completes and ignores teardown errors (research note).
  pc down
  for _ in $(seq 1 300); do
    left=""
    for pid in $pids; do kill -0 "$pid" 2>/dev/null && left="$left $pid"; done
    if [ -z "$left" ] && ! alive; then exit 0; fi
    sleep 0.2
  done
  echo "after down: service pids still alive:${left:- none}; api alive: $(alive && echo yes || echo no)" >&2
  exit 1
  ;;
*)
  echo "usage: rwb-pc.sh up|ready|status|down" >&2
  exit 2
  ;;
esac

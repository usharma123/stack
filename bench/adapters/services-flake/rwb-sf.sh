#!/usr/bin/env bash
# Benchmark-owned glue around the generated services-flake wrapper (`services`, the
# process-compose-flake package for this checkout). Run inside `nix develop` from the
# checkout root: the wrapper resolves data paths against the caller's CWD.
#
#   rwb-sf.sh up       `services up -D` unless this checkout's manager already answers
#   rwb-sf.sh ready    native `project is-ready --wait` (120 s outer deadline), then require
#                      pg/rd Running+Ready in the JSON process list
#   rwb-sf.sh status   native JSON process list
#   rwb-sf.sh logs     bounded native status + manager/process logs (diagnostics only)
#   rwb-sf.sh down     native `down`, then wait for the recorded server PIDs and API to go
set -euo pipefail
cd "$(dirname "$0")"
alive() { services process list -o json >/dev/null 2>&1; }
# Bounded evidence: the native status (if a manager still answers), then the tails of the
# process output log (settings.log_location) and the manager log (cli.options.log-file).
logs() {
  echo "== services process list"
  timeout 10 services process list -o json 2>&1 | head -c 8192
  echo
  for f in .rwb-state/sf/processes.log .rwb-state/sf/process-compose.log; do
    [ -f "$f" ] || { echo "== $f: absent"; continue; }
    echo "== last 80 lines of $f (at most 16 KiB)"
    tail -n 80 "$f" | head -c 16384
    echo
  done
}

case "${1:-}" in
up)
  if alive; then
    echo "services manager already answers; not starting another" >&2
    exit 0
  fi
  services up -D
  ;;
ready)
  # A nonzero status (124 = the 120 s deadline) only adds bounded diagnostics and is
  # returned unchanged.
  rc=0
  timeout 120 services project is-ready --wait || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "native readiness exited $rc; bounded diagnostics follow" >&2
    logs >&2 || :
    exit "$rc"
  fi
  services process list -o json | jq -e '
    [.[] | select(.name == "pg" or .name == "rd")
         | select(.is_running == true and .status == "Running" and .is_ready == "Ready")]
    | length == 2' >/dev/null
  ;;
status)
  services process list -o json
  ;;
logs)
  logs || :
  ;;
down)
  if ! alive; then
    echo "no services manager answers" >&2
    exit 0
  fi
  # Process Compose PIDs here are the start-script processes, which exec the servers.
  pids=$(services process list -o json | jq -r '.[] | select(.name == "pg" or .name == "rd") | select(.pid > 0) | .pid')
  services down
  for _ in $(seq 1 300); do
    left=""
    for pid in $pids; do kill -0 "$pid" 2>/dev/null && left="$left $pid"; done
    if [ -z "$left" ] && ! alive; then exit 0; fi
    sleep 0.2
  done
  echo "after down: service pids still alive:${left:- none}" >&2
  exit 1
  ;;
*)
  echo "usage: rwb-sf.sh up|ready|status|logs|down" >&2
  exit 2
  ;;
esac

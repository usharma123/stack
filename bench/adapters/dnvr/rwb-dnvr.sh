#!/usr/bin/env bash
# Benchmark-owned unattended driver for dnvr's default tmux runner (reported as `scripted`).
# Run inside `nix develop path:<checkout>/dnvr#rwb` from the checkout root (DNVR_STATE set).
#
# dnvr a66c2bb has no detached start or down command: `dnvr up` creates the tmux session and
# then always attaches a client. This driver gives `dnvr up` a real PTY (util-linux
# `script`, transcript kept under .dnvr/logs/), waits for the attached client and for the
# processes' own readiness keys, then presses Ctrl-G, dnvr's bound detach key. Nothing
# replaces the runner: the services run in dnvr's session, under dnvr's process wrappers.
#
#   up      dnvr up on a PTY; wait pg.url + redis.url (dnvr-state); detach with Ctrl-G
#   status  dnvr ps (flock-based liveness table)
#   down    Ctrl-C each process pane (the dashboard's `x`), wait until `dnvr ps` shows no
#           running process, then kill the session (the dashboard's `Q`)
set -euo pipefail
cd "$(dirname "$0")"
: "${DNVR_STATE:?run inside the dnvr devshell}"
sock="$DNVR_STATE/runtime/tmux-rwb-up.sock"
mkdir -p "$DNVR_STATE/logs" "$DNVR_STATE/runtime"

session() { tmux -S "$sock" has-session -t =dnvr 2>/dev/null; }
running() { dnvr ps | awk 'NR > 1 && $3 == "running"' | grep -q .; }

up() {
  local transcript fifo spid rc=0 ready=true
  transcript="$DNVR_STATE/logs/rwb-pty-$(date +%Y%m%dT%H%M%S%N).log"
  fifo=$(mktemp -u "$DNVR_STATE/runtime/rwb-keys.XXXXXX")
  mkfifo "$fifo"
  TERM=xterm-256color script -qfec "dnvr up" "$transcript" < "$fifo" > /dev/null 2>&1 &
  spid=$!
  exec 9> "$fifo"          # keyboard of the PTY session; held open until detach
  rm -f "$fifo"
  attached=false
  for _ in $(seq 1 600); do
    if tmux -S "$sock" list-clients -t =dnvr 2>/dev/null | grep -q .; then attached=true; break; fi
    kill -0 "$spid" 2>/dev/null || break
    sleep 0.1
  done
  if ! "$attached"; then
    echo "dnvr up did not attach a client (transcript $transcript)" >&2
    exec 9>&-
    wait "$spid" || true
    exit 1
  fi
  # Readiness keys are live only while their producer holds its pid lock.
  dnvr-state wait pg.url --timeout 120 >/dev/null || ready=false
  "$ready" && { dnvr-state wait redis.url --timeout 60 >/dev/null || ready=false; }
  printf '\007' >&9        # Ctrl-G -> detach-client (dnvr's binding); `dnvr up` then exits
  exec 9>&-
  for _ in $(seq 1 300); do kill -0 "$spid" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$spid" 2>/dev/null; then
    echo "dnvr up client did not detach (transcript $transcript)" >&2
    exit 1
  fi
  wait "$spid" || rc=$?
  echo "dnvr up transcript: $transcript (client exit $rc)" >&2
  "$ready" || { echo "services did not publish readiness keys" >&2; dnvr ps >&2; exit 1; }
  exit "$rc"
}

down() {
  if ! session; then
    echo "no dnvr session on $sock" >&2
    if running; then dnvr ps >&2; exit 1; fi
    exit 0
  fi
  for pane in $(tmux -S "$sock" list-panes -s -t =dnvr -F '#{@dnvr_role} #{pane_id}' | awk '$1 == "process" {print $2}'); do
    tmux -S "$sock" send-keys -t "$pane" C-c
  done
  for _ in $(seq 1 300); do running || break; sleep 0.2; done
  if running; then
    echo "processes still hold their pid locks after Ctrl-C:" >&2
    dnvr ps >&2
    exit 1
  fi
  tmux -S "$sock" kill-session -t =dnvr
  for _ in $(seq 1 100); do session || exit 0; sleep 0.1; done
  echo "dnvr session still present" >&2
  exit 1
}

case "${1:-}" in
up) up ;;
status) dnvr ps ;;
down) down ;;
*) echo "usage: rwb-dnvr.sh up|status|down" >&2; exit 2 ;;
esac

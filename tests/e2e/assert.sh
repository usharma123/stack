#!/usr/bin/env bash
# Shared by every scenario. Defaults describe the Docker image (tests/e2e/run.sh); the native
# runner (tests/e2e/native.sh) overrides them for macOS and Linux hosts.
set -Eeuo pipefail
: "${STACK_E2E_EXAMPLES:=/examples}" "${STACK_E2E_SRV:=/srv}" "${STACK_E2E_WORK:=$HOME}" "${STACK_E2E_TMP:=/tmp}"
# Used by the scenarios that source this file.
# shellcheck disable=SC2034
EX=$STACK_E2E_EXAMPLES SRV=$STACK_E2E_SRV W=$STACK_E2E_WORK T=$STACK_E2E_TMP
export PATH="${STACK_E2E_BIN:-/opt/stack}:$PATH"
# Never touch the user's own git configuration.
export GIT_CONFIG_GLOBAL="$T/stack-e2e-gitconfig"

fail() { echo "FAIL: $*" >&2; exit 1; }
assert_json() { jq -e "$1" "$2" >/dev/null || fail "JSON assertion $1 in $2"; }
# The command's output goes to a file, not a command substitution: bash 3.2 runs the ERR trap
# for a function's failing return inside one (as `within` returns), even under `if`.
expect_error() {
  local code="$1" output="$T/expect-error.out"; shift
  if "$@" >"$output"; then fail "expected $code, command succeeded: $*"; fi
  jq -e --arg code "$code" '.ok == false and .error.code == $code' "$output" >/dev/null || fail "expected $code: $(cat "$output")"
}
# In-place edit that behaves the same with GNU and BSD userlands.
edit() { perl -pi -e "$1" "$2"; }
# PIDs recorded in an `up --json` result, one per line.
recorded_pids() { jq -r '.data.checks[].pid // empty' "$1"; }
# A service's field in an `up --json` result's checks.
checked() { jq -r --arg s "$1" --arg f "$2" '.data.checks[] | select(.service == $s) | .[$f]' "$3"; }
assert_dead() {
  local pid
  for pid in "$@"; do
    if kill -0 "$pid" 2>/dev/null; then fail "process $pid survived"; fi
  done
}

# Processes a scenario leaves running in the background. `track PID...` names this shell's own
# children, and `track_pids FILE` names a file other commands append theirs to, one per line in
# the form `record_pid` writes. Every process is known by its PID and its start time: cleanup
# signals a PID only while the process holding it still started then, so one that has ended,
# whose PID the system may have given to another process, is never touched. `stop_tracked`,
# run on exit however the scenario ends (an error, a signal), stops each one still running with
# TERM and, two seconds later, KILL. A scenario that sets its own EXIT trap calls it there.
# Children are tracked in a directory, one file per PID, so `within` tracks the same way in a
# command substitution or pipeline, and the scenario's cleanup finds them all.
E2E_TRACKED="$T/tracked.$$" E2E_TRACKED_FILES=()
# started PID: when process PID started, as one line, or nothing if there is no such process.
started() {
  local words=()
  read -r -a words <<<"$(ps -o lstart= -p "$1" 2>/dev/null || true)"
  echo "${words[*]+${words[*]}}"
}
# record_pid FILE [PID]: the shell command with which a process outside this shell appends
# PID (by default its own, `$$`) to FILE for `track_pids`. Its `echo` joins `ps`' words with
# single spaces, as `started` does.
record_pid() {
  local pid=${2:-'$$'}
  # shellcheck disable=SC2016 # Expanded by the process that runs the command.
  printf 'echo %s $(ps -o lstart= -p %s) >>%s' "$pid" "$pid" "$1"
}
track() {
  local pid when
  mkdir -p "$E2E_TRACKED"
  for pid in "$@"; do
    when=$(started "$pid")
    if [[ -n "$when" ]]; then echo "$when" >"$E2E_TRACKED/$pid"; fi
  done
}
untrack() {
  local pid
  for pid in "$@"; do rm -f "$E2E_TRACKED/$pid"; done
}
track_pids() { E2E_TRACKED_FILES+=("$@"); }
# alive PID WHEN: whether process PID is running and started at WHEN.
alive() { [[ -n "$2" && "$(started "$1")" == "$2" ]]; }
# halt PID WHEN...: send TERM to each process still alive, then KILL to any still alive two
# seconds later.
halt() {
  local args=("$@") i tenths running
  for ((i = 0; i < ${#args[@]}; i += 2)); do
    if alive "${args[i]}" "${args[i + 1]}"; then kill -TERM "${args[i]}" 2>/dev/null || true; fi
  done
  for ((tenths = 0; tenths < 20; tenths++)); do
    running=0
    for ((i = 0; i < ${#args[@]}; i += 2)); do
      if alive "${args[i]}" "${args[i + 1]}"; then running=1; fi
    done
    ((running)) || return 0
    sleep 0.1
  done
  for ((i = 0; i < ${#args[@]}; i += 2)); do
    if alive "${args[i]}" "${args[i + 1]}"; then kill -KILL "${args[i]}" 2>/dev/null || true; fi
  done
}
# tracked PID: the start time `track` recorded for PID, or nothing.
tracked() { cat "$E2E_TRACKED/$1" 2>/dev/null || true; }
# stop_child PID: stop this shell's tracked child PID (TERM, then KILL), collect its exit status
# and stop tracking it.
stop_child() {
  local when
  when=$(tracked "$1")
  if [[ -n "$when" ]]; then halt "$1" "$when"; fi
  wait "$1" 2>/dev/null || true
  untrack "$1"
}
stop_tracked() {
  local file pid when targets=()
  for file in "$E2E_TRACKED"/*; do
    [[ -f "$file" ]] || continue
    targets+=("${file##*/}" "$(tracked "${file##*/}")")
  done
  for file in ${E2E_TRACKED_FILES[@]+"${E2E_TRACKED_FILES[@]}"}; do
    [[ -f "$file" ]] || continue
    while read -r pid when || [[ -n "$pid" ]]; do
      if [[ "$pid" =~ ^[0-9]+$ ]]; then targets+=("$pid" "$when"); fi
    done <"$file"
  done
  if ((${#targets[@]})); then halt "${targets[@]}"; fi
  rm -rf "$E2E_TRACKED"
}
trap stop_tracked EXIT
# Turn cancellation into an ordinary exit so the scenario can run its cleanup, including when
# Bash was waiting for a pipeline or command substitution.
trap 'exit 130' INT
trap 'exit 143' TERM
# within SECONDS COMMAND...: run COMMAND with this shell's standard input and return its status.
# If it is still running after SECONDS, stop it (TERM, then KILL), print what `diagnose` prints
# when the scenario defines that function, and fail naming the command and the limit. COMMAND
# is tracked only while it runs.
within() {
  local limit=$1 pid='' tenths=0 status=0
  shift
  # Stop a tracked child here on cancellation. If it inherited the substitution's stdout, the
  # outer shell cannot run its cleanup until the child closes the pipe.
  if ((BASH_SUBSHELL > 0)); then
    trap 'if [[ -f "$E2E_TRACKED/$pid" ]]; then stop_child "$pid"; fi; exit 130' INT
    trap 'if [[ -f "$E2E_TRACKED/$pid" ]]; then stop_child "$pid"; fi; exit 143' TERM
  fi
  "$@" <&0 &
  pid=$!
  track "$pid"
  # Poll this shell's child without forking a second command substitution. stop_child still
  # checks the tracked start time before sending a signal.
  while kill -0 "$pid" 2>/dev/null; do
    if ((tenths >= limit * 10)); then
      stop_child "$pid"
      if declare -F diagnose >/dev/null; then diagnose >&2 || true; fi
      fail "still running after ${limit}s, stopped: $*"
    fi
    sleep 0.1
    tenths=$((tenths + 1))
  done
  wait "$pid" || status=$?
  untrack "$pid"
  if ((BASH_SUBSHELL > 0)); then trap - INT TERM; fi
  return "$status"
}
# eventually SECONDS WHAT COMMAND...: run COMMAND every tenth of a second until it succeeds; if
# it has not after SECONDS, print what `diagnose` prints and fail saying WHAT never happened.
eventually() {
  local limit=$1 what=$2 deadline
  shift 2
  deadline=$(($(date +%s) + limit))
  until "$@"; do
    if (($(date +%s) >= deadline)); then
      if declare -F diagnose >/dev/null; then diagnose >&2 || true; fi
      fail "$what (not within ${limit}s)"
    fi
    sleep 0.1
  done
}

report_failure() {
  local status=$?
  echo "FAIL at line $1: $2" >&2
  local file
  for file in "$T/up-appA.json" "$T/up-appB.json" "$T/old-generation.json" "$T/new-generation.json"; do
    if [[ -f "$file" ]]; then jq -c '.error // empty' "$file" >&2 || true; fi
  done
  return "$status"
}
trap 'report_failure "$LINENO" "$BASH_COMMAND"' ERR

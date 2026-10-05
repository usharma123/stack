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
expect_error() {
  local code="$1"; shift
  local output
  if output=$("$@"); then fail "expected $code, command succeeded: $*"; fi
  printf '%s\n' "$output" | jq -e --arg code "$code" '.ok == false and .error.code == $code' >/dev/null || fail "expected $code: $output"
}
# In-place edit that behaves the same with GNU and BSD userlands.
edit() { perl -pi -e "$1" "$2"; }
# PIDs recorded in an `up --json` result, one per line.
recorded_pids() { jq -r '.data.session.services[].pid // empty' "$1"; }
assert_dead() {
  local pid
  for pid in "$@"; do
    if kill -0 "$pid" 2>/dev/null; then fail "process $pid survived"; fi
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

#!/usr/bin/env bash
set -Eeuo pipefail
fail() { echo "FAIL: $*" >&2; exit 1; }
assert_json() { jq -e "$1" "$2" >/dev/null || fail "JSON assertion $1 in $2"; }
expect_error() {
  local code="$1"; shift
  local output
  if output=$("$@"); then fail "expected $code, command succeeded: $*"; fi
  printf '%s\n' "$output" | jq -e --arg code "$code" '.ok == false and .error.code == $code' >/dev/null || fail "expected $code: $output"
}

report_failure() {
  local status=$?
  echo "FAIL at line $1: $2" >&2
  local file
  for file in /tmp/up-appA.json /tmp/up-appB.json /tmp/old-generation.json /tmp/new-generation.json; do
    if [[ -f "$file" ]]; then jq -c '.error // empty' "$file" >&2 || true; fi
  done
  return "$status"
}
trap 'report_failure "$LINENO" "$BASH_COMMAND"' ERR

#!/usr/bin/env bash
# Service and session scenarios on this machine (macOS or Linux) against real mise + Pitchfork.
#
#   tests/e2e/native.sh <path-to-stack-binary> [scenario ...]
#
# Needs mise and jq on PATH and port 5432 free. HOME and XDG directories are isolated in a short
# /tmp directory: Pitchfork's and Postgres' sockets must fit sun_path (104 bytes on macOS).
# Scenario 5 needs an OCI registry: set STACK_E2E_REGISTRY=host:port, otherwise it is skipped
# and reported as skipped. Results: one line per scenario on stdout and $STACK_E2E_RESULTS.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
shift
command -v mise >/dev/null || { echo 'mise is not on PATH' >&2; exit 2; }
command -v jq >/dev/null || { echo 'jq is not on PATH' >&2; exit 2; }
if (exec 3<>/dev/tcp/127.0.0.1/5432) 2>/dev/null; then
  echo 'port 5432 is in use; scenario 2 starts its own foreign server there' >&2
  exit 2
fi
work=$(mktemp -d /tmp/se2e.XXXXXX)
work=$(cd "$work" && pwd -P)
mkdir -p "$work/bin" "$work/h" "$work/srv" "$work/w" "$work/t"
ln -s "$binary" "$work/bin/stack"
export HOME="$work/h" XDG_CONFIG_HOME="$work/h/.config" XDG_CACHE_HOME="$work/h/.cache" \
  XDG_DATA_HOME="$work/h/.local/share" XDG_STATE_HOME="$work/h/.local/state"
export STACK_E2E_EXAMPLES="$root/examples" STACK_E2E_SRV="$work/srv" STACK_E2E_WORK="$work/w" \
  STACK_E2E_TMP="$work/t" STACK_E2E_BIN="$work/bin" STACK_E2E_ASSERT="$root/tests/e2e/assert.sh"
export MISE_YES=1
results=${STACK_E2E_RESULTS:-$work/results.jsonl}
# shellcheck disable=SC2329 # Invoked by the EXIT trap below.
cleanup() {
  status=$?
  for app in "$work"/w/*/; do
    [ -f "$app/stack.toml" ] && "$binary" -C "$app" down --json >/dev/null 2>&1 || true
  done
  (cd "$work/w/appA" 2>/dev/null && mise exec -- pitchfork supervisor stop >/dev/null 2>&1) || true
  if [ "$status" -eq 0 ] && [ -z "${STACK_E2E_KEEP:-}" ]; then rm -rf "$work"; else echo "work dir kept: $work" >&2; fi
  exit "$status"
}
trap cleanup EXIT
{ "$binary" --version; mise --version 2>/dev/null | head -1; uname -sm; } >"$work/versions.txt"
cat "$work/versions.txt"
scenarios=("$@")
if [ ${#scenarios[@]} -eq 0 ]; then
  for t in "$root"/tests/e2e/[0-9]-*.sh; do scenarios+=("$(basename "$t" .sh)"); done
fi
failed=0
for name in "${scenarios[@]}"; do
  if [[ "$name" == 5-* && -z "${STACK_E2E_REGISTRY:-}" ]]; then
    echo "=== $name: SKIPPED (no STACK_E2E_REGISTRY)"
    printf '{"scenario":"%s","result":"skipped","reason":"no OCI registry"}\n' "$name" >>"$results"
    continue
  fi
  echo "=== $name"
  start=$(date +%s)
  if bash "$root/tests/e2e/$name.sh"; then result=passed; else result=failed; failed=1; fi
  printf '{"scenario":"%s","result":"%s","seconds":%s}\n' "$name" "$result" "$(( $(date +%s) - start ))" >>"$results"
  echo "--- $name: $result"
  [ "$result" = passed ] || break
done
exit "$failed"

#!/usr/bin/env bash
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"
originals=(1-independent-checkouts 2-wrong-instance 3-leases 4-mcp)
pass2=(13-port-isolation 14-run-logs-cursor 15-mcp-eof)
# The original four run in order through the one chosen (each leaves what the next needs). A
# Pass 2 scenario runs after the first, which sets up git and mise's tools; `pass2` runs all three.
case "${1:-}" in
  1-independent-checkouts|2-wrong-instance|3-leases|4-mcp)
    scenarios=()
    for prerequisite in "${originals[@]}"; do
      scenarios+=("$prerequisite")
      [ "$prerequisite" != "$1" ] || break
    done
    expected=("$1") ;;
  13-port-isolation|14-run-logs-cursor|15-mcp-eof) scenarios=(1-independent-checkouts "$1"); expected=("$1") ;;
  pass2) scenarios=(1-independent-checkouts "${pass2[@]}"); expected=("${pass2[@]}") ;;
  *) echo "Choose ${originals[*]} ${pass2[*]} or pass2." >&2; exit 2 ;;
esac
mkdir -p target/verification
evidence=$(mktemp -d "$root/target/verification/cli.XXXXXX")
printf 'Evidence: %s\n' "$evidence"
{
  git rev-parse HEAD
  git status --porcelain
} >"$evidence/source.log"
target_dir="$root/target/verification/build"
read -r -a cargo <<<"${CARGO:-cargo}"
"${cargo[@]}" build --locked --bin stack --target-dir "$target_dir"
binary="$target_dir/debug/stack"
{
  "$binary" --version
  command -v mise
  command -v jq
  printf 'Scenario: %s\n' "$1"
} | tee "$evidence/doctor.log"
STACK_E2E_RESULTS="$evidence/results.jsonl" STACK_E2E_EVIDENCE="$evidence" \
  bash tests/e2e/native.sh "$binary" "${scenarios[@]}" 2>&1 | tee "$evidence/transcript.log"
for scenario in "${expected[@]}"; do
  jq -e --arg name "$scenario" 'select(.scenario == $name and .result == "passed")' "$evidence/results.jsonl"
done

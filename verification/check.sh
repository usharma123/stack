#!/usr/bin/env bash
# Run Stack's formal verification: each TLA+ verify skill's models and their Rust regressions.
#   verification/check.sh                    every skill
#   verification/check.sh session snapshot   only those (pgtemplate, session, snapshot)
# VERIFY_SKIP_RUST=1 checks only the models; CARGO="cargo +1.93.1" picks the toolchain.
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
skills=("$@")
[ ${#skills[@]} -gt 0 ] || skills=(pgtemplate session snapshot)
for skill in "${skills[@]}"; do
  script="$root/.cursor/skills/verify-stack-$skill/check.sh"
  [ -x "$script" ] || { echo "no verify skill named $skill ($script)" >&2; exit 2; }
  echo "== $skill"
  "$script"
done
echo "All verification passed: ${skills[*]}"

#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'A task and a command receive granted fnox secrets, redacted from captured output'
mkdir -p "$W/appS"
cd "$W/appS" || exit 1
cat >stack.toml <<'TOML'
[tools]
fnox = "1.39.0"

[tasks.deploy]
run = "printf 'task got %s\n' \"$DEPLOY_KEY\"; printf 'err %s\n' \"$DEPLOY_KEY\" >&2; printf 'other %s\n' \"${OTHER_KEY-unset}\""
secrets = ["DEPLOY_KEY"]
TOML
# fnox's own configuration, isolated with HOME; plain values stand in for a real provider.
cat >fnox.toml <<'TOML'
[providers.plain]
type = "plain"

[secrets]
DEPLOY_KEY = { provider = "plain", value = "e2e-sentinel-deploy-4b1d" }
OTHER_KEY = { provider = "plain", value = "e2e-sentinel-other-77aa" }
SHORT_KEY = { provider = "plain", value = "abc1234" }
TOML
sentinels='e2e-sentinel-deploy-4b1d|e2e-sentinel-other-77aa|e2e-sentinel-malformed-9f0e'
no_leak() { if grep -Eq "$sentinels" "$@"; then fail "a secret value reached $*"; fi; }
stack compile --json >"$T/s-compile.json"
assert_json '.ok and .data.stack.tasks.deploy.value.secrets == ["DEPLOY_KEY"]' "$T/s-compile.json"
stack install --json >"$T/s-install.json"
assert_json '.ok' "$T/s-install.json"

stack run deploy --json >"$T/s-run.json" 2>"$T/s-run.err"
assert_json '.ok and .data.exit_code == 0 and .data.secrets == ["DEPLOY_KEY"]' "$T/s-run.json"
assert_json '(.data.stdout | contains("task got [redacted:DEPLOY_KEY]")) and (.data.stdout | contains("other unset"))' "$T/s-run.json"
assert_json '.data.stderr | contains("err [redacted:DEPLOY_KEY]")' "$T/s-run.json"
no_leak "$T/s-run.json" "$T/s-run.err"

stack exec --json --secret DEPLOY_KEY -- sh -c 'echo "x$DEPLOY_KEY"' >"$T/s-exec.json"
assert_json '.ok and .data.stdout == "x[redacted:DEPLOY_KEY]\n"' "$T/s-exec.json"
no_leak "$T/s-exec.json"
# On the terminal the command owns its output, as documented: nothing is redacted.
[ "$(stack exec --secret DEPLOY_KEY -- sh -c 'printf %s "$DEPLOY_KEY"')" = e2e-sentinel-deploy-4b1d ] || fail 'terminal exec did not receive the grant'
expect_error secret_unsupported stack exec --json --secret SHORT_KEY -- true
expect_error secret_missing stack exec --json --secret NOPE_KEY -- true
expect_error invalid_secret stack exec --json --secret PATH -- true

printf '%s\n' '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}' \
  "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"stack_exec\",\"arguments\":{\"dir\":\"$W/appS\",\"command\":[\"sh\",\"-c\",\"echo \$DEPLOY_KEY\"],\"secrets\":[\"DEPLOY_KEY\"]}}}" \
  | stack mcp >"$T/s-mcp.jsonl"
jq -s '.[] | select(.id == 1) | .result.structuredContent' "$T/s-mcp.jsonl" >"$T/s-mcp.json"
assert_json '.ok and .data.stdout == "[redacted:DEPLOY_KEY]\n" and .data.secrets == ["DEPLOY_KEY"]' "$T/s-mcp.json"
no_leak "$T/s-mcp.jsonl"

echo 'A malformed fnox.toml fails without its quoted line reaching the error'
cp fnox.toml fnox.toml.good
printf '[secrets]\nDEPLOY_KEY = { provider = "plain", value = "e2e-sentinel-malformed-9f0e"\n' >fnox.toml
expect_error secret_unavailable stack exec --json --secret DEPLOY_KEY -- true
stack exec --json --secret DEPLOY_KEY -- true >"$T/s-malformed.json" 2>"$T/s-malformed.err" || true
assert_json '.error.details[0].kind == "config"' "$T/s-malformed.json"
no_leak "$T/s-malformed.json" "$T/s-malformed.err"
mv fnox.toml.good fnox.toml

stack doctor --json >"$T/s-doctor.json" || true
jq -e '(.data // .error.details)[] | select(.name == "fnox") | .ok' "$T/s-doctor.json" >/dev/null || fail "doctor's fnox check failed: $(cat "$T/s-doctor.json")"
no_leak "$T/s-doctor.json"
# Nothing stack wrote holds a value.
written=(.stack .config stack.lock)
for dir in "${STACK_STATE_DIR:-$HOME/.local/state/stack}" "${STACK_CACHE_DIR:-$HOME/.cache/stack}"; do
  [ -d "$dir" ] && written+=("$dir")
done
if grep -rEl "$sentinels" "${written[@]}" 2>/dev/null; then fail 'a secret value was written to disk'; fi

#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
echo 'A custom service with an identity probe verifies as its own instance'
for a in appF appG; do
  mkdir -p "$W/$a"
  printf '[[use]]\nbundle = "path:%s"\n' "$EX/bundles/webid" >"$W/$a/stack.toml"
done
cd "$W/appF" || exit 1
stack compile >/dev/null
stack up --json >"$T/up-appF.json"
assert_json '.ok and .data.checks[0].ready and .data.checks[0].identity == "instance"' "$T/up-appF.json"
stack exec --require web -- python3 -c 'import os,urllib.request; urllib.request.urlopen("http://127.0.0.1:%s/identity" % os.environ["WEB_PORT"]).read()'
f_port=$(jq -r '.data.session.services.web.port' "$T/up-appF.json")

echo "A healthy server of another checkout cannot satisfy this checkout's probe"
cd "$W/appG" || exit 1
# The app's connection settings point at appF's (healthy, running) server.
printf '[env]\nWEB_PORT = "%s"\n' "$f_port" >>stack.toml
stack compile >/dev/null
if stack up --json >"$T/up-appG.json"; then fail "appG verified against appF's server"; fi
assert_json '.ok == false and .error.code == "not_ready" and (.error.details[0].reason | contains("reached a different instance"))' "$T/up-appG.json"
expect_error service_unavailable stack exec --require web --json -- true
stack down --json >"$T/down-appG.json"
assert_json '.ok and .data.confirmed' "$T/down-appG.json"
cd "$W/appF" || exit 1
stack status --json >"$T/status-appF.json"
assert_json '.ok and .data.checks[0].identity == "instance"' "$T/status-appF.json"
stack down --json >"$T/down-appF.json"
assert_json '.ok and .data.confirmed' "$T/down-appF.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/up-appF.json")

#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
cp -r "$EX/app" "$W/appD"
cd "$W/appD" || exit 1
rm -f stack.lock
edit "s|path:../bundles/pybase|git+file://$SRV/pybase?ref=v1|; s|path:../bundles/obs|git+file://$SRV/obs?ref=v1|" stack.toml
printf '\n[override.services.redis]\npreset = "redis"\nversion = "7"\n' >>stack.toml
stack compile --json >"$T/generation-compile.json"
stack up --json >"$T/old-generation.json"
old=$(stack exec --require redis -- bash -c 'redis-cli -u "$REDIS_URL" info server | grep redis_version')
[[ "$old" == redis_version:7.* ]] || fail "unexpected initial Redis: $old"
stack exec --require redis -- bash -c 'redis-cli -u "$REDIS_URL" set stack-generation preserved' >/dev/null
edit 's/version = "7"/version = "8"/' stack.toml
stack compile --json >"$T/generation-recompile.json"
# A project override changes the locked service version, never the bundle pins.
jq -se '(.[0].data.bundles | map({source, commit, digest, content_hash})) == (.[1].data.bundles | map({source, commit, digest, content_hash}))' "$T/generation-compile.json" "$T/generation-recompile.json" >/dev/null || fail 'bundle pins changed for a project override'
jq -e '.data.versions[] | select(.name == "redis") | .requested == "8" and (.resolved | startswith("8.")) and (.moved_from | startswith("7."))' "$T/generation-recompile.json" >/dev/null || fail 'the changed Redis request was not re-resolved'
if stack status --json >"$T/stale.json"; then fail 'stale service configuration was reported healthy'; fi
assert_json '.ok and .data.stale and all(.data.checks[]; .ready == false)' "$T/stale.json"
expect_error service_unavailable stack exec --require-all --json -- touch "$T/stale-executed"
[[ ! -e "$T/stale-executed" ]] || fail 'stale command was executed'
# mise's preset deliberately rejects data created under another major version.
# Stack must report that rejection, never adopt the old process as the new generation.
if stack up --json >"$T/new-generation.json"; then fail 'version change bypassed the provider data migration gate'; fi
assert_json '.ok == false and .error.code == "start_failed" and any(.error.details[]; .changed == true)' "$T/new-generation.json"
old_pid=$(jq -r '.data.session.services.redis.pid' "$T/old-generation.json")
if kill -0 "$old_pid" 2>/dev/null; then fail 'old Redis process survived the generation change'; fi
expect_error service_unavailable stack exec --require-all --json -- true
edit 's/version = "8"/version = "7"/' stack.toml
stack compile >/dev/null
stack up --json >"$T/recovered-generation.json"
assert_json '.ok and all(.data.checks[]; .ready)' "$T/recovered-generation.json"
jq -se '.[0].data.session.id != .[1].data.session.id and .[0].data.session.services.redis.pid != .[1].data.session.services.redis.pid' "$T/old-generation.json" "$T/recovered-generation.json" >/dev/null
recovered=$(stack exec --require redis -- bash -c 'redis-cli -u "$REDIS_URL" get stack-generation')
[[ "$recovered" == preserved ]] || fail 'version rejection reset the Redis data'
stack status --json >"$T/current.json"
assert_json '.ok and (.data.stale == false) and all(.data.checks[]; .ready)' "$T/current.json"
stack down --json >"$T/down.json"
assert_json '.ok and .data.confirmed' "$T/down.json"

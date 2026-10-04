#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source /tmp/stack-e2e-assert.sh
export PATH=/opt/stack:$PATH
cp -r /examples/app ~/appD
cd ~/appD || exit 1
rm -f stack.lock
sed -i 's|path:../bundles/pybase|git+file:///srv/pybase?ref=v1|; s|path:../bundles/obs|git+file:///srv/obs?ref=v1|' stack.toml
printf '\n[override.services.redis]\npreset = "redis"\nversion = "7"\n' >>stack.toml
stack compile >/dev/null
stack up --json >/tmp/old-generation.json
old=$(stack exec --require redis -- bash -c 'redis-cli -u "$REDIS_URL" info server | grep redis_version')
[[ "$old" == redis_version:7.* ]] || fail "unexpected initial Redis: $old"
stack exec --require redis -- bash -c 'redis-cli -u "$REDIS_URL" set stack-generation preserved' >/dev/null
cp stack.lock /tmp/generation.lock
sed -i 's/version = "7"/version = "8"/' stack.toml
stack compile >/dev/null
cmp -s stack.lock /tmp/generation.lock || fail 'bundle pins changed for a project override'
if stack status --json >/tmp/stale.json; then fail 'stale service configuration was reported healthy'; fi
assert_json '.ok and .data.stale and all(.data.checks[]; .ready == false)' /tmp/stale.json
expect_error service_unavailable stack exec --require-all --json -- touch /tmp/stale-executed
[[ ! -e /tmp/stale-executed ]] || fail 'stale command was executed'
# mise's preset deliberately rejects data created under another version declaration.
# Stack must report that rejection, never adopt the old process as the new generation.
if stack up --json >/tmp/new-generation.json; then fail 'version change bypassed the provider data migration gate'; fi
assert_json '.ok == false and .error.code == "start_failed" and any(.error.details[]; .changed == true)' /tmp/new-generation.json
old_pid=$(jq -r '.data.session.services.redis.pid' /tmp/old-generation.json)
if kill -0 "$old_pid" 2>/dev/null; then fail 'old Redis process survived the generation change'; fi
expect_error service_unavailable stack exec --require-all --json -- true
sed -i 's/version = "8"/version = "7"/' stack.toml
stack compile >/dev/null
stack up --json >/tmp/recovered-generation.json
assert_json '.ok and all(.data.checks[]; .ready)' /tmp/recovered-generation.json
jq -se '.[0].data.session.id != .[1].data.session.id and .[0].data.session.services.redis.pid != .[1].data.session.services.redis.pid' /tmp/old-generation.json /tmp/recovered-generation.json >/dev/null
recovered=$(stack exec --require redis -- bash -c 'redis-cli -u "$REDIS_URL" get stack-generation')
[[ "$recovered" == preserved ]] || fail 'version rejection reset the Redis data'
stack status --json >/tmp/current.json
assert_json '.ok and (.data.stale == false) and all(.data.checks[]; .ready)' /tmp/current.json
stack down --json >/tmp/down.json
assert_json '.ok and .data.confirmed' /tmp/down.json

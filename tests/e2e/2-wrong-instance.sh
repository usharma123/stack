#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source /tmp/stack-e2e-assert.sh
export PATH=/opt/stack:$PATH
cd ~/appA || exit 1
PGBIN=$(dirname "$(mise which postgres)")
"$PGBIN/initdb" -D /tmp/f5432 -U postgres --auth=trust >/dev/null
"$PGBIN/pg_ctl" -D /tmp/f5432 -l /tmp/f5432.log -o '-p 5432 -k /tmp -c listen_addresses=127.0.0.1' -w start >/dev/null
trap '"$PGBIN/pg_ctl" -D /tmp/f5432 -m fast stop >/dev/null' EXIT
stack down --json >/tmp/down.json
assert_json '.ok and .data.confirmed' /tmp/down.json
actual=$(stack exec -- bash -c 'printf "%s" "$DATABASE_URL"')
[[ "$actual" == *unverified.stack.invalid* ]] || fail 'unverified DATABASE_URL was not poisoned'
if stack exec -- bash -c 'uv run pytest -q' >/tmp/wrong-instance.log 2>&1; then
  fail 'tests passed against a foreign default-port Postgres'
fi
rg_check=$(grep -c 'unverified.stack.invalid' /tmp/wrong-instance.log || true)
(( rg_check > 0 )) || fail 'test did not fail on the poisoned endpoint'
expect_error service_unavailable stack exec --require-all --json -- true

echo 'Project endpoint changes invalidate the generation before execution'
stack up >/dev/null
printf '\nDATABASE_URL = "postgresql://postgres@127.0.0.1:5432/postgres"\n' >>stack.toml
stack compile >/dev/null
if stack status --json >/tmp/wrong-status.json; then fail 'changed endpoint was still ready'; fi
assert_json '.ok and .data.stale and all(.data.checks[]; .ready == false)' /tmp/wrong-status.json
actual=$(stack exec -- bash -c 'printf "%s" "$DATABASE_URL"')
[[ "$actual" == *unverified.stack.invalid* ]] || fail 'foreign endpoint was exported'
expect_error service_unavailable stack exec --require postgres --json -- true
git checkout -q -- stack.toml
stack compile >/dev/null
stack down --json >/tmp/down.json
assert_json '.ok and .data.confirmed' /tmp/down.json

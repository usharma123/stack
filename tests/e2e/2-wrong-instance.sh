#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
cd "$W/appA" || exit 1
PGBIN=$(dirname "$(mise which postgres)")
"$PGBIN/initdb" -D "$T/f5432" -U postgres --auth=trust >/dev/null
"$PGBIN/pg_ctl" -D "$T/f5432" -l "$T/f5432.log" -o "-p 5432 -k $T -c listen_addresses=127.0.0.1" -w start >/dev/null
trap '"$PGBIN/pg_ctl" -D "$T/f5432" -m fast stop >/dev/null' EXIT
stack down --json >"$T/down.json"
assert_json '.ok and .data.confirmed' "$T/down.json"
actual=$(stack exec -- bash -c 'printf "%s" "$DATABASE_URL"')
[[ "$actual" == *unverified.stack.invalid* ]] || fail 'unverified DATABASE_URL was not poisoned'
if stack exec -- bash -c 'uv run pytest -q' >"$T/wrong-instance.log" 2>&1; then
  fail 'tests passed against a foreign default-port Postgres'
fi
rg_check=$(grep -c 'unverified.stack.invalid' "$T/wrong-instance.log" || true)
(( rg_check > 0 )) || fail 'test did not fail on the poisoned endpoint'
expect_error service_unavailable stack exec --require-all --json -- true

echo 'libpq overrides in withheld endpoints cannot reach the foreign server'
for url in 'postgresql://postgres@localhost:5432/postgres#x?host=127.0.0.1' \
  "host = $T port = 5432 dbname = postgres" \
  "host=$T password='a b' port=5432 dbname=postgres" ''; do
  printf '\nDATABASE_URL = "%s"\n' "$url" >>stack.toml
  stack compile >/dev/null
  for args in '"$DATABASE_URL"' ''; do
    if PGHOSTADDR=127.0.0.1 PGHOST="$T" PGPORT=5432 PGCONNECT_TIMEOUT=3 \
      stack exec -- bash -c "psql -w -Atc 'select 1' $args" >"$T/libpq.log" 2>&1; then
      fail "psql reached the foreign server through '$url' ($args)"
    fi
    grep -q 'unverified.stack.invalid' "$T/libpq.log" || fail "psql did not use the invalid host for '$url' ($args)"
  done
  git checkout -q -- stack.toml
done
stack compile >/dev/null

echo 'Project endpoint changes invalidate the generation before execution'
stack up >/dev/null
printf '\nDATABASE_URL = "postgresql://postgres@127.0.0.1:5432/postgres"\n' >>stack.toml
stack compile >/dev/null
if stack status --json >"$T/wrong-status.json"; then fail 'changed endpoint was still ready'; fi
assert_json '.ok and .data.stale and all(.data.checks[]; .ready == false)' "$T/wrong-status.json"
actual=$(stack exec -- bash -c 'printf "%s" "$DATABASE_URL"')
[[ "$actual" == *unverified.stack.invalid* ]] || fail 'foreign endpoint was exported'
expect_error service_unavailable stack exec --require postgres --json -- true
git checkout -q -- stack.toml
stack compile >/dev/null
stack down --json >"$T/down.json"
assert_json '.ok and .data.confirmed' "$T/down.json"

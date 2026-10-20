#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
# A first `up` with no UTF-8 locale (CI, `env -i`, containers) used to create a SQL_ASCII
# cluster, so the same commit passed on a laptop and failed in CI.
mkdir -p "$W/appL"
cd "$W/appL" || exit 1
git init -q
cat >stack.toml <<'TOML'
[services.postgres]
preset = "postgres"
version = "17"
TOML
stack compile >/dev/null

echo 'A first up under LC_ALL=C creates a UTF8 cluster with the C.UTF-8 locale'
LC_ALL=C LANG= stack up --json >"$T/up-appL.json"
assert_json '.ok and all(.data.checks[]; .ready) and (.data.warnings == null)' "$T/up-appL.json"
settings=$(LC_ALL=C stack exec --require postgres -- sh -c 'psql "$DATABASE_URL" -Atc "show server_encoding" -c "select datcollate from pg_database where datname = current_database()"' | tr '\n' ' ')
[[ "$settings" == "UTF8 C.UTF-8 " ]] || fail "cluster settings: $settings"

echo 'Text round-trips as characters, not bytes'
roundtrip=$(LC_ALL=C stack exec --require postgres -- sh -c 'psql "$DATABASE_URL" -qAt -c "create table t (v text)" -c "insert into t values ('\''Zoë Café ✓'\'')" -c "select length(v), octet_length(v) from t"')
[[ "$roundtrip" == "10|14" ]] || fail "length and octet_length: $roundtrip"
stack down --json >"$T/down-appL.json"
assert_json '.ok and .data.confirmed' "$T/down-appL.json"

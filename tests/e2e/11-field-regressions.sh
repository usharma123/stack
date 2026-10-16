#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
# What field use of 0.1.21 found: a custom service's URL, data a recreated checkout inherited,
# a service process the supervisor lost, and results too long for agents.
mkdir -p "$W/appR"
cd "$W/appR" || exit 1
git init -q
cat >stack.toml <<EOF
[[use]]
bundle = "path:$EX/bundles/pybase"

[services.api]
run = "exec python3 -m http.server \$PORT --bind 127.0.0.1"
ready_cmd = "curl -fsS http://127.0.0.1:\$PORT/ >/dev/null"
EOF
stack compile >/dev/null

echo 'Services start and verify, and the result is one check per service'
stack up --json >"$T/up-appR.json"
assert_json '.ok and ([.data.checks[].service] | sort) == ["api", "postgres", "redis"] and all(.data.checks[]; .ready)' "$T/up-appR.json"
assert_json '.data.session | keys == ["id", "started_at"]' "$T/up-appR.json"
(( $(wc -c <"$T/up-appR.json") < 1500 )) || fail "up result is $(wc -c <"$T/up-appR.json") bytes"

echo "A custom service's URL is its loopback address and answers"
port=$(checked api port "$T/up-appR.json")
url=$(stack exec --require api -- sh -c 'curl -fsS "$API_URL/" >/dev/null && printf %s "$API_URL"')
[[ "$url" == "http://127.0.0.1:$port" ]] || fail "API_URL is $url"

echo 'Services carry the checkout token; commands do not'
token=$(stack exec -- python3 -c 'import tomllib; print(tomllib.load(open(".config/mise/conf.d/stack.toml", "rb"))["daemons"]["postgres"]["env"]["STACK_CHECKOUT_TOKEN"])')
[[ ${#token} -eq 32 ]] || fail "no token rendered: $token"
pg_pid=$(checked postgres pid "$T/up-appR.json")
ps eww -p "$pg_pid" | grep -q "STACK_CHECKOUT_TOKEN=$token" || fail 'postgres does not carry the checkout token'
if stack exec -- env | grep -q STACK_CHECKOUT_TOKEN; then fail 'a command received the checkout token'; fi

echo 'The same checkout keeps its data across down and up'
stack exec --require postgres -- sh -c 'psql "$DATABASE_URL" -qc "create table note (v text); insert into note values ('\''previous checkout'\'')"'
stack down --json >"$T/down-appR.json"
assert_json '.ok and .data.confirmed' "$T/down-appR.json"
stack up --json >"$T/up-appR2.json"
assert_json '.ok and (.data.warnings == null)' "$T/up-appR2.json"
[[ "$(stack exec --require postgres -- sh -c 'psql "$DATABASE_URL" -Atc "select v from note"')" == 'previous checkout' ]] || fail 'data lost across a restart'
stack down >/dev/null

echo 'A checkout recreated at the same path starts empty; the old data is moved aside, not deleted'
cd "$W" || exit 1
mv appR appR-old   # held, so the new directory cannot reuse its inode
cp -R appR-old appR
rm -rf appR/.stack
cd "$W/appR" || exit 1
stack up --json >"$T/up-recreated.json"
assert_json '.ok and (.data.warnings | length == 2) and all(.data.warnings[]; test("moved to .*replaced-"))' "$T/up-recreated.json"
if stack exec --require postgres -- sh -c 'psql "$DATABASE_URL" -Atc "select v from note"' 2>/dev/null; then fail 'the recreated checkout inherited the old data'; fi
moved=$(jq -r '.data.warnings[] | select(startswith("postgres:")) | capture("moved to (?<p>.*); ").p' "$T/up-recreated.json")
[[ -f "$moved/PG_VERSION" ]] || fail "old Postgres data not kept at $moved"
stack down >/dev/null

echo "A service process of this checkout that nothing recorded is stopped by up, not called foreign"
# The recreated directory has a token of its own.
token=$(stack exec -- python3 -c 'import tomllib; print(tomllib.load(open(".config/mise/conf.d/stack.toml", "rb"))["daemons"]["postgres"]["env"]["STACK_CHECKOUT_TOKEN"])')
stray=$(stack exec -- sh -c "STACK_CHECKOUT_TOKEN=$token exec python3 -m http.server $port --bind 127.0.0.1 >/dev/null 2>&1 & echo \$!")
for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null && break; sleep 0.1; done
stack up --json >"$T/up-stray.json"
jq -e --argjson pid "$stray" '.ok and any(.data.steps[]; .step == "stop_previous" and any(.detail.stopped[]; .pid == $pid and .unrecorded))' "$T/up-stray.json" >/dev/null || fail "stray: $(cat "$T/up-stray.json")"
assert_dead "$stray"
assert_json 'all(.data.checks[]; .ready)' "$T/up-stray.json"
stack down --json >"$T/down-stray.json"
assert_json '.ok and .data.confirmed and (.data.conflicts == null)' "$T/down-stray.json"
# shellcheck disable=SC2046
assert_dead $(recorded_pids "$T/up-stray.json")
cd "$W" && rm -rf appR-old

echo "A service waiting for one declared after it still starts, though each start holds mise's lock until ready"
mkdir -p "$W/appD"
cd "$W/appD" || exit 1
git init -q
cat >stack.toml <<'TOML'
[tools]
python = "3.13"

[services.first]
run = "while [ ! -e second-up ]; do sleep 0.2; done; exec python3 -m http.server $PORT --bind 127.0.0.1"
ready_cmd = "curl -fsS http://127.0.0.1:$PORT/ >/dev/null"

[services.second]
run = "touch second-up && exec python3 -m http.server $PORT --bind 127.0.0.1"
ready_cmd = "curl -fsS http://127.0.0.1:$PORT/ >/dev/null"
TOML
stack compile >/dev/null
stack up --timeout 2m --json >"$T/up-appD.json" || fail "reverse dependency: $(cat "$T/up-appD.json")"
assert_json '.ok and all(.data.checks[]; .ready)' "$T/up-appD.json"
stack down --json >"$T/down-appD.json"
assert_json '.ok and .data.confirmed' "$T/down-appD.json"

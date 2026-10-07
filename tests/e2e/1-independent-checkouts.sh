#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
git config --global user.email a@b
git config --global user.name a
git config --global init.defaultBranch main
for b in pybase obs; do
  cp -r "$EX/bundles/$b" "$SRV/$b"
  cd "$SRV/$b" || exit 1
  git init -q && git add -A && git commit -qm v1 && git tag v1
done
mkapp() {
  cp -r "$EX/app" "$W/$1"
  cd "$W/$1" || exit 1
  rm -f stack.lock
  git init -q
  edit "s|path:../bundles/pybase|git+file://$SRV/pybase?ref=v1|; s|path:../bundles/obs|git+file://$SRV/obs?ref=v1|" stack.toml
  stack compile >/dev/null
  git add -A && git commit -qm init
}
mkapp appA
mkapp appB
echo 'Exact tool and service versions are locked and rendered'
cd "$W/appA" || exit 1
stack compile --locked --json >"$T/versions.json"
assert_json '.ok and all(.data.versions[]; .resolved != null and (.resolved | test("^[0-9]+(\\.[0-9]+)+$")))' "$T/versions.json"
assert_json '[.data.versions[] | select(.kind == "service")] | length == 2' "$T/versions.json"
for v in $(jq -r '.data.versions[] | select(.kind == "service") | .resolved' "$T/versions.json"); do
  grep -q "version = \"$v\"" .config/mise/conf.d/stack.toml || fail "service version $v was not rendered"
done
echo 'Two independent checkouts start with distinct verified instances'
for a in appA appB; do
  cd "$W/$a" || exit 1
  stack up --json > "$T/up-$a.json"
  assert_json '.ok and (.data.checks | length == 2) and all(.data.checks[]; .ready and .identity == "instance" and .port >= 40000 and .port <= 49999)' "$T/up-$a.json"
  stack exec --require-all -- bash -c 'set -euo pipefail; uv sync -q; uv run pytest -q'
  # The declared task, through mise's task runner, without mise starting the daemons again.
  stack run test >"$T/run-$a.out" 2>&1 || { cat "$T/run-$a.out"; fail "stack run test failed in $a"; }
  if grep -q 'already running' "$T/run-$a.out"; then cat "$T/run-$a.out"; fail "mise run restarted daemons in $a"; fi
  actual=$(stack exec --require postgres -- bash -c 'psql "$DATABASE_URL" -Atc "show data_directory"')
  expected=$(jq -r '.data.session.services.postgres.data_dir' "$T/up-$a.json")
  [[ "$actual" == "$expected" ]] || fail "wrong Postgres in $a"
  # The running servers are the locked releases, not whatever the request resolves to today.
  pg=$(stack exec --require postgres -- bash -c 'psql "$DATABASE_URL" -Atc "show server_version"')
  locked=$(jq -r '.data.versions[] | select(.name == "postgres") | .resolved' "$T/versions.json")
  [[ "$pg" == "$locked"* ]] || fail "Postgres $pg is not the locked $locked"
done
jq -se '.[0].data.session.services.postgres.port != .[1].data.session.services.postgres.port and .[0].data.session.services.redis.port != .[1].data.session.services.redis.port and .[0].data.session.services.postgres.data_dir != .[1].data.session.services.postgres.data_dir' "$T/up-appA.json" "$T/up-appB.json" >/dev/null

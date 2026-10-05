#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source /tmp/stack-e2e-assert.sh
export PATH=/opt/stack:$PATH
git config --global user.email a@b
git config --global user.name a
git config --global init.defaultBranch main
for b in pybase obs; do
  cp -r /examples/bundles/$b /srv/$b
  cd /srv/$b || exit 1
  git init -q && git add -A && git commit -qm v1 && git tag v1
done
mkapp() {
  cp -r /examples/app ~/"$1"
  cd ~/"$1" || exit 1
  rm -f stack.lock
  git init -q
  sed -i 's|path:../bundles/pybase|git+file:///srv/pybase?ref=v1|; s|path:../bundles/obs|git+file:///srv/obs?ref=v1|' stack.toml
  stack compile >/dev/null
  git add -A && git commit -qm init
}
mkapp appA
mkapp appB
echo 'Two independent checkouts start with distinct verified instances'
for a in appA appB; do
  cd ~/"$a" || exit 1
  stack up --json > /tmp/up-$a.json
  assert_json '.ok and (.data.checks | length == 2) and all(.data.checks[]; .ready and .identity == "instance" and .port >= 40000 and .port <= 49999)' /tmp/up-$a.json
  stack exec --require-all -- bash -c 'set -euo pipefail; uv sync -q; uv run pytest -q'
  actual=$(stack exec --require postgres -- bash -c 'psql "$DATABASE_URL" -Atc "show data_directory"')
  expected=$(jq -r '.data.session.services.postgres.data_dir' /tmp/up-$a.json)
  [[ "$actual" == "$expected" ]] || fail "wrong Postgres in $a"
done
jq -se '.[0].data.session.services.postgres.port != .[1].data.session.services.postgres.port and .[0].data.session.services.redis.port != .[1].data.session.services.redis.port and .[0].data.session.services.postgres.data_dir != .[1].data.session.services.postgres.data_dir' /tmp/up-appA.json /tmp/up-appB.json >/dev/null

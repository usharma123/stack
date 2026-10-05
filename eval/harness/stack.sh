#!/usr/bin/env bash
# Run inside a fresh ev-mise container with the published binary in /opt/stack.
# Mount eval at /eval, examples at /examples and a writable result dir at /results.
set -Eeuo pipefail
export PATH=/opt/stack:$PATH
TOOL=stack
source /eval/harness/lib.sh
source /tmp/stack-e2e-assert.sh
mkdir -p /srv/bench
cp -r /examples/bundles /srv/bench/bundles
mkapp() {
  cp -r /eval/fixture "/srv/bench/$1"
  cp /examples/app/stack.toml "/srv/bench/$1/stack.toml"
}
mkapp app1
cd /srv/bench/app1
step compile_cold 'stack compile --json'
step setup_cold 'stack up --json'
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' /results/logs/stack.setup_cold.log
step tests 'stack exec --require-all -- bash -c "set -e; uv sync -q; uv run pytest -q"'
step bundle_files 'stack exec --require-all -- bash -c "set -e; mise run seed; acme"'
step svc_start_again 'stack up --json'
step svc_status_json 'stack status --json'
for i in $(seq 1 10); do
  step "exec_verified_$i" 'stack exec --require-all -- true'
done
mkapp app2
cd /srv/bench/app2
step compile_warm 'stack compile --json'
step setup_warm 'stack up --json'
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' /results/logs/stack.setup_warm.log
step second_tests 'stack exec --require-all -- bash -c "set -e; uv sync -q; uv run pytest -q"'
jq -se '.[0].data.session.services.postgres.port != .[1].data.session.services.postgres.port and .[0].data.session.services.redis.port != .[1].data.session.services.redis.port and .[0].data.session.services.postgres.data_dir != .[1].data.session.services.postgres.data_dir' /results/logs/stack.setup_cold.log /results/logs/stack.setup_warm.log >/dev/null
step independent_stop 'stack -C /srv/bench/app1 down --json && stack exec --require-all -- bash -c "uv run pytest -q"'
step svc_stop 'stack down --json'
assert_json '.ok and .data.confirmed' /results/logs/stack.svc_stop.log
step leftovers 'n=$(ps -eo stat,comm --no-headers | awk '\''$1 !~ /Z/ && $2 ~ /^(postgres|redis-server)$/ '\'' | wc -l); echo count=$n; test "$n" -eq 0'
for i in $(seq 1 10); do
  step "exec_unverified_$i" 'stack exec -- true'
done
mkapp conflict
sed -i '/\[override.env\]/,$d' /srv/bench/conflict/stack.toml
step conflict_rejected 'if stack -C /srv/bench/conflict compile --json; then exit 1; fi'
assert_json '.ok == false and .error.code == "conflict"' /results/logs/stack.conflict_rejected.log
mkdir -p /srv/bench/bad
printf '[tools]\nnode = "99.0.0"\n' >/srv/bench/bad/stack.toml
stack -C /srv/bench/bad compile >/dev/null
step bad_version 'if stack -C /srv/bench/bad up --json; then exit 1; fi'
assert_json '.ok == false and .error.code == "install_failed"' /results/logs/stack.bad_version.log
printf '[tools]\ndefinitely-not-a-pkg-zz = "latest"\n' >/srv/bench/bad/stack.toml
stack -C /srv/bench/bad compile >/dev/null
step bad_pkg 'if stack -C /srv/bench/bad up --json; then exit 1; fi'
assert_json '.ok == false and .error.code == "install_failed"' /results/logs/stack.bad_pkg.log
step provider_versions 'mise --version; mise exec -- pitchfork --version; stack --version; mise ls --json'

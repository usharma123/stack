#!/bin/bash
TOOL=flox; source /harness/lib.sh; source /harness/lib_procs.sh
export FLOX_DISABLE_METRICS=true
X() { echo "flox activate -d $1 -- bash -c '$2'"; }
mkenv() { cp -r /fixture "$1" && (cd "$1" && flox init --no-auto-setup >/dev/null 2>&1 && cp /configs/flox/manifest.toml .flox/env/manifest.toml); }
mkenv ~/proj && cd ~/proj
step setup_cold "flox activate -- true"
step exec_python "$(X ~/proj 'python --version; command -v python')"
step inspect_json "flox list --json"
step svc_start_detached "flox services start"
# Workaround an agent must discover: hold an activation open in the background
step svc_start_bg "nohup flox activate --start-services -- sleep infinity > /tmp/flox-hold.log 2>&1 & echo \$! > /tmp/flox-hold.pid"
wait_ready svc_ready_pg "$(X ~/proj 'pg_isready -q -h 127.0.0.1 -p 5432')" 90
wait_ready svc_ready_redis "$(X ~/proj 'redis-cli -p 6379 ping | grep -q PONG')" 30
step svc_status_json "flox services status --json"
step tests "$(X ~/proj 'uv sync -q && uv run pytest -q 2>&1 | tail -5')"
step svc_start_again "flox activate --start-services -- flox services status --json"
mkenv ~/proj2
step conflict_start "cd ~/proj2 && (nohup flox activate --start-services -- sleep infinity > /tmp/flox-hold2.log 2>&1 &) ; sleep 8; flox services status --json; flox activate -- flox services logs postgres 2>&1 | tail -5"
step bad_pkg "flox install definitely-not-a-pkg-zz"
step bad_version "flox install nodejs@99.0.0"
step interrupt "timeout -s KILL 4 flox install go"
step after_interrupt "flox list -n; $(X ~/proj 'go version')"
step retry_install "flox install go && $(X ~/proj 'go version')"
step procs_before_kill "source /harness/lib_procs.sh; svc_procs"
step kill_holder "kill -9 \$(cat /tmp/flox-hold.pid); sleep 5; source /harness/lib_procs.sh; svc_procs"
step status_after_kill "flox services status --json; $(X ~/proj 'pg_isready -h 127.0.0.1 -p 5432')"
step svc_stop "for d in ~/proj ~/proj2; do (cd \$d && flox activate -- flox services stop); done; sleep 3; source /harness/lib_procs.sh; svc_procs"
step leftovers "source /harness/lib_procs.sh; n=\$(svc_count); echo count=\$n; [ \$n -eq 0 ]"
mkenv ~/proj3
step setup_warm "cd ~/proj3 && flox activate -- python --version"

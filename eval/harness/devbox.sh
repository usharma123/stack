#!/bin/bash
TOOL=devbox; source /harness/lib.sh; source /harness/lib_procs.sh
X() { echo "devbox run -c $1 -- bash -c '$2'"; }
mkenv() { cp -r /fixture "$1" && cp /configs/devbox/devbox.json "$1/"; }
mkenv ~/proj && cd ~/proj
step setup_cold "devbox install"
step exec_python "$(X ~/proj 'python --version; command -v python')"
step inspect_json "devbox list --json"
step svc_start_bg "devbox services up -b"
wait_ready svc_ready_pg "$(X ~/proj 'pg_isready -q -h 127.0.0.1 -p 5432')" 90
wait_ready svc_ready_redis "$(X ~/proj 'redis-cli -p 6379 ping | grep -q PONG')" 30
step svc_status_json "devbox services ls --json || devbox services ls"
step tests "$(X ~/proj 'uv sync -q && uv run pytest -q 2>&1 | tail -5')"
step svc_start_again "devbox services up -b; devbox services ls"
mkenv ~/proj2
step conflict_start "cd ~/proj2 && devbox install -q && devbox services up -b; sleep 8; devbox services ls"
step bad_pkg "devbox add definitely-not-a-pkg-zz"
step bad_version "devbox add nodejs@99.0.0"
step interrupt "timeout -s KILL 4 devbox add go"
step after_interrupt "cat devbox.json | jq -c .packages; $(X ~/proj 'go version')"
step retry_install "devbox add go && $(X ~/proj 'go version')"
step procs_before_kill "source /harness/lib_procs.sh; svc_procs"
step kill_holder "pkill -9 -x process-compose; sleep 5; source /harness/lib_procs.sh; svc_procs"
step status_after_kill "devbox services ls; $(X ~/proj 'pg_isready -h 127.0.0.1 -p 5432')"
step svc_stop "for d in ~/proj ~/proj2; do (cd \$d && devbox services stop); done; sleep 3; source /harness/lib_procs.sh; svc_procs"
step leftovers "source /harness/lib_procs.sh; n=\$(svc_count); echo count=\$n; [ \$n -eq 0 ]"
mkenv ~/proj3
step setup_warm "cd ~/proj3 && devbox run -- python --version"

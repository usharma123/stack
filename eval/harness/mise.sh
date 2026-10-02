#!/bin/bash
TOOL=mise; source /harness/lib.sh; source /harness/lib_procs.sh
X() { echo "cd $1 && mise exec -- bash -c '$2'"; }
mkenv() { cp -r /fixture "$1" && cp /configs/mise/mise.toml "$1/"; }
mkenv ~/proj && cd ~/proj
step untrusted_install "mise install"
step setup_cold "mise trust && mise install -y"
step exec_python "$(X ~/proj 'python --version; command -v python')"
step inspect_json "mise ls --json --current | jq -c 'to_entries|map({k:.key,v:.value[0].version})'"
step svc_start_bg "mise run up"
wait_ready svc_ready_pg "$(X ~/proj 'pg_isready -q -d \"\$DATABASE_URL\"')" 90
wait_ready svc_ready_redis "$(X ~/proj 'redis-cli -u \"\$REDIS_URL\" ping | grep -q PONG')" 30
step svc_status_json "echo 'no service status command'; exit 1"
step tests "$(X ~/proj 'uv sync -q && uv run pytest -q 2>&1 | tail -5')"
step svc_start_again "mise run up"
mkenv ~/proj2
step conflict_start "cd ~/proj2 && mise trust -q && mise run up"
step bad_pkg "mise use definitely-not-a-pkg-zz"
step bad_version "mise use node@99.0.0"
step interrupt "timeout -s KILL 4 mise use go@latest"
step after_interrupt "grep -E '^go' mise.toml; $(X ~/proj 'go version')"
step retry_install "mise use go@latest && $(X ~/proj 'go version')"
step procs_before_kill "source /harness/lib_procs.sh; svc_procs"
step svc_stop "for d in ~/proj ~/proj2; do (cd \$d && mise run down); done; sleep 2; source /harness/lib_procs.sh; svc_procs"
step leftovers "n=\$(ps -eo stat,comm --no-headers | awk '\$1 !~ /Z/ && \$2 ~ /^(postgres|redis-server)\$/' | wc -l); echo count=\$n; [ \$n -eq 0 ]"
mkenv ~/proj3
step setup_warm "cd ~/proj3 && mise trust -q && mise install -y && mise exec -- python --version"

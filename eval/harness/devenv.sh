#!/bin/bash
TOOL=devenv; source /harness/lib.sh; source /harness/lib_procs.sh
X() { echo "cd $1 && devenv shell --no-tui -- bash -c '$2'"; }
mkenv() { cp -r /fixture "$1" && (cd "$1" && devenv init > /tmp/devenv-init.log 2>&1; cp /configs/devenv/devenv.nix .); }
mkenv ~/proj && cd ~/proj
step setup_cold "devenv shell --no-tui -- true"
step exec_python "$(X ~/proj 'python --version; command -v python')"
step inspect_json "devenv info; devenv eval services.postgres.port packages"
step svc_start_bg "devenv up -d --no-tui"
step native_wait "devenv processes wait --timeout 90"
wait_ready svc_ready_pg "$(X ~/proj 'pg_isready -q -d \"\$DATABASE_URL\"')" 90
wait_ready svc_ready_redis "$(X ~/proj 'redis-cli -u \"\$REDIS_URL\" ping | grep -q PONG')" 30
step svc_status_json "devenv processes list"
step tests "$(X ~/proj 'uv sync -q && uv run pytest -q 2>&1 | tail -5')"
step svc_start_again "devenv up -d --no-tui; devenv processes list"
mkenv ~/proj2
step conflict_start "cd ~/proj2 && devenv up -d --no-tui && devenv processes wait --timeout 90; devenv processes list"
step conflict_env "$(X ~/proj2 'echo DATABASE_URL=\$DATABASE_URL REDIS_URL=\$REDIS_URL PGPORT=\$PGPORT')"
step conflict_tests "$(X ~/proj2 'uv sync -q && uv run pytest -q 2>&1 | tail -5')"
sed -i 's/packages = \[ \];/packages = [ pkgs.definitely-not-a-pkg-zz ];/' devenv.nix
step bad_pkg "devenv shell --no-tui -- true"
cp /configs/devenv/devenv.nix .
step bad_version "devenv shell --no-tui -O languages.python.version:string 3.99 -- python --version"
sed -i 's/packages = \[ \];/packages = [ pkgs.go ];/' devenv.nix
step interrupt "timeout -s KILL 4 devenv shell --no-tui -- go version"
step after_interrupt "$(X ~/proj 'go version')"
step procs_before_kill "source /harness/lib_procs.sh; svc_procs"
step kill_holder "pp=\$(ps -o ppid= -p \$(ps -eo pid,comm,args --no-headers | awk '\$2==\"postgres\" && \$0 ~ /proj\\// && \$0 !~ /proj2/ {print \$1; exit}')); echo supervisor=\$pp \$(ps -o comm= -p \$pp); kill -9 \$pp; sleep 5; source /harness/lib_procs.sh; svc_procs"
step status_after_kill "devenv processes list; $(X ~/proj 'pg_isready -d \"\$DATABASE_URL\"')"
step svc_stop "for d in ~/proj ~/proj2; do (cd \$d && devenv down); done; sleep 3; source /harness/lib_procs.sh; svc_procs"
step leftovers "source /harness/lib_procs.sh; n=\$(svc_count); echo count=\$n; [ \$n -eq 0 ]"
mkenv ~/proj3
step setup_warm "cd ~/proj3 && devenv shell --no-tui -- python --version"

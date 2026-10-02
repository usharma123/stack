# Process helpers that never match the harness's own shell
svc_procs() { ps -eo pid,ppid,etimes,comm,args --no-headers | awk '$4 ~ /^(postgres|redis-server|process-compose|devenv|devbox|flox-watchdog|mise)$/'; }
svc_count() { ps -eo comm --no-headers | grep -cE '^(postgres|redis-server)$'; }

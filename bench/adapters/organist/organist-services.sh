#!/usr/bin/env bash
# Benchmark-owned (scripted) foreground service launcher, run by Organist's generated
# Procfile under Honcho. Organist declares the commands; data layout, first-run initdb,
# ports and the Redis durability policy are project scripting. Processes stay in the
# foreground (Honcho supervises them and stops siblings when one exits).
#
#   organist-services.sh postgres|redis <bin dir>
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=/dev/null
source ./rwb-env.sh
bin=${2:?usage: organist-services.sh postgres|redis <bin dir>}
case "${1:?}" in
  postgres)
    mkdir -p "$PGDATA"
    if [ ! -s "$PGDATA/PG_VERSION" ]; then
      "$bin/initdb" -D "$PGDATA" -U postgres --auth=trust --encoding=UTF8 --locale=C >/dev/null
    fi
    exec "$bin/postgres" -D "$PGDATA" -h 127.0.0.1 -p "$PGPORT" \
      -c unix_socket_directories= -c cluster_name="$RWB_INSTANCE"
    ;;
  redis)
    mkdir -p "$REDIS_DATA"
    exec "$bin/redis-server" --bind 127.0.0.1 --port "$REDIS_PORT" --dir "$REDIS_DATA" \
      --appendonly yes --appendfsync always
    ;;
  *) echo "unknown service $1" >&2; exit 2 ;;
esac

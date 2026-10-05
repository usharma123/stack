#!/usr/bin/env bash
# Keep variable expansion inside the activated environment. In particular, do not
# pass a command-substitution loop through Devbox's command-string reconstruction.
for i in $(seq 1 100); do
  if pg_isready -q -d "$DATABASE_URL" && redis-cli -u "$REDIS_URL" ping | grep -q PONG; then
    exit 0
  fi
  sleep .2
done
exit 1

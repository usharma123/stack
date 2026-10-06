#!/bin/sh
# Benchmark glue (scripted): derive the Compose/app environment from the block workz 0.11.0
# manages in .env.local. workz supplies PORT, DB_NAME, COMPOSE_PROJECT_NAME and REDIS_URL
# (on PORT+1). Its DATABASE_URL (postgres://localhost/<db>) carries no port or credentials,
# so the PostgreSQL URL is composed here from workz's PORT and DB_NAME.
# Writes .env, which Docker Compose also reads natively for interpolation.
set -eu
cd "$(dirname "$0")"
test -f .env.local || { echo ".env.local missing: run workz start/sync --isolated first" >&2; exit 2; }
get() {
  value=$(sed -n "s/^$1=//p" .env.local)
  case "$value" in ''|*'
'*) echo "workz did not write exactly one $1 in .env.local" >&2; exit 2;; esac
  printf '%s' "$value"
}
port=$(get PORT)
db=$(get DB_NAME)
project=$(get COMPOSE_PROJECT_NAME)
redis_url=$(get REDIS_URL)
redis_port=${redis_url##*:}
redis_port=${redis_port%%/*}
test "$redis_port" -eq $((port + 1)) || { echo "workz REDIS_URL $redis_url is not on PORT+1" >&2; exit 2; }
cat > .env <<ENV
COMPOSE_PROJECT_NAME=$project
PORT=$port
REDIS_PORT=$redis_port
DB_NAME=$db
DATABASE_URL=postgresql://bench:bench@127.0.0.1:$port/$db
REDIS_URL=$redis_url
ENV

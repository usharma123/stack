#!/usr/bin/env bash
# usage: pg_encoding.sh <isolated root with stack installed under cli/> <app copy dir> [LC_CTYPE value or empty]
# Runs compile/up/query/down for fixtures/pg-encoding in a minimal env (as CI would), printing server_encoding.
root=$1 app=$2 lc=${3-}
nodebin=$(dirname "$(command -v node)")
env -i HOME="$HOME" USER="$USER" TMPDIR="${TMPDIR:-/tmp}" ${lc:+LC_CTYPE=$lc} ISO="$root" \
  PATH="$root/cli/node_modules/.bin:$nodebin:/usr/bin:/bin:/usr/sbin:/sbin" bash --noprofile --norc -c '
export STACK_STATE_DIR=$ISO/stack/state STACK_DATA_DIR=$ISO/stack/data STACK_CACHE_DIR=$ISO/stack/cache
export MISE_DATA_DIR=$ISO/mise/data MISE_CACHE_DIR=$ISO/mise/cache MISE_STATE_DIR=$ISO/mise/state MISE_CONFIG_DIR=$ISO/mise/config MISE_GLOBAL_CONFIG_FILE=$ISO/mise/config/config.toml
export PITCHFORK_CONFIG_DIR=$ISO/pf/config PITCHFORK_STATE_DIR=$ISO/pf/state PITCHFORK_LOGS_DIR=$ISO/pf/logs
export XDG_CONFIG_HOME=$ISO/xdg/config XDG_DATA_HOME=$ISO/xdg/data XDG_CACHE_HOME=$ISO/xdg/cache XDG_STATE_HOME=$ISO/xdg/state
cd "'"$app"'" && stack compile >/dev/null && stack up --ttl 10m >/dev/null || { echo "up failed"; exit 2; }
echo "LC_CTYPE=${LC_CTYPE-unset} server_encoding=$(stack exec --require postgres -- psql "$DATABASE_URL" -Atc "show server_encoding") lc_ctype=$(stack exec --require postgres -- psql "$DATABASE_URL" -Atc "select datctype from pg_database where datname = current_database()")"
stack down >/dev/null
stack exec -- pitchfork supervisor stop >/dev/null 2>&1
'

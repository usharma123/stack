#!/usr/bin/env bash
# Hosted/CI-style cold bootstrap: fresh isolated root, minimal PATH (no mise, no Homebrew, no Docker),
# install a Stack package, clone an app repo, then setup -> doctor -> compile --locked -> up -> run test
# -> down -> supervisor stop -> gc. Every step is noninteractive (stdin=/dev/null) and bounded.
# usage: hosted_bootstrap.sh <stack package spec or .tgz> <git repo to clone> <results dir>
# Prints the isolated root first. Keeps it (for evidence) and prints a verdict line per step.
set -u
pkg=$1 repo=$2 out=$3
mkdir -p "$out"
nodebin=$(dirname "$(command -v node)")
iso=$(mktemp -d /tmp/sah.XXXX) || exit 1
echo "isolated root: $iso"
echo "$iso" >"$out/root"
env -i HOME="$HOME" USER="$USER" LOGNAME="${LOGNAME:-$USER}" TMPDIR="${TMPDIR:-/tmp}" CI=1 \
  PATH="$nodebin:/usr/bin:/bin:/usr/sbin:/sbin" ISO="$iso" PKG="$pkg" REPO="$repo" OUT="$out" \
  bash --noprofile --norc -c '
set -u
export STACK_STATE_DIR=$ISO/stack/state STACK_DATA_DIR=$ISO/stack/data STACK_CACHE_DIR=$ISO/stack/cache
export MISE_DATA_DIR=$ISO/mise/data MISE_CACHE_DIR=$ISO/mise/cache MISE_STATE_DIR=$ISO/mise/state \
  MISE_CONFIG_DIR=$ISO/mise/config MISE_GLOBAL_CONFIG_FILE=$ISO/mise/config/config.toml
export PITCHFORK_CONFIG_DIR=$ISO/pf/config PITCHFORK_STATE_DIR=$ISO/pf/state PITCHFORK_LOGS_DIR=$ISO/pf/logs
export XDG_CONFIG_HOME=$ISO/xdg/config XDG_DATA_HOME=$ISO/xdg/data XDG_CACHE_HOME=$ISO/xdg/cache \
  XDG_STATE_HOME=$ISO/xdg/state UV_CACHE_DIR=$ISO/uv-cache
export npm_config_cache=$ISO/npm/cache npm_config_userconfig=$ISO/npm/npmrc \
  npm_config_globalconfig=$ISO/npm/global-npmrc npm_config_prefix=$ISO/npm/prefix
export GIT_TERMINAL_PROMPT=0
n=0
step() {  # step <label> <timeout secs> <cmd...>
  n=$((n+1)); local label=$1 t=$2; shift 2
  local base=$(printf "%s/%02d-%s" "$OUT" $n "$label")
  local s=$(date +%s)
  perl -e '"'"'setpgrp(0,0); $SIG{ALRM}=sub{kill "KILL", -$$; exit 137}; alarm shift; $p=fork; if(!$p){exec @ARGV} waitpid($p,0); exit(($?&127) ? ($?&127)+128 : $?>>8)'"'"' "$t" "$@" >"$base.out" 2>"$base.err" </dev/null
  local rc=$?
  echo "step $n $label exit=$rc secs=$(( $(date +%s) - s ))" | tee "$base.meta"
  return $rc
}
command -v mise && echo "WARNING: mise on PATH"
step npm-install 180 npm install --prefix "$ISO/cli" --save-exact "$PKG" || exit 1
export PATH="$ISO/cli/node_modules/.bin:$PATH"
step version 10 stack --version
step setup 300 stack --json setup || exit 1
step clone 60 git clone -q "$REPO" "$ISO/app" || exit 1
cd "$ISO/app"
step doctor 60 stack --json doctor
step compile-locked 120 stack --json compile --locked || exit 1
step up 900 stack --json up --ttl 20m --timeout 14m; up=$?
[ $up = 0 ] && step run-test 300 stack --json run test --timeout 4m
step status 30 stack --json status
step down 120 stack --json down
step supervisor-stop 60 stack exec -- pitchfork supervisor stop
step supervisor-status 30 stack exec -- pitchfork supervisor status --json
step gc 60 stack --json gc
step git-status 10 git status --porcelain --ignored
'
echo "done; evidence in $out"

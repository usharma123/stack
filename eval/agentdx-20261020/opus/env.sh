# Source me: isolates every Stack/mise/Pitchfork/XDG/uv/npm store under $ISO, keeping HOME.
# usage: ISO=/tmp/sao-xxxx . eval/agentdx-20261020/opus/env.sh
# Mirrors docs/user/worktrees.md "Isolated runs" and examples/isolated-stack.sh.
: "${ISO:?set ISO to a short disposable root (macOS socket path limit 104 bytes)}"
for v in $(env | sed -n "s/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p"); do
  case $v in MISE_*|__MISE*|PITCHFORK_*|STACK_*|npm_config_*|NPM_CONFIG_*) unset "$v" ;; esac
done
export STACK_STATE_DIR=$ISO/stack/state STACK_DATA_DIR=$ISO/stack/data STACK_CACHE_DIR=$ISO/stack/cache
export MISE_DATA_DIR=$ISO/mise/data MISE_CACHE_DIR=$ISO/mise/cache MISE_STATE_DIR=$ISO/mise/state \
  MISE_CONFIG_DIR=$ISO/mise/config MISE_GLOBAL_CONFIG_FILE=$ISO/mise/config/config.toml
export PITCHFORK_CONFIG_DIR=$ISO/pf/config PITCHFORK_STATE_DIR=$ISO/pf/state PITCHFORK_LOGS_DIR=$ISO/pf/logs
export XDG_CONFIG_HOME=$ISO/xdg/config XDG_DATA_HOME=$ISO/xdg/data XDG_CACHE_HOME=$ISO/xdg/cache \
  XDG_STATE_HOME=$ISO/xdg/state UV_CACHE_DIR=$ISO/uv-cache
export npm_config_cache=$ISO/npm/cache npm_config_userconfig=$ISO/npm/npmrc \
  npm_config_globalconfig=$ISO/npm/global-npmrc npm_config_prefix=$ISO/npm/prefix
export PATH="$ISO/cli/node_modules/.bin:$PATH"

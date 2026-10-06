# Benchmark-owned endpoint propagation for tools without native service wiring (scripted).
# Source from the checkout root inside the tool's environment.
_rwb_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
source "$_rwb_root/bench.local.env"
export PGPORT REDIS_PORT RWB_INSTANCE
export PGDATA="$_rwb_root/.rwb-state/postgres"
export REDIS_DATA="$_rwb_root/.rwb-state/redis"
export DATABASE_URL="postgresql://postgres@127.0.0.1:$PGPORT/postgres"
export REDIS_URL="redis://127.0.0.1:$REDIS_PORT/0"
export UV_PYTHON_DOWNLOADS=never
unset _rwb_root

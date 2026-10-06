#!/usr/bin/env bash
# Benchmark glue (scripted): run $RWB_BODY inside `pixi run` with the checkout's endpoint
# variables from rwb-env.sh. Pixi's task shell is not bash, so the body is handed over here.
set -eo pipefail
cd "$(dirname "$0")"
# shellcheck source=/dev/null
source ./rwb-env.sh
exec bash -c "${RWB_BODY:?RWB_BODY is not set}"

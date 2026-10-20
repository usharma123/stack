#!/usr/bin/env bash
# GitGrove custom-shell stop script (benchmark glue, scripted). Stops containers and keeps
# the project's named volumes: data survives; destroying it is a separate cleanup step.
set -euo pipefail
: "${COMPOSE_PROJECT_NAME:?}" "${GROVE_ENV_FILE:?}"
docker compose -p "$COMPOSE_PROJECT_NAME" --env-file "$GROVE_ENV_FILE" -f compose.yaml \
  stop --timeout 30 >&2

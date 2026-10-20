#!/usr/bin/env bash
# GitGrove custom-shell start script (benchmark glue, scripted). GitGrove runs it with
# cwd = the worktree, .env.worktree values in the environment and GROVE_ENV_FILE set.
# stdout belongs to `grove start --json`, so Compose output goes to stderr.
set -euo pipefail
: "${COMPOSE_PROJECT_NAME:?}" "${GROVE_ENV_FILE:?}"
docker compose -p "$COMPOSE_PROJECT_NAME" --env-file "$GROVE_ENV_FILE" -f compose.yaml \
  up -d --wait --wait-timeout 120 >&2

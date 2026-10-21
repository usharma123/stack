#!/bin/sh
# Project responsibility in Devbox: initialize the plugin's PGDATA once (scripted).
set -eu
: "${PGDATA:?}"
mkdir -p "$PGDATA"
if [ ! -f "$PGDATA/PG_VERSION" ]; then
  initdb -D "$PGDATA" -U postgres --auth=trust --encoding=UTF8 --locale=C
fi

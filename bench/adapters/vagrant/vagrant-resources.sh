#!/bin/bash
# Benchmark-owned (scripted) Docker glue for the Vagrant Docker provider recipe: the
# per-checkout network and named data volumes that Vagrant's provider does not create.
# Every resource is named "$RWB_INSTANCE-..." and labelled with this run. bash 3.2 safe.
#
#   vagrant-resources.sh create|remove|identity    (after sourcing vagrant.local.env)
set -euo pipefail
: "${RWB_INSTANCE:?}" "${RWB_RUN:?}"
case "$RWB_INSTANCE" in rwb-*) ;; *) echo "refusing instance $RWB_INSTANCE" >&2; exit 2 ;; esac
net="$RWB_INSTANCE-net"
volumes="$RWB_INSTANCE-pgdata $RWB_INSTANCE-redisdata"
labels="--label rwb.run=$RWB_RUN --label rwb.instance=$RWB_INSTANCE"

owned() {  # $1 = network|volume, $2 = name: exists AND carries this run's label
  [ "$(docker "$1" inspect -f '{{index .Labels "rwb.run"}}' "$2" 2>/dev/null)" = "$RWB_RUN" ]
}

case "${1:?usage: create|remove|identity}" in
  create)
    owned network "$net" || docker network create $labels "$net" >/dev/null
    for v in $volumes; do owned volume "$v" || docker volume create $labels "$v" >/dev/null; done
    ;;
  remove)
    if owned network "$net"; then docker network rm "$net" >/dev/null; fi
    for v in $volumes; do if owned volume "$v"; then docker volume rm "$v" >/dev/null; fi; done
    ;;
  identity)
    # Each Docker query is a separate guarded assignment (a failing substitution inside printf
    # would still exit 0); empty results fail; the receipt is printed only after all pass.
    pg=$(docker inspect -f '{{.Id}}' "$RWB_INSTANCE-pg") || exit 1
    redis=$(docker inspect -f '{{.Id}}' "$RWB_INSTANCE-redis") || exit 1
    netid=$(docker network inspect -f '{{.Id}}' "$net") || exit 1
    names=""
    for v in $volumes; do
      name=$(docker volume inspect -f '{{.Name}}' "$v") || exit 1
      [ -n "$name" ] || { echo "identity: empty volume name for $v" >&2; exit 1; }
      names="$names$name "
    done
    for x in "$pg" "$redis" "$netid"; do [ -n "$x" ] || { echo "identity: empty Docker result" >&2; exit 1; }; done
    printf '{"instance":"%s","pg":"%s","redis":"%s","network":"%s","volumes":"%s"}\n' \
      "$RWB_INSTANCE" "$pg" "$redis" "$netid" "$names"
    ;;
  *) echo "unknown action $1" >&2; exit 2 ;;
esac

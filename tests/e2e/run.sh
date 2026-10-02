#!/usr/bin/env bash
# End-to-end tests against real mise + Pitchfork and a local OCI registry, in Docker.
# Needs the eval images (eval/images: ev-base, ev-mise) and Docker. Run from the repo root.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
docker run --rm -v "$root":/src -v stack-cargo-registry:/usr/local/cargo/registry -w /src \
  -e CARGO_TARGET_DIR=/src/target/linux rust:1-bookworm cargo build --release
docker rm -f stack-e2e stack-e2e-reg >/dev/null 2>&1 || true
docker run -d --init --name stack-e2e -v "$root/examples":/examples:ro \
  -v "$root/target/linux/release":/opt/stack:ro ev-mise >/dev/null
docker run -d --name stack-e2e-reg --network container:stack-e2e registry:2 >/dev/null
trap 'docker rm -f stack-e2e stack-e2e-reg >/dev/null' EXIT
docker exec stack-e2e bash -c 'mkdir -p /srv && chown agent /srv'
for t in "$root"/tests/e2e/[0-9]-*.sh; do
  echo "=== $(basename "$t")"
  docker cp "$t" stack-e2e:/tmp/t.sh
  docker exec -u agent stack-e2e bash /tmp/t.sh
done

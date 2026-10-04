#!/usr/bin/env bash
# Assert OCI publish, locked replay, moved-tag updates, and validation failures.
set -euo pipefail
binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
work=$(mktemp -d)
registry="stack-ci-oci-$$"
cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then docker logs "$registry" >&2 || true; fi
  docker rm -f "$registry" >/dev/null 2>&1 || true
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
export STACK_STATE_DIR="$work/state" XDG_CACHE_HOME="$work/cache"
docker run -d --name "$registry" -p 127.0.0.1::5000 "${STACK_TEST_REGISTRY_IMAGE:-registry:2.8.3}" >/dev/null
port=$(docker port "$registry" 5000/tcp | cut -d: -f2)
for _ in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:$port/v2/" >/dev/null; then break; fi
  sleep 1
done
curl -fsS "http://127.0.0.1:$port/v2/" >/dev/null
mkdir "$work/bundle" "$work/project"
printf '[bundle]\nname = "ci-test"\nversion = "1.0.0"\n[env]\nCI_VALUE = "first"\n' > "$work/bundle/bundle.toml"
ref="oci:localhost:$port/ci/bundle:1.0.0"
"$binary" publish "$work/bundle" "$ref" --json > "$work/publish.json"
jq -e '.ok == true and (.data.digest | startswith("sha256:"))' "$work/publish.json" >/dev/null
printf '[[use]]\nbundle = "%s"\n' "$ref" > "$work/project/stack.toml"
"$binary" -C "$work/project" compile --json > "$work/first.json"
jq -e '.ok == true' "$work/first.json" >/dev/null
cp "$work/project/stack.lock" "$work/original.lock"
"$binary" -C "$work/project" compile --locked --json | jq -e '.ok == true' >/dev/null
cmp "$work/original.lock" "$work/project/stack.lock"
sed 's/first/second/' "$work/bundle/bundle.toml" > "$work/bundle/new.toml"
mv "$work/bundle/new.toml" "$work/bundle/bundle.toml"
"$binary" publish "$work/bundle" "$ref" --json | jq -e '.ok == true' >/dev/null
"$binary" -C "$work/project" compile --locked --json | jq -e '.ok == true' >/dev/null
cmp "$work/original.lock" "$work/project/stack.lock"
"$binary" -C "$work/project" compile --update --json | jq -e '.ok == true' >/dev/null
if cmp -s "$work/original.lock" "$work/project/stack.lock"; then
  echo 'Update did not change the pinned OCI digest' >&2
  exit 1
fi
printf '\n[services.redis]\npreset = "redis"\nversion = "8"\nport = 6379\n' >> "$work/bundle/bundle.toml"
if "$binary" publish "$work/bundle" "oci:localhost:$port/ci/bundle:bad" --json > "$work/invalid.json"; then
  echo 'Invalid bundle unexpectedly published' >&2
  exit 1
fi
jq -e '.ok == false and .error.code == "bundle_fixed_port"' "$work/invalid.json" >/dev/null
status=$(curl -sS -o /dev/null -w '%{http_code}' "http://localhost:$port/v2/ci/bundle/manifests/bad")
test "$status" = 404
echo 'OCI publish, immutable replay, updates, and rejected upload passed'

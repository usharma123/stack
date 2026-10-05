#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source "${STACK_E2E_ASSERT:-/tmp/stack-e2e-assert.sh}"
REG=${STACK_E2E_REGISTRY:-localhost:5000}
stack publish "$SRV/pybase" "oci:$REG/acme/pybase:1.0.0" --json >"$T/publish.json"
assert_json '.ok and .data.name == "pybase" and (.data.digest | test("^sha256:[0-9a-f]{64}$"))' "$T/publish.json"
first=$(jq -r '.data.digest' "$T/publish.json")
cp -r "$EX/app" "$W/appC"
cd "$W/appC" || exit 1
rm -f stack.lock
edit "s|path:../bundles/pybase|oci:$REG/acme/pybase:1.0.0|; s|path:../bundles/obs|git+file://$SRV/obs?ref=v1|" stack.toml
stack compile --json >"$T/oci-compile.json"
assert_json '.ok and (.data.bundles | length == 2)' "$T/oci-compile.json"
stack -C "$W/appA" inspect --json >"$T/git-inspect.json"
jq -se '(.[0].data.bundles | map(select(.name == "pybase")) | .[0].content_hash) == (.[1].data.bundles | map(select(.name == "pybase")) | .[0].content_hash)' "$T/oci-compile.json" "$T/git-inspect.json" >/dev/null
stack up --json >"$T/oci-up.json"
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' "$T/oci-up.json"
stack exec --require-all -- bash -c 'set -euo pipefail; uv sync -q; uv run pytest -q; acme'
cp -r "$SRV/pybase" "$T/pybase2"
rm -rf "$T/pybase2/.git"
edit 's/pybase-1.0.0/pybase-1.0.1/' "$T/pybase2/fixtures/seed.sql"
expect_error tag_exists stack publish "$T/pybase2" "oci:$REG/acme/pybase:1.0.0" --json
stack publish "$T/pybase2" "oci:$REG/acme/pybase:1.0.0" --force --json >"$T/republish.json"
second=$(jq -er '.data.digest' "$T/republish.json")
[[ "$first" != "$second" ]] || fail 'changed contents did not change the digest'
stack compile --json >"$T/locked.json"
jq -e --arg first "$first" '.ok and .data.bundles[0].digest == $first and .data.bundles[0].moved_from == null' "$T/locked.json" >/dev/null
stack compile --update --json >"$T/updated.json"
jq -e --arg first "$first" --arg second "$second" '.ok and .data.bundles[0].digest == $second and .data.bundles[0].moved_from == $first' "$T/updated.json" >/dev/null
edit 's/version = "8"/version = "8"\nport = 6379/' "$T/pybase2/bundle.toml"
expect_error bundle_fixed_port stack publish "$T/pybase2" "oci:$REG/acme/pybase:bad" --json
edit 's|pybase:1.0.0|nothere:9|' stack.toml
expect_error oci_not_found stack compile --json
stack down --json >"$T/down.json"
assert_json '.ok and .data.confirmed' "$T/down.json"

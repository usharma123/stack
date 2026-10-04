#!/usr/bin/env bash
# Variables in command strings are expanded by the child.
# shellcheck disable=SC2016
set -Eeuo pipefail
# shellcheck source=tests/e2e/assert.sh
source /tmp/stack-e2e-assert.sh
export PATH=/opt/stack:$PATH
stack publish /srv/pybase oci:localhost:5000/acme/pybase:1.0.0 --json >/tmp/publish.json
assert_json '.ok and .data.name == "pybase" and (.data.digest | test("^sha256:[0-9a-f]{64}$"))' /tmp/publish.json
first=$(jq -r '.data.digest' /tmp/publish.json)
cp -r /examples/app ~/appC
cd ~/appC || exit 1
rm -f stack.lock
sed -i 's|path:../bundles/pybase|oci:localhost:5000/acme/pybase:1.0.0|; s|path:../bundles/obs|git+file:///srv/obs?ref=v1|' stack.toml
stack compile --json >/tmp/oci-compile.json
assert_json '.ok and (.data.bundles | length == 2)' /tmp/oci-compile.json
stack -C ~/appA inspect --json >/tmp/git-inspect.json
jq -se '(.[0].data.bundles | map(select(.name == "pybase")) | .[0].content_hash) == (.[1].data.bundles | map(select(.name == "pybase")) | .[0].content_hash)' /tmp/oci-compile.json /tmp/git-inspect.json >/dev/null
stack up --json >/tmp/oci-up.json
assert_json '.ok and all(.data.checks[]; .ready and .identity == "instance")' /tmp/oci-up.json
stack exec --require-all -- bash -c 'set -euo pipefail; uv sync -q; uv run pytest -q; acme'
cp -r /srv/pybase /tmp/pybase2
rm -rf /tmp/pybase2/.git
sed -i 's/pybase-1.0.0/pybase-1.0.1/' /tmp/pybase2/fixtures/seed.sql
stack publish /tmp/pybase2 oci:localhost:5000/acme/pybase:1.0.0 --json >/tmp/republish.json
second=$(jq -er '.data.digest' /tmp/republish.json)
[[ "$first" != "$second" ]] || fail 'changed contents did not change the digest'
stack compile --json >/tmp/locked.json
jq -e --arg first "$first" '.ok and .data.bundles[0].digest == $first and .data.bundles[0].moved_from == null' /tmp/locked.json >/dev/null
stack compile --update --json >/tmp/updated.json
jq -e --arg first "$first" --arg second "$second" '.ok and .data.bundles[0].digest == $second and .data.bundles[0].moved_from == $first' /tmp/updated.json >/dev/null
sed -i 's/version = "8"/version = "8"\nport = 6379/' /tmp/pybase2/bundle.toml
expect_error bundle_fixed_port stack publish /tmp/pybase2 oci:localhost:5000/acme/pybase:bad --json
sed -i 's|pybase:1.0.0|nothere:9|' stack.toml
expect_error oci_not_found stack compile --json
stack down --json >/tmp/down.json
assert_json '.ok and .data.confirmed' /tmp/down.json

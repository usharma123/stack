# 5. OCI: publish, consume, digest pinning, retagging, validation before upload.
export PATH=/opt/stack:$PATH
echo "### S5: publish bundle to OCI, consume it, pin by digest"
stack publish /srv/pybase oci:localhost:5000/acme/pybase:1.0.0 --json | jq -c '.data | {name, digest, pinned}'
rm -rf ~/appC && cp -r /examples/app ~/appC && cd ~/appC && rm -f stack.lock
sed -i 's|path:../bundles/pybase|oci:localhost:5000/acme/pybase:1.0.0|; s|path:../bundles/obs|git+file:///srv/obs?ref=v1|' stack.toml
stack compile --json | jq -c '[.data.bundles[] | {name, pin: (.digest // .commit), content_hash}]'
echo "same content as the git-sourced bundle: $(grep -A3 'name = "pybase"' ~/appA/stack.lock | grep content_hash | cut -d'"' -f2 | cut -c1-23) (git) vs $(grep -A4 'name = "pybase"' stack.lock | grep content_hash | cut -d'"' -f2 | cut -c1-23) (oci)"
stack up --json | jq -c '{ok, checks: [.data.checks[] | {service, port, identity}]}'
stack exec --require-all -- bash -c 'uv sync -q && uv run pytest -q 2>&1 | tail -1; acme'
echo "### republish different content under the same tag"
cp -r /srv/pybase /tmp/pybase2 && rm -rf /tmp/pybase2/.git && sed -i "s/pybase-1.0.0/pybase-1.0.1/" /tmp/pybase2/fixtures/seed.sql
stack publish /tmp/pybase2 oci:localhost:5000/acme/pybase:1.0.0 --json | jq -c '.data.digest'
echo "locked compile: $(stack compile --json | jq -c '[.data.bundles[0] | .digest[0:19], .moved_from]')"
echo "--update:       $(stack compile --update --json | jq -c '[.data.bundles[0] | .digest[0:19], (.moved_from // "")[0:19]]')"
echo "### invalid bundle is rejected before upload"
sed -i 's/version = "8"/version = "8"\nport = 6379/' /tmp/pybase2/bundle.toml
stack publish /tmp/pybase2 oci:localhost:5000/acme/pybase:bad --json | jq -c '.error | {code}'
echo "### missing tag / unknown repo"
sed -i 's|pybase:1.0.0|nothere:9|' stack.toml; stack compile --json | jq -c '.error | {code, message}'
git -C ~/appC status >/dev/null 2>&1; stack -C ~/appC down >/dev/null 2>&1 || (cd ~/appC && mise daemons stop >/dev/null 2>&1)

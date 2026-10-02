# 3. TTL and owner-pid leases are reclaimed by gc and the next `stack up`.
export PATH=/opt/stack:$PATH
alive() { ps -eo stat,comm --no-headers | awk '$1 !~ /Z/ && $2 ~ /^(postgres|redis-server)$/' | wc -l; }
echo "### S3a: TTL lease expires and gc reclaims it"
cd ~/appB && stack up --ttl 4s --json | jq -c '.data.session.lease'
echo "live service processes: $(alive)"
echo "renew keeps it alive:"; sleep 3; stack renew --json | jq -c '{ok}'; sleep 2; stack gc --json | jq -c '.data'
sleep 4; echo "after idle > ttl:"; stack gc --json | jq -c '.data[] | {project, reason, stopped}'
echo "live service processes: $(alive); session file: $(ls ~/appB/.stack/session.json 2>/dev/null || echo removed)"
echo "### S3b: owner-pid lease (agent runner exits)"
sleep 300 & RUNNER=$!
cd ~/appA && stack up --owner-pid $RUNNER --json | jq -c '.data.session.lease'
stack exec --require-all -- true && echo "exec ok while runner alive"
kill $RUNNER; wait $RUNNER 2>/dev/null
stack status --json | jq -c '{lease_expired: .data.lease_expired}'
echo "next 'stack up' anywhere reaps it first:"; cd ~/appB && stack up --json | jq -c '{reaped: [.data.reaped[] | {project, reason, stopped}]}'
cd ~/appB && stack down --json | jq -c '.data.confirmed'
echo "live service processes: $(alive)"

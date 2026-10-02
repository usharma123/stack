# 2. Foreign servers and redirected endpoints: unverified endpoints are poisoned, never reached.
export PATH=/opt/stack:$PATH
cd ~/appA; PGBIN=$(dirname $(mise which postgres))
[ -d /tmp/f5432 ] || $PGBIN/initdb -D /tmp/f5432 -U postgres --auth=trust >/dev/null
$PGBIN/pg_ctl -D /tmp/f5432 -l /tmp/f5432.log -o "-p 5432 -k /tmp -c listen_addresses=127.0.0.1" -w start >/dev/null 2>&1
echo "### S2b': appA down, foreign Postgres on 5432; app has a hardcoded :5432 fallback"
stack down >/dev/null
stack exec -- bash -c 'echo "DATABASE_URL=$DATABASE_URL STACK_UNVERIFIED=$STACK_UNVERIFIED"' 2>/dev/null
stack exec -- bash -c 'uv run pytest -q 2>&1 | grep -E "Error|passed|failed" | sed "s/^E *//" | head -3' 2>/dev/null
echo "### S2c: our Postgres healthy, but the project points DATABASE_URL at the foreign server"
stack up >/dev/null && printf '\n[override.env]\nDATABASE_URL = "postgresql://postgres@127.0.0.1:5432/postgres"\n' >> stack.toml
sed -i '0,/^\[override.env\]$/!{/^\[override.env\]$/d}' stack.toml   # merge into the existing [override.env] table
stack compile >/dev/null 2>&1 || stack compile
stack status --json | jq -c '[.data.checks[] | {service, ready, reason}]'
stack exec -- bash -c 'echo DATABASE_URL=$DATABASE_URL' 2>&1
git checkout -q stack.toml; stack compile >/dev/null; stack down --json | jq -c '.data.confirmed'
$PGBIN/pg_ctl -D /tmp/f5432 -m fast stop >/dev/null

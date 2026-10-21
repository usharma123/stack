#!/bin/sh
# Prints this checkout's DB migrations, task count/titles, Redis keys and server data dirs (run under `stack exec`).
psql "$DATABASE_URL" -At <<'SQL'
select 'migrations=' || string_agg(name, ',' order by name) from schema_migrations;
select 'tasks=' || count(*) || ' titles=' || coalesce(string_agg(distinct left(title, 12), '|'), '') from tasks;
select 'pg_data=' || current_setting('data_directory');
SQL
echo "redis_keys=$(redis-cli -u "$REDIS_URL" --scan | sort | tr '\n' ' ')"
echo "redis_dir=$(redis-cli -u "$REDIS_URL" config get dir | tail -1)"

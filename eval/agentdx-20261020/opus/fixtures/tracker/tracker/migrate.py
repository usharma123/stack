"""Applies migrations/*.sql in order, once each, recording them in schema_migrations."""
import os
import pathlib

import psycopg

MIGRATIONS = pathlib.Path(__file__).resolve().parent.parent / "migrations"


def migrate(conn):
    conn.execute("CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY)")
    applied = {r[0] for r in conn.execute("SELECT name FROM schema_migrations")}
    done = []
    for path in sorted(MIGRATIONS.glob("*.sql")):
        if path.name in applied:
            continue
        conn.execute(path.read_text())
        conn.execute("INSERT INTO schema_migrations (name) VALUES (%s)", (path.name,))
        done.append(path.name)
    conn.commit()
    return done


if __name__ == "__main__":
    with psycopg.connect(os.environ["DATABASE_URL"]) as c:
        print("applied:", migrate(c) or "nothing")

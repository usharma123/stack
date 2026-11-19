"""Apply pending SQL migrations from migrations/, in file-name order, each exactly once."""
from pathlib import Path

from ledger import db

MIGRATIONS = Path(__file__).resolve().parent / 'migrations'

with db.connect() as conn:
    conn.execute('CREATE TABLE IF NOT EXISTS schema_migrations (version text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())')
    applied = {row[0] for row in conn.execute('SELECT version FROM schema_migrations')}
    for path in sorted(MIGRATIONS.glob('*.sql')):
        if path.stem in applied:
            continue
        with conn.transaction():
            conn.execute(path.read_text())
            conn.execute('INSERT INTO schema_migrations (version) VALUES (%s)', (path.stem,))
        print(f'applied {path.stem}')
print('migrations up to date')

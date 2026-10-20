import os
import psycopg

with psycopg.connect(os.environ['DATABASE_URL']) as connection:
    connection.execute("ALTER TABLE invoices ADD COLUMN IF NOT EXISTS note text NOT NULL DEFAULT ''")

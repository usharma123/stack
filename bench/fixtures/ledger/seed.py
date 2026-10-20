"""Load the ledger's existing data. Safe to run on every start."""
import os
import time

import psycopg
import redis

deadline = time.monotonic() + 30
while True:
    try:
        connection = psycopg.connect(os.environ['DATABASE_URL'])
        break
    except psycopg.OperationalError:
        if time.monotonic() > deadline:
            raise
        time.sleep(0.5)
with connection:
    connection.execute('CREATE TABLE IF NOT EXISTS invoices (id integer PRIMARY KEY, customer text NOT NULL, cents integer NOT NULL)')
    connection.execute("INSERT INTO invoices (id, customer, cents) VALUES (1, 'Acme', 1200) ON CONFLICT (id) DO NOTHING")
redis.Redis.from_url(os.environ['REDIS_URL']).delete('invoice:1')
print('seeded existing invoice')

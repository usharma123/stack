"""Grader-only data, inserted into the STARTING schema before the submission's migrations run.

Agents never see these rows. They test that migrations keep existing data (including names
with non-ASCII characters and duplicate customer names) and that receipts already issued survive.
"""
import os

import psycopg
import redis

INVOICES = [(41, 'Ünïcode GmbH', 9900, 'open'), (42, 'Umbrella', 300, 'open'), (43, 'Umbrella', 450, 'paid'),
            (44, 'Acme Corp', 77, 'open')]

with psycopg.connect(os.environ['DATABASE_URL']) as conn:
    for row in INVOICES:
        conn.execute('INSERT INTO invoices (id, customer, cents, status) VALUES (%s, %s, %s, %s)', row)
    conn.execute('INSERT INTO receipts (invoice_id, cents) VALUES (43, 450)')
redis.Redis.from_url(os.environ['REDIS_URL']).flushdb()
print('hidden data loaded')

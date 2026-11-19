"""Load the demo data the API and smoke test expect. Safe to run again."""
from ledger import cache, db

INVOICES = [(1, 'Acme Corp', 1200, 'open'), (2, 'Globex', 5000, 'paid'), (3, 'Zoë Café', 750, 'open')]

with db.connect() as conn:
    for invoice in INVOICES:
        conn.execute('INSERT INTO invoices (id, customer, cents, status) VALUES (%s, %s, %s, %s) ON CONFLICT (id) DO NOTHING', invoice)
    conn.execute("SELECT setval('invoices_id_seq', GREATEST((SELECT max(id) FROM invoices), 100))")
    conn.execute('INSERT INTO receipts (invoice_id, cents) SELECT 2, 5000 WHERE NOT EXISTS (SELECT 1 FROM receipts WHERE invoice_id = 2)')
for invoice_id, *_ in INVOICES:
    cache.forget_invoice(invoice_id)
print('seeded demo data')

"""Background jobs, queued in Redis and run by worker.py."""
import json

from ledger import cache, config, db


def enqueue(kind, **payload):
    cache.client().rpush(config.QUEUE, json.dumps({'kind': kind, **payload}))


def handle(job):
    handler = HANDLERS.get(job.get('kind'))
    if handler is None:
        raise ValueError(f'unknown job kind: {job!r}')
    handler(job)


def issue_receipt(job):
    """Record a receipt for a paid invoice."""
    with db.connect() as conn:
        invoice = db.get_invoice(conn, job['invoice_id'])
        if invoice is None:
            return
        conn.execute('INSERT INTO receipts (invoice_id, cents) VALUES (%s, %s)', (invoice.id, invoice.cents))


HANDLERS = {'issue_receipt': issue_receipt}

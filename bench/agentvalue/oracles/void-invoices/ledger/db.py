"""Database access for the ledger."""
import copy
from dataclasses import asdict, dataclass

import psycopg

from ledger import config


@dataclass(frozen=True)
class Invoice:
    id: int
    customer: str
    cents: int
    status: str

    def to_json(self):
        return asdict(self)


def connect():
    return psycopg.connect(config.database_url())


def get_invoice(conn, invoice_id, lock=False):
    row = conn.execute('SELECT id, customer, cents, status FROM invoices WHERE id = %s' + (' FOR UPDATE' if lock else ''),
                       (invoice_id,)).fetchone()
    return Invoice(*row) if row else None


def create_invoice(conn, customer, cents):
    row = conn.execute('INSERT INTO invoices (customer, cents) VALUES (%s, %s) RETURNING id, customer, cents, status',
                       (customer, cents)).fetchone()
    return Invoice(*row)


def update_invoice(conn, invoice, **changes):
    updated = copy.replace(invoice, **changes)  # Python 3.13+
    conn.execute('UPDATE invoices SET customer = %s, cents = %s, status = %s WHERE id = %s',
                 (updated.customer, updated.cents, updated.status, updated.id))
    return updated


def list_receipts(conn, invoice_id):
    rows = conn.execute('SELECT id, invoice_id, cents FROM receipts WHERE invoice_id = %s ORDER BY id', (invoice_id,)).fetchall()
    return [{'id': r[0], 'invoice_id': r[1], 'cents': r[2]} for r in rows]


def void_invoice(conn, invoice_id):
    """Void an open invoice; None when it is missing or not open."""
    row = conn.execute("UPDATE invoices SET status = 'void' WHERE id = %s AND status = 'open' RETURNING id, customer, cents, status",
                       (invoice_id,)).fetchone()
    return Invoice(*row) if row else None

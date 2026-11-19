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


SELECT_INVOICE = 'SELECT i.id, c.name, i.cents, i.status FROM invoices i JOIN customers c ON c.id = i.customer_id'


def get_invoice(conn, invoice_id):
    row = conn.execute(SELECT_INVOICE + ' WHERE i.id = %s', (invoice_id,)).fetchone()
    return Invoice(*row) if row else None


def customer_id(conn, name):
    return conn.execute('INSERT INTO customers (name) VALUES (%s) ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id',
                        (name,)).fetchone()[0]


def create_invoice(conn, customer, cents):
    (ident,) = conn.execute('INSERT INTO invoices (customer_id, cents) VALUES (%s, %s) RETURNING id',
                            (customer_id(conn, customer), cents)).fetchone()
    return get_invoice(conn, ident)


def update_invoice(conn, invoice, **changes):
    updated = copy.replace(invoice, **changes)  # Python 3.13+
    conn.execute('UPDATE invoices SET customer_id = %s, cents = %s, status = %s WHERE id = %s',
                 (customer_id(conn, updated.customer), updated.cents, updated.status, updated.id))
    return updated


def customer_invoices(conn, ident):
    if not conn.execute('SELECT 1 FROM customers WHERE id = %s', (ident,)).fetchone():
        return None
    rows = conn.execute(SELECT_INVOICE + ' WHERE c.id = %s ORDER BY i.id', (ident,)).fetchall()
    return [Invoice(*row).to_json() for row in rows]


def list_receipts(conn, invoice_id):
    rows = conn.execute('SELECT id, invoice_id, cents FROM receipts WHERE invoice_id = %s ORDER BY id', (invoice_id,)).fetchall()
    return [{'id': r[0], 'invoice_id': r[1], 'cents': r[2]} for r in rows]

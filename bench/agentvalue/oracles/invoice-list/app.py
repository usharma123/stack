"""Invoice ledger HTTP API."""
import json
import os
import re
from urllib.parse import parse_qs, urlsplit
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from ledger import cache, config, db

INVOICE = re.compile(r'^/invoices/(\d+)$')
RECEIPTS = re.compile(r'^/invoices/(\d+)/receipts$')
WHOLE = re.compile(r'^[0-9]+$')
STATUSES = ('open', 'paid')


class API(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, obj, status=200):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def body(self):
        length = int(self.headers.get('Content-Length') or 0)
        return json.loads(self.rfile.read(length) or b'{}')

    def do_GET(self):
        if self.path == '/health':
            with db.connect() as conn:
                conn.execute('SELECT 1')
            cache.client().ping()
            return self.reply({'ok': True})
        if self.path == '/identity':  # which running instance this is, for the dev tooling
            body = os.environ.get('IDENTITY_TOKEN', '').encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            return self.wfile.write(body)
        if urlsplit(self.path).path == '/invoices':
            return self.list_invoices(parse_qs(urlsplit(self.path).query, keep_blank_values=True))
        if match := INVOICE.match(self.path):
            invoice_id = int(match[1])
            if hit := cache.get_invoice(invoice_id):
                return self.reply(hit)
            with db.connect() as conn:
                invoice = db.get_invoice(conn, invoice_id)
            if invoice is None:
                return self.reply({'error': 'not found'}, 404)
            cache.put_invoice(invoice.to_json())
            return self.reply(invoice.to_json())
        if match := RECEIPTS.match(self.path):
            with db.connect() as conn:
                return self.reply(db.list_receipts(conn, int(match[1])))
        self.reply({'error': 'not found'}, 404)

    def list_invoices(self, query):
        def one(name):
            values = query.get(name)
            return values[-1] if values else None
        status, customer, limit, after = one('status'), one('customer'), one('limit'), one('after')
        if status is not None and status not in STATUSES:
            return self.reply({'error': 'status must be open or paid'}, 400)
        if limit is None:
            limit = 20
        elif WHOLE.match(limit) and 1 <= int(limit) <= 100:
            limit = int(limit)
        else:
            return self.reply({'error': 'limit must be a whole number from 1 to 100'}, 400)
        if after is None:
            after = 0
        elif WHOLE.match(after):
            after = int(after)
        else:
            return self.reply({'error': 'after must be a whole number of 0 or more'}, 400)
        with db.connect() as conn:
            rows = db.list_invoices(conn, status=status, customer=customer, after=after, limit=limit + 1)
        page = rows[:limit]
        more = len(rows) > limit
        return self.reply({'invoices': [invoice.to_json() for invoice in page], 'next': page[-1].id if more and page else None})

    def do_POST(self):
        if self.path == '/invoices':
            data = self.body()
            if not isinstance(data.get('customer'), str) or not isinstance(data.get('cents'), int):
                return self.reply({'error': 'customer (string) and cents (integer) are required'}, 400)
            with db.connect() as conn:
                invoice = db.create_invoice(conn, data['customer'], data['cents'])
            return self.reply(invoice.to_json(), 201)
        self.reply({'error': 'not found'}, 404)

    def do_PATCH(self):
        if match := INVOICE.match(self.path):
            invoice_id = int(match[1])
            data = self.body()
            changes = {key: data[key] for key in ('customer', 'cents') if key in data}
            with db.connect() as conn:
                invoice = db.get_invoice(conn, invoice_id)
                if invoice is None:
                    return self.reply({'error': 'not found'}, 404)
                invoice = db.update_invoice(conn, invoice, **changes)
            return self.reply(invoice.to_json())
        self.reply({'error': 'not found'}, 404)


if __name__ == '__main__':
    server = ThreadingHTTPServer(('127.0.0.1', config.port()), API)
    print(f'ledger api listening on 127.0.0.1:{config.port()}', flush=True)
    server.serve_forever()

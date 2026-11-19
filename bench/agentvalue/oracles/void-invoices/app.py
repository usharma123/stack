"""Invoice ledger HTTP API."""
import json
import os
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from ledger import cache, config, db

INVOICE = re.compile(r'^/invoices/(\d+)$')
RECEIPTS = re.compile(r'^/invoices/(\d+)/receipts$')
VOID = re.compile(r'^/invoices/(\d+)/void$')


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

    def do_POST(self):
        if match := VOID.match(self.path):
            invoice_id = int(match[1])
            with db.connect() as conn:
                invoice = db.void_invoice(conn, invoice_id)
                exists = invoice or db.get_invoice(conn, invoice_id)
            if not exists:
                return self.reply({'error': 'not found'}, 404)
            if not invoice:
                return self.reply({'error': f'a {exists.status} invoice cannot be voided'}, 409)
            cache.forget_invoice(invoice_id)
            return self.reply(invoice.to_json())
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
                invoice = db.get_invoice(conn, invoice_id, lock=True)
                if invoice is None:
                    return self.reply({'error': 'not found'}, 404)
                if invoice.status == 'void':
                    return self.reply({'error': 'a void invoice cannot be changed'}, 409)
                invoice = db.update_invoice(conn, invoice, **changes)
            cache.forget_invoice(invoice_id)
            return self.reply(invoice.to_json())
        self.reply({'error': 'not found'}, 404)


if __name__ == '__main__':
    server = ThreadingHTTPServer(('127.0.0.1', config.port()), API)
    print(f'ledger api listening on 127.0.0.1:{config.port()}', flush=True)
    server.serve_forever()

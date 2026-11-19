"""Hidden acceptance checks. Run by the grader's own interpreter against the submitted API.

Every check is computed here from HTTP responses, SQL and Redis; nothing the submission prints
is trusted. Prints one JSON object of check name -> bool on the last line.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

import psycopg
import redis

API = os.environ['API_URL']
DB = os.environ['DATABASE_URL']
R = redis.Redis.from_url(os.environ['REDIS_URL'], decode_responses=True)
TREE = Path(os.environ['GRADE_TREE'])
STARTING = Path(os.environ['GRADE_STARTING'])
ORIGINAL_MIGRATIONS = ['20261001_create_invoices', '20261008_create_receipts']
HIDDEN = {41: ('Ünïcode GmbH', 9900), 42: ('Umbrella', 300), 43: ('Umbrella', 450), 44: ('Acme Corp', 77)}
checks = {}


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(API + path, data=data, method=method, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw) if raw else None
        except ValueError:
            return error.code, None
    except (OSError, ValueError):
        return None, None


def check(name, fn):
    try:
        checks[name] = bool(fn())
    except Exception as error:  # a crashing check is a failed check
        checks[name] = False
        print(f'{name}: {type(error).__name__}: {error}', file=sys.stderr)


def sql(query, args=()):
    with psycopg.connect(DB) as conn:
        return conn.execute(query, args).fetchall()


def existing_preserved():
    for ident, (customer, cents) in HIDDEN.items():
        status, body = call('GET', f'/invoices/{ident}')
        if status != 200 or body.get('customer') != customer or body.get('cents') != cents:
            print(f'existing invoice {ident}: {status} {body}', file=sys.stderr)
            return False
    status, body = call('GET', '/invoices/1')
    return status == 200 and body.get('customer') == 'Acme Corp' and body.get('cents') == 1200


def migrations_sound():
    """Original migration files unchanged, at least one new one, and a rerun succeeds and changes nothing."""
    for name in ORIGINAL_MIGRATIONS:
        mine, original = TREE / 'migrations' / f'{name}.sql', STARTING / 'migrations' / f'{name}.sql'
        if not mine.exists() or hashlib.sha256(mine.read_bytes()).digest() != hashlib.sha256(original.read_bytes()).digest():
            print(f'original migration {name} was changed or removed', file=sys.stderr)
            return False
    versions = [row[0] for row in sql('SELECT version FROM schema_migrations ORDER BY version')]
    new = [v for v in versions if v not in ORIGINAL_MIGRATIONS]
    if not new or not set(ORIGINAL_MIGRATIONS) <= set(versions):
        print(f'schema_migrations: {versions}', file=sys.stderr)
        return False
    before = schema_fingerprint()
    rerun = subprocess.run([os.environ['GRADE_UV'], 'run', '--frozen', 'python', 'migrate.py'], cwd=TREE, capture_output=True,
                           text=True, timeout=120)
    if rerun.returncode != 0:
        print(f'migrate rerun failed: {rerun.stderr[-1000:]}', file=sys.stderr)
        return False
    untouched = all(call('GET', f'/invoices/{i}')[1].get('customer') == HIDDEN[i][0] for i in (41, 44))
    return schema_fingerprint() == before and untouched


def schema_fingerprint():
    return sql("SELECT table_name, column_name, data_type, is_nullable FROM information_schema.columns "
               "WHERE table_schema = 'public' ORDER BY 1, 2")


# ---------------------------------------------------------------- invoice-notes

def invoice_notes():
    check('existing_preserved', existing_preserved)
    check('note_default', lambda: call('GET', '/invoices/41')[1].get('note') == '' and call('GET', '/invoices/1')[1].get('note') == '')

    def create_with_note():
        status, body = call('POST', '/invoices', {'customer': 'Hooli', 'cents': 10, 'note': 'net 30 – ✓'})
        if status != 201:
            return False
        got = call('GET', f"/invoices/{body['id']}")[1]
        plain_status, plain = call('POST', '/invoices', {'customer': 'Pied Piper', 'cents': 11})
        return got.get('note') == 'net 30 – ✓' and plain_status == 201 and call('GET', f"/invoices/{plain['id']}")[1].get('note') == ''
    check('create_with_note', create_with_note)

    state = {}

    def patch_note():
        call('GET', '/invoices/42')  # warm the cache
        status, body = call('PATCH', '/invoices/42', {'note': 'call first', 'cents': 333})
        state['patched'] = status == 200
        return status == 200
    check('patch_note', patch_note)
    check('cache_after_patch', lambda: state.get('patched') and (lambda b: b.get('note') == 'call first' and b.get('cents') == 333)(call('GET', '/invoices/42')[1]))

    def database_after_patch():
        R.flushdb()  # whatever the cache holds, the next read must come from the database
        body = call('GET', '/invoices/42')[1]
        return body.get('note') == 'call first' and body.get('cents') == 333 and body.get('customer') == 'Umbrella'
    check('database_after_patch', database_after_patch)

    def other_untouched():
        body = call('GET', '/invoices/41')[1]
        return body.get('note') == '' and body.get('cents') == 9900 and body.get('customer') == 'Ünïcode GmbH'
    check('other_invoice_untouched', other_untouched)
    check('migration_rerun_safe', migrations_sound)


# ---------------------------------------------------------------- pay-receipts

def receipts_for(ident):
    return sql('SELECT cents FROM receipts WHERE invoice_id = %s ORDER BY id', (ident,))


def wait_receipts(ident, count, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if len(receipts_for(ident)) >= count:
            break
        time.sleep(0.2)
    return receipts_for(ident)


class Monitor:
    """Capture the exact arguments of every command sent to Redis (raw MONITOR protocol), so a job
    the API queued can be replayed byte for byte, whatever key or format the submission uses."""

    def __init__(self):
        import socket
        from urllib.parse import urlparse
        url = urlparse(os.environ['REDIS_URL'])
        self.sock = socket.create_connection((url.hostname, url.port or 6379), timeout=5)
        self.sock.sendall(b'*1\r\n$7\r\nMONITOR\r\n')
        self.buffer, self.commands = b'', []
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()
        time.sleep(0.3)

    def run(self):
        self.sock.settimeout(None)
        while True:
            try:
                chunk = self.sock.recv(65536)
            except OSError:
                return
            if not chunk:
                return
            self.buffer += chunk
            while b'\r\n' in self.buffer:
                line, self.buffer = self.buffer.split(b'\r\n', 1)
                if line.startswith(b'+') and b'"' in line:
                    self.commands.append(parse_monitor(line.decode('latin-1')))

    def close(self):
        self.sock.close()


def parse_monitor(line):
    """Arguments of one MONITOR line: quoted strings with C-style escapes (\\" \\\\ \\n \\xHH)."""
    args, i = [], line.index('"')
    while i < len(line):
        if line[i] != '"':
            i += 1
            continue
        i += 1
        out = bytearray()
        while line[i] != '"':
            if line[i] == '\\':
                nxt = line[i + 1]
                if nxt == 'x':
                    out.append(int(line[i + 2:i + 4], 16))
                    i += 4
                    continue
                out += {'n': b'\n', 'r': b'\r', 't': b'\t', 'a': b'\a', 'b': b'\b'}.get(nxt, nxt.encode('latin-1'))
                i += 2
                continue
            out += line[i].encode('latin-1')
            i += 1
        args.append(bytes(out))
        i += 1
    return args


WRITES = ('RPUSH', 'LPUSH', 'XADD', 'ZADD', 'SADD', 'LPUSHX', 'RPUSHX')


def pay_receipts():
    check('existing_preserved', existing_preserved)
    state = {}

    def pay_marks_paid():
        call('GET', '/invoices/41')  # warm the cache
        status, _ = call('POST', '/invoices/41/pay')
        state['paid'] = status is not None and 200 <= status < 300
        return state['paid']
    check('pay_marks_paid', pay_marks_paid)
    check('cache_after_pay', lambda: state.get('paid') and call('GET', '/invoices/41')[1].get('status') == 'paid')
    check('receipt_recorded', lambda: state.get('paid') and [r[0] for r in wait_receipts(41, 1)] == [9900])

    def duplicate_job_one_receipt():
        monitor = Monitor()
        status, _ = call('POST', '/invoices/42/pay')
        if status is None or not 200 <= status < 300:
            return False
        if [r[0] for r in wait_receipts(42, 1)] != [300]:
            return False
        time.sleep(0.5)
        monitor.close()
        replays = [args for args in list(monitor.commands) if args and args[0].decode('latin-1').upper() in WRITES]
        if not replays:
            print('no queued job observed on Redis', file=sys.stderr)
            return False
        raw = redis.Redis.from_url(os.environ['REDIS_URL'])  # bytes in, bytes out
        for _ in range(2):
            for args in replays:
                raw.execute_command(*args)
        time.sleep(4)
        got = [r[0] for r in receipts_for(42)]
        if got != [300]:
            print(f'receipts for 42 after duplicate delivery: {got}', file=sys.stderr)
        return got == [300]
    check('duplicate_job_one_receipt', duplicate_job_one_receipt)

    def pay_twice():
        status, _ = call('POST', '/invoices/41/pay')
        time.sleep(2)
        return status == 409 and [r[0] for r in receipts_for(41)] == [9900]
    check('pay_twice_conflict', pay_twice)
    check('pay_unknown_404', lambda: call('POST', '/invoices/999999/pay')[0] == 404)
    check('existing_receipts_kept', lambda: [r[0] for r in receipts_for(43)] == [450] and [r[0] for r in receipts_for(2)] == [5000])


# ---------------------------------------------------------------- customer-table

def customer_table():
    check('existing_preserved', existing_preserved)

    def deduplicated():
        names = [row[0] for row in sql('SELECT name FROM customers')]
        expected = {'Acme Corp', 'Globex', 'Zoë Café', 'Ünïcode GmbH', 'Umbrella'}
        return expected <= set(names) and len(names) == len(set(names))
    check('customers_deduplicated', deduplicated)
    check('customer_column_dropped', lambda: not sql("SELECT 1 FROM information_schema.columns WHERE table_schema = 'public' "
                                                     "AND table_name = 'invoices' AND column_name = 'customer'"))

    def customer_fk():
        fks = sql("SELECT ccu.table_name FROM information_schema.table_constraints tc "
                  "JOIN information_schema.key_column_usage kcu ON tc.constraint_name = kcu.constraint_name "
                  "JOIN information_schema.constraint_column_usage ccu ON tc.constraint_name = ccu.constraint_name "
                  "WHERE tc.table_name = 'invoices' AND tc.constraint_type = 'FOREIGN KEY' AND kcu.column_name = 'customer_id'")
        rows = dict(sql('SELECT i.id, c.name FROM invoices i JOIN customers c ON c.id = i.customer_id WHERE i.id IN (41, 42, 43, 44)'))
        return any(t == 'customers' for (t,) in fks) and rows == {k: v[0] for k, v in HIDDEN.items()}
    check('customer_fk', customer_fk)

    state = {}

    def reuses():
        (umbrella,) = sql("SELECT id FROM customers WHERE name = 'Umbrella'")[0]
        state['umbrella'] = umbrella
        status, body = call('POST', '/invoices', {'customer': 'Umbrella', 'cents': 5})
        if status != 201 or body.get('customer') != 'Umbrella':
            return False
        state['new_invoice'] = body['id']
        count = sql("SELECT count(*) FROM customers WHERE name = 'Umbrella'")[0][0]
        linked = sql('SELECT customer_id FROM invoices WHERE id = %s', (body['id'],))[0][0]
        return count == 1 and linked == umbrella and call('GET', f"/invoices/{body['id']}")[1].get('customer') == 'Umbrella'
    check('post_reuses_customer', reuses)

    def creates():
        status, body = call('POST', '/invoices', {'customer': 'Soylent', 'cents': 6})
        return status == 201 and sql("SELECT count(*) FROM customers WHERE name = 'Soylent'")[0][0] == 1 \
            and call('GET', f"/invoices/{body['id']}")[1].get('customer') == 'Soylent'
    check('post_new_customer', creates)

    def endpoint():
        status, body = call('GET', f"/customers/{state['umbrella']}/invoices")
        if status != 200:
            return False
        items = body if isinstance(body, list) else next((v for v in body.values() if isinstance(v, list)), None) if isinstance(body, dict) else None
        ids = {item.get('id') for item in items or [] if isinstance(item, dict)}
        expected = {42, 43} | ({state['new_invoice']} if 'new_invoice' in state else set())
        return expected <= ids and not ids & {41, 44, 1, 2, 3}
    check('customer_invoices_endpoint', endpoint)
    check('migration_rerun_safe', migrations_sound)


TASKS = {'invoice-notes': invoice_notes, 'pay-receipts': pay_receipts, 'customer-table': customer_table}
TASKS[sys.argv[1]]()
print(json.dumps(checks))

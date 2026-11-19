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
STATUS = {41: 'open', 42: 'open', 43: 'paid', 44: 'open', 1: 'open', 2: 'paid', 3: 'open'}
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
    """Every pre-existing invoice keeps customer, amount and status (API), and every receipt (SQL)."""
    for ident, (customer, cents) in HIDDEN.items():
        status, body = call('GET', f'/invoices/{ident}')
        if status != 200 or body.get('customer') != customer or body.get('cents') != cents or body.get('status') != STATUS[ident]:
            print(f'existing invoice {ident}: {status} {body}', file=sys.stderr)
            return False
    status, body = call('GET', '/invoices/1')
    if not (status == 200 and body.get('customer') == 'Acme Corp' and body.get('cents') == 1200):
        return False
    receipts = sql('SELECT invoice_id, cents FROM receipts WHERE invoice_id IN (2, 43) ORDER BY invoice_id')
    if receipts != [(2, 5000), (43, 450)]:
        print(f'pre-existing receipts changed: {receipts}', file=sys.stderr)
        return False
    stored = dict(sql('SELECT id, status FROM invoices WHERE id IN (1, 2, 3, 41, 42, 43, 44)'))
    if stored != {k: v for k, v in STATUS.items()}:
        print(f'pre-existing statuses changed in the database: {stored}', file=sys.stderr)
        return False
    return True


def existing_api_intact():
    """Existing routes still work: PATCH of the amount, and listing receipts."""
    status, body = call('PATCH', '/invoices/44', {'cents': 78})
    if status != 200:
        print(f'PATCH /invoices/44: {status} {body}', file=sys.stderr)
        return False
    # Read back from the database: a stale cached GET after PATCH is the starting code's known bug,
    # which only invoice-notes is asked to fix.
    stored = sql('SELECT cents FROM invoices WHERE id = 44')
    status, receipts = call('GET', '/invoices/43/receipts')
    return stored == [(78,)] and status == 200 and [r.get('cents') for r in receipts or []] == [450]


def migrations_sound():
    """Original migration files unchanged, at least one new one, and a rerun succeeds and changes nothing."""
    for name in ORIGINAL_MIGRATIONS:
        mine, original = TREE / 'migrations' / f'{name}.sql', STARTING / 'migrations' / f'{name}.sql'
        if not mine.exists() or hashlib.sha256(mine.read_bytes()).digest() != hashlib.sha256(original.read_bytes()).digest():
            print(f'original migration {name} was changed or removed', file=sys.stderr)
            return False
    versions = [row[0] for row in sql('SELECT version FROM schema_migrations ORDER BY version')]
    new = {v for v in versions if v not in ORIGINAL_MIGRATIONS}
    files = {p.stem for p in (TREE / 'migrations').glob('*.sql')} - set(ORIGINAL_MIGRATIONS)
    if not new or not set(ORIGINAL_MIGRATIONS) <= set(versions) or new != files:
        print(f'schema_migrations {sorted(versions)} vs new migration files {sorted(files)}', file=sys.stderr)
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
    columns = sql("SELECT table_name, column_name, data_type, is_nullable, column_default FROM information_schema.columns "
                  "WHERE table_schema = 'public' ORDER BY 1, 2")
    constraints = sql("SELECT conrelid::regclass::text, pg_get_constraintdef(oid) FROM pg_constraint "
                      "WHERE connamespace = 'public'::regnamespace ORDER BY 1, 2")
    indexes = sql("SELECT indexdef FROM pg_indexes WHERE schemaname = 'public' ORDER BY 1")
    contents = [(t, sql(f'SELECT count(*), md5(coalesce(string_agg(x::text, \',\' ORDER BY x::text), \'\')) FROM "{t}" x')[0])
                for (t,) in sql("SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' "
                                "AND table_name <> 'schema_migrations' ORDER BY 1")]
    return columns, constraints, indexes, contents


# ---------------------------------------------------------------- invoice-notes

def invoice_notes():
    check('existing_preserved', existing_preserved)
    check('existing_api_intact', existing_api_intact)
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
    def cache_after_patch():
        body = call('GET', '/invoices/42')[1]
        if not (state.get('patched') and body.get('note') == 'call first' and body.get('cents') == 333):
            return False
        # The read path must still use the cache: change the stored amount behind the API's back;
        # a read answered from the cache still shows the patched value, a database read would not.
        with psycopg.connect(DB) as conn:
            conn.execute('UPDATE invoices SET cents = 4242 WHERE id = 42')
        try:
            hit = call('GET', '/invoices/42')[1]
        finally:
            with psycopg.connect(DB) as conn:
                conn.execute('UPDATE invoices SET cents = 333 WHERE id = 42')
        if hit.get('cents') != 333:
            print(f'GET after a database-only change returned {hit}: the read did not come from the cache', file=sys.stderr)
            return False
        return True
    check('cache_after_patch', cache_after_patch)

    def database_after_patch():
        R.flushdb()  # whatever the cache holds, the next read must come from the database
        body = call('GET', '/invoices/42')[1]
        stored = sql("SELECT row_to_json(i)::text FROM invoices i WHERE id = 42")[0][0]
        return body.get('note') == 'call first' and body.get('cents') == 333 and body.get('customer') == 'Umbrella' \
            and '"call first"' in stored
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


class Worker:
    """The grader runs the submitted worker itself, so it can hold jobs back and release them."""

    def __init__(self):
        self.proc = None

    def start(self):
        import signal as _signal
        if self.proc is None or self.proc.poll() is not None:
            self.proc = subprocess.Popen([os.environ['GRADE_UV'], 'run', '--frozen', 'python', 'worker.py'], cwd=TREE,
                                         stdout=subprocess.DEVNULL, stderr=open(os.environ.get('GRADE_WORKER_LOG', os.devnull), 'ab'),
                                         stdin=subprocess.DEVNULL, start_new_session=True)
            time.sleep(1.5)

    def stop(self):
        import signal as _signal
        if self.proc is not None and self.proc.poll() is None:
            for sig in (_signal.SIGTERM, _signal.SIGKILL):
                try:
                    os.killpg(self.proc.pid, sig)
                except ProcessLookupError:
                    break
                try:
                    self.proc.wait(5)
                    break
                except subprocess.TimeoutExpired:
                    continue
        self.proc = None


def writes_during(fn):
    """Run fn() while capturing every Redis write command; returns (fn's result, the writes)."""
    monitor = Monitor()
    try:
        result = fn()
        time.sleep(0.5)
    finally:
        monitor.close()
    return result, [a for a in monitor.commands if a and a[0].decode('latin-1').upper() in WRITES]


def replayable(args):
    """A captured enqueue as it can be sent again: an explicit stream ID becomes '*'."""
    args = list(args)
    if args[0].decode('latin-1').upper() == 'XADD':
        i = 2
        while i < len(args) and args[i].decode('latin-1').upper() in ('NOMKSTREAM', 'MAXLEN', 'MINID', '=', '~', 'LIMIT') or \
                (i > 2 and args[i - 1].decode('latin-1').upper() in ('MAXLEN', 'MINID', 'LIMIT', '=', '~')):
            i += 1
        if i < len(args):
            args[i] = b'*'
    return args


def pay_receipts():
    check('existing_preserved', existing_preserved)
    check('existing_api_intact', existing_api_intact)
    worker, state = Worker(), {}
    try:
        def pay_marks_paid():
            call('GET', '/invoices/41')  # warm the cache
            (status, _), writes = writes_during(lambda: call('POST', '/invoices/41/pay'))
            stored = sql('SELECT status FROM invoices WHERE id = 41')
            state['paid'] = status is not None and 200 <= status < 300 and stored == [('paid',)]
            state['enqueued'] = writes
            return state['paid']
        check('pay_marks_paid', pay_marks_paid)
        check('cache_after_pay', lambda: state.get('paid') and call('GET', '/invoices/41')[1].get('status') == 'paid')

        def receipt_recorded():
            # With the worker held back, paying must queue a job and record nothing yet.
            if not state.get('paid') or not state.get('enqueued'):
                print(f"paid={state.get('paid')} enqueued={len(state.get('enqueued') or [])}", file=sys.stderr)
                return False
            if receipts_for(41):
                print('a receipt existed before the worker ran', file=sys.stderr)
                return False
            worker.start()
            return [r[0] for r in wait_receipts(41, 1)] == [9900]
        check('receipt_recorded', receipt_recorded)

        def duplicate_job_one_receipt():
            if not state.get('enqueued') or receipts_for(41) == []:
                return False
            worker.stop()
            keys = set()
            raw = redis.Redis.from_url(os.environ['REDIS_URL'])
            for _ in range(2):
                for args in state['enqueued']:
                    raw.execute_command(*replayable(args))
                    keys.add(args[1])
            # The README's contract: jobs go on a Redis list. Delivery is checked by the list draining.
            lists = [k for k in keys if raw.type(k) == b'list']
            if not lists or len(lists) != len(keys):
                print(f'jobs were not queued on a Redis list: {keys}', file=sys.stderr)
                return False
            worker.start()
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and any(raw.llen(k) for k in lists):
                time.sleep(0.2)
            if any(raw.llen(k) for k in lists):
                print('the redelivered job was never consumed', file=sys.stderr)
                return False
            time.sleep(3)
            got = [r[0] for r in receipts_for(41)]
            if got != [9900]:
                print(f'receipts for 41 after duplicate delivery: {got}', file=sys.stderr)
            return got == [9900]
        check('duplicate_job_one_receipt', duplicate_job_one_receipt)

        def pay_twice():
            (status, _), writes = writes_during(lambda: call('POST', '/invoices/41/pay'))
            time.sleep(1.5)
            return status == 409 and not writes and [r[0] for r in receipts_for(41)] == [9900]
        check('pay_twice_conflict', pay_twice)

        def pay_unknown():
            (status, _), writes = writes_during(lambda: call('POST', '/invoices/999999/pay'))
            return status == 404 and not writes
        check('pay_unknown_404', pay_unknown)
        check('existing_receipts_kept', lambda: [r[0] for r in receipts_for(43)] == [450] and [r[0] for r in receipts_for(2)] == [5000])
    finally:
        worker.stop()


# ---------------------------------------------------------------- customer-table

def customer_table():
    check('existing_preserved', existing_preserved)
    check('existing_api_intact', existing_api_intact)

    def deduplicated():
        names = [row[0] for row in sql('SELECT name FROM customers')]
        expected = {'Acme Corp', 'Globex', 'Zoë Café', 'Ünïcode GmbH', 'Umbrella'}
        unique = sql("SELECT 1 FROM pg_index x JOIN pg_attribute a ON a.attrelid = x.indrelid AND a.attnum = x.indkey[0] "
                     "WHERE x.indrelid = 'customers'::regclass AND x.indisunique AND x.indnatts = 1 AND a.attname = 'name' "
                     "AND x.indpred IS NULL")
        return expected <= set(names) and len(names) == len(set(names)) and bool(unique)
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


# ---------------------------------------------------------------- invoice-list

def rows_as_items(where='TRUE', args=()):
    """The invoices the listing should return, straight from the database, ascending by id."""
    return [{'id': i, 'customer': c, 'cents': n, 'status': s}
            for i, c, n, s in sql(f'SELECT id, customer, cents, status FROM invoices WHERE {where} ORDER BY id', args)]


def listing(query=''):
    """GET /invoices?<query> -> (status, items or None, next, raw body)."""
    status, body = call('GET', '/invoices' + (f'?{query}' if query else ''))
    if status != 200 or not isinstance(body, dict) or not isinstance(body.get('invoices'), list) or 'next' not in body:
        return status, None, None, body
    return status, body['invoices'], body['next'], body


def same_page(query, expected, expected_next=None):
    status, items, nxt, body = listing(query)
    ok = items == expected and nxt == expected_next and (nxt is None or isinstance(nxt, int))
    if not ok:
        print(f'GET /invoices?{query}: {status} items={[i.get("id") for i in items or [] if isinstance(i, dict)]} next={nxt!r}; '
              f'expected {[e["id"] for e in expected]} next={expected_next!r}', file=sys.stderr)
    return ok


def walk(query, limit):
    """Follow `next` from the start; returns (ids in order, pages, every page within the limit and
    every non-final next equal to its page's last id), or None when a page is malformed."""
    ids, pages, after, consistent = [], 0, None, True
    while pages < 200:
        q = '&'.join(p for p in [query, f'limit={limit}', f'after={after}' if after is not None else ''] if p)
        status, items, nxt, _ = listing(q)
        if items is None:
            print(f'walk {q}: status {status}', file=sys.stderr)
            return None
        pages += 1
        ids += [item.get('id') for item in items]
        if len(items) > limit:
            consistent = False
        if nxt is None:
            return ids, pages, consistent
        if not items or nxt != items[-1].get('id'):
            consistent = False
            return ids, pages, consistent
        after = nxt
    return ids, pages, False


BULK = list(range(200, 225))  # 25 rows the grader adds through SQL before any listing call


def invoice_list():
    check('existing_preserved', existing_preserved)
    check('existing_api_intact', existing_api_intact)
    # More rows than one default page, added before the listing is first called and with Redis
    # emptied, so nothing the API could have cached predates them. Statuses are mixed.
    with psycopg.connect(DB) as conn:
        for i in BULK:
            conn.execute('INSERT INTO invoices (id, customer, cents, status) VALUES (%s, %s, %s, %s)',
                         (i, 'Bulk Ltd' if i % 3 else 'Umbrella', i, 'paid' if i % 6 == 0 else 'open'))
    R.flushdb()

    def all_ascending():
        expected = rows_as_items()
        if not same_page('limit=100', expected):
            return False
        # Every item carries exactly the fields GET /invoices/<id> returns, with the same values.
        for item in expected[:4] + expected[-2:]:
            status, single = call('GET', f"/invoices/{item['id']}")
            if status != 200 or set(single) != set(item) or single != item:
                print(f'GET /invoices/{item["id"]} = {single} vs listed {item}', file=sys.stderr)
                return False
        return True
    check('list_all_ascending', all_ascending)

    def default_limit():
        expected = rows_as_items()
        return len(expected) > 20 and same_page('', expected[:20], expected[19]['id'])
    check('default_limit_20', default_limit)

    def filter_status():
        return same_page('status=open&limit=100', rows_as_items("status = 'open'")) and \
            same_page('status=paid&limit=100', rows_as_items("status = 'paid'"))
    check('filter_status', filter_status)

    def filter_customer():
        from urllib.parse import quote
        return (same_page('customer=Umbrella&limit=100', rows_as_items("customer = 'Umbrella'"))
                and same_page(f"customer={quote('Ünïcode GmbH')}&limit=100", rows_as_items("customer = %s", ('Ünïcode GmbH',)))
                and same_page(f"customer={quote('Acme Corp')}&limit=100", rows_as_items("customer = 'Acme Corp'"))
                and same_page('customer=Acme&limit=100', [])          # exact match, not a prefix
                and same_page('customer=umbrella&limit=100', [])      # exact match, not case-folded
                and same_page('customer=Nobody%20Inc', []))
    check('filter_customer', filter_customer)

    def combined():
        return (same_page('status=open&customer=Umbrella&limit=100', rows_as_items("status = 'open' AND customer = 'Umbrella'"))
                and same_page('status=paid&customer=Umbrella&limit=100', rows_as_items("status = 'paid' AND customer = 'Umbrella'"))
                and same_page('customer=Umbrella&status=paid&after=42&limit=100',
                              rows_as_items("status = 'paid' AND customer = 'Umbrella' AND id > 42"))
                and same_page('status=paid&customer=Zo%C3%AB%20Caf%C3%A9', []))
    check('filters_combined', combined)

    def after_filter():
        return (same_page('after=41&limit=100', rows_as_items('id > 41'))
                and same_page('after=0&limit=100', rows_as_items())
                and same_page('after=100000&limit=100', [])
                and same_page('after=3&limit=3', rows_as_items('id > 3')[:3], rows_as_items('id > 3')[2]['id']))
    check('after_filter', after_filter)

    def paging():
        for query, limit in [('', 2), ('', 5), ('', 7), ('status=open', 3), ('customer=Umbrella', 2)]:
            where = {'': 'TRUE', 'status=open': "status = 'open'", 'customer=Umbrella': "customer = 'Umbrella'"}[query]
            expected = [r['id'] for r in rows_as_items(where)]
            got = walk(query, limit)
            if got is None or got[0] != expected or not got[2] or len(set(got[0])) != len(got[0]):
                print(f'walk {query!r} limit={limit}: {got} vs {expected}', file=sys.stderr)
                return False
        return True
    check('paging_walk', paging)

    def full_last_page():
        # The last page holds exactly `limit` items: nothing lies beyond it, so next is null.
        total = rows_as_items()
        paid = rows_as_items("status = 'paid'")
        umbrella = rows_as_items("customer = 'Umbrella'")
        checks_ = [same_page(f'limit={len(total)}', total),
                   same_page(f'status=paid&limit={len(paid)}', paid),
                   same_page(f"customer=Umbrella&after={umbrella[-3]['id']}&limit=2", umbrella[-2:])]
        for query, limit in [('', 4), ('status=paid', 2)]:
            where = 'TRUE' if not query else "status = 'paid'"
            expected = [r['id'] for r in rows_as_items(where)]
            if len(expected) % limit:
                print(f'grader data: {len(expected)} rows do not fill pages of {limit}', file=sys.stderr)
                return False
            got = walk(query, limit)
            checks_.append(got is not None and got[0] == expected and got[1] == len(expected) // limit)
        return all(checks_)
    check('next_null_on_full_last_page', full_last_page)

    def bad_params():
        bad = ['status=void', 'status=closed', 'limit=0', 'limit=101', 'limit=abc', 'limit=1.5', 'limit=-1',
               'limit=2x', 'after=-1', 'after=abc', 'after=1.5', 'after=x1']
        codes = {q: call('GET', f'/invoices?{q}')[0] for q in bad}
        good = {q: call('GET', f'/invoices?{q}')[0] for q in ['limit=1', 'limit=100', 'after=0', 'status=open', 'status=paid']}
        if any(c != 400 for c in codes.values()) or any(c != 200 for c in good.values()):
            print(f'bad parameters: {codes}; good parameters: {good}', file=sys.stderr)
            return False
        return True
    check('bad_params_400', bad_params)

    def status_index():
        # An index on invoices, other than the primary key, whose key columns include status, not partial.
        found = sql("SELECT x.indexrelid::regclass::text FROM pg_index x "
                    "WHERE x.indrelid = 'invoices'::regclass AND NOT x.indisprimary AND x.indpred IS NULL AND x.indisvalid "
                    "AND EXISTS (SELECT 1 FROM generate_series(1, x.indnkeyatts) k "
                    "WHERE pg_get_indexdef(x.indexrelid, k, true) IN ('status', '\"status\"'))")
        if not found:
            defs = sql("SELECT indexdef FROM pg_indexes WHERE tablename = 'invoices'")
            print(f'indexes on invoices: {defs}', file=sys.stderr)
        return bool(found)
    check('status_index', status_index)

    def post_appears():
        status, body = call('POST', '/invoices', {'customer': 'Initrode', 'cents': 5})
        if status != 201:
            return False
        new_id = int(body['id'])
        new = {'id': new_id, 'customer': 'Initrode', 'cents': 5, 'status': 'open'}
        after = rows_as_items(f'id > {new_id - 1}')
        return (new in rows_as_items() and same_page('customer=Initrode', [new])
                and same_page(f'after={new_id - 1}&limit=1', after[:1], new_id if len(after) > 1 else None)
                and same_page('limit=100', rows_as_items()))
    check('post_appears_in_list', post_appears)
    check('migration_rerun_safe', migrations_sound)


# ---------------------------------------------------------------- void-invoices

def stored(ident):
    rows = sql('SELECT customer, cents, status FROM invoices WHERE id = %s', (ident,))
    return rows[0] if rows else None


def void_invoices():
    check('existing_preserved', existing_preserved)
    check('existing_api_intact', existing_api_intact)
    state = {}

    def void_open():
        call('GET', '/invoices/42')  # warm the cache
        status, body = call('POST', '/invoices/42/void')
        state['voided'] = status is not None and 200 <= status < 300 and stored(42) == ('Umbrella', 300, 'void')
        if not state['voided']:
            print(f'void 42: {status} {body}; stored {stored(42)}', file=sys.stderr)
        return state['voided'] and isinstance(body, dict) and body.get('id') == 42 and body.get('status') == 'void' \
            and body.get('cents') == 300 and body.get('customer') == 'Umbrella'
    check('void_open', void_open)

    def cache_after_void():
        body = call('GET', '/invoices/42')[1] or {}
        if not (state.get('voided') and body.get('status') == 'void' and body.get('cents') == 300):
            print(f'GET after void: {body}', file=sys.stderr)
            return False
        # The read path must still use the cache: a change made behind the API's back must not show.
        with psycopg.connect(DB) as conn:
            conn.execute('UPDATE invoices SET cents = 4242 WHERE id = 42')
        try:
            hit = call('GET', '/invoices/42')[1] or {}
        finally:
            with psycopg.connect(DB) as conn:
                conn.execute('UPDATE invoices SET cents = 300 WHERE id = 42')
        if hit.get('cents') != 300 or hit.get('status') != 'void':
            print(f'GET after a database-only change returned {hit}: the read did not come from the cache', file=sys.stderr)
            return False
        return True
    check('cache_after_void', cache_after_void)

    def void_paid():
        before = sql('SELECT invoice_id, cents FROM receipts ORDER BY id')
        results = []
        for ident, row in [(43, ('Umbrella', 450, 'paid')), (2, ('Globex', 5000, 'paid'))]:
            call('GET', f'/invoices/{ident}')  # cached first: the answer must not depend on the cache
            status, _ = call('POST', f'/invoices/{ident}/void')
            results.append((status, stored(ident), call('GET', f'/invoices/{ident}')[1].get('status')))
            if status != 409 or stored(ident) != row:
                print(f'void paid {ident}: {status}, stored {stored(ident)}', file=sys.stderr)
                return False
        return all(r[2] == 'paid' for r in results) and sql('SELECT invoice_id, cents FROM receipts ORDER BY id') == before
    check('void_paid_409', void_paid)

    def void_twice():
        if not state.get('voided'):
            return False
        status, _ = call('POST', '/invoices/42/void')
        return status == 409 and stored(42) == ('Umbrella', 300, 'void')
    check('void_twice_409', void_twice)

    def void_unknown():
        count = sql('SELECT count(*), max(id) FROM invoices')[0]
        status, _ = call('POST', '/invoices/999999/void')
        return status == 404 and sql('SELECT count(*), max(id) FROM invoices')[0] == count
    check('void_unknown_404', void_unknown)

    def patch_void():
        if not state.get('voided'):
            return False
        results = [call('PATCH', '/invoices/42', body)[0] for body in ({'cents': 1}, {'customer': 'Someone Else'})]
        if results != [409, 409] or stored(42) != ('Umbrella', 300, 'void'):
            print(f'PATCH on a void invoice: {results}, stored {stored(42)}', file=sys.stderr)
            return False
        # An open invoice can still be patched.
        status, _ = call('PATCH', '/invoices/41', {'cents': 9900})
        return status == 200
    check('patch_void_409', patch_void)

    def constraint():
        # Tried inside transactions that are rolled back, so nothing the checks write survives.
        def attempt(statement, args):
            with psycopg.connect(DB) as conn:
                try:
                    with conn.transaction():
                        conn.execute(statement, args)
                        raise RuntimeError('rollback')
                except RuntimeError:
                    return 'accepted'
                except (psycopg.errors.IntegrityError, psycopg.errors.DataError):
                    return 'rejected'   # a check constraint, an enum or a reference all reject
                except psycopg.Error as error:
                    return type(error).__name__
        results = {
            'update_void': attempt("UPDATE invoices SET status = 'void' WHERE id = %s", (44,)),
            'update_bogus': attempt("UPDATE invoices SET status = 'bogus' WHERE id = %s", (44,)),
            'update_cancelled': attempt("UPDATE invoices SET status = 'cancelled' WHERE id = %s", (41,)),
            'insert_void': attempt("INSERT INTO invoices (customer, cents, status) VALUES (%s, 1, 'void')", ('Check',)),
            'insert_closed': attempt("INSERT INTO invoices (customer, cents, status) VALUES (%s, 1, 'closed')", ('Check',)),
            'insert_paid': attempt("INSERT INTO invoices (customer, cents, status) VALUES (%s, 1, 'paid')", ('Check',)),
        }
        expected = {'update_void': 'accepted', 'update_bogus': 'rejected', 'update_cancelled': 'rejected',
                    'insert_void': 'accepted', 'insert_closed': 'rejected', 'insert_paid': 'accepted'}
        if results != expected:
            print(f'status constraint: {results}', file=sys.stderr)
        return results == expected and stored(44)[2] == 'open' and not sql("SELECT 1 FROM invoices WHERE customer = 'Check'")
    check('constraint_allows_void_only', constraint)

    def other_untouched():
        body41 = call('GET', '/invoices/41')[1] or {}
        return stored(41) == ('Ünïcode GmbH', 9900, 'open') and body41.get('status') == 'open' \
            and stored(44)[0::2] == ('Acme Corp', 'open') and stored(1) == ('Acme Corp', 1200, 'open')
    check('other_invoice_untouched', other_untouched)
    check('migration_rerun_safe', migrations_sound)


TASKS = {'invoice-notes': invoice_notes, 'pay-receipts': pay_receipts, 'customer-table': customer_table,
         'invoice-list': invoice_list, 'void-invoices': void_invoices}
TASKS[sys.argv[1]]()
print(json.dumps(checks))

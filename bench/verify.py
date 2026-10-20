#!/usr/bin/env python3
"""Hidden acceptance checks. Never copied into an agent's workspace.

Run inside the grading workspace's environment, after the harness has started the
submitted code against grader-only seed data. Prints one JSON object and exits 0 only
when every check passed.

Threat model: submitted code is untrusted but runs as the same user. Every check is
computed in this process from values it observes itself (HTTP responses, database rows,
files, and slug() return values); nothing a submission prints, returns as a verdict or does
in an exit handler can mark a check passed, and submitted subprocesses never share this
process's stdout. Out of scope: code that tampers with the host outside the band, such as
editing the benchmark files, reading this process's memory or the result nonce, or tracing
it. The workspace boundary is a convention, not a sandbox; the trial re-hashes the benchmark
files after grading to detect edits to them.
"""
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import urllib.error
import urllib.request

CHECKS = {}


def request(base, method='GET', data=None, path=''):
    body = json.dumps(data).encode() if data is not None else None
    req = urllib.request.Request(base + path, method=method, data=body, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            status, raw = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, raw = error.code, error.read()
    try:
        return status, json.loads(raw)
    except ValueError:
        return status, None


def check(name, function):
    try:
        CHECKS[name] = function() is True
    except Exception as error:  # A crash fails only this check; the rest still run.
        CHECKS[name] = False
        CHECKS.setdefault('_errors', {})[name] = f'{type(error).__name__}: {error}'


def ledger(workspace):
    import psycopg
    import redis
    api, database = os.environ['API_URL'], os.environ['DATABASE_URL']
    cache = redis.Redis.from_url(os.environ['REDIS_URL'], decode_responses=True)
    note = 'Paid café invoice'
    other = {'id': 7, 'customer': 'Zoë Café', 'cents': 450, 'note': ''}
    state = {}

    def rows():
        with psycopg.connect(database) as connection:
            return connection.execute('SELECT id, customer, cents FROM invoices ORDER BY id').fetchall()

    def old_default():
        state['first'] = request(api, path='/invoices/1')[1]
        return state['first'] == {'id': 1, 'customer': 'Acme', 'cents': 1200, 'note': ''} and request(api, path='/invoices/7')[1] == other

    def patch():
        return request(api, 'PATCH', {'cents': 1729, 'note': note}, '/invoices/1')[0] == 200

    updated = {'id': 1, 'customer': 'Acme', 'cents': 1729, 'note': note}

    def read_after_write():
        return request(api, path='/invoices/1')[1] == updated

    def database_row():
        with psycopg.connect(database) as connection:
            state['row'] = connection.execute('SELECT customer, cents, note FROM invoices WHERE id = 1').fetchone()
        return state['row'] == ('Acme', 1729, note)

    def idempotent():
        # Output captured: submitted code must not write to the grader's own stdout.
        subprocess.run([sys.executable, str(workspace / 'migrate.py')], cwd=workspace, check=True, timeout=30,
                       stdin=subprocess.DEVNULL, capture_output=True)
        with psycopg.connect(database) as connection:
            current = connection.execute('SELECT id, customer, cents, note FROM invoices ORDER BY id').fetchall()
        return current == [(1, 'Acme', 1729, note), (7, 'Zoë Café', 450, '')]

    check('existing_invoices_preserved', lambda: rows() == [(1, 'Acme', 1200), (7, 'Zoë Café', 450)])
    check('old_note_default', old_default)
    check('patch_accepted', patch)
    check('read_after_write', read_after_write)
    def cached():
        values = []
        for key in cache.scan_iter():
            try:
                values.append(json.loads(cache.get(key) or 'null'))
            except (ValueError, redis.ResponseError):
                pass
        return values

    # A second read must agree, and some cache entry must hold the updated invoice. The
    # prompt names no key, so the starter's `invoice:1` is not required.
    check('cache_after_write', lambda: request(api, path='/invoices/1')[1] == updated and updated in cached())
    check('database_after_write', database_row)
    check('other_invoice_untouched', lambda: request(api, path='/invoices/7')[1] == other)
    check('migration_idempotent', idempotent)


def catalog(workspace):
    web = os.environ['WEB_URL']
    path = workspace / 'products.json'
    initial = [{'sku': 'BOOK-1', 'name': 'Original'}, {'sku': 'MUG-7', 'name': 'Mug'}]
    state = {}

    def duplicates():
        state['bytes'] = path.read_bytes()
        statuses = [request(web, 'POST', {'sku': sku, 'name': 'duplicate'})[0] for sku in [' book-1 ', 'Book-1', '\tBOOK-1\n', ' mug-7']]
        return statuses == [409, 409, 409, 409]

    def normalized():
        status, product = request(web, 'POST', {'sku': ' pen-2 ', 'name': 'Pen'})
        return status == 201 and product == {'sku': 'PEN-2', 'name': 'Pen'}

    def stored():
        expected = initial + [{'sku': 'PEN-2', 'name': 'Pen'}]
        return request(web)[1] == expected and json.loads(path.read_text()) == expected

    check('existing_products_preserved', lambda: request(web)[1] == initial)
    check('duplicates_rejected', duplicates)
    check('rejection_preserves_bytes', lambda: path.read_bytes() == state.get('bytes'))
    check('new_product_normalized', normalized)
    check('storage_matches_response', stored)


SLUG_CASES = [('  Crème & BRÛLÉE! ', 'creme-brulee'), ('a---b', 'a-b'), ('A\t B\nC', 'a-b-c'), ('!!', ''), ('', ''),
              ('version 42', 'version-42'), ('Ünïcödé -- Test', 'unicode-test'), ('hello world', 'hello-world')]

# Runs in a Node process that also loads the submission, so it only reports what slug()
# returned; this file decides every check. The request is read from stdin before the
# submission loads, and the one result line carries its random nonce. Results are built by
# string concatenation of primitives with functions captured before the import, so prototype
# or global tampering cannot reshape them.
SLUG_HARNESS = r"""
import fs from 'node:fs';
const writeSync = fs.writeSync, readFileSync = fs.readFileSync, stringify = JSON.stringify, parse = JSON.parse;
const request = parse(readFileSync(0, 'utf8'));
const nonce = request.nonce, inputs = request.inputs, count = inputs.length;
const encode = (value) => typeof value === 'string' ? stringify(value) : 'null';
const { slug } = await import(request.module);
let line = '\n' + nonce + ' [';
for (let i = 0; i < count; i++) {
  let first, second;
  try { first = slug(inputs[i]); } catch { first = undefined; }
  try { second = typeof first === 'string' ? slug(first) : undefined; } catch { second = undefined; }
  line = line + (i ? ',' : '') + encode(first) + ',' + encode(second);
}
writeSync(1, line + ']\n');
"""


def slug(workspace):
    """Call the submitted slug() on every case and compare here, in the grader."""
    names = [f'case_{i}' for i in range(len(SLUG_CASES))] + ['idempotent']
    CHECKS.update({name: False for name in names})
    nonce = secrets.token_hex(16)
    request = {'nonce': nonce, 'module': (workspace / 'slug.mjs').as_uri(), 'inputs': [text for text, _ in SLUG_CASES]}
    try:
        result = subprocess.run(['node', '--input-type=module', '-e', SLUG_HARNESS], cwd=workspace, input=json.dumps(request),
                                capture_output=True, text=True, timeout=30)
    except subprocess.TimeoutExpired:
        CHECKS['_errors'] = {'node': 'timed out after 30 s'}
        return
    frames = [line[len(nonce) + 1:] for line in result.stdout.splitlines() if line.startswith(nonce + ' ')]
    if result.returncode or len(frames) != 1:
        CHECKS['_errors'] = {'node': f'exit {result.returncode}, {len(frames)} result lines; {result.stderr[-2000:]}'}
        return
    values = json.loads(frames[0])
    if not (isinstance(values, list) and len(values) == 2 * len(SLUG_CASES)):
        CHECKS['_errors'] = {'node': 'malformed result line'}
        return
    pairs = [values[i:i + 2] for i in range(0, len(values), 2)]
    for i, ((_, expected), (first, _)) in enumerate(zip(SLUG_CASES, pairs)):
        CHECKS[f'case_{i}'] = isinstance(first, str) and first == expected
    CHECKS['idempotent'] = all(isinstance(first, str) and second == first for first, second in pairs)


def main():
    task, workspace = sys.argv[1], Path(sys.argv[2]).resolve()
    graders = {'ledger-note-cache': ledger, 'catalog-sku-uniqueness': catalog, 'slug-unicode': slug}
    try:
        graders[task](workspace)
    except Exception as error:
        CHECKS.setdefault('_errors', {})['grader'] = f'{type(error).__name__}: {error}'
    errors = CHECKS.pop('_errors', {})
    passed = bool(CHECKS) and not errors and all(CHECKS.values())
    print(json.dumps({'ok': passed, 'checks': CHECKS, 'errors': errors}))
    return 0 if passed else 1


if __name__ == '__main__':
    sys.exit(main())

#!/usr/bin/env python3
"""Hidden acceptance checks. Never copied into an agent's workspace.

Run inside the grading workspace's environment, after the harness has started the
submitted code against grader-only seed data. Prints one JSON object and exits 0 only
when every check passed.
"""
import json
import os
from pathlib import Path
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
        subprocess.run([sys.executable, str(workspace / 'migrate.py')], cwd=workspace, check=True, timeout=30)
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


SLUG_PROGRAM = r'''
import {slug} from './slug.mjs';
const cases = [['  Crème & BRÛLÉE! ', 'creme-brulee'], ['a---b', 'a-b'], ['A\t B\nC', 'a-b-c'], ['!!', ''], ['', ''],
  ['version 42', 'version-42'], ['Ünïcödé -- Test', 'unicode-test'], ['hello world', 'hello-world']];
const checks = {};
cases.forEach(([input, expected], i) => { try { checks['case_' + i] = slug(input) === expected; } catch { checks['case_' + i] = false; } });
try { checks.idempotent = cases.every(([input]) => slug(slug(input)) === slug(input)); } catch { checks.idempotent = false; }
console.log(JSON.stringify(checks));
'''


def slug(workspace):
    result = subprocess.run(['node', '--input-type=module', '-e', SLUG_PROGRAM], cwd=workspace,
                            capture_output=True, text=True, timeout=30)
    if result.returncode:
        CHECKS['_errors'] = {'node': result.stderr[-2000:]}
        return
    CHECKS.update({name: value is True for name, value in json.loads(result.stdout.strip().splitlines()[-1]).items()})


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

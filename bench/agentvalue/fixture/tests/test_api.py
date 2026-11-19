"""API tests. They need the services running and API_URL, DATABASE_URL and REDIS_URL set."""
import json
import os
import urllib.error
import urllib.request

import psycopg

API = os.environ['API_URL']


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(API + path, data=data, method=method, headers={'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def test_health():
    assert call('GET', '/health') == (200, {'ok': True})


def test_seeded_invoice():
    status, invoice = call('GET', '/invoices/1')
    assert status == 200
    assert (invoice['customer'], invoice['cents']) == ('Acme Corp', 1200)


def test_create_invoice():
    status, invoice = call('POST', '/invoices', {'customer': 'Initech', 'cents': 4200})
    assert status == 201 and invoice['customer'] == 'Initech'
    assert call('GET', f"/invoices/{invoice['id']}")[1]['cents'] == 4200


def test_missing_invoice():
    assert call('GET', '/invoices/999999')[0] == 404


def test_schema_is_migrated():
    with psycopg.connect(os.environ['DATABASE_URL']) as conn:
        versions = {row[0] for row in conn.execute('SELECT version FROM schema_migrations')}
    assert {'20261001_create_invoices', '20261008_create_receipts'} <= versions

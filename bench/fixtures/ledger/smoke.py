"""Existing smoke test: the API is up and serves the seeded invoice."""
import json
import os
import urllib.request

base = os.environ['API_URL']
with urllib.request.urlopen(base + '/health', timeout=5) as response:
    assert json.load(response) == {'ok': True}
with urllib.request.urlopen(base + '/invoices/1', timeout=5) as response:
    invoice = json.load(response)
assert (invoice['id'], invoice['customer'], invoice['cents']) == (1, 'Acme', 1200), invoice
print('smoke ok')

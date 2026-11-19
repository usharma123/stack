"""Read-through cache for invoices in Redis."""
import json

import redis

from ledger import config

_client = None


def client():
    global _client
    if _client is None:
        _client = redis.Redis.from_url(config.redis_url(), decode_responses=True)
    return _client


def get_invoice(invoice_id):
    hit = client().get(config.invoice_key(invoice_id))
    return json.loads(hit) if hit else None


def put_invoice(invoice):
    client().set(config.invoice_key(invoice['id']), json.dumps(invoice), ex=config.CACHE_TTL_SECONDS)


def forget_invoice(invoice_id):
    client().delete(config.invoice_key(invoice_id))

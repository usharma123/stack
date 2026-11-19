"""Connection settings. Every address comes from the environment; nothing is hardcoded."""
import os


def database_url():
    return os.environ['DATABASE_URL']


def redis_url():
    return os.environ['REDIS_URL']


def port():
    return int(os.environ['PORT'])


# Redis keys. The cache and the job queue share one Redis.
QUEUE = 'ledger:jobs'
CACHE_TTL_SECONDS = 300


def invoice_key(invoice_id):
    return f'ledger:invoice:{invoice_id}'

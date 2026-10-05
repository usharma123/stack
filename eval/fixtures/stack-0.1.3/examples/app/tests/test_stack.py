import os, sys, psycopg, redis

def test_python():
    assert sys.version_info[:2] >= (3, 12)

def test_postgres():
    with psycopg.connect(os.environ.get("DATABASE_URL", "postgresql://postgres@127.0.0.1:5432/postgres"), connect_timeout=3) as c:
        assert c.execute("select 1").fetchone()[0] == 1

def test_redis():
    r = redis.Redis.from_url(os.environ.get("REDIS_URL", "redis://127.0.0.1:6379/0"), socket_timeout=3)
    assert r.ping()

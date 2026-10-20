import os,psycopg,redis
with psycopg.connect(os.environ['DATABASE_URL']) as c:
    c.execute('CREATE TABLE IF NOT EXISTS invoices (id integer PRIMARY KEY, customer text NOT NULL, cents integer NOT NULL)')
    c.execute('INSERT INTO invoices VALUES (1,%s,1200) ON CONFLICT (id) DO NOTHING',(os.environ.get('CUSTOMER','Acme'),))
redis.Redis.from_url(os.environ['REDIS_URL']).delete('invoice:1')
print('seeded existing invoice')

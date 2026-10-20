import os,psycopg
with psycopg.connect(os.environ['DATABASE_URL']) as c:
    before=c.execute('SELECT id,customer,cents FROM invoices ORDER BY id').fetchall()
    c.execute("ALTER TABLE invoices ADD COLUMN IF NOT EXISTS note text NOT NULL DEFAULT ''")
    after=c.execute('SELECT id,customer,cents FROM invoices ORDER BY id').fetchall()
    assert before==after and len(after)>0,(before,after)
    print({'preserved_rows':after,'migration':'invoice note'})

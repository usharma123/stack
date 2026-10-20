import json,os,urllib.request,psycopg,redis
url=os.environ['API_URL']+'/invoices/1'
def request(method='GET',data=None):
    req=urllib.request.Request(url,data=json.dumps(data).encode() if data else None,method=method,headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req) as r: return json.load(r)
old=request(); new=old['cents']+25
request('PATCH',{'cents':new,'note':'paid by bank'})
after=request()
assert after['cents']==new,{'cached_old':old,'returned':after,'expected_cents':new}
assert after['note']=='paid by bank',after
with psycopg.connect(os.environ['DATABASE_URL']) as c:
    row=c.execute('SELECT customer,cents,note FROM invoices WHERE id=1').fetchone()
assert row==(old['customer'],new,'paid by bank'),row
cached=json.loads(redis.Redis.from_url(os.environ['REDIS_URL']).get('invoice:1'))
assert cached==after,(cached,after)
print(json.dumps({'ok':True,'before':old,'after':after,'db':row,'cache':cached}))

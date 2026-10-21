import json, os
from http.server import BaseHTTPRequestHandler, HTTPServer
import psycopg, redis
cache=redis.Redis.from_url(os.environ['REDIS_URL'],decode_responses=True)
def db(): return psycopg.connect(os.environ['DATABASE_URL'])
class API(BaseHTTPRequestHandler):
    def reply(self,obj,status=200):
        data=json.dumps(obj).encode(); self.send_response(status); self.send_header('Content-Type','application/json'); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        if self.path=='/identity':
            self.send_response(200); self.end_headers(); self.wfile.write(os.environ['IDENTITY_TOKEN'].encode()); return
        if self.path=='/health': return self.reply({'ok':True})
        ident=int(self.path.rsplit('/',1)[-1]); key=f'invoice:{ident}'
        hit=cache.get(key)
        if hit: return self.reply(json.loads(hit))
        with db() as conn:
            row=conn.execute('SELECT id,customer,cents,note FROM invoices WHERE id=%s',(ident,)).fetchone()
        if not row: return self.reply({'error':'missing'},404)
        result=dict(zip(['id','customer','cents','note'],row)); cache.set(key,json.dumps(result),ex=60); self.reply(result)
    def do_PATCH(self):
        ident=int(self.path.rsplit('/',1)[-1]); change=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with db() as conn: conn.execute('UPDATE invoices SET cents=%s,note=%s WHERE id=%s',(change['cents'],change.get('note',''),ident))
        cache.delete(f'invoice:{ident}')
        self.reply({'updated':ident})
HTTPServer(('127.0.0.1',int(os.environ['PORT'])),API).serve_forever()

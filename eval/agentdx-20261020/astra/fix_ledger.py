import harness as h
from mcp_client import Client
p=h.RAW/'projects/ledger-a'; app=p/'app.py'; text=app.read_text(); text=text.replace('SELECT id,customer,cents FROM','SELECT id,customer,cents,note FROM').replace("['id','customer','cents']","['id','customer','cents','note']").replace("'UPDATE invoices SET cents=%s WHERE id=%s',(change['cents'],ident)","'UPDATE invoices SET cents=%s,note=%s WHERE id=%s',(change['cents'],change.get('note',''),ident)").replace("        self.reply({'updated':ident})","        cache.delete(f'invoice:{ident}')\n        self.reply({'updated':ident})")
app.write_text(text);(h.HERE/'fixtures/ledger/app.fixed.py').write_text(text)
c=Client('mcp-ledger-fix');c.call('stack_status',p);c.call('stack_up',p,ttl='30m',timeout_secs=30);c.call('stack_status',p);c.call('stack_restart',p,services=['api'],timeout_secs=30)
c.call('stack_exec',p,command=['uv','run','python','test_api.py'],require_all=True);c.call('stack_run',p,task='test');c.close()
# Deliberately controlled inherited endpoint to isolate task-vs-exec precedence.
cmd=[h.D['binary'],'--json','-C',str(p)]
h.receipt('task-env-controlled',cmd+['run','endpoints'],extra={'API_URL':'http://deliberately-wrong.invalid:9'})
h.receipt('exec-env-controlled',cmd+['exec','--','python','-c','import os;print(os.environ["API_URL"])'],extra={'API_URL':'http://deliberately-wrong.invalid:9'})

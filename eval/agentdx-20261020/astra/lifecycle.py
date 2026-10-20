import harness as h,json,os,signal,time,shutil,subprocess
from mcp_client import Client
p=h.RAW/'projects/ledger-a';b=h.RAW/'checkout-b/invoice-app'
# Record the exact target process, verify it is a run-owned Redis before signalling it.
s=json.loads((p/'.stack/session.json').read_text());pid=s['services']['redis']['pid']
r=h.receipt('redis-before-signal',['ps','-p',str(pid),'-o','pid=,command='])
assert 'redis' in r['stdout'] and str(s['services']['redis']['port']) in r['stdout'],r
os.kill(pid,signal.SIGTERM);(h.RAW/'redis-signal.json').write_text(json.dumps({'pid':pid,'signal':'SIGTERM','session_id':s['id']}));time.sleep(.2)
h.stack('death-status',p,'status');h.stack('death-poison-exec',p,'exec','--','python','-c','import os;print(os.environ["REDIS_URL"])');h.stack('death-required-exec',p,'exec','--require-all','--','true');h.stack('death-recovery-up',p,'up','--ttl','30m','--timeout','30s');h.stack('death-recovered-test',p,'exec','--require-all','--','uv','run','python','test_api.py')
# A fresh MCP process resumes the same running session without launch knowledge.
c=Client('mcp-handoff');c.request('initialize',{'protocolVersion':'2025-06-18'});c.call('stack_status',p);c.call('stack_exec',p,command=['uv','run','python','test_api.py'],require_all=True);c.close()
# A stopped checkout is disposed and recreated at precisely the same path.
h.stack('b-before-dispose-down',b,'down');archive=h.RAW/'disposed-b';b.rename(archive);b.mkdir()
for f in archive.iterdir():
    if f.is_file():shutil.copy2(f,b/f.name)
h.stack('b-recreated-compile',b,'compile','--locked');h.stack('b-recreated-up',b,'up','--ttl','30m','--timeout','30s');h.stack('b-recreated-data',b,'exec','--require-all','--','uv','run','python','-c',"import os,psycopg; c=psycopg.connect(os.environ['DATABASE_URL']); row=c.execute(\"select to_regclass('public.invoices')\").fetchone(); print(row); assert row==(None,)")
h.stack('b-recreated-down',b,'down')
# Short unattended lease. No exec/renew between expiry and gc.
q=h.RAW/'projects/lease';shutil.copytree(h.HERE/'fixtures/catalog',q,dirs_exist_ok=True)
h.stack('lease-compile',q,'compile');h.stack('lease-up',q,'up','--ttl','1s','--timeout','30s');time.sleep(2);h.stack('lease-gc',q,'gc');h.stack('lease-status',q,'status');h.stack('lease-down',q,'down')

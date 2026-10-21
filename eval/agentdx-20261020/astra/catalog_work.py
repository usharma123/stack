import harness as h,shutil,json
from mcp_client import Client
p=h.RAW/'projects/catalog';shutil.copytree(h.HERE/'fixtures/catalog',p,dirs_exist_ok=True)
r=json.loads((h.RAW/'resources.json').read_text());r['checkouts'].append(str(p));(h.RAW/'resources.json').write_text(json.dumps(r,indent=2))
c=Client('mcp-catalog');c.call('stack_compile',p);c.call('stack_up',p,ttl='30m',timeout_secs=60);c.call('stack_exec',p,command=['node','test.mjs'],require_all=True)
s=(p/'server.mjs').read_text().replace("const product=JSON.parse(body), rows=read();","const product=JSON.parse(body), rows=read();\n product.sku=product.sku.trim().toUpperCase();")
(p/'server.mjs').write_text(s);(h.HERE/'fixtures/catalog/server.fixed.mjs').write_text(s)
# The deliberately failing baseline inserted a duplicate. Restore the exact seed for the retry.
shutil.copy2(h.HERE/'fixtures/catalog/products.json',p/'products.json')
c.call('stack_status',p);c.call('stack_restart',p,services=['web'],timeout_secs=30);c.call('stack_exec',p,command=['node','test.mjs'],require_all=True);c.call('stack_run',p,task='test');c.call('stack_logs',p,service='web',since_start=True,tail=10);c.close()

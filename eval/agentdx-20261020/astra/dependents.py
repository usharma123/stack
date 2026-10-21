import harness as h,json
p=h.RAW/'projects/catalog';original=(p/'stack.toml').read_text()
(p/'stack.toml').write_text(original.replace('[services.web]\n','[services.web]\ndepends_on=["worker"]\n'));h.stack('dependent-naive-schema',p,'compile');(p/'stack.toml').write_text(original)
s=(p/'server.mjs').read_text().replace(" if(req.url==='/identity')", " if(req.url==='/version'){res.end(JSON.stringify({version:'v1'}));return;}\n if(req.url==='/identity')")
(p/'server.mjs').write_text(s)
(p/'worker.mjs').write_text('''import http from 'node:http';
const loaded=await(await fetch(process.env.WEB_URL+'/version')).json();
http.createServer((req,res)=>res.end(req.url==='/identity'?process.env.STACK_IDENTITY_WORKER:JSON.stringify(loaded))).listen(Number(process.env.PORT),'127.0.0.1');
''')
with (p/'stack.toml').open('a') as f:f.write('''\n[services.worker]\nrun="exec node worker.mjs"\nwatch=["worker.mjs"]\n[services.worker.identity]\ncommand="node -e 'fetch(process.env.WORKER_URL+\\\"/identity\\\").then(r=>r.text()).then(t=>process.stdout.write(t))'"\n''')
h.stack('dependent-compile',p,'compile');h.stack('dependent-up',p,'up','--ttl','30m','--timeout','30s')
(p/'server.mjs').write_text(s.replace("version:'v1'","version:'v2'"));h.stack('dependent-watch',p,'status');h.stack('dependent-restart-web',p,'restart','web','--timeout','30s')
probe="console.log(JSON.stringify({web:await(await fetch(process.env.WEB_URL+'/version')).json(),worker:await(await fetch(process.env.WORKER_URL)).json()}))"
h.stack('dependent-stale-live',p,'exec','--require-all','--','node','--input-type=module','-e',probe);h.stack('dependent-restart-worker',p,'restart','worker','--timeout','30s');h.stack('dependent-recovered-live',p,'exec','--require-all','--','node','--input-type=module','-e',probe)
(h.HERE/'fixtures/catalog/worker.mjs').write_text((p/'worker.mjs').read_text());(h.HERE/'fixtures/catalog/server.dependent.mjs').write_text((p/'server.mjs').read_text());(h.HERE/'fixtures/catalog/stack.dependent.toml').write_text((p/'stack.toml').read_text())

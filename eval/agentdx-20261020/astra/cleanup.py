import harness as h,json,subprocess,socket,shutil,os
projects=[p.parent for p in (h.RAW/'projects').glob('*/stack.toml')]+[h.RAW/'checkout-b/invoice-app']
resources=json.loads((h.RAW/'resources.json').read_text());resources['all_project_dirs']=[str(p) for p in projects];resources['native_observed_services']=[]
# Every receipt, including failed/partial starts and MCP responses, contributes observed identities.
def walk(x):
 if isinstance(x,dict):
  if 'service' in x and ('port' in x or 'pid' in x):resources['native_observed_services'].append({k:v for k,v in x.items() if k in ['service','port','pid','identity','ready']})
  for k,v in x.items():
   if isinstance(v,str) and k=='stdout':
    try:walk(json.loads(v))
    except (ValueError,TypeError):pass
   else:walk(v)
 elif isinstance(x,list):
  for v in x:walk(v)
for f in h.RAW.glob('*.json'):
 if f.name.startswith('linux-') or f.name=='resources.json':continue
 try:walk(json.loads(f.read_text()))
 except ValueError:pass
# Save pre-cleanup sessions and provider status before removing session records.
for p in projects:
 if (p/'.stack/session.json').exists():shutil.copy2(p/'.stack/session.json',h.RAW/('cleanup-session-'+p.name+'.json'))
(h.RAW/'resources.json').write_text(json.dumps(resources,indent=2))
for p in projects:h.stack('cleanup-down-'+p.name,p,'down',timeout=40)
p=h.RAW/'projects/ledger-a'
h.stack('cleanup-supervisor-before',p,'exec','--','pitchfork','supervisor','status','--json')
h.stack('cleanup-supervisor-stop',p,'exec','--','pitchfork','supervisor','stop')
h.stack('cleanup-supervisor-after',p,'exec','--','pitchfork','supervisor','status','--json')
h.stack('cleanup-gc',p,'gc')
ports=sorted({s['port'] for s in resources['native_observed_services'] if isinstance(s.get('port'),int)})
probes=[]
for port in ports:
 with socket.socket() as sock:
  sock.settimeout(.2);probes.append({'port':port,'listening':sock.connect_ex(('127.0.0.1',port))==0})
(h.RAW/'cleanup-native-ports.json').write_text(json.dumps(probes,indent=2))
pids=sorted({s['pid'] for s in resources['native_observed_services'] if isinstance(s.get('pid'),int)})
for file in ['cancel-repro-stack.json','external-signal.json']:
 r=json.loads((h.RAW/file).read_text());pids += list(r.get('tree',{}).values())[:2]
h.receipt('cleanup-native-pids',['ps','-p',','.join(map(str,sorted(set(pids)))),'-o','pid=,ppid=,pgid=,lstart=,command='])
h.receipt('cleanup-owned-process-scan',['/usr/bin/python3','-c',"import subprocess,json; rows=subprocess.check_output(['ps','-axo','pid=,ppid=,command='],text=True).splitlines(); print(json.dumps([r for r in rows if any(x in r for x in "+repr([str(h.ISO),str(h.RAW/'projects'),str(h.RAW/'checkout-b')])+ ") and ' -c import subprocess,json;' not in r],indent=2))"])
before=json.loads((h.RAW/'global-before.json').read_text());after=h.globals_snapshot('global-after');(h.RAW/'global-comparison.json').write_text(json.dumps({'unchanged':before==after,'before_entries':len(before),'after_entries':len(after)},indent=2))
print('CLEANUP',json.dumps({'ports':probes,'global_unchanged':before==after}))

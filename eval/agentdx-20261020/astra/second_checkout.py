import harness as h,shutil,json
p=h.RAW/'projects/ledger-a'; b=h.RAW/'checkout-b'
h.receipt('worktree-b-create',['git','worktree','add','--detach',str(b),'8b3402fa1db8f11917b5ba33a333c9093545d9ce'])
app=b/'invoice-app';app.mkdir(exist_ok=True)
for f in p.iterdir():
    if f.is_file():shutil.copy2(f,app/f.name)
r=json.loads((h.RAW/'resources.json').read_text());r['checkouts']+=[str(p),str(app)];r['git_worktrees']=[str(b)];(h.RAW/'resources.json').write_text(json.dumps(r,indent=2))
h.stack('b-locked-replay',app,'compile','--locked');h.stack('b-up',app,'up','--ttl','30m','--timeout','60s')
h.receipt('b-seed',[h.D['binary'],'--json','-C',str(app),'exec','--require-all','--','uv','run','python','seed.py'],extra={'CUSTOMER':'Beta checkout'})
h.stack('b-migrate',app,'exec','--require-all','--','uv','run','python','migrate.py');h.stack('b-test',app,'exec','--require-all','--','uv','run','python','test_api.py')
for name,project in [('a',p),('b',app)]:
    h.stack(name+'-live-identity',project,'exec','--require-all','--','uv','run','python','-c',"import os,json,psycopg,redis,urllib.request,pathlib,hashlib; c=psycopg.connect(os.environ['DATABASE_URL']); print(json.dumps({'cwd':os.getcwd(),'source_sha':hashlib.sha256(pathlib.Path('app.py').read_bytes()).hexdigest(),'pg':c.execute('select customer,cents,note from invoices').fetchall(),'redis':redis.Redis.from_url(os.environ['REDIS_URL'],decode_responses=True).get('invoice:1'),'api_identity':urllib.request.urlopen(os.environ['API_URL']+'/identity').read().decode(),'endpoints':{k:os.environ[k] for k in ['DATABASE_URL','REDIS_URL','API_URL']}}))")
    shutil.copy2(project/'.stack/session.json',h.RAW/(name+'-live-session.json'))

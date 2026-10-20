import harness as h, shutil,json
p=h.RAW/'projects/ledger-a'; fixture=h.HERE/'fixtures/ledger'
for name in ['app.py','seed.py','migrate.py','test_api.py','pyproject.toml','uv.lock']:
    shutil.copy2(fixture/name,p/name)
h.stack('ledger-seed-existing',p,'exec','--require-all','--','uv','run','python','seed.py',timeout=120)
h.stack('ledger-migration',p,'exec','--require-all','--','uv','run','python','migrate.py')
with (p/'stack.toml').open('a') as f: f.write('''
[services.api]
run = "exec uv run python app.py"
watch = ["app.py"]
[services.api.identity]
command = "python -c 'import os,urllib.request; print(urllib.request.urlopen(os.environ[\"API_URL\"]+\"/identity\").read().decode())'"
'''.replace('os.environ["API_URL"]+"/identity"','os.environ[\\"API_URL\\"]+\\"/identity\\"'))
h.stack('ledger-api-compile',p,'compile')
h.stack('ledger-api-up',p,'up','--ttl','30m','--timeout','60s',timeout=75)
h.stack('ledger-bug-reproduced',p,'run','test')

#!/usr/bin/env python3
"""Pixi package environment plus explicit, project-local service scripts.

Service scripts are benchmark glue, not a native Pixi supervisor feature.
"""
import json
from pathlib import Path
import shlex
import subprocess
import sys
import time
import uuid

root=Path(__file__).resolve().parents[2]
base=Path(sys.argv[1]).resolve()
out=base/'pixi'
out.mkdir(exist_ok=False)
manifest='''[workspace]
name = "comparison"
channels = ["conda-forge"]
platforms = ["linux-aarch64"]
[dependencies]
python = "3.13.*"
uv = "*"
postgresql = "17.*"
redis-server = "8.*"
'''
(out/'pixi.toml').write_text(manifest)
name='cmp-pixi-'+uuid.uuid4().hex[:8]
records=[]
def run(label,body,timeout=600):
    command='export PATH=/bench/bin/linux:$PATH; '+body
    t=time.monotonic()
    p=subprocess.run(['docker','exec','-u','agent',name,'bash','-c',command],stdin=subprocess.DEVNULL,capture_output=True,timeout=timeout)
    records.append(dict(step=label,code=p.returncode,seconds=round(time.monotonic()-t,4),command=command))
    (out/(label+'.stdout')).write_bytes(p.stdout)
    (out/(label+'.stderr')).write_bytes(p.stderr)
    (out/'steps.json').write_text(json.dumps(records,indent=2)+'\n')
    print(label,p.returncode,records[-1]['seconds'],flush=True)
    return p
def env(app,body):
    pg,redis=(45432,46379) if app=='appA' else (45433,46380)
    return f'cd /home/agent/{app}; export UV_PYTHON_DOWNLOADS=never PGDATA=$PWD/pgdata PGHOST=127.0.0.1 PGPORT={pg} DATABASE_URL=postgresql://postgres@127.0.0.1:{pg}/postgres REDIS_URL=redis://127.0.0.1:{redis}/0; pixi run bash -c '+shlex.quote(body)
try:
    subprocess.run(['docker','run','-d','--init','--name',name,'-v',str(base)+':/bench:ro','-v',str(root/'eval/fixture')+':/fixture:ro','ev-base','sleep','infinity'],check=True,stdout=subprocess.DEVNULL)
    run('version','pixi --version')
    for app in ['appA','appB']:
        run(app+'-prepare',f'cp -r /fixture /home/agent/{app}; cp /bench/pixi/pixi.toml /home/agent/{app}/')
        p=run(app+'-install',f'cd /home/agent/{app}; pixi install')
        if p.returncode:
            raise SystemExit('Pixi package solve/install blocked; see raw stderr')
        redis=46379 if app=='appA' else 46380
        p=run(app+'-start',env(app,f'set -e; initdb -D "$PGDATA" -U postgres --auth=trust; pg_ctl -D "$PGDATA" -l "$PWD/postgres.log" -w -o "-p $PGPORT -k /tmp -c listen_addresses=127.0.0.1" start; redis-server --port {redis} --daemonize yes --save "" --appendonly no --pidfile "$PWD/redis.pid" --logfile "$PWD/redis.log"'))
        assert p.returncode==0
        assert run(app+'-tests',env(app,'set -e; uv sync -q; uv run pytest -q')).returncode==0
        run(app+'-identity',env(app,'psql "$DATABASE_URL" -Atc "show data_directory"'))
    run('resolved-versions',env('appA','python --version; uv --version; postgres --version; redis-server --version'))
    for i in range(15): run('exec-'+str(i),env('appA','true'))
    run('stop-A',env('appA','pg_ctl -D "$PGDATA" -m fast -w stop; redis-cli -u "$REDIS_URL" shutdown'))
    assert run('B-survives',env('appB','uv run pytest -q')).returncode==0
    run('stop-B',env('appB','pg_ctl -D "$PGDATA" -m fast -w stop; redis-cli -u "$REDIS_URL" shutdown'))
    run('lock','cat /home/agent/appA/pixi.lock')
finally:
    subprocess.run(['docker','rm','-f',name],stdout=subprocess.DEVNULL)

#!/usr/bin/env python3
"""Two Compose projects with health checks, private networks, and marker isolation."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

from release_artifacts import host_tools

root=Path(sys.argv[1]).resolve()
out=root/'compose'
out.mkdir(exist_ok=False)
binary=host_tools(root)/'docker-compose'
config=out/'compose.json'
config.write_text(json.dumps({'services':{
    'postgres':{'image':'postgres:17.6-alpine','environment':{'POSTGRES_HOST_AUTH_METHOD':'trust'},
                'healthcheck':{'test':['CMD','pg_isready','-U','postgres'],'interval':'1s','timeout':'3s','retries':30}},
    'redis':{'image':'redis:8-alpine','healthcheck':{'test':['CMD','redis-cli','ping'],'interval':'1s','timeout':'3s','retries':30}},
}},indent=2)+'\n')
projects=['cmp-'+uuid.uuid4().hex[:8] for _ in range(2)]
records=[]
docker_config=out/'docker-config'
docker_config.mkdir()
endpoint=subprocess.check_output(['docker','context','inspect','--format','{{.Endpoints.docker.Host}}'],text=True).strip()
env=dict(os.environ,DOCKER_CONFIG=str(docker_config),DOCKER_HOST=endpoint)
def run(label,project,args,timeout=180):
    cmd=[str(binary),'-f',str(config),'-p',project]+args
    t=time.monotonic()
    p=subprocess.run(cmd,capture_output=True,timeout=timeout,stdin=subprocess.DEVNULL,env=env)
    records.append(dict(step=label,code=p.returncode,seconds=round(time.monotonic()-t,4),command=cmd))
    (out/(label+'.stdout')).write_bytes(p.stdout)
    (out/(label+'.stderr')).write_bytes(p.stderr)
    (out/'steps.json').write_text(json.dumps(records,indent=2)+'\n')
    print(label,p.returncode,records[-1]['seconds'],flush=True)
    return p
try:
    run('version',projects[0],['version'])
    run('pull',projects[0],['pull'])
    for i,project in enumerate(projects):
        assert run('up-'+str(i),project,['up','-d','--wait']).returncode==0
        assert run('set-pg-'+str(i),project,['exec','-T','postgres','psql','-U','postgres','-c',
               "CREATE TABLE marker (v text); INSERT INTO marker VALUES ('"+project+"');"]).returncode==0
        assert run('set-redis-'+str(i),project,['exec','-T','redis','redis-cli','set','marker',project]).returncode==0
    for i,project in enumerate(projects):
        assert run('get-pg-'+str(i),project,['exec','-T','postgres','psql','-U','postgres','-Atc','select v from marker']).stdout.decode().strip()==project
        assert run('get-redis-'+str(i),project,['exec','-T','redis','redis-cli','get','marker']).stdout.decode().strip()==project
    for i in range(15): run('exec-'+str(i),projects[0],['exec','-T','postgres','true'])
    run('up-again',projects[0],['up','-d','--wait'])
    run('down-A',projects[0],['down','-v'])
    assert run('B-survives',projects[1],['exec','-T','postgres','psql','-U','postgres','-Atc','select v from marker']).stdout.decode().strip()==projects[1]
    run('state',projects[1],['ps','--format','json'])
finally:
    cleanup_errors=[]
    for i,project in enumerate(projects):
        try:
            if run('cleanup-'+str(i),project,['down','-v']).returncode:
                cleanup_errors.append(project+' down failed')
        except Exception as error:
            cleanup_errors.append(str(error))
    p=subprocess.run(['docker','ps','-a','--format','{{.Names}}'],capture_output=True,text=True)
    assert p.returncode==0 and not any(project in p.stdout for project in projects)
    if cleanup_errors:
        raise RuntimeError('; '.join(cleanup_errors))

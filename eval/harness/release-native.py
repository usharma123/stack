#!/usr/bin/env python3
"""Run the matching published host binary through native E2E and pilot."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import uuid

from release_artifacts import host_tools, released_binary

root=Path(__file__).resolve().parents[2]
base=Path(sys.argv[1]).resolve()
binary,info=released_binary(base)
if any((base/name).exists() for name in ('native-outcomes.json','native-scenarios.jsonl','native-progress.log','git-lock','pilot')):
    raise SystemExit('refusing to overwrite native receipts; use a new results directory')
assert info['commit']==subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
registry='cmp-native-reg-'+uuid.uuid4().hex[:8]
env=dict(os.environ,PATH=str(host_tools(base))+os.pathsep+os.environ['PATH'])
outcomes=[]
def run(label,args,timeout=900):
    with (base/(label+'-progress.log')).open('w') as f:
        p=subprocess.run(args,cwd=root,env=env,stdout=f,stderr=subprocess.STDOUT,timeout=timeout)
    outcomes.append({'step':label,'code':p.returncode})
    (base/'native-outcomes.json').write_text(json.dumps(outcomes,indent=2)+'\n')
    print(label,p.returncode,flush=True)
try:
    subprocess.run(['docker','run','-d','--name',registry,'-p','127.0.0.1::5000','registry:2'],check=True,stdout=subprocess.DEVNULL)
    endpoint=subprocess.check_output(['docker','port',registry,'5000/tcp'],text=True).strip()
    env.update(STACK_E2E_REGISTRY=endpoint,STACK_E2E_RESULTS=str(base/'native-scenarios.jsonl'))
    run('native',['bash','tests/e2e/native.sh',str(binary)])
finally:
    subprocess.run(['docker','rm','-f',registry],stdout=subprocess.DEVNULL)
run('git-lock',['python3','eval/harness/stack-lock.py',str(binary),str(base/'git-lock')])
run('pilot',['python3','eval/harness/pilot.py','--stack',str(binary),'--projects','4','--rounds','2','--out',str(base/'pilot')])

sys.exit(1 if any(row["code"] for row in outcomes) else 0)

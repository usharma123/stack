#!/usr/bin/env python3
"""Run this checkout's scenarios against its matching published Linux binary."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time
import uuid

root=Path(__file__).resolve().parents[2]
base=Path(sys.argv[1]).resolve()
out=base/'release-e2e'
out.mkdir(exist_ok=False)
info=json.loads((base/'package/package/build-info.json').read_text())
if not (info['commit']==subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()):
    raise SystemExit("published artifact identity mismatch")
binary=base/'package/package/binaries/linux-arm64/stack'
if not (hashlib.sha256(binary.read_bytes()).hexdigest()==info['hashes']['linux-arm64']):
    raise SystemExit("published artifact identity mismatch")
name='cmp-e2e-'+uuid.uuid4().hex[:8]
registry=name+'-reg'
records=[]
def run(args,**kwargs): return subprocess.run(args,check=True,**kwargs)
try:
    run(['docker','run','-d','--init','--name',name,'-v',str(root)+'/examples:/examples:ro',
        '-v',str(root)+'/tests/e2e:/scripts:ro','-v',str(base)+':/bench:ro','ev-mise'],stdout=subprocess.DEVNULL)
    run(['docker','run','-d','--name',registry,'--network','container:'+name,'registry:2'],stdout=subprocess.DEVNULL)
    run(['docker','exec',name,'bash','-c','mkdir -p /srv /opt/stack; chown agent /srv; cp /scripts/assert.sh /tmp/stack-e2e-assert.sh; cp /bench/package/package/binaries/linux-arm64/stack /opt/stack/; cp /bench/bin/linux/mise /usr/local/bin/mise'])
    for script in sorted((root/'tests/e2e').glob('[0-9]-*.sh')):
        start=time.monotonic()
        with (out/(script.stem+'.log')).open('w') as log:
            p=subprocess.run(['docker','exec','-u','agent',name,'bash','/scripts/'+script.name],stdout=log,stderr=subprocess.STDOUT,timeout=600)
        records.append(dict(scenario=script.stem,code=p.returncode,seconds=round(time.monotonic()-start,3)))
        (out/'scenarios.json').write_text(json.dumps(records,indent=2)+'\n')
        print(records[-1],flush=True)
        if p.returncode: break
    run(['docker','cp',name+':/tmp',str(out/'evidence')],stdout=subprocess.DEVNULL)
finally:
    subprocess.run(['docker','rm','-f',registry,name],stdout=subprocess.DEVNULL)

sys.exit(0 if len(records)==len(list((root/'tests/e2e').glob('[0-9]-*.sh'))) and all(row['code']==0 for row in records) else 1)

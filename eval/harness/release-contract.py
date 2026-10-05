#!/usr/bin/env python3
"""Probe published CLI failure contracts in disposable projects."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from release_artifacts import host_tools, released_binary

base=Path(sys.argv[1]).resolve()
out=base/'contract'
out.mkdir(exist_ok=False)
binary,_=released_binary(base)
rows=[]
with tempfile.TemporaryDirectory(prefix='scontract-',dir='/tmp') as work:
    w=Path(work)
    env={k:v for k,v in os.environ.items() if not k.startswith(('MISE_','__MISE','PITCHFORK_','STACK_'))}
    env.update(PATH=str(host_tools(base))+os.pathsep+os.environ['PATH'],HOME=str(w/'home'),
               STACK_CACHE_DIR=str(w/'cache'),STACK_STATE_DIR=str(w/'state'))
    (w/'home').mkdir()
    def check(name,args,code):
        p=subprocess.run([str(binary),'-C',str(w),'--json']+args,env=env,capture_output=True,text=True,timeout=90)
        (out/(name+'.stdout')).write_text(p.stdout)
        (out/(name+'.stderr')).write_text(p.stderr)
        body=json.loads(p.stdout)
        assert p.returncode!=0 and not body['ok'] and body['error']['code']==code,(name,p.returncode,p.stdout,p.stderr)
        rows.append({'case':name,'exit_code':p.returncode,'error_code':code,'single_json_object':True})
    check('invalid-subcommand',['not-a-command'],'usage')
    (w/'stack.toml').write_text('[tools]\nnode = "99.0.0"\n')
    check('impossible-version',['compile'],'resolve_failed')
    (w/'stack.toml').write_text('[tools]\ndefinitely-not-a-pkg-zz = "latest"\n')
    check('unknown-tool',['compile'],'resolve_failed')
    for name,value in [('a','one'),('b','two')]:
        (w/name).mkdir()
        (w/name/'bundle.toml').write_text(f'[bundle]\nname = "{name}"\n[env]\nVALUE = "{value}"\n')
    (w/'stack.toml').write_text('[[use]]\nbundle = "path:./a"\n[[use]]\nbundle = "path:./b"\n')
    check('conflicting-bundles',['compile'],'conflict')
(out/'summary.json').write_text(json.dumps(rows,indent=2)+'\n')
print(json.dumps(rows,indent=2))

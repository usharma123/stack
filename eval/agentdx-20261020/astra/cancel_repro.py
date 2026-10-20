import harness as h,subprocess,json,os,time,signal
p=h.RAW/'projects/tools'
code="import subprocess,time,pathlib,os,json; p=subprocess.Popen(['sleep','15']);pathlib.Path('cancel-tree.json').write_text(json.dumps({'parent':os.getpid(),'child':p.pid,'group':os.getpgrp()}));print('started',flush=True);time.sleep(15)"
cmd=[h.D['binary'],'--json','-C',str(p),'exec','--','/usr/bin/python3','-c',code]
proc=subprocess.Popen(cmd,env=h.env(),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
for _ in range(100):
 if (p/'cancel-tree.json').exists():break
 time.sleep(.05)
tree=json.loads((p/'cancel-tree.json').read_text());time.sleep(.5)
h.receipt('cancel-repro-tree-before',['ps','-p',','.join(map(str,[proc.pid,tree['parent'],tree['child']])),'-o','pid=,ppid=,pgid=,lstart=,command='])
os.kill(proc.pid,signal.SIGTERM);out,err=proc.communicate(timeout=10)
(h.RAW/'cancel-repro-stack.json').write_text(json.dumps({'argv':cmd,'stack_pid':proc.pid,'tree':tree,'exit':proc.returncode,'stdout':out,'stderr':err},indent=2))
for i in range(3):
 time.sleep(.4);h.receipt('cancel-repro-after-'+str(i),['ps','-p',','.join(map(str,[tree['parent'],tree['child']])),'-o','pid=,ppid=,pgid=,lstart=,command='])
# Every owned descendant has a 15-second natural bound. Let it end, then verify; no unrelated kill.

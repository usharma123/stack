import harness as h,subprocess,os,signal,time,json
p=h.RAW/'projects/tools'
code="import subprocess,time,pathlib; p=subprocess.Popen(['sleep','30']);pathlib.Path('child.pid').write_text(str(p.pid));print('spawned',p.pid,flush=True);time.sleep(30)"
h.stack('timeout-descendant',p,'exec','--timeout','1s','--','/usr/bin/python3','-c',code)
pid=int((p/'child.pid').read_text());h.receipt('timeout-descendant-after',['ps','-p',str(pid),'-o','pid=,stat=,command='])
# External cancellation while a real Stack command is running.
cmd=[h.D['binary'],'--json','-C',str(p),'exec','--','/usr/bin/python3','-c',code.replace('child.pid','cancel-child.pid')]
proc=subprocess.Popen(cmd,env=h.env(),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,start_new_session=True)
for _ in range(100):
 if (p/'cancel-child.pid').exists():break
 time.sleep(.05)
assert (p/'cancel-child.pid').exists();os.kill(proc.pid,signal.SIGTERM)
out,err=proc.communicate(timeout=15);(h.RAW/'external-signal.json').write_text(json.dumps({'argv':cmd,'stack_pid':proc.pid,'signal':'SIGTERM','exit':proc.returncode,'stdout':out,'stderr':err},indent=2))
h.receipt('external-signal-child-after',['ps','-p',(p/'cancel-child.pid').read_text(),'-o','pid=,stat=,command='])
# Failure after a service has actually launched; preserve its session before down.
q=h.RAW/'projects/partial';q.mkdir(exist_ok=True);(q/'stack.toml').write_text('''[tools]\nnode="24"\n[services.web]\nrun="exec node -e 'require(\\\"node:http\\\").createServer((q,s)=>s.end(\\\"ok\\\")).listen(process.env.PORT)'"\nready_cmd="sleep 30; false"\n''')
h.stack('partial-compile',q,'compile');h.stack('partial-up-timeout',q,'up','--timeout','2s','--ttl','30m',timeout=20);h.stack('partial-status',q,'status');h.stack('partial-down',q,'down')

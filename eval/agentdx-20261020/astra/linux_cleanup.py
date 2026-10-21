import harness as h,json
N='agentdx-astra-'+h.ISO.name
def run(label,*args,root=False):return h.receipt(label,['docker','exec',* ([] if root else ['--user','runner','-e','HOME=/home/runner']),N,'bash','/work/'+('root-env.sh' if root else 'env.sh'),*args],timeout=135)
run('linux-session-before-cleanup','cat','/work/run/ledger/.stack/session.json')
run('linux-api-failure-logs','stack','--json','logs','api','--tail','35')
for root in [False,True]:
 name='linux-root-cleanup' if root else 'linux-cleanup'
 run(name+'-down','stack','--json','down',root=root)
 run(name+'-supervisor-stop','stack','--json','exec','--','pitchfork','supervisor','stop',root=root)
 run(name+'-supervisor-status','stack','--json','exec','--','pitchfork','supervisor','status','--json',root=root)
 run(name+'-gc','stack','--json','gc',root=root)
run('linux-cleanup-port-probes','stack','--json','exec','--','python','-c',"import socket,json; ports=[23308,22374,28591,24724,27488,26046];print(json.dumps({p:socket.socket().connect_ex(('127.0.0.1',p))==0 for p in ports}))")
h.receipt('linux-cleanup-ps',['docker','exec',N,'ps','-eo','pid,ppid,state,args'])
# Preserve project/state/log artifacts before removing the run-owned container. Installed tool caches are reproducible.
h.receipt('linux-preserve',['docker','exec',N,'tar','-czf','/tmp/evidence.tgz','--exclude=.venv','--exclude=cache','--exclude=installs','--exclude=downloads','/work/run/ledger','/work/run/pf','/work/run/stack','/work/rootrun/ledger','/work/rootrun/pf','/work/rootrun/stack'])
h.receipt('linux-evidence-copy',['docker','cp',N+':/tmp/evidence.tgz',str(h.RAW/'linux-evidence.tgz')])
h.receipt('linux-container-remove',['docker','rm','-f',N])
h.receipt('linux-container-absent',['docker','ps','-a','--filter','name=^/'+N+'$','--format','{{.ID}} {{.Names}}'])

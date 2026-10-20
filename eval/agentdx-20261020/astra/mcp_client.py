import harness as h, subprocess,json,selectors,time,os,signal
class Client:
    def __init__(self,label,cwd=None):
        self.label=label; self.rows=[]; self.seq=0
        self.p=subprocess.Popen([h.D['binary'],'mcp'],cwd=cwd or h.RAW,env=h.env(),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(h.RAW/(label+'.stderr'),'w'),text=True,start_new_session=True)
    def request(self,method,params=None,timeout=90):
        self.seq+=1; msg={'jsonrpc':'2.0','id':self.seq,'method':method,'params':params or {}}
        self.p.stdin.write(json.dumps(msg)+'\n'); self.p.stdin.flush(); start=time.monotonic()
        sel=selectors.DefaultSelector();sel.register(self.p.stdout,selectors.EVENT_READ)
        if not sel.select(timeout):
            os.killpg(self.p.pid,signal.SIGTERM); raise TimeoutError(method)
        reply=json.loads(self.p.stdout.readline()); sel.close()
        self.rows.append({'request':msg,'response':reply,'seconds':round(time.monotonic()-start,3)});self.save()
        print(json.dumps({'method':method,'result':reply})[:900]);return reply
    def call(self,name,dir,**kwargs): return self.request('tools/call',{'name':name,'arguments':{'dir':str(dir),**kwargs}})
    def save(self): (h.RAW/(self.label+'.json')).write_text(json.dumps(self.rows,indent=2))
    def close(self): self.p.stdin.close();self.p.wait(timeout=10);self.save()
if __name__=='__main__':
    c=Client('mcp-discovery');c.request('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'astra-independent-eval','version':'1'}});c.request('tools/list');p=h.RAW/'projects/ledger-a'
    c.call('stack_exec',p,command='echo hi');c.call('stack_exec',p,command=['echo','hi'],timeout=1);c.call('stack_compile',p,locked=True,update=True);c.call('unknown_tool',p);c.call('stack_status',p);c.close()

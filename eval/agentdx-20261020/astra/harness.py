"""Receipt runner. No product implementation changes. All spawned commands are bounded."""
import os, sys, json, time, subprocess, signal, pathlib, hashlib, tempfile
HERE=pathlib.Path(__file__).resolve().parent
REPO=HERE.parents[2]
POINTER=HERE/'run.json'
def init():
    raw=REPO/'eval/results'/('agentdx-astra-'+time.strftime('%Y%m%d-%H%M%S'))
    raw.mkdir(parents=True)
    iso=pathlib.Path(tempfile.mkdtemp(prefix='ax-',dir='/tmp'))
    data={'raw':str(raw),'iso':str(iso),'repo':str(REPO),'binary':str(raw/'build/debug/stack')}
    POINTER.write_text(json.dumps(data,indent=2))
    (raw/'resources.json').write_text(json.dumps({'isolation_root':str(iso),'checkouts':[],'containers':[]},indent=2))
    return data
D=init() if len(sys.argv)>1 and sys.argv[1]=='new' else (json.loads(POINTER.read_text()) if POINTER.exists() else init())
RAW=pathlib.Path(D['raw']); ISO=pathlib.Path(D['iso'])
def env():
    e={k:v for k,v in os.environ.items() if not k.startswith(('STACK_','MISE_','__MISE','PITCHFORK_','npm_config_','NPM_CONFIG_','UV_','FNOX_','XDG_'))}
    mappings={'STACK_STATE_DIR':'stack/state','STACK_DATA_DIR':'stack/data','STACK_CACHE_DIR':'stack/cache','MISE_DATA_DIR':'mise/data','MISE_CACHE_DIR':'mise/cache','MISE_STATE_DIR':'mise/state','MISE_CONFIG_DIR':'mise/config','MISE_GLOBAL_CONFIG_FILE':'mise/config/config.toml','PITCHFORK_CONFIG_DIR':'pf/config','PITCHFORK_STATE_DIR':'pf/state','PITCHFORK_LOGS_DIR':'pf/logs','XDG_CONFIG_HOME':'xdg/config','XDG_DATA_HOME':'xdg/data','XDG_CACHE_HOME':'xdg/cache','XDG_STATE_HOME':'xdg/state','UV_CACHE_DIR':'uv/cache','UV_PYTHON_INSTALL_DIR':'uv/python','UV_TOOL_DIR':'uv/tools','npm_config_cache':'npm/cache','npm_config_userconfig':'npm/npmrc','npm_config_globalconfig':'npm/global-npmrc','npm_config_prefix':'npm/prefix','FNOX_CONFIG_DIR':'fnox/config'}
    e.update({k:str(ISO/v) for k,v in mappings.items()}); e['CI']='1'; e['CARGO_TARGET_DIR']=str(RAW/'build')
    return e

def receipt(label,cmd,cwd=None,timeout=120,extra=None):
    e=env(); e.update(extra or {}); start=time.time(); p=subprocess.Popen(cmd,cwd=cwd or REPO,env=e,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
    expired=False
    try: out,err=p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        expired=True; os.killpg(p.pid,signal.SIGTERM)
        try: out,err=p.communicate(timeout=8)
        except subprocess.TimeoutExpired: os.killpg(p.pid,signal.SIGKILL); out,err=p.communicate()
    r={'label':label,'argv':cmd,'cwd':str(cwd or REPO),'pid':p.pid,'exit':p.returncode,'outer_timeout':expired,'seconds':round(time.time()-start,3),'stdout':out.decode(errors='replace'),'stderr':err.decode(errors='replace')}
    (RAW/(label+'.json')).write_text(json.dumps(r,indent=2)); print(json.dumps(r,indent=2)); return r

def stack(label,project,*args,timeout=120): return receipt(label,[D['binary'],'--json','-C',str(project),*args],timeout=timeout)
def globals_snapshot(label):
    roots=[pathlib.Path.home()/p for p in ['.config/pitchfork','.local/state/pitchfork','.config/mise']]
    rows={}
    for root in roots:
        rows[str(root)]='exists' if root.exists() else 'absent'
        if root.exists():
            for p in sorted(root.rglob('*')):
                if p.is_file(): rows[str(p)]=hashlib.sha256(p.read_bytes()).hexdigest()
    (RAW/(label+'.json')).write_text(json.dumps(rows,indent=2)); return rows
if __name__=='__main__':
    if sys.argv[1] in ('init','new'):
        globals_snapshot('global-before'); (RAW/'isolated-env.json').write_text(json.dumps({k:v for k,v in env().items() if str(ISO) in v},indent=2)); print(json.dumps(D))
    elif sys.argv[1]=='run': receipt(sys.argv[2],sys.argv[3:])
    elif sys.argv[1]=='stack': stack(sys.argv[2],sys.argv[3],*sys.argv[4:])

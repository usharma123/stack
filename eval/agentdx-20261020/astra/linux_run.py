import harness as h,json
N='agentdx-astra-'+h.ISO.name
script='''export STACK_STATE_DIR=/work/run/stack/state STACK_DATA_DIR=/work/run/stack/data STACK_CACHE_DIR=/work/run/stack/cache
export MISE_DATA_DIR=/work/run/mise/data MISE_CACHE_DIR=/work/run/mise/cache MISE_STATE_DIR=/work/run/mise/state MISE_CONFIG_DIR=/work/run/mise/config MISE_GLOBAL_CONFIG_FILE=/work/run/mise/config/config.toml
export PITCHFORK_CONFIG_DIR=/work/run/pf/config PITCHFORK_STATE_DIR=/work/run/pf/state PITCHFORK_LOGS_DIR=/work/run/pf/logs
export XDG_CONFIG_HOME=/work/run/xdg/config XDG_DATA_HOME=/work/run/xdg/data XDG_CACHE_HOME=/work/run/xdg/cache XDG_STATE_HOME=/work/run/xdg/state UV_CACHE_DIR=/work/run/uv/cache UV_PYTHON_INSTALL_DIR=/work/run/uv/python
export npm_config_cache=/work/run/npm/cache npm_config_userconfig=/work/run/npm/npmrc npm_config_globalconfig=/work/run/npm/global-npmrc npm_config_prefix=/work/run/npm/prefix
export CI=1 PATH=/work/src/target/debug:/usr/local/bin:/usr/bin:/bin
cd /work/run/ledger
exec timeout -k 5 120 "$@"
'''
(h.RAW/'linux-env.sh').write_text(script)
h.receipt('linux-user',['docker','exec',N,'bash','-c','useradd -m runner && mkdir -p /work/run/ledger && chown -R runner:runner /work/run'])
h.receipt('linux-env-copy',['docker','cp',str(h.RAW/'linux-env.sh'),N+':/work/env.sh'])
p=h.RAW/'projects/ledger-a'
for f in ['stack.toml','stack.lock','app.py','seed.py','migrate.py','test_api.py','pyproject.toml','uv.lock']:
    h.receipt('linux-copy-'+f.replace('.','-'),['docker','cp',str(p/f),N+':/work/run/ledger/'+f])
h.receipt('linux-permissions',['docker','exec',N,'chown','-R','runner:runner','/work/run'])
def run(label,*args):return h.receipt(label,['docker','exec','--user','runner','-e','HOME=/home/runner',N,'bash','/work/env.sh',*args],timeout=135)
run('linux-platform','sh','-c','uname -a; id; stack --version; sha256sum /work/src/target/debug/stack')
run('linux-clean-setup','stack','--json','setup')
run('linux-locked-replay','stack','--json','compile','--locked')
run('linux-up','stack','--json','up','--ttl','30m','--timeout','90s')
run('linux-seed','stack','--json','exec','--require-all','--','uv','run','python','seed.py')
run('linux-migrate','stack','--json','exec','--require-all','--','uv','run','python','migrate.py')
run('linux-app-integration','stack','--json','exec','--require-all','--','uv','run','python','test_api.py')
run('linux-task-endpoints','stack','--json','run','endpoints')
run('linux-task-test','stack','--json','run','test')

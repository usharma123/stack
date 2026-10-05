#!/usr/bin/env python3
"""Release comparison, isolated containers, raw logs and timed steps.

Usage: python3 eval/harness/current-benchmark.py RESULTS_DIR TOOL...
RESULTS_DIR contains package/package and bin/linux/mise downloaded separately.
No product code is built or changed. Timings include docker exec transport.
"""
import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
OUT = Path(sys.argv[1]).resolve()
q = shlex.quote


def benchmark(tool):
    out = OUT / tool
    out.mkdir(parents=True, exist_ok=False)
    dynamic = tool.endswith('-dynamic')
    if dynamic:
        tool = tool.removesuffix('-dynamic')
    configured = tool.endswith('-configured')
    if configured:
        tool = tool.removesuffix('-configured')
    latest_devenv = tool == 'devenv-latest'
    if latest_devenv:
        tool = 'devenv'
    name = 'comparison-' + tool + '-' + uuid.uuid4().hex[:7]
    image = 'ev-' + ('mise' if tool == 'stack' else tool)
    records = []

    def run(label, command, timeout=600, user='agent'):
        start = time.monotonic()
        try:
            p = subprocess.run(['docker', 'exec', '-u', user, name, 'bash', '-c', command],
                               stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout)
            code, stdout, stderr = p.returncode, p.stdout, p.stderr
        except subprocess.TimeoutExpired as e:
            code, stdout, stderr = 124, e.stdout or b'', e.stderr or b''
        record = dict(step=label, code=code, seconds=round(time.monotonic()-start, 4), command=command)
        records.append(record)
        (out / (label+'.stdout')).write_bytes(stdout)
        (out / (label+'.stderr')).write_bytes(stderr)
        (out / 'steps.json').write_text(json.dumps(records, indent=2)+'\n')
        print(tool, label, code, record['seconds'], flush=True)
        return code, stdout.decode(errors='replace')

    try:
        subprocess.run(['docker', 'run', '-d', '--init', '--name', name,
                        '-v', str(ROOT)+':/repo:ro', '-v', str(OUT)+':/bench:ro', image], check=True,
                       stdout=subprocess.DEVNULL)
        metadata = dict(image=subprocess.check_output(['docker','image','inspect',image,'--format','{{.Id}}'],text=True).strip(),
                        started_utc=time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),
                        harness_commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip())
        (out/'metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
        run('prepare', 'mkdir -p /srv; chown agent /srv; cp /bench/bin/linux/mise /usr/local/bin/mise; '
            'chmod 755 /usr/local/bin/mise; '
            'if test -f /usr/local/bin/devbox; then chmod a+rx /usr/local/bin/devbox; fi', user='root')
        prefix = 'export PATH=/bench/package/package/binaries/linux-arm64:$PATH MISE_YES=1 FLOX_DISABLE_METRICS=true; '
        if tool == 'stack':
            info=json.loads((OUT/'package/package/build-info.json').read_text())
            binary=OUT/'package/package/binaries/linux-arm64/stack'
            assert hashlib.sha256(binary.read_bytes()).hexdigest()==info['hashes']['linux-arm64']
        if tool == 'devenv':
            upgrade = 'nix profile add --profile /nix/var/nix/profiles/default --priority 4 --accept-flake-config github:cachix/devenv/v2.4.0#devenv' if latest_devenv else 'nix profile upgrade --profile /nix/var/nix/profiles/default devenv'
            code,_=run('upgrade', upgrade, user='root', timeout=900)
            if code:
                return
        version = {'stack':'stack --version; mise --version','mise':'mise --version','flox':'flox --version; nix --version',
                   'devbox':'devbox version; nix --version','devenv':'devenv version; nix --version','nix':'nix --version'}[tool]
        run('versions',prefix+version)
        def setup(app):
            prep=f'cp -r /repo/eval/fixture /srv/{app}; cd /srv/{app}; '
            if tool=='stack':
                prep+='cp /repo/examples/app/stack.toml .; test -d /srv/bundles || cp -r /repo/examples/bundles /srv/bundles'
            elif tool=='mise': prep+='cp /repo/eval/configs/mise-daemons/mise.toml .; mise trust'
            elif tool=='flox': prep+='flox init --no-auto-setup; cp /repo/eval/configs/flox/manifest.toml .flox/env/manifest.toml'
            elif tool=='devenv': prep+='devenv init; cp /repo/eval/configs/devenv/devenv.nix .'
            elif tool=='devbox': prep+='cp /repo/eval/configs/devbox/devbox.json .'
            elif tool=='nix': prep+='cp /repo/eval/configs/nix/flake.nix .'
            if dynamic and tool=='devenv':
                prep+="; sed -i 's/config.services.postgres.port/config.processes.postgres.ports.main.value/g; s/config.services.redis.port/config.processes.redis.ports.main.value/g' devenv.nix"
            if configured and app=='appB':
                if tool=='mise':
                    prep+="; sed -i '0,/port = \"auto\"/s/port = \"auto\"/port = 55432/; s/port = \"auto\"/port = 56379/' mise.toml"
                elif tool=='flox':
                    prep+="; sed -i 's/5432/55432/g; s/6379/56379/g' .flox/env/manifest.toml"
                elif tool=='devenv':
                    prep+="; sed -i '/services.postgres = {/a\\    port = 55432;' devenv.nix; sed -i '/services.redis.enable/a\\  services.redis.port = 56379;' devenv.nix"
                elif tool=='devbox':
                    prep+="; jq '.env.PGPORT = \"55432\" | .env.REDIS_PORT = \"56379\" | .env.DATABASE_URL = \"postgresql://postgres@127.0.0.1:55432/postgres\" | .env.REDIS_URL = \"redis://127.0.0.1:56379/0\"' devbox.json >devbox.new.json; mv devbox.new.json devbox.json"
            return run(app+'-prepare',prefix+prep)
        def cmd(app,body): return prefix+f'cd /srv/{app}; '+body
        def enter(body):
            base={'stack':'stack exec --','mise':'mise exec --','flox':'flox activate --','devenv':'devenv shell --no-tui --',
                  'devbox':'devbox run --','nix':'nix develop --command'}[tool]
            return base+' bash -c '+q(body)
        install={'stack':'stack compile --json && stack up --json','mise':'mise install && mise daemons start',
                 'flox':'flox activate -- true','devenv':'devenv shell --no-tui -- true',
                 'devbox':'devbox install','nix':'nix develop --command true'}[tool]
        setup('appA')
        code,_=run('setup-first',cmd('appA',install),timeout=900)
        if code: return
        run('resolved-versions',cmd('appA',enter('python --version; uv --version; postgres --version; redis-server --version')))
        for i in range(15): run(f'exec-{i:02}',cmd('appA',enter('true')))
        start={'stack':'stack up --json','mise':'mise daemons start',
               'flox':'nohup flox activate --start-services -- sleep infinity >/tmp/hold-A.log 2>&1 </dev/null &',
               'devenv':'devenv up -d --no-tui && devenv processes wait --timeout 60','devbox':'devbox services up -b',
               'nix':'true'}[tool]
        if tool!='nix':
            run('start',cmd('appA',start))
            # Probe inside the environment, with shell expansion occurring there.
            probe='bash /repo/eval/configs/readiness.sh'
            run('readiness',cmd('appA',enter(probe)),timeout=120)
            run('tests',cmd('appA',enter('set -e; uv sync -q; uv run pytest -q')))
            run('identity-A',cmd('appA',enter('printf "DATABASE_URL=%s REDIS_URL=%s\n" "$DATABASE_URL" "$REDIS_URL"; psql "$DATABASE_URL" -Atc "show data_directory"')))
            if tool=='stack':
                for i in range(15): run(f'exec-required-{i:02}',cmd('appA','stack exec --require-all -- true'))
            run('start-again',cmd('appA',start if tool!='flox' else 'flox activate --start-services -- true'))
        setup('appB')
        run('setup-second',cmd('appB',install),timeout=900)
        if tool!='nix':
            start_b=start.replace('/tmp/hold-A.log','/tmp/hold-B.log')
            run('start-B',cmd('appB',start_b))
            # Allow service managers to settle; this delay is not a latency metric.
            run('settle-B','sleep 3')
            run('identity-B',cmd('appB',enter('printf "DATABASE_URL=%s REDIS_URL=%s\n" "$DATABASE_URL" "$REDIS_URL"; psql "$DATABASE_URL" -Atc "show data_directory"')))
            run('tests-B',cmd('appB',enter('set -e; uv sync -q; uv run pytest -q')))
            status={'stack':'stack status --json','mise':'mise daemons status --json','flox':'flox services status --json',
                    'devenv':'devenv processes list','devbox':'devbox services ls'}[tool]
            run('status-B',cmd('appB',status))
            stop={'stack':'stack down --json','mise':'mise daemons stop','flox':'flox activate -- flox services stop',
                  'devenv':'devenv down','devbox':'devbox services stop'}[tool]
            run('stop-A',cmd('appA',stop))
            run('B-after-A-stop',cmd('appB',enter('uv run pytest -q')))
            run('stop-B',cmd('appB',stop))
            run('post-stop-env',cmd('appA',enter('printf "DATABASE_URL=%s REDIS_URL=%s\n" "$DATABASE_URL" "$REDIS_URL"')))
            run('leftovers','ps -eo pid,ppid,stat,comm,args')
        run('locks','find /srv -maxdepth 4 -type f \\( -name "*lock*" -o -name "*toml" -o -name "devenv.yaml" \\) -print -exec cat {} \\;')
    finally:
        subprocess.run(['docker','rm','-f',name],stdout=subprocess.DEVNULL)

for tool in sys.argv[2:]: benchmark(tool)

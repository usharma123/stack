#!/usr/bin/env python3
"""Scripted Stack regression checks for findings from the 2026-10-20 hands-on sessions.

Each check reproduces one finding deterministically in its own isolated runtime and
passes only when Stack behaves as documented. Where possible a control in the same
check shows the fixture itself is sound. These are product correctness gates, not agent
results. On Stack 0.1.22 several are expected to fail; that is the point of keeping them.

  python3 bench/regressions.py --stack target/debug/stack --out <new dir> [--only ID ...]
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

import arms
from arms import digest
from run import Cancelled, host_lock, invoke, last_json, stdout_of

CHECKS = {}


def check(finding):
    def register(function):
        CHECKS[function.__name__.replace('_', '-')] = (finding, function)
        return function
    return register


class Runtime:
    """One isolated root with any number of checkouts, and receipts under `out`."""

    def __init__(self, binary, out, locale, cache=None):
        self.root = Path(tempfile.mkdtemp(prefix='ar.', dir='/tmp')).resolve()
        self.binary, self.out, self.cache = binary, out, cache
        self.env = arms.isolated_env(self.root, cache, locale)
        self.checkouts, self.children = [], []

    def spawn(self, locale):
        """A second root, so a different locale cannot reuse this root's supervisor."""
        child = Runtime(self.binary, self.out, locale, self.cache)
        self.children.append(child)
        return child

    def project(self, name, files):
        path = self.root / name
        path.mkdir()
        for relative, text in files.items():
            (path / relative).write_text(text)
        self.checkouts.append(path)
        return path

    def stack(self, label, checkout, *args, timeout=600, env=None):
        record = invoke([str(self.binary), '--json', '-C', str(checkout), *args], checkout, env or self.env, timeout,
                        self.out / label)
        record['json'] = last_json(stdout_of(record, self.out))
        return record

    def close(self):
        """`down` every checkout that still exists, stop this root's supervisor, verify nothing is left."""
        result = {'down': {}, 'runtime': str(self.root)}
        for checkout in self.checkouts:
            if checkout.exists():
                record = self.stack(f'cleanup-down-{checkout.name}', checkout, 'down', timeout=120)
                result['down'][checkout.name] = ((record['json'] or {}).get('data') or {}).get('confirmed')
        alive_checkout = next((c for c in self.checkouts if c.exists()), None)
        if alive_checkout is not None and (self.root / 'pf/state').is_dir():
            record = invoke([str(self.binary), 'exec', '--', 'pitchfork', 'supervisor', 'stop'], alive_checkout, self.env, 60,
                            self.out / 'cleanup-supervisor-stop')
            result['supervisor_stop'] = record['exit_code']
        deadline = time.monotonic() + 15
        left = arms.owned_processes(self.root)
        while left and time.monotonic() < deadline:
            time.sleep(0.25)
            left = arms.owned_processes(self.root)
        result['left'] = left
        if left:
            result['reclaimed_by_harness'] = arms.reclaim(left)
            result['after_reclaim'] = arms.owned_processes(self.root)
        result['ok'] = not left and all(value is True for value in result['down'].values())
        return result


def data(record):
    return ((record.get('json') or {}).get('data')) or {}


@check('P1-1: `stack run` gives custom services the proxy URL instead of the verified loopback URL')
def run_exec_url_parity(rt):
    app = rt.project('urlparity', {'stack.toml': '[services.web]\nrun = "exec python3 -m http.server $PORT --bind 127.0.0.1"\n\n'
                                                 '[tasks.url]\nrun = "printf %s \\"$WEB_URL\\""\nservices = ["web"]\n'})
    for step in ['compile', 'up']:
        if rt.stack(step, app, step, *(['--ttl', '20m'] if step == 'up' else []))['exit_code']:
            return {'error': f'{step} failed'}
    exec_url = data(rt.stack('exec-url', app, 'exec', '--require', 'web', '--', 'sh', '-c', 'printf %s "$WEB_URL"')).get('stdout')
    run_url = data(rt.stack('run-url', app, 'run', 'url')).get('stdout')
    control = isinstance(exec_url, str) and exec_url.startswith('http://127.0.0.1:')
    return {'passed': control and run_url == exec_url, 'control_passed': control,
            'observed': {'exec': exec_url, 'run': run_url}}


@check('P1-2: the Postgres cluster encoding depends on the locale of the shell that first runs `up`')
def postgres_locale_independent(rt):
    observed = {}
    for label, locale in [('C', {'LC_ALL': 'C'}), ('utf8', {'LANG': 'en_US.UTF-8'})]:
        sub = rt.spawn(locale)
        app = sub.project(f'pg-{label}', {'stack.toml': '[services.postgres]\npreset = "postgres"\nversion = "17.11"\n'})
        for step in ['compile', 'up']:
            if sub.stack(f'{label}-{step}', app, step, *(['--ttl', '20m'] if step == 'up' else []))['exit_code']:
                return {'error': f'{label} {step} failed', 'observed': observed}
        query = 'psql "$DATABASE_URL" -Atc "show server_encoding"'
        observed[label] = data(sub.stack(f'{label}-encoding', app, 'exec', '--require', 'postgres', '--', 'sh', '-c', query)).get('stdout', '').strip()
    return {'passed': observed.get('C') == 'UTF8', 'control_passed': observed.get('utf8') == 'UTF8', 'observed': observed}


@check('Astra #2: SIGTERM to `stack --json exec` leaves the command and its descendant running')
def external_sigterm_exec(rt):
    app = rt.project('cancel', {'stack.toml': '[tools]\n'})
    if rt.stack('compile', app, 'compile')['exit_code']:
        return {'error': 'compile failed'}
    pidfile = app / 'tree.json'
    code = ("import subprocess,os,json,time,pathlib; child=subprocess.Popen(['sleep','30']); "
            f"pathlib.Path({str(pidfile)!r}).write_text(json.dumps(dict(parent=os.getpid(),child=child.pid,group=os.getpgrp()))); "
            "time.sleep(30)")
    sentinel = subprocess.Popen(['sleep', '60'], start_new_session=True)
    result, tree = {}, None
    with (rt.out / 'exec.stdout').open('wb') as stdout, (rt.out / 'exec.stderr').open('wb') as stderr:
        process = subprocess.Popen([str(rt.binary), '--json', '-C', str(app), 'exec', '--', sys.executable, '-c', code],
                                   env=rt.env, stdout=stdout, stderr=stderr, start_new_session=True)
        try:
            deadline = time.monotonic() + 30
            while not pidfile.exists() and time.monotonic() < deadline and process.poll() is None:
                time.sleep(0.05)
            if not pidfile.exists():
                return {'error': 'the command never reached the cancellation checkpoint'}
            tree = json.loads(pidfile.read_text())
            time.sleep(0.5)
            result['before'] = subprocess.run(['ps', '-o', 'pid=,ppid=,pgid=,command=', '-p',
                                               f'{process.pid},{tree["parent"]},{tree["child"]}'], capture_output=True, text=True).stdout
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=10)
            time.sleep(0.5)
            result['observed'] = {'stack_exit': process.returncode, 'command_alive': arms.alive(tree['parent']),
                                  'descendant_alive': arms.alive(tree['child']), 'sentinel_alive': sentinel.poll() is None}
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            # Only the group this check created, identified by its recorded leader.
            if tree and arms.alive(tree['parent']) and os.getpgid(tree['parent']) == tree['group']:
                os.killpg(tree['group'], signal.SIGTERM)
            sentinel.terminate()
            sentinel.wait()
    seen = result['observed']
    result['passed'] = not seen['command_alive'] and not seen['descendant_alive'] and seen['sentinel_alive']
    result['control_passed'] = seen['sentinel_alive']
    return result


@check('P2-4: `down` then deleting the checkout keeps its port reservations; `gc` does not reclaim them')
def removed_checkout_reclaim(rt):
    app = rt.project('reclaim', {'stack.toml': '[services.redis]\npreset = "redis"\nversion = "7.4.7"\n'})
    for step in ['compile', 'up', 'down']:
        record = rt.stack(step, app, step, *(['--ttl', '20m'] if step == 'up' else []))
        if record['exit_code']:
            return {'error': f'{step} failed'}
    confirmed = data(record).get('confirmed') is True
    shutil.rmtree(app)  # The check's own scratch checkout, after a confirmed `down`.
    gc = rt.stack('gc', rt.root, 'gc')
    registry = json.loads((rt.root / 'stack/state/ports.json').read_text())
    kept = [entry for entry in registry.get('reservations', []) if entry.get('project') == str(app)]
    daemons = rt.root / 'mise/state/daemons'
    data_kb = int(subprocess.run(['du', '-sk', str(daemons)], capture_output=True, text=True).stdout.split()[0]) if daemons.exists() else 0
    return {'passed': confirmed and not kept, 'control_passed': confirmed and gc['exit_code'] == 0,
            'observed': {'reservations_kept': kept, 'gc': data(gc), 'service_data_kb_kept': data_kb}}


@check('L5: `stack down` in a fresh checkout that never ran `up` fails instead of reporting nothing to stop')
def down_before_up(rt):
    app = rt.project('fresh', {'stack.toml': '[services.web]\nrun = "exec python3 -m http.server $PORT --bind 127.0.0.1"\n'})
    if rt.stack('compile', app, 'compile')['exit_code']:
        return {'error': 'compile failed'}
    # A new clone has stack.toml and the committed stack.lock, not the generated provider config.
    shutil.rmtree(app / '.config')
    down = rt.stack('down', app, 'down', timeout=120)
    return {'passed': down['exit_code'] == 0 and data(down).get('confirmed') is True, 'control_passed': True,
            'observed': {'exit_code': down['exit_code'], 'result': (down.get('json') or {}).get('error') or data(down)}}


@check('P2-3: `stack run` resolves task secrets before installing the pinned fnox')
def secrets_before_install(rt):
    files = {'stack.toml': '[tools]\nfnox = "1.39.0"\n\n[tasks.sign]\nrun = "python3 sign.py"\nsecrets = ["WEBHOOK_KEY"]\n',
             'fnox.toml': '# FAKE value for evaluation only.\n[providers.plain]\ntype = "plain"\n\n[secrets]\n'
                          'WEBHOOK_KEY = { provider = "plain", value = "fake-webhook-key-agentdx-5e1f" }\n',
             'sign.py': 'import os\nprint("key-length", len(os.environ["WEBHOOK_KEY"]))\n'}
    app = rt.project('secrets', files)
    if rt.stack('compile', app, 'compile')['exit_code']:
        return {'error': 'compile failed'}
    first = rt.stack('run-before-install', app, 'run', 'sign')
    rt.stack('install', app, 'install')
    second = rt.stack('run-after-install', app, 'run', 'sign')
    ok = lambda record: record['exit_code'] == 0 and 'key-length 29' in (data(record).get('stdout') or '')
    return {'passed': ok(first), 'control_passed': ok(second),
            'observed': {'before_install': record_error(first), 'after_install_exit': second['exit_code']}}


def record_error(record):
    payload = record.get('json') or {}
    return payload.get('error') or {'exit_code': record['exit_code']}


def main():
    def cancel(signum, frame):
        raise Cancelled(f'signal {signum}')
    for signum in [signal.SIGTERM, signal.SIGHUP]:
        signal.signal(signum, cancel)
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--stack', required=True)
    parser.add_argument('--out', required=True)
    parser.add_argument('--only', nargs='+', choices=list(CHECKS))
    parser.add_argument('--cache-root', help='share tool downloads/installs between checks')
    args = parser.parse_args()
    binary, out = Path(args.stack).resolve(), Path(args.out).resolve()
    cache = Path(args.cache_root).resolve() if args.cache_root else None
    out.mkdir(parents=True, exist_ok=False)
    summary = {'kind': 'scripted_stack_regressions', 'stack_sha256': digest(binary), 'checks': {}}
    before = arms.globals_snapshot()
    with host_lock():
        for name in args.only or CHECKS:
            finding, function = CHECKS[name]
            directory = out / name
            directory.mkdir()
            rt = Runtime(binary, directory, {'LANG': 'en_US.UTF-8'}, cache)
            entry = {'finding': finding, 'runtime': str(rt.root)}
            try:
                entry.update(function(rt))
            except Exception as error:
                entry['error'] = f'{type(error).__name__}: {error}'
            finally:
                entry['cleanup'] = [runtime.close() for runtime in [rt, *rt.children]]
            entry['passed'] = entry.get('passed') is True and 'error' not in entry
            summary['checks'][name] = entry
            (directory / 'result.json').write_text(json.dumps(entry, indent=2) + '\n')
            print(f'{name}: {"PASS" if entry["passed"] else "FAIL"} {json.dumps(entry.get("observed"))}', flush=True)
    summary['globals_changed'] = arms.globals_diff(before, arms.globals_snapshot())
    summary['cleanup_ok'] = all(c['ok'] for entry in summary['checks'].values() for c in entry['cleanup'])
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    return 0 if all(entry['passed'] for entry in summary['checks'].values()) else 1


if __name__ == '__main__':
    sys.exit(main())

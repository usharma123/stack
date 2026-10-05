#!/usr/bin/env python3
"""Paired direct-host Stack command-entry benchmark. See eval/BENCHMARK.md."""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import shutil
import signal
import struct
import subprocess
import tempfile
import time

from pilot import isolated_env

ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


REQUIRED_PROVIDERS = {
    'tool-only': ('uv',),
    'verified': ('uv', 'pitchfork', 'postgres', 'psql', 'redis-server', 'redis-cli'),
}

# The actual mise Conda wrapper observed on this host. Only its install prefix
# may vary. Unknown wrapper formats fail closed rather than hiding logic changes.
CONDA_WRAPPER = '''#!/bin/sh
export CONDA_PREFIX='{prefix}'
export CONDA_DEFAULT_ENV='{prefix}'
export CONDA_SHLVL=1
export PATH="$CONDA_PREFIX/bin${{PATH:+:$PATH}}"
for _mise_conda_script in "$CONDA_PREFIX"/etc/conda/activate.d/*.sh; do
\tif [ -f "$_mise_conda_script" ]; then
\t\t. "$_mise_conda_script" || exit $?
\tfi
done
unset _mise_conda_script
exec '{prefix}/bin/{tool}' "$@"
'''


def relocated_binary_identity(path, prefix):
    """Hash all bytes after narrowly canonicalizing Conda's Mach-O relocation.

    Preserve raw bytes/hash separately. Only null-padded prefix strings and
    verified ad-hoc CodeDirectory page digests can differ between installations.
    Other signature metadata, instructions and data remain in the comparison.
    """
    raw = path.read_bytes()
    needle = prefix.encode()
    if needle not in raw:
        return {'path': str(path), 'sha256': sha(path)}
    if raw[:4] != b'\xcf\xfa\xed\xfe':
        raise RuntimeError(f'unsupported relocated provider binary: {path}')
    normalized = bytearray(raw)
    ncmds, command_bytes = struct.unpack_from('<II', raw, 16)
    position, signature, cstrings = 32, None, []
    for _ in range(ncmds):
        command, size = struct.unpack_from('<II', raw, position)
        if size < 8 or position + size > 32 + command_bytes:
            raise RuntimeError('invalid Mach-O load command')
        if command == 0x1d:  # LC_CODE_SIGNATURE
            if signature is not None or size != 16:
                raise RuntimeError('invalid Mach-O signature command')
            signature = struct.unpack_from('<II', raw, position + 8)
        if command == 0x19:  # LC_SEGMENT_64
            if size < 72:
                raise RuntimeError('invalid Mach-O segment')
            sections = struct.unpack_from('<I', raw, position + 64)[0]
            if 72 + sections * 80 != size:
                raise RuntimeError('invalid Mach-O section count')
            for section in range(sections):
                entry = position + 72 + section * 80
                name = raw[entry:entry + 16].rstrip(b'\0')
                segment = raw[entry + 16:entry + 32].rstrip(b'\0')
                length = struct.unpack_from('<Q', raw, entry + 40)[0]
                start = struct.unpack_from('<I', raw, entry + 48)[0]
                flags = struct.unpack_from('<I', raw, entry + 64)[0]
                if name == b'__cstring' and segment == b'__TEXT' and flags & 0xff == 2:
                    cstrings.append((start, start + length))
        position += size
    if not signature or position != 32 + command_bytes:
        raise RuntimeError('missing Mach-O signature')
    offset, size = signature
    if offset + size != len(raw):
        raise RuntimeError('unsupported Mach-O signature extent')
    magic, length, count = struct.unpack_from('>III', raw, offset)
    if magic != 0xfade0cc0 or length > size or 12 + count * 8 > length:
        raise RuntimeError('invalid Mach-O signature superblob')
    digest_ranges = []
    for index in range(count):
        slot, relative = struct.unpack_from('>II', raw, offset + 12 + index * 8)
        if relative + 8 > length:
            raise RuntimeError('invalid Mach-O signature blob')
        start = offset + relative
        blob_magic, blob_length = struct.unpack_from('>II', raw, start)
        if relative + blob_length > length:
            raise RuntimeError('invalid Mach-O signature length')
        if slot == 0 or 0x1000 <= slot <= 0x1005:
            if blob_magic != 0xfade0c02 or blob_length < 44:
                raise RuntimeError('invalid CodeDirectory')
            flags, hash_offset, _, _, slots, limit = struct.unpack_from('>6I', raw, start + 12)
            hash_size, hash_type = struct.unpack_from('>2B', raw, start + 36)
            if flags != 2 or hash_size != 32 or hash_type != 2 or limit != offset:
                raise RuntimeError('unsupported non-ad-hoc CodeDirectory')
            if hash_offset + slots * hash_size > blob_length:
                raise RuntimeError('invalid CodeDirectory digest range')
            digest_ranges.append((start + hash_offset, slots * hash_size))
    if not digest_ranges:
        raise RuntimeError('missing ad-hoc CodeDirectory')
    verified = subprocess.run(['/usr/bin/codesign', '--verify', '--strict', str(path)],
                              capture_output=True, text=True, timeout=30)
    if verified.returncode != 0:
        raise RuntimeError(f'invalid relocated binary signature: {path}: {verified.stderr}')
    replacements = []
    cursor = 0
    while True:
        start = raw.find(needle, cursor, offset)
        if start < 0:
            break
        end = raw.find(b'\0', start, offset)
        if end < 0:
            raise RuntimeError('relocated path is not null terminated')
        padded_end = end
        while padded_end < offset and raw[padded_end] == 0:
            padded_end += 1
        if not any(left <= start < end < padded_end <= right for left, right in cstrings):
            raise RuntimeError('relocated prefix outside __TEXT,__cstring')
        # Conda also relocates printable configure-argument strings containing
        # several prefixes. Canonicalize every exact prefix in the same padded
        # C string; keep all arguments, suffixes and their ordering unchanged.
        literal = raw[start:end]
        if any(c < 32 or c > 126 for c in literal):
            raise RuntimeError('unsupported relocated non-string data')
        for occurrence in re.finditer(re.escape(needle), literal):
            after = occurrence.end()
            if after < len(literal) and literal[after:after + 1] not in (b'/', b"'", b'=', b' '):
                raise RuntimeError('unsupported relocated path boundary')
        replacement = literal.replace(needle, b'<INSTALL_PREFIX>')
        if len(replacement) >= padded_end - start:
            raise RuntimeError('insufficient null padding for normalized path')
        normalized[start:padded_end] = replacement.ljust(padded_end - start, b'\0')
        replacements.append({'offset': start, 'padded_end': padded_end,
                             'literal_path': raw[start:end].decode(),
                             'normalized_path': replacement.decode()})
        cursor = padded_end
    for start, length in digest_ranges:
        normalized[start:start + length] = b'\0' * length
    return {'path': str(path), 'sha256': hashlib.sha256(raw).hexdigest(),
            'normalized_sha256': hashlib.sha256(normalized).hexdigest(),
            'normalization': {'literal_install_prefix': prefix, 'paths': replacements,
                              'verified_ad_hoc_signature': True,
                              'signature_page_digest_ranges': digest_ranges}}


def provider_artifact(path, tool, data_dir):
    path = Path(path).resolve(strict=True)
    if not path.is_file() or not os.access(path, os.X_OK):
        raise RuntimeError(f'{tool}: provider is not an executable file: {path}')
    receipt = {'path': str(path), 'sha256': sha(path), 'kind': 'direct'}
    if path.parent.name != '.mise-bins':
        return receipt
    text = path.read_bytes().decode('utf-8')
    match = re.search(r"^export CONDA_PREFIX='([^'\n]+)'$", text, re.MULTILINE)
    if not match:
        raise RuntimeError(f'{tool}: unsupported mise wrapper: {path}')
    prefix = match.group(1)
    root = Path(prefix).resolve(strict=True)
    installs = (Path(data_dir) / 'installs').resolve(strict=True)
    if root != path.parent.parent or not root.is_relative_to(installs):
        raise RuntimeError(f'{tool}: wrapper prefix escapes its isolated install: {prefix}')
    if text != CONDA_WRAPPER.format(prefix=prefix, tool=tool):
        raise RuntimeError(f'{tool}: unsupported mise wrapper logic: {path}')
    target = (root / 'bin' / tool).resolve(strict=True)
    if not target.is_relative_to(root) or not target.is_file() or not os.access(target, os.X_OK):
        raise RuntimeError(f'{tool}: invalid wrapper target: {target}')
    # The wrapper sources these scripts, so they are part of its identity too.
    activation = {}
    for script in sorted((root / 'etc/conda/activate.d').glob('*.sh')):
        if not script.is_file():
            continue  # matches the wrapper's [ -f ... ] guard
        if not script.resolve(strict=True).is_relative_to(root):
            raise RuntimeError(f'{tool}: activation script escapes install: {script}')
        activation[str(script.relative_to(root))] = sha(script)
    normalized = CONDA_WRAPPER.format(prefix='<INSTALL_PREFIX>', tool=tool)
    receipt.update(kind='mise-conda-wrapper', underlying=relocated_binary_identity(target, prefix),
                   wrapper={'normalized_sha256': hashlib.sha256(normalized.encode()).hexdigest(),
                            'normalized_text': normalized,
                            'normalization': {'literal_install_prefix': prefix,
                                              'resolved_install_prefix': str(root),
                                              'replacement': '<INSTALL_PREFIX>',
                                              'occurrences': text.count(prefix)},
                            'activation_sha256': activation})
    return receipt


def provider_identity(receipt):
    if receipt['kind'] == 'direct':
        return {'kind': 'direct', 'sha256': receipt['sha256']}
    underlying = receipt['underlying']
    return {'kind': receipt['kind'], 'sha256': underlying.get('normalized_sha256', underlying['sha256']),
            'wrapper_sha256': receipt['wrapper']['normalized_sha256'],
            'activation_sha256': receipt['wrapper']['activation_sha256']}


def distribution(values):
    values = sorted(values)
    def percentile(q):
        return values[max(0, math.ceil(q * len(values)) - 1)] if values else None
    return {'n': len(values), 'p50_ms': percentile(.50), 'p95_ms': percentile(.95)}


def invoke(command, cwd, env, timeout):
    # Include host process creation, Stack verification and child exit, excluding setup/log I/O.
    started = time.perf_counter_ns()
    try:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    except OSError as error:
        return {'elapsed_ns': time.perf_counter_ns() - started, 'exit_code': None,
                'timed_out': False, 'launch_error': str(error)}, b'', str(error).encode()
    timed_out = False
    drain_timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except BaseException as error:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        # Descendants can detach from the process group and retain our pipes.
        # Never wait for their EOF before entering benchmark cleanup.
        try:
            stdout, stderr = process.communicate(timeout=.1)
        except subprocess.TimeoutExpired as drain_error:
            drain_timed_out = True
            stdout, stderr = drain_error.output or b'', drain_error.stderr or b''
            process.stdout.close()
            process.stderr.close()
            try:
                process.wait(timeout=.1)
            except subprocess.TimeoutExpired:
                process.kill()
                # Still bounded if the OS cannot reap the killed process.
                try:
                    process.wait(timeout=.1)
                except subprocess.TimeoutExpired:
                    pass
        if not isinstance(error, subprocess.TimeoutExpired):
            raise
        timed_out = True
    return {'elapsed_ns': time.perf_counter_ns() - started, 'exit_code': process.returncode,
            'timed_out': timed_out, 'drain_timed_out': drain_timed_out}, stdout, stderr


class Benchmark:
    def __init__(self, args, out, work):
        self.args, self.out, self.work = args, out, work
        self.contexts, self.rows, self.cleanup_errors = {}, [], []
        self.sequence = 0
        self.provider_hashes = {}
        self.rng = random.Random(args.seed)

    def record(self, label, command, context, **fields):
        self.sequence += 1
        result, stdout, stderr = invoke(command, context['app'], context['env'], self.args.timeout)
        stem = f'{self.sequence:05}-{label}'
        (self.out / (stem + '.stdout')).write_bytes(stdout)
        (self.out / (stem + '.stderr')).write_bytes(stderr)
        row = dict(seq=self.sequence, label=label, command=command, cwd=str(context['app']),
                   log_stem=stem, **fields, **result)
        self.rows.append(row)
        with (self.out / 'events.jsonl').open('a') as file:
            file.write(json.dumps(row) + '\n')
        return row, stdout

    def checked(self, label, command, context):
        row, stdout = self.record(label, command, context, phase='setup')
        if row['exit_code'] != 0 or row['timed_out']:
            raise RuntimeError(f'{label} failed; see {row["log_stem"]}.stderr')
        return stdout

    def setup(self):
        for case in self.args.cases.split(','):
            for variant in ('baseline', 'candidate'):
                directory = self.work / case / variant
                app = directory / 'app'
                app.mkdir(parents=True)
                env = isolated_env(directory, inherited={k: v for k, v in os.environ.items()
                                    if not k.startswith(('STACK_', 'PG', 'REDIS_'))})
                env['PATH'] = str(self.args.mise.parent) + os.pathsep + env.get('PATH', '')
                Path(env['HOME']).mkdir(parents=True)
                manifest = '[tools]\nuv = "0.12.23"\n'
                if case == 'verified':
                    manifest += '\n[services.postgres]\npreset = "postgres"\nversion = "17"\n\n[services.redis]\npreset = "redis"\nversion = "8"\n'
                (app / 'stack.toml').write_text(manifest)
                context = {'app': app, 'env': env, 'binary': str(getattr(self.args, variant)),
                           'directory': directory, 'case': case, 'variant': variant}
                self.contexts[(case, variant)] = context  # register before anything can fail
                prefix = [context['binary'], '-C', str(app)]
                self.checked(f'{case}-{variant}-compile', prefix + ['compile'], context)
                # `exec -- true` need not install uv. Both variants must have the
                # fixture's expected tool, even in the service-free case.
                self.checked(f'{case}-{variant}-install-uv',
                             [str(self.args.mise), 'install', 'uv@0.12.23'], context)
                # Prime tool installation/activation before warmups and provider hash capture.
                self.checked(f'{case}-{variant}-prime', prefix + ['exec', '--', 'true'], context)
                if case == 'verified':
                    # Even a failed up may have launched its isolated supervisor.
                    # Resolve once during setup and keep the path through cleanup.
                    context['supervisor_needed'] = True
                    try:
                        stdout = self.checked(f'{case}-{variant}-up', prefix + ['--json', 'up', '--owner-pid', str(os.getpid())], context)
                    finally:
                        found = self.checked(f'{case}-{variant}-cleanup-pitchfork-path',
                                             [str(self.args.mise), 'which', 'pitchfork'], context)
                        supervisor = found.decode().strip()
                        if not supervisor or len(supervisor.splitlines()) != 1 or not Path(supervisor).is_absolute():
                            raise RuntimeError('missing required cleanup supervisor path')
                        context['supervisor'] = provider_artifact(supervisor, 'pitchfork', env['MISE_DATA_DIR'])
                    session = json.loads(stdout)['data']['session']
                    expected = {name: os.path.realpath(service['data_dir'])
                                for name, service in session['services'].items()}
                    self.verify(context, expected)
                    context['expected'] = expected
                self.checked(f'{case}-{variant}-resolved-tools', [str(self.args.mise), 'ls', '--json'], context)
                artifacts = {}
                for path in app.rglob('*'):
                    if path.is_file() and (path.name == 'stack.lock' or path.suffix == '.toml'):
                        relative = str(path.relative_to(app))
                        artifacts[relative] = sha(path)
                        destination = self.out / 'fixtures' / case / variant / relative
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        shutil.copyfile(path, destination)
                for tool in REQUIRED_PROVIDERS[case]:
                    stdout = self.checked(f'{case}-{variant}-which-{tool}',
                                          [str(self.args.mise), 'which', tool], context)
                    resolved = stdout.decode().strip()
                    if not resolved or len(resolved.splitlines()) != 1 or not Path(resolved).is_absolute():
                        raise RuntimeError(f'{case}-{variant}: missing or invalid required provider {tool}')
                    artifacts[tool] = provider_artifact(resolved, tool, env['MISE_DATA_DIR'])
                (self.out / f'{case}-{variant}-artifacts.json').write_text(json.dumps(artifacts, indent=2) + '\n')
                hashes = {tool: provider_identity(artifacts[tool]) for tool in REQUIRED_PROVIDERS[case]}
                if case in self.provider_hashes and hashes != self.provider_hashes[case]:
                    raise RuntimeError(f'{case}: provider executable hashes differ between variants')
                self.provider_hashes[case] = hashes

    def verify(self, context, expected):
        script = 'set -eu; psql "$DATABASE_URL" -Atc "show data_directory"; redis-cli -u "$REDIS_URL" --no-auth-warning --raw config get dir'
        stdout = self.checked(f'verified-{context["variant"]}-identity',
                              [context['binary'], 'exec', '--require-all', '--', 'sh', '-c', script], context)
        lines = stdout.decode().strip().splitlines()
        if len(lines) != 3 or lines[1] != 'dir' or os.path.realpath(lines[0]) != expected['postgres'] or os.path.realpath(lines[2]) != expected['redis']:
            raise RuntimeError(f'wrong service identity: {lines!r}, expected {expected!r}')

    def sample(self):
        for case in self.args.cases.split(','):
            for phase, count in [('warmup', self.args.warmup), ('measurement', self.args.pairs)]:
                for pair in range(count):
                    order = ['baseline', 'candidate']
                    if (self.args.order == 'alternating' and pair % 2) or (self.args.order == 'random' and self.rng.randrange(2)):
                        order.reverse()
                    for position, variant in enumerate(order):
                        context = self.contexts[(case, variant)]
                        command = [context['binary'], 'exec']
                        if case == 'verified':
                            command += ['--require-all']
                        command += ['--', 'true']
                        self.record(f'{case}-{variant}-{phase}-{pair}', command, context,
                                    case=case, variant=variant, phase=phase, pair=pair, position=position)
            if case == 'verified':
                for variant in ('baseline', 'candidate'):
                    context = self.contexts[(case, variant)]
                    self.verify(context, context['expected'])

    def cleanup(self):
        # All state directories are unique to this invocation. Never use global down/gc/kill.
        for context in self.contexts.values():
            try:
                if context.get('supervisor_needed'):
                    row, stdout = self.record('cleanup-down', [context['binary'], '--json', 'down'], context, phase='cleanup')
                    if row['exit_code'] != 0 or row['timed_out'] or not json.loads(stdout).get('data', {}).get('confirmed'):
                        raise RuntimeError(f'down unconfirmed: {stdout!r}')
            except Exception as error:
                self.cleanup_errors.append(str(error))
            try:
                # Stop only the supervisor using this context's isolated PITCHFORK_STATE_DIR.
                if context.get('supervisor_needed'):
                    supervisor = context.get('supervisor')
                    if supervisor is None:
                        raise RuntimeError('isolated supervisor path unavailable; stop unconfirmed')
                    current = provider_artifact(supervisor['path'], 'pitchfork', context['env']['MISE_DATA_DIR'])
                    if current != supervisor:
                        raise RuntimeError('isolated supervisor executable changed; stop unconfirmed')
                    row, _ = self.record('cleanup-supervisor', [supervisor['path'], 'supervisor', 'stop'], context, phase='cleanup')
                    if row['exit_code'] != 0 or row['timed_out']:
                        raise RuntimeError('isolated supervisor stop failed')
            except Exception as error:
                self.cleanup_errors.append(str(error))

    def summary(self):
        result = {}
        measured = [r for r in self.rows if r.get('phase') == 'measurement']
        for case in self.args.cases.split(','):
            rows = [r for r in measured if r['case'] == case]
            result[case] = {}
            for variant in ('baseline', 'candidate'):
                subset = [r for r in rows if r['variant'] == variant]
                good = [r for r in subset if r['exit_code'] == 0 and not r['timed_out']]
                result[case][variant] = {**distribution([r['elapsed_ns'] / 1e6 for r in good]),
                                         'attempts': len(subset), 'failures': len(subset) - len(good)}
            deltas = []
            for pair in range(self.args.pairs):
                matched = {r['variant']: r for r in rows if r['pair'] == pair}
                if len(matched) == 2 and all(r['exit_code'] == 0 and not r['timed_out'] for r in matched.values()):
                    deltas.append((matched['candidate']['elapsed_ns'] - matched['baseline']['elapsed_ns']) / 1e6)
            result[case]['paired_candidate_minus_baseline'] = distribution(deltas)
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--mise', type=Path, required=True)
    parser.add_argument('--baseline-build-info', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--pairs', type=int, default=50)
    parser.add_argument('--warmup', type=int, default=5)
    parser.add_argument('--seed', type=int, default=20261005)
    parser.add_argument('--order', choices=['alternating', 'random'], default='alternating')
    parser.add_argument('--cases', default='tool-only,verified')
    parser.add_argument('--timeout', type=float, default=900)
    args = parser.parse_args()
    if args.pairs < 1 or args.warmup < 0 or args.timeout <= 0 or not set(args.cases.split(',')) <= {'tool-only', 'verified'} or len(set(args.cases.split(','))) != len(args.cases.split(',')):
        parser.error('invalid counts, timeout or cases')
    for field in ('baseline', 'candidate', 'mise'):
        path = getattr(args, field).resolve(strict=True)
        if not os.access(path, os.X_OK):
            parser.error(f'{field} must be executable')
        setattr(args, field, path)
    info = None
    if args.baseline_build_info:
        info = json.loads(args.baseline_build_info.read_text())
        target = {'Darwin': 'darwin', 'Linux': 'linux'}[platform.system()] + '-' + {'arm64': 'arm64', 'aarch64': 'arm64', 'x86_64': 'x64'}[platform.machine()]
        if info['version'] != '0.1.4' or info['hashes'][target] != sha(args.baseline):
            parser.error('baseline is not the recorded released 0.1.4 host artifact')
    def interrupt(signum, frame):
        raise KeyboardInterrupt(f'signal {signum}')
    signal.signal(signal.SIGTERM, interrupt)
    # One host benchmark at a time. Other tools must be kept idle by the caller.
    with open(Path(tempfile.gettempdir()) / 'stack-paired-host.lock', 'a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error('another paired-host benchmark holds the host lock')
        out = args.out.resolve()
        out.mkdir(parents=True, exist_ok=False)
        work = Path(tempfile.mkdtemp(prefix='sph.', dir='/tmp')).resolve()
        run = Benchmark(args, out, work)
        metadata = {'schema_version': 1, 'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
                    'args': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
                    'platform': platform.platform(), 'python': platform.python_version(), 'cpus': os.cpu_count(),
                    'work': str(work), 'baseline_build_info': info,
                    'hashes': {key: sha(getattr(args, key)) for key in ('baseline', 'candidate', 'mise')},
                    'harness_sha256': sha(__file__), 'isolation_helper_sha256': sha(ROOT / 'eval/harness/pilot.py'),
                    'timing': 'perf_counter_ns, host process launch through child exit; setup excluded',
                    'percentiles': 'nearest-rank; successes only, failures reported separately'}
        (out / 'metadata.json').write_text(json.dumps(metadata, indent=2) + '\n')
        error = None
        try:
            context = {'app': ROOT, 'env': dict(os.environ)}
            run.checked('source-commit', ['git', 'rev-parse', 'HEAD'], context)
            run.checked('source-status', ['git', 'status', '--porcelain'], context)
            for field in ('baseline', 'candidate', 'mise'):
                run.checked(field + '-version', [str(getattr(args, field)), '--version'], context)
            run.setup()
            run.sample()
            if any(sha(getattr(args, key)) != metadata['hashes'][key] for key in ('baseline', 'candidate', 'mise')):
                raise RuntimeError('benchmark executable changed during the run')
        except BaseException as exception:
            error = f'{type(exception).__name__}: {exception}'
        finally:
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            signal.signal(signal.SIGINT, signal.SIG_IGN)
            run.cleanup()
            samples = [r for r in run.rows if r.get('phase') in ('measurement', 'warmup')]
            valid = error is None and not run.cleanup_errors and all(r['exit_code'] == 0 and not r['timed_out'] for r in samples)
            summary = {'valid': valid, 'error': error, 'cleanup_errors': run.cleanup_errors,
                       'results': run.summary(), 'work_retained': bool(run.cleanup_errors),
                       'limits': ['One host; warmed caches; not a cold install or agent productivity study.',
                                  'Successful sample distributions are not usable as a win claim if valid is false.',
                                  'Paired deltas are descriptive; no significance or universal win claim.']}
            (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
            if not run.cleanup_errors:
                shutil.rmtree(work)
        print(json.dumps(summary, indent=2))
        return 0 if valid else 1


if __name__ == '__main__':
    raise SystemExit(main())

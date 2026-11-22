"""Process and connection observation, identical for both arms.

Every process the harness launches for a worktree carries AGENTVALUE_WT=<worktree name> in its
environment, and every descendant inherits it (daemonizing does not clear the environment). A
process is attributed to a worktree by that marker, or by the worktree path in its command line
(Stack's supervisor, for example). Only the marker's value is ever read from another process's
environment; the rest of it (which holds session tokens) is never stored.
"""
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
import arms as legacy  # bench/arms.py: process_table, same_process, reclaim, owns
sys.path.insert(0, str(Path(__file__).resolve().parent))
import redact  # command lines are scrubbed before they are kept or written anywhere

MARKER = re.compile(r'(?:^|\s)AGENTVALUE_WT=([A-Za-z0-9_.-]+)')
ROLE = re.compile(r'(?:^|\s)AGENTVALUE_ROLE=(agent|recovery)(?=\s|$)')
OBSERVE_TIMEOUT = 60  # every observation subprocess is bounded, so a join on an observer always ends
# Markers are "<group>.<run id>", so a process from another run (or an unrelated program) never
# counts as this run's. Set by the runner before anything is observed.
RUN_SUFFIX = None


def group_of_marker(mark):
    if mark and RUN_SUFFIX and mark.endswith(RUN_SUFFIX):
        return mark[:-len(RUN_SUFFIX)]
    return None


def _out(argv):
    """A command's stdout as text; process environments and file names need not be UTF-8. A failed
    `ps` raises: an empty listing must never pass for "nothing running". (`lsof` exits 1 whenever
    some file cannot be inspected, so its status is not used.)"""
    try:
        result = subprocess.run(argv, capture_output=True, timeout=OBSERVE_TIMEOUT)
    except subprocess.TimeoutExpired:
        raise RuntimeError(f'{argv[0]} did not finish within {OBSERVE_TIMEOUT} s')
    # A whole-user listing is never empty while a run is alive: an empty one is a failed listing.
    if argv[0] == 'lsof' and '-u' in argv and '-p' not in argv and not result.stdout.strip():
        raise RuntimeError(f'lsof returned an empty listing ({result.returncode}): {result.stderr[-300:]!r}')
    if argv[0] == 'ps' and result.returncode not in (0, 1) or (argv[0] == 'ps' and '-ax' in ' '.join(argv) and not result.stdout):
        raise RuntimeError(f'{argv[0]} failed ({result.returncode}): {result.stderr[-300:]!r}')
    return result.stdout.decode('utf-8', errors='replace')


def _process_table():
    """bench/arms.py's process_table, decoding command lines that are not UTF-8."""
    rows = {}
    for line in _out(['ps', '-axww', '-o', 'pid=,ppid=,pgid=,stat=,lstart=,command=']).splitlines():
        parts = line.split(None, 9)
        if len(parts) < 10 or parts[3].startswith('Z'):
            continue
        pid = int(parts[0])
        rows[pid] = {'pid': pid, 'ppid': int(parts[1]), 'pgid': int(parts[2]), 'stat': parts[3], 'started': ' '.join(parts[4:9]),
                     'command': parts[9]}
    return rows


def _process_identity(pid):
    parts = _out(['ps', '-ww', '-p', str(pid), '-o', 'stat=,lstart=,command=']).strip().split(None, 6)
    if len(parts) < 7 or parts[0].startswith('Z'):
        return None
    return {'started': ' '.join(parts[1:6]), 'command': parts[6]}


def _owned_processes(root):
    """bench/arms.py's owned_processes (command line, cwd or open file under root), decoding safely."""
    roots = legacy._roots(root)
    rows = _process_table()
    owned = {pid: 'command' for pid, row in rows.items() if legacy._under(roots, row['command'])}
    current = None
    for line in _out(['lsof', '-nP', '-w', '-u', str(os.getuid()), '-Fpn']).splitlines():
        if line.startswith('p'):
            current = int(line[1:])
        elif line.startswith('n') and current is not None and legacy._under(roots, line[1:]):
            owned.setdefault(current, 'open_file')
    ancestors, pid = set(), os.getpid()
    while pid in rows and pid not in ancestors:
        ancestors.add(pid)
        pid = rows[pid]['ppid']
    return [dict(rows[pid], reason=reason) for pid, reason in sorted(owned.items()) if pid in rows and pid not in ancestors]


def _owns(root, pid):
    roots = legacy._roots(root)
    identity = _process_identity(pid)
    if identity is None:
        return False
    if legacy._under(roots, identity['command']):
        return True
    return any(line.startswith('n') and legacy._under(roots, line[1:])
               for line in _out(['lsof', '-nP', '-w', '-a', '-p', str(pid), '-Fn']).splitlines())


legacy.process_table = _process_table
legacy.process_identity = _process_identity
legacy.owned_processes = _owned_processes
legacy.owns = _owns


def marks_and_roles():
    """pid -> worktree marker, and pid -> role marker (AGENTVALUE_ROLE: agent or recovery), for every
    live process whose environment carries them. Only these two values are read; nothing else of
    any environment is kept."""
    out = _out(['ps', '-axww', '-E', '-o', 'pid=,command='])
    found, roles = {}, {}
    for line in out.splitlines():
        line = line.strip()
        pid, _, rest = line.partition(' ')
        if not pid.isdigit():
            continue
        match = MARKER.search(rest)
        if match:
            found[int(pid)] = match[1]
            role = ROLE.search(rest)
            if role:
                roles[int(pid)] = role[1]
    return found, roles


def markers():
    """pid -> worktree marker for every live process that carries one."""
    return marks_and_roles()[0]


def open_paths(pids=None):
    """pid -> paths the process has open, its working directory included (lsof)."""
    argv = ['lsof', '-nP', '-w', '-Fpn'] + (['-p', ','.join(str(p) for p in pids)] if pids else ['-u', str(os.getuid())])
    if pids is not None and not pids:
        return {}
    out = _out(argv)
    paths, pid = {}, None
    for line in out.splitlines():
        if line.startswith('p'):
            pid = int(line[1:])
        elif line.startswith('n/') and pid is not None:
            paths.setdefault(pid, set()).add(line[1:])
    return paths


def owner_of(pid, command, marks, paths, groups):
    """The group a process belongs to: its marker, else its command line or any open path under a
    group's paths. `groups` maps name -> list of paths (the worktree, plus any per-checkout state
    directories the arm keeps elsewhere)."""
    mark = group_of_marker(marks.get(pid))
    if mark in groups:
        return mark
    texts = [command] + sorted(paths.get(pid, ()))
    for name, roots in groups.items():
        for root in roots:
            variants = legacy._roots(root)
            if any(legacy._under(variants, text) for text in texts):
                return name
    return None


def attributed(groups, exclude=()):
    """Live processes attributed to each group: {name: [row, ...]}. The harness's own ancestors
    and the pids in `exclude` (the agent CLIs) are skipped."""
    groups = {name: ([roots] if isinstance(roots, str) else list(roots or [])) for name, roots in groups.items()}
    rows = legacy.process_table()
    marks = markers()
    paths = open_paths()
    ancestors, pid = set(), os.getpid()
    while pid in rows and pid not in ancestors:
        ancestors.add(pid)
        pid = rows[pid]['ppid']
    result = {name: [] for name in groups}
    for pid, row in rows.items():
        if pid in ancestors or pid in exclude:
            continue
        owner = owner_of(pid, row['command'], marks, paths, groups)
        if owner is not None:
            result[owner].append(dict(row, marker=marks.get(pid)))
    return result


LOOPBACK = ('127.0.0.1', 'localhost', '[::1]', '*', '0.0.0.0', '[::]')


def _split(endpoint):
    host, _, port = endpoint.rpartition(':')
    return host, port


def tcp():
    """Listening ports on this host's loopback/wildcard addresses -> pid, and established
    connections as (pid, remote host, remote port). Only connections to a local address count
    as reaching another worktree's listener."""
    out = _out(['lsof', '-nP', '-w', '-iTCP', '-a', '-u', str(os.getuid()), '-FpnT'])
    listeners, connections, pid, name = {}, [], None, None
    for line in out.splitlines():
        if line.startswith('p'):
            pid = int(line[1:])
        elif line.startswith('n'):
            name = line[1:]
        elif line.startswith('TST=') and pid is not None and name is not None:
            state = line[4:]
            if state == 'LISTEN':
                host, port = _split(name)
                if port.isdigit() and host in LOOPBACK:
                    listeners[int(port)] = pid
                    listeners.setdefault('by_endpoint', {})[(host, int(port))] = pid
            elif state == 'ESTABLISHED' and '->' in name:
                local, remote = name.split('->')
                host, port = _split(remote)
                if host in LOOPBACK[:3]:
                    connections.append((pid, host, port))
    return listeners, connections


# A durable service process: a postmaster (its children are titled "postgres: ..."), Redis, the
# API or the worker (the `uv run` wrapper and the interpreter it starts), or Stack's supervisor.
DURABLE = re.compile(r'/bin/postgres\s|redis-server|\bpython3?\s+(?:app|worker)\.py\b|__supervise')
POSTMASTER = re.compile(r'/bin/postgres\s+(?:.*\s)?-D\s+(\S+)')
SERVICES = ('postgres', 'redis', 'api', 'worker')


def service_kind(command):
    """Which of a worktree's services a process is, from its command line (None if none)."""
    if POSTMASTER.search(command):
        return 'postgres'
    if re.search(r'(?:^|/)redis-server(?:\s|$)', command):
        return 'redis'
    if re.search(r'\bpython3?\s+app\.py\b', command):
        return 'api'
    if re.search(r'\bpython3?\s+worker\.py\b', command):
        return 'worker'
    if '__supervise' in command:
        return 'supervisor'
    return None


def postmaster_data_dir(command):
    match = POSTMASTER.search(command)
    return match[1] if match else None


def under_any(paths, text):
    return any(legacy._under(legacy._roots(p), text) for p in paths)


def exiting(row):
    command = row.get('command', '').strip()
    return 'E' in row.get('stat', '') or (command.startswith('(') and command.endswith(')'))


def norm_path(path):
    path = str(path or '').rstrip('/')
    return path[len('/private'):] if path.startswith('/private/') else path


def ready_services(group, groups, owner, listeners, rows, recorded=None):
    """The four services of `group` that run and listen as the arm's own: kind -> {pid, started,
    command, port}. `recorded` is what the arm recorded for that worktree: {'data_dir': ..., 'ports':
    {'postgres': p, 'redis': p, 'api': p, 'worker': p}}. Each must listen on its recorded port, have
    the matching command class and belong to the worktree (marker, command or open files); the
    postmaster must also be on the recorded data directory. A scratch service elsewhere never counts."""
    recorded = recorded or {}
    ports, paths = recorded.get('ports') or {}, groups.get(group) or []
    found = {}
    for kind in SERVICES:
        port = ports.get(kind)
        pid = listeners.get(port) if port else None
        row = rows.get(pid)
        if row is None or owner.get(pid) != group or service_kind(row['command']) != kind:
            continue
        if kind == 'postgres' and (norm_path(postmaster_data_dir(row['command'])) != norm_path(recorded.get('data_dir'))
                                   or not under_any(paths, postmaster_data_dir(row['command']) or '')):
            continue
        found[kind] = {'pid': pid, 'started': row['started'], 'command': redact.scrub(row['command'])[:300], 'port': port}
    return found


def listening_ports(pids):
    """pid -> set of TCP ports it listens on now (one lsof call)."""
    if not pids:
        return {}
    out = _out(['lsof', '-nP', '-w', '-a', '-p', ','.join(str(p) for p in pids), '-iTCP', '-sTCP:LISTEN', '-Fpn'])
    found, pid = {}, None
    for line in out.splitlines():
        if line.startswith('p') and line[1:].isdigit():
            pid = int(line[1:])
        elif line.startswith('n') and pid is not None:
            port = line.rpartition(':')[2]
            if port.isdigit():
                found.setdefault(pid, set()).add(int(port))
    return found


class Sampler(threading.Thread):
    """Samples every `interval` seconds: which worktree owns which live processes and listening
    ports, and any TCP connection from a process of one worktree to a port another worktree's
    process listens on (cross-worktree access). Short-lived connections between samples are
    missed, so this is evidence of access, never proof of its absence.

    Each sample also records which durable service processes of a group are gone from the
    process table since the previous sample (event (g) and teardown collateral work from these),
    every postmaster seen listening (service restored), and is handed to every callable in
    `listeners` (the fault trigger). A sample's time is taken after its listings."""

    def __init__(self, groups, out, interval=1.0, ignore_clients=(), t0=None, listeners=()):
        """`groups` is a callable returning {name: [paths]}, refreshed on every sample because an
        arm may create a checkout's state directories only when it first starts. Times `m` are
        monotonic seconds since `t0` (the run's clock)."""
        super().__init__(daemon=True)
        self.groups_fn, self.out, self.interval = groups, Path(out), interval
        self.t0 = time.monotonic() if t0 is None else t0
        self.listeners = list(listeners)
        self.durable_prev = {}  # (pid, started) -> (group, command, m, ports)
        self.losses = []        # durable processes gone between two samples
        self.postmasters = {}   # (group, pid, started) -> (first m seen listening, data directory)
        self.data_dirs = {}     # group -> data directories of every PostgreSQL and Redis seen running for it
        self.pg_dirs = {}       # group -> data directories of every PostgreSQL seen running for it
        self.redis_cwd = {}     # (pid, started) -> working directory
        self.times = []         # (begun_m, stamped_m) of every sample
        self.groups, self.resolved = groups(), {}
        # Stack's own probes (`stack ps`, `status`) connect to every checkout's ports by design;
        # they are not an agent reaching another worktree's data.
        self.ignore_clients = tuple(ignore_clients)
        self.stop_event = threading.Event()
        self.seen = {}        # (marker, pid, started) -> {first, last, command}
        self.ports = {}       # port -> set of markers that listened on it
        self.cross = []       # cross-worktree connection events
        self.samples = 0
        self.errors = []

    def run(self):
        with self.out.open('a') as log:
            while not self.stop_event.is_set():
                try:
                    self.sample(log)
                except Exception as error:  # observation must never kill a trial; recorded instead
                    self.errors.append(repr(error))
                self.stop_event.wait(self.interval)

    def sample(self, log):
        begun = round(time.monotonic() - self.t0, 3)
        self.groups = self.groups_fn()
        rows = legacy.process_table()
        record_lineage(rows)
        marks, roles = marks_and_roles()
        listeners, connections = tcp()
        endpoints = listeners.pop('by_endpoint', {})
        # Processes with no marker or path in their command line (redis, supervisors) are resolved
        # through their open files, cached by identity: those with sockets and every durable service.
        unknown = [pid for pid in set(listeners.values()) | {c[0] for c in connections} |
                   {pid for pid, row in rows.items() if DURABLE.search(row['command'])}
                   if pid in rows and self.resolved.get((pid, rows[pid]['started'])) is None
                   and owner_of(pid, rows[pid]['command'], marks, {}, self.groups) is None]
        paths = open_paths(unknown) if unknown else {}
        for pid in unknown:
            found = owner_of(pid, rows[pid]['command'], marks, paths, self.groups)
            if found:  # unresolved processes are retried on later samples, once paths are known
                self.resolved[(pid, rows[pid]['started'])] = found
        now = time.time()                                   # stamped after the listings
        mono = round(time.monotonic() - self.t0, 3)
        self.times.append((begun, mono))
        owner = {}
        for pid, row in rows.items():
            mark = owner_of(pid, row['command'], marks, {}, self.groups) or self.resolved.get((pid, row['started']))
            if mark:
                owner[pid] = mark
                key = (mark, pid, row['started'])
                entry = self.seen.setdefault(key, {'first': now, 'first_m': mono, 'command': redact.scrub(row['command'])[:300]})
                entry['last'] = now
                entry['last_m'] = mono
        ports_of = {}
        for port, pid in listeners.items():
            ports_of.setdefault(pid, []).append(port)
        # Durable processes gone from the process table since the previous sample (pid and start time).
        # A process that is exiting (macOS shows its command as "(name)", without arguments or
        # environment, and an exiting state) counts as gone: it can no longer be attributed or classified.
        alive = {(pid, row['started']) for pid, row in rows.items() if not exiting(row)}
        for key, (group, command, seen_m, ports) in self.durable_prev.items():
            if key not in alive:
                self.losses.append({'group': group, 'pid': key[0], 'started': key[1], 'command': redact.scrub(command)[:1000],
                                    'kind': service_kind(command), 'ports': ports, 'last_seen_m': seen_m, 'gone_m': mono})
        current = {(pid, rows[pid]['started']): (group, rows[pid]['command'], mono, sorted(ports_of.get(pid, [])))
                   for pid, group in owner.items() if DURABLE.search(rows[pid]['command'])}
        # A known durable process still alive but not attributable in this sample stays tracked (with
        # the time it was last attributed), so its loss is recorded when it goes.
        current.update({k: v for k, v in self.durable_prev.items() if k in alive and k not in current})
        self.durable_prev = current
        for pid in ports_of:
            if pid in owner and service_kind(rows[pid]['command']) == 'postgres':
                self.postmasters.setdefault((owner[pid], pid, rows[pid]['started']), (mono, postmaster_data_dir(rows[pid]['command'])))
        for pid, group in owner.items():
            kind = service_kind(rows[pid]['command'])
            if kind == 'postgres':
                self.data_dirs.setdefault(group, set()).add(postmaster_data_dir(rows[pid]['command']))
                self.pg_dirs.setdefault(group, set()).add(postmaster_data_dir(rows[pid]['command']))
            elif kind == 'redis':
                key = (pid, rows[pid]['started'])
                if key not in self.redis_cwd:
                    out = _out(['lsof', '-nP', '-w', '-a', '-p', str(pid), '-d', 'cwd', '-Fn'])
                    cwd = next((l[1:] for l in out.splitlines() if l.startswith('n/')), None)
                    if cwd:
                        self.redis_cwd[key] = cwd
                if self.redis_cwd.get(key):
                    self.data_dirs.setdefault(group, set()).add(self.redis_cwd[key])
        port_owner = {port: owner.get(pid) for port, pid in listeners.items()}
        for port, mark in port_owner.items():
            if mark:
                self.ports.setdefault(port, set()).add(mark)
        events = []
        for pid, host, remote in connections:
            client = owner.get(pid)
            if not client or not remote.isdigit():
                continue
            if rows.get(pid, {}).get('command', '').startswith(self.ignore_clients):
                continue
            # The listener must be on the address the client reached (or a wildcard): an IPv4
            # connection never reaches an IPv6-only listener on the same port.
            server_pid = endpoints.get((host, int(remote))) or endpoints.get(('*', int(remote))) or \
                endpoints.get(('0.0.0.0', int(remote))) or endpoints.get(('[::]', int(remote)) if host == '[::1]' else None)
            server = owner.get(server_pid)
            if server and server != client:
                events.append({'t': now, 'm': mono, 'client': client, 'client_pid': pid, 'client_started': rows.get(pid, {}).get('started'),
                               'client_role': roles.get(pid), 'client_cmd': redact.scrub(rows.get(pid, {}).get('command', ''))[:200],
                               'server': server, 'port': int(remote)})
        self.cross.extend(events)
        self.samples += 1
        log.write(json.dumps({'t': round(now, 2), 'm': mono, 'begun_m': begun,
                              'procs': {m: sorted(p for p, o in owner.items() if o == m) for m in set(owner.values())},
                              'listen': {str(p): m for p, m in port_owner.items() if m}, 'cross': events}) + '\n')
        log.flush()
        info = {'m': mono, 'owner': owner, 'listeners': listeners, 'rows': rows, 'groups': self.groups, 'roles': roles}
        for listener in self.listeners:
            try:
                listener(info)
            except Exception as error:  # a listener's failure is recorded, never fatal to sampling
                self.errors.append(f'listener: {error!r}')

    def stop(self):
        self.stop_event.set()
        self.join(OBSERVE_TIMEOUT * 4)

    def summary(self):
        processes = {}
        for (mark, pid, started), entry in self.seen.items():
            processes.setdefault(mark, []).append({'pid': pid, 'started': started, **entry})
        stamps = [m for _, m in self.times]
        gaps = [{'after_m': a, 'before_m': b, 'seconds': round(b - a, 3)} for a, b in zip(stamps, stamps[1:]) if b - a > 3]
        return {'samples': self.samples, 'errors': self.errors[:20], 'processes': processes,
                'ports': {str(p): sorted(m) for p, m in self.ports.items()}, 'cross_worktree_connections': self.cross,
                'durable_losses': self.losses, 'gaps_over_3s': gaps,
                'first_m': stamps[0] if stamps else None, 'last_m': stamps[-1] if stamps else None,
                'postmasters_listening': [{'group': g, 'pid': pid, 'started': st, 'first_listen_m': m, 'data_dir': d}
                                          for (g, pid, st), (m, d) in self.postmasters.items()],
                'service_data_dirs': {g: sorted(d for d in dirs if d) for g, dirs in self.data_dirs.items()},
                'postgres_data_dirs': {g: sorted(d for d in dirs if d) for g, dirs in self.pg_dirs.items()}}


def role_of(pid):
    """The AGENTVALUE_ROLE marker of one process (None if unreadable)."""
    match = ROLE.search(_out(['ps', '-ww', '-E', '-p', str(pid), '-o', 'command=']))
    return match[1] if match else None


def mark_of(pid):
    match = MARKER.search(_out(['ps', '-ww', '-E', '-p', str(pid), '-o', 'command=']))
    return match[1] if match else None


def still_listening(pids):
    """The subset of `pids` that hold a listening TCP socket now (one lsof call)."""
    if not pids:
        return set()
    out = _out(['lsof', '-nP', '-w', '-a', '-p', ','.join(str(p) for p in pids), '-iTCP', '-sTCP:LISTEN', '-Fp'])
    return {int(l[1:]) for l in out.splitlines() if l.startswith('p') and l[1:].isdigit()}


def listens(pid):
    out = _out(['lsof', '-nP', '-w', '-a', '-p', str(pid), '-iTCP', '-sTCP:LISTEN', '-Fp'])
    return any(line == f'p{pid}' for line in out.splitlines())


def _same(row):
    """Still the recorded process: same start time and command line. Trailing whitespace is
    ignored, because `ps` keeps it in one listing format and strips it in another (a postgres
    started with `-k ''` ends in a space)."""
    now = legacy.process_identity(row['pid'])
    return now is not None and now['started'] == row['started'] and now['command'].rstrip() == row['command'].rstrip()


# Lineage (PROTOCOL.md, "What the harness may signal"): processes the harness launched, and their
# descendants as the sampler saw them, by (pid, start time).
LINEAGE = {'launched': set(), 'seen': set()}
GRADE_MARK = re.compile(r'(?:^|\s)AGENTVALUE_GRADE=([A-Za-z0-9_.-]+)')


def _environment(pid):
    try:
        return _out(['ps', '-ww', '-E', '-p', str(pid), '-o', 'command='])
    except RuntimeError:
        return ''


def _cwd(pid):
    out = _out(['lsof', '-nP', '-w', '-a', '-p', str(pid), '-d', 'cwd', '-Fn'])
    return next((l[1:] for l in out.splitlines() if l.startswith('n/')), None)


def tie(row, root, table=None, grade_token=None, _memo=None):
    """Why a process may be signalled, or None. Only a process positively tied to the run qualifies:
    'marker' (it carries the run's marker, or the grader's token), 'lineage' (it is, or descends from,
    a process the harness launched, or from a tied process), or 'service' (an arm's service, by its
    command class together with a data or state directory of the run: a postmaster on a data
    directory under the root, a redis-server whose working directory is under it, Stack's supervisor
    on a state directory under it, the app or worker interpreter of a venv under it). A process that
    merely has a run file open, or whose command mentions a run path, gets None."""
    memo = {} if _memo is None else _memo
    pid = row['pid']
    if pid in memo:
        return memo[pid]
    memo[pid] = None   # guards against cycles
    env = _environment(pid)
    mark = MARKER.search(env)
    reason = None
    if mark and group_of_marker(mark[1]) is not None:
        reason = 'marker'
    elif grade_token and any(m == grade_token for m in GRADE_MARK.findall(env)):
        reason = 'marker'
    elif (pid, row.get('started')) in LINEAGE['launched'] or (pid, row.get('started')) in LINEAGE['seen']:
        reason = 'lineage'
    else:
        command, roots = row.get('command', ''), [str(root)]
        kind = service_kind(command)
        if kind == 'postgres' and under_any(roots, postmaster_data_dir(command) or ''):
            reason = 'service'
        elif kind == 'redis' and under_any(roots, _cwd(pid) or ''):
            reason = 'service'
        elif kind == 'supervisor' and (m := re.search(r'__supervise\s+(\S+)', command)) and under_any(roots, m[1]):
            reason = 'service'
        elif kind in ('api', 'worker') and '/.venv/bin/' in command.split()[0] and under_any(roots, command.split()[0]):
            reason = 'service'
        if reason is None:
            table = table if table is not None else legacy.process_table()
            parent = table.get(row.get('ppid') if row.get('ppid') is not None else table.get(pid, {}).get('ppid'))
            if parent and parent['pid'] > 1 and parent['pid'] != os.getpid() and tie(parent, root, table, grade_token, memo):
                reason = 'lineage'
    memo[pid] = reason
    return reason


def record_lineage(rows):
    """Add to LINEAGE['seen'] every process in `rows` (a process table) that descends from a launched
    or already-seen process. Called on every sample, so a daemon is caught before it is reparented."""
    known = LINEAGE['launched'] | LINEAGE['seen']
    children = {}
    for pid, row in rows.items():
        children.setdefault(row['ppid'], []).append(pid)
    stack = [pid for pid, row in rows.items() if (pid, row['started']) in known]
    while stack:
        for child in children.get(stack.pop(), []):
            key = (child, rows[child]['started'])
            if key not in LINEAGE['seen'] and key not in LINEAGE['launched']:
                LINEAGE['seen'].add(key)
                stack.append(child)


def _owned(pid, root):
    """Kept for callers that ask whether a pid belongs to the run: now the positive tie only."""
    identity = legacy.process_identity(pid)
    return identity is not None and tie({'pid': pid, **identity}, root) is not None


def _signal(row, signum, root, grade_token=None):
    if not _same(row) or tie(row, root, grade_token=grade_token) is None:
        return False
    try:
        os.kill(row['pid'], signum)
        return True
    except ProcessLookupError:
        return False


def reclaim(rows, root, grace=5, grade_token=None):
    """Stop exactly these processes: SIGTERM, a grace period, then SIGKILL. Before every signal the
    start time and command line are checked again and the process must be positively tied to the run
    (see `tie`); one that is not is listed under 'seen_not_signalled' and never signalled.
    'survivors' are run-tied processes still alive; outsiders never make a survivor."""
    result = {'terminated': [], 'killed': [], 'not_signalled': [], 'seen_not_signalled': [], 'survivors': []}
    table = legacy.process_table()
    ties = {row['pid']: tie(row, root, table, grade_token) for row in rows}
    for row in rows:
        if ties[row['pid']] is None:
            result['seen_not_signalled'].append({'pid': row['pid'], 'command': redact.scrub(row.get('command', ''))[:200],
                                                 'reason': row.get('reason', 'listed')})
            continue
        (result['terminated'] if _signal(row, signal.SIGTERM, root, grade_token) else result['not_signalled']).append(row['pid'])
    signalled = [row for row in rows if row['pid'] in result['terminated']]
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline and any(_same(row) for row in signalled):
        time.sleep(0.1)
    for row in signalled:
        if _signal(row, signal.SIGKILL, root, grade_token):
            result['killed'].append(row['pid'])
    time.sleep(0.2)
    result['survivors'] = [row['pid'] for row in rows if ties[row['pid']] is not None and _same(row)]
    return result


def run_processes(root):
    """Every live process of this run: under its root (command, cwd, open file) or carrying its marker."""
    found = {p['pid']: p for p in legacy.owned_processes(root)}
    # Recorded lineage seeds discovery too: live identities (pid + start time) the harness launched
    # or the sampler saw descending from them.
    known = LINEAGE['launched'] | LINEAGE['seen']
    if known:
        table = legacy.process_table()
        for pid, started in known:
            row = table.get(pid)
            if row is not None and row['started'] == started and pid != os.getpid():
                found.setdefault(pid, dict(row, reason='lineage'))
    if RUN_SUFFIX:
        rows = legacy.process_table()
        ancestors, pid = set(), os.getpid()
        while pid in rows and pid not in ancestors:
            ancestors.add(pid)
            pid = rows[pid]['ppid']
        for pid, mark in markers().items():
            if pid in rows and pid not in ancestors and group_of_marker(mark) is not None:
                found.setdefault(pid, dict(rows[pid], reason='marker'))
    return list(found.values())

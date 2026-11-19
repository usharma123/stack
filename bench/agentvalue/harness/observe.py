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

MARKER = re.compile(r'(?:^|\s)AGENTVALUE_WT=([A-Za-z0-9_.-]+)')
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
    result = subprocess.run(argv, capture_output=True)
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
        rows[pid] = {'pid': pid, 'ppid': int(parts[1]), 'pgid': int(parts[2]), 'started': ' '.join(parts[4:9]), 'command': parts[9]}
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


def markers():
    """pid -> worktree marker for every live process that carries one."""
    out = _out(['ps', '-axww', '-E', '-o', 'pid=,command='])
    found = {}
    for line in out.splitlines():
        line = line.strip()
        pid, _, rest = line.partition(' ')
        match = MARKER.search(rest)
        if match and pid.isdigit():
            found[int(pid)] = match[1]
    return found


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
POSTMASTER = re.compile(r'/bin/postgres\s')


class Sampler(threading.Thread):
    """Samples every `interval` seconds: which worktree owns which live processes and listening
    ports, and any TCP connection from a process of one worktree to a port another worktree's
    process listens on (cross-worktree access). Short-lived connections between samples are
    missed, so this is evidence of access, never proof of its absence.

    Each sample also records which durable service processes of a group are gone from the
    process table since the previous sample (event (g) works from these), and is handed to every
    callable in `listeners` (the fault injector's trigger)."""

    def __init__(self, groups, out, interval=1.0, ignore_clients=(), t0=None, listeners=()):
        """`groups` is a callable returning {name: [paths]}; it is refreshed every few samples
        because an arm may create a checkout's state directories only when it first starts.
        Times `m` are monotonic seconds since `t0` (the run's clock)."""
        super().__init__(daemon=True)
        self.groups_fn, self.out, self.interval = groups, Path(out), interval
        self.t0 = time.monotonic() if t0 is None else t0
        self.listeners = list(listeners)
        self.durable_prev = {}  # (pid, started) -> (group, command, m)
        self.losses = []        # durable processes gone between two samples
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
        now = time.time()
        mono = round(time.monotonic() - self.t0, 3)
        if self.samples % 5 == 0:
            self.groups = self.groups_fn()
        rows = legacy.process_table()
        marks = markers()
        listeners, connections = tcp()
        endpoints = listeners.pop('by_endpoint', {})
        # Processes with sockets but no marker or path in their command line (redis, postgres
        # children, supervisors) are resolved once through their open files, cached by identity.
        unknown = [pid for pid in set(listeners.values()) | {c[0] for c in connections}
                   if pid in rows and self.resolved.get((pid, rows[pid]['started'])) is None
                   and owner_of(pid, rows[pid]['command'], marks, {}, self.groups) is None]
        paths = open_paths(unknown) if unknown else {}
        for pid in unknown:
            found = owner_of(pid, rows[pid]['command'], marks, paths, self.groups)
            if found:  # unresolved processes are retried on later samples, once paths are known
                self.resolved[(pid, rows[pid]['started'])] = found
        owner = {}
        for pid, row in rows.items():
            mark = owner_of(pid, row['command'], marks, {}, self.groups) or self.resolved.get((pid, row['started']))
            if mark:
                owner[pid] = mark
                key = (mark, pid, row['started'])
                entry = self.seen.setdefault(key, {'first': now, 'first_m': mono, 'command': row['command'][:300]})
                entry['last'] = now
                entry['last_m'] = mono
        # Durable processes gone from the process table since the previous sample (pid and start time).
        alive = {(pid, row['started']) for pid, row in rows.items()}
        for key, (group, command, seen_m) in self.durable_prev.items():
            if key not in alive:
                self.losses.append({'group': group, 'pid': key[0], 'started': key[1], 'command': command[:300],
                                    'last_seen_m': seen_m, 'gone_m': mono})
        self.durable_prev = {(pid, rows[pid]['started']): (group, rows[pid]['command'], mono)
                             for pid, group in owner.items() if DURABLE.search(rows[pid]['command'])}
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
                events.append({'t': now, 'm': mono, 'client': client, 'client_pid': pid, 'client_cmd': rows.get(pid, {}).get('command', '')[:200],
                               'server': server, 'port': int(remote)})
        self.cross.extend(events)
        self.samples += 1
        log.write(json.dumps({'t': round(now, 2), 'm': mono, 'procs': {m: sorted(p for p, o in owner.items() if o == m) for m in set(owner.values())},
                              'listen': {str(p): m for p, m in port_owner.items() if m}, 'cross': events}) + '\n')
        log.flush()
        info = {'m': mono, 'owner': owner, 'listeners': listeners, 'rows': rows}
        for listener in self.listeners:
            try:
                listener(info)
            except Exception as error:  # a listener's failure is recorded, never fatal to sampling
                self.errors.append(f'listener: {error!r}')

    def stop(self):
        self.stop_event.set()
        self.join(10)

    def summary(self):
        processes = {}
        for (mark, pid, started), entry in self.seen.items():
            processes.setdefault(mark, []).append({'pid': pid, 'started': started, **entry})
        return {'samples': self.samples, 'errors': self.errors[:20], 'processes': processes,
                'ports': {str(p): sorted(m) for p, m in self.ports.items()}, 'cross_worktree_connections': self.cross,
                'durable_losses': self.losses}


def _same(row):
    """Still the recorded process: same start time and command line. Trailing whitespace is
    ignored, because `ps` keeps it in one listing format and strips it in another (a postgres
    started with `-k ''` ends in a space)."""
    now = legacy.process_identity(row['pid'])
    return now is not None and now['started'] == row['started'] and now['command'].rstrip() == row['command'].rstrip()


def _owned(pid, root):
    if RUN_SUFFIX and group_of_marker(markers().get(pid)) is not None:
        return True
    return legacy.owns(root, pid)


def _signal(row, signum, root):
    if not _same(row) or not _owned(row['pid'], root):
        return False
    try:
        os.kill(row['pid'], signum)
        return True
    except ProcessLookupError:
        return False


def reclaim(rows, root, grace=5):
    """Stop exactly these processes: SIGTERM, a grace period, then SIGKILL. Before every signal the
    start time, command line and ownership by `root` are checked again, so a reused PID gets nothing."""
    result = {'terminated': [], 'killed': [], 'not_signalled': [], 'survivors': []}
    for row in rows:
        (result['terminated'] if _signal(row, signal.SIGTERM, root) else result['not_signalled']).append(row['pid'])
    signalled = [row for row in rows if row['pid'] in result['terminated']]
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline and any(_same(row) for row in signalled):
        time.sleep(0.1)
    for row in signalled:
        if _signal(row, signal.SIGKILL, root):
            result['killed'].append(row['pid'])
    time.sleep(0.2)
    result['survivors'] = [row['pid'] for row in rows if _same(row)]
    return result


def run_processes(root):
    """Every live process of this run: under its root (command, cwd, open file) or carrying its marker."""
    found = {p['pid']: p for p in legacy.owned_processes(root)}
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

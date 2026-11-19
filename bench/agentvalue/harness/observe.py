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


def markers():
    """pid -> worktree marker for every live process that carries one."""
    out = subprocess.run(['ps', '-axww', '-E', '-o', 'pid=,command='], capture_output=True, text=True).stdout
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
    out = subprocess.run(argv, capture_output=True, text=True).stdout
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
    mark = marks.get(pid)
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


def tcp():
    """Listening sockets and established connections of this user's processes."""
    out = subprocess.run(['lsof', '-nP', '-w', '-iTCP', '-a', '-u', str(os.getuid()), '-FpnT'], capture_output=True, text=True).stdout
    listeners, connections, pid, name = {}, [], None, None
    for line in out.splitlines():
        if line.startswith('p'):
            pid = int(line[1:])
        elif line.startswith('n'):
            name = line[1:]
        elif line.startswith('TST=') and pid is not None and name is not None:
            state = line[4:]
            if state == 'LISTEN':
                port = name.rsplit(':', 1)[-1]
                if port.isdigit():
                    listeners[int(port)] = pid
            elif state == 'ESTABLISHED' and '->' in name:
                local, remote = name.split('->')
                connections.append((pid, local.rsplit(':', 1)[-1], remote.rsplit(':', 1)[-1]))
    return listeners, connections


class Sampler(threading.Thread):
    """Samples every `interval` seconds: which worktree owns which live processes and listening
    ports, and any TCP connection from a process of one worktree to a port another worktree's
    process listens on (cross-worktree access). Short-lived connections between samples are
    missed, so this is evidence of access, never proof of its absence."""

    def __init__(self, groups, out, interval=1.0):
        """`groups` is a callable returning {name: [paths]}; it is refreshed every few samples
        because an arm may create a checkout's state directories only when it first starts."""
        super().__init__(daemon=True)
        self.groups_fn, self.out, self.interval = groups, Path(out), interval
        self.groups, self.resolved = groups(), {}
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
        if self.samples % 5 == 0:
            self.groups = self.groups_fn()
        rows = legacy.process_table()
        marks = markers()
        listeners, connections = tcp()
        # Processes with sockets but no marker or path in their command line (redis, postgres
        # children, supervisors) are resolved once through their open files, cached by identity.
        unknown = [pid for pid in set(listeners.values()) | {c[0] for c in connections}
                   if pid in rows and (pid, rows[pid]['started']) not in self.resolved
                   and owner_of(pid, rows[pid]['command'], marks, {}, self.groups) is None]
        paths = open_paths(unknown) if unknown else {}
        for pid in unknown:
            self.resolved[(pid, rows[pid]['started'])] = owner_of(pid, rows[pid]['command'], marks, paths, self.groups)
        owner = {}
        for pid, row in rows.items():
            mark = owner_of(pid, row['command'], marks, {}, self.groups) or self.resolved.get((pid, row['started']))
            if mark:
                owner[pid] = mark
                key = (mark, pid, row['started'])
                entry = self.seen.setdefault(key, {'first': now, 'command': row['command'][:300]})
                entry['last'] = now
        port_owner = {port: owner.get(pid) for port, pid in listeners.items()}
        for port, mark in port_owner.items():
            if mark:
                self.ports.setdefault(port, set()).add(mark)
        events = []
        for pid, local, remote in connections:
            client = owner.get(pid)
            if not client or not remote.isdigit():
                continue
            server = port_owner.get(int(remote))
            if server and server != client:
                events.append({'t': now, 'client': client, 'client_pid': pid, 'client_cmd': rows.get(pid, {}).get('command', '')[:200],
                               'server': server, 'port': int(remote)})
        self.cross.extend(events)
        self.samples += 1
        log.write(json.dumps({'t': round(now, 2), 'procs': {m: sorted(p for p, o in owner.items() if o == m) for m in set(owner.values())},
                              'listen': {str(p): m for p, m in port_owner.items() if m}, 'cross': events}) + '\n')

    def stop(self):
        self.stop_event.set()
        self.join(10)

    def summary(self):
        processes = {}
        for (mark, pid, started), entry in self.seen.items():
            processes.setdefault(mark, []).append({'pid': pid, 'started': started, **entry})
        return {'samples': self.samples, 'errors': self.errors[:20], 'processes': processes,
                'ports': {str(p): sorted(m) for p, m in self.ports.items()}, 'cross_worktree_connections': self.cross}


def _same(row):
    """Still the recorded process: same start time and command line. Trailing whitespace is
    ignored, because `ps` keeps it in one listing format and strips it in another (a postgres
    started with `-k ''` ends in a space)."""
    now = legacy.process_identity(row['pid'])
    return now is not None and now['started'] == row['started'] and now['command'].rstrip() == row['command'].rstrip()


def _signal(row, signum, root):
    if not _same(row) or not legacy.owns(root, row['pid']):
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

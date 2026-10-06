"""List or terminate host processes owned by one Tilt benchmark run (stdlib only; run with -I).

A process is owned only when its displayed argv starts with this run's private executable path:
  - <tools>/docker-compose whose very first argument selects a project of this run
    (`-p X`, `--project-name X`, `--project-name=X`, `-pX`, X = <prefix><checkout letters>,
    prefix `rwb-<run id>-`) and in which no other token could select a project. These are the
    generated shapes: Tilt's `--project-name X --project-directory D -f F events --json` and the
    harness's `-p X -f compose.yaml <cmd>`. Tilt itself spawns these readers, and a failed
    `tilt ci` can exit while one survives with PPID 1. Another run's projects or another Compose
    binary never match.
  - <tools>/tilt, only with --tilt-owned (the tools dir is inside the run-owned work dir, so
    its path alone identifies this run; a shared pre-provisioned dir does not).

ps shows argv joined by spaces, so an argument containing a space (a path, an exec payload) is
indistinguishable from several arguments, and quotes cannot be recovered (shlex cannot help).
The accepted shape is decidable anyway: argv[1] is the project option, so no earlier value can
swallow it, and with no other project token later Compose can only run as X or reject its own
command line. Every other row at the private Compose path that contains a token naming one of
this run's projects (leading options, spaced values, duplicate or payload project flags) is
AMBIGUOUS: it is never signalled, it is printed as `ambiguous ...`, and the command exits 2, so
teardown is not clean. Rows that never name this run's project are not owned. The general
Compose CLI (subcommand flag semantics, env/compose-file project names) is deliberately not
modelled; a forged argv[0] containing spaces is not detected.

Identity is (pid, start time, argv), read from `ps -ww` with LC_ALL=C. PPID is not part of it
(reparenting to 1 is the leak). Before every signal the exact PID is re-read and must still
carry the same identity. This is best effort, not atomic: ps shows a displayed command string
and second-resolution start time, and a PID could still be recycled between that re-read and
kill(). Only single PIDs are signalled:
no process group, no pattern kill. SIGTERM, a bounded wait, then SIGKILL for survivors with the
same identity, then the whole table is listed again; exit 1 if any owned process remains.

Output lines: `process <pid> ppid=<ppid>,pgid=<pgid>,start=<iso> <args>` (one per owned
process), `ambiguous <same fields>` (one per undecidable row), plus `signal`/`gone`/`skipped`
receipts from terminate. Exit 1: owned processes remain after terminate. Exit 2: ps failed or
an ambiguous row exists (undecided, so never reported clean).
"""
import argparse
import os
import re
import signal
import subprocess
import sys
import time
from collections import namedtuple

Row = namedtuple("Row", "pid ppid pgid start args")
Owned = namedtuple("Owned", "kind row")
ROW = re.compile(r"\s*(\d+)\s+(\d+)\s+(\d+)\s+(\w{3} \w{3}\s+\d+ \d\d:\d\d:\d\d \d{4})\s(.*)")


class ProbeError(RuntimeError):
    pass


class Ambiguous(ProbeError):
    """The displayed argv may belong to this run, but its ownership cannot be decided."""

    def __init__(self, row):
        super().__init__(f"ambiguous ownership: pid {row.pid}: {row.args[:300]!r}")
        self.row = row


def parse_ps(text):
    """Rows of `ps -o pid=,ppid=,pgid=,lstart=,args=` under LC_ALL=C."""
    rows = []
    for line in text.splitlines():
        if not line.strip():
            continue
        m = ROW.fullmatch(line)
        if not m:
            raise ProbeError(f"unparsable ps row: {line[:200]!r}")
        pid, ppid, pgid, lstart, args = m.groups()
        start = time.strftime("%Y-%m-%dT%H:%M:%S", time.strptime(" ".join(lstart.split()), "%a %b %d %H:%M:%S %Y"))
        rows.append(Row(int(pid), int(ppid), int(pgid), start, args.strip()))
    return rows


def ps(pid=None):
    """Whole table, or [row] / [] for one exact PID. A failed table query raises (never 'clean')."""
    argv = ["ps", "-ww", "-o", "pid=,ppid=,pgid=,lstart=,args="]
    argv[1:1] = ["-p", str(pid)] if pid is not None else ["-ax"]
    r = subprocess.run(argv, capture_output=True, text=True, env={**os.environ, "LC_ALL": "C"})
    if pid is not None and r.returncode != 0 and not r.stdout.strip():
        return []  # ps -p exits 1 when the PID does not exist
    if r.returncode != 0:
        raise ProbeError(f"ps exit {r.returncode}: {r.stderr.strip()[:200]}")
    return [row for row in parse_ps(r.stdout) if pid is None or row.pid == pid]


class Matcher:
    def __init__(self, tools, project_prefix, tilt_owned):
        paths = lambda name: {os.path.join(tools, name), os.path.realpath(os.path.join(tools, name))}
        self.compose, self.tilt = paths("docker-compose"), paths("tilt") if tilt_owned else set()
        self.project = re.compile(re.escape(project_prefix) + r"[a-z]+")

    @staticmethod
    def _rest(args, paths):
        for path in paths:
            if args == path or args.startswith(path + " "):
                return args[len(path):].split()
        return None

    def _names_ours(self, tok):
        """True if tok, as one argument, could name one of this run's projects."""
        for head in ("", "-p", "--project-name="):
            if tok.startswith(head) and self.project.fullmatch(tok[len(head):]):
                return True
        return False

    @staticmethod
    def _may_select_project(tok):
        """True for any token that could be a project option: long form, or a short cluster with p."""
        if tok.startswith("--"):
            return tok == "--project-name" or tok.startswith("--project-name=")
        return tok.startswith("-") and "p" in tok[1:]

    def _leading_project(self, tokens):
        """This run's project when argv[1] selects it (consuming 1 or 2 tokens), else None."""
        if not tokens:
            return None
        first = tokens[0]
        if first in ("-p", "--project-name"):
            return (tokens[1], 2) if len(tokens) > 1 and self.project.fullmatch(tokens[1]) else None
        for head in ("--project-name=", "-p"):
            if first.startswith(head) and self.project.fullmatch(first[len(head):]):
                return first[len(head):], 1
        return None

    def kind(self, row):
        """'tilt', 'compose' or None; raises Ambiguous when a row might be this run's."""
        if row.pid == os.getpid():
            return None
        if self._rest(row.args, self.tilt) is not None:
            return "tilt"
        tokens = self._rest(row.args, self.compose)
        if tokens is None or not any(self._names_ours(t) for t in tokens):
            return None  # never names this run's project: cannot be this run's Compose client
        lead = self._leading_project(tokens)
        if lead and not any(self._may_select_project(t) for t in tokens[lead[1]:]):
            return "compose"
        raise Ambiguous(row)

    def scan(self, rows):
        """(owned, ambiguous): Owned entries, Tilt before its children; undecidable rows."""
        found, unclear = [], []
        for row in rows:
            try:
                k = self.kind(row)
            except Ambiguous:
                unclear.append(row)
                continue
            if k:
                found.append(Owned(k, row))
        return sorted(found, key=lambda o: (o.kind != "tilt", o.row.pid)), unclear

    def owned(self, rows):
        return self.scan(rows)[0]


def describe(row, label="process"):
    return f"{label} {row.pid} ppid={row.ppid},pgid={row.pgid},start={row.start} {row.args}"


def same(row, probe):
    """True while row's exact PID still carries row's identity (start time and argv)."""
    return any(now.start == row.start and now.args == row.args for now in probe(row.pid))


def terminate(matcher, probe=ps, kill=os.kill, sleep=time.sleep, clock=time.monotonic,
              grace=10.0, rounds=3, out=print):
    for _ in range(rounds):
        owned = matcher.owned(probe())
        if not owned:
            break
        pending = [o.row for o in owned]
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for row in list(pending):
                if not same(row, probe):  # exited or PID recycled since the scan: skip it
                    out(f"gone pid={row.pid} start={row.start}")
                    pending.remove(row)
                    continue
                try:
                    kill(row.pid, sig)
                    out(f"signal {signal.Signals(sig).name} pid={row.pid} start={row.start} {row.args}")
                except ProcessLookupError:
                    out(f"gone pid={row.pid} start={row.start}")
                    pending.remove(row)
                except PermissionError as error:
                    out(f"skipped pid={row.pid}: {error}")
                    pending.remove(row)
            deadline = clock() + grace
            while pending and clock() < deadline:
                sleep(0.1)
                pending = [row for row in pending if same(row, probe)]
            if not pending:
                break
    left, unclear = matcher.scan(probe())
    for o in left:
        out(describe(o.row))
    for row in unclear:  # never signalled: ownership undecided, so teardown is not clean
        out(describe(row, "ambiguous"))
    return 2 if unclear else 1 if left else 0


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("list", "terminate"))
    parser.add_argument("--tools", required=True)
    parser.add_argument("--project-prefix", required=True)
    parser.add_argument("--tilt-owned", action="store_true")
    args = parser.parse_args(argv)
    if not re.fullmatch(r"rwb-[a-z0-9-]+-", args.project_prefix):
        parser.error(f"bad project prefix {args.project_prefix!r}")
    matcher = Matcher(args.tools, args.project_prefix, args.tilt_owned)
    try:
        if args.action == "list":
            found, unclear = matcher.scan(ps())
            for o in found:
                print(describe(o.row))
            for row in unclear:
                print(describe(row, "ambiguous"))
            code = 2 if unclear else 0
        else:
            code = terminate(matcher)
        if code == 2:
            print("owned-processes: ambiguous Compose rows name this run's project; not signalled",
                  file=sys.stderr)
        return code
    except ProbeError as error:
        print(f"owned-processes: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

"""Offline regressions for Tilt host-process ownership, termination and teardown verification.

No Docker, no network, no Tilt: real short-lived `sleep` processes carry the exact argv shapes
(`exec -a`) seen in smoke-tilt-1, where a failed `tilt ci` left the private Compose
`events --json` reader alive with PPID 1 after the harness reported a clean teardown.

python3 -m unittest bench/tests/test_tilt_process_cleanup.py -v
"""
import importlib.util
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

import run as harness  # noqa: E402
from rwb.adapters.tilt import TiltAdapter, remove_owned  # noqa: E402
from rwb.record import Recorder  # noqa: E402
from rwb.transport import HostTransport  # noqa: E402

spec = importlib.util.spec_from_file_location("owned_processes", BENCH / "adapters/tilt/owned_processes.py")
owned = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owned)

RUN = "20261006t000000-abc123"
OTHER_RUN = "20261006t000000-zzz999"
# The exact live row recorded in remaining-runtime-verification/tilt-live-event-process.json.
EVIDENCE_WORK = "/var/folders/l3/6x2gqh2x1rbdkr21_hzckff40000gn/T/rwb-20261006t161123-8af6e8-n94mftjj"
EVIDENCE_ROW = (
    f"68320     1 68311 Tue Oct  6 12:12:03 2026     {EVIDENCE_WORK}/tools/docker-compose "
    "--project-name rwb-20261006t161123-8af6e8-d --project-directory "
    f"/private{EVIDENCE_WORK}/w/d -f /private{EVIDENCE_WORK}/w/d/compose.yaml events --json")


def row(pid, args, start="2026-10-06T12:12:03", ppid=1):
    return owned.Row(pid, ppid, pid, start, args)


AMBIGUOUS = "ambiguous"


def kind_or_ambiguous(matcher, r):
    try:
        return matcher.kind(r)
    except owned.Ambiguous:
        return AMBIGUOUS


def events_argv(compose, project, work):
    return f"{compose} --project-name {project} --project-directory {work}/w/d -f {work}/w/d/compose.yaml events --json"


class Matching(unittest.TestCase):
    def setUp(self):
        self.m = owned.Matcher("/T/run/tools", f"rwb-{RUN}-", tilt_owned=True)
        self.c = "/T/run/tools/docker-compose"

    def test_evidence_row_is_parsed_and_owned(self):
        [r] = owned.parse_ps(EVIDENCE_ROW)
        self.assertEqual((r.pid, r.ppid, r.pgid, r.start), (68320, 1, 68311, "2026-10-06T12:12:03"))
        m = owned.Matcher(f"{EVIDENCE_WORK}/tools", "rwb-20261006t161123-8af6e8-", tilt_owned=True)
        self.assertEqual(m.kind(r), "compose")
        # The old probe searched only <tools>/tilt and could never see this reader.
        self.assertNotIn(f"{EVIDENCE_WORK}/tools/tilt", r.args)

    def test_only_this_runs_private_binaries_and_projects(self):
        cases = {
            events_argv(self.c, f"rwb-{RUN}-d", "/w"): "compose",
            f"{self.c} -p rwb-{RUN}-a -f compose.yaml exec -T app bash -c true": "compose",
            f"{self.c} --project-name=rwb-{RUN}-b down": "compose",
            "/T/run/tools/tilt ci --port 0 --timeout 900s": "tilt",
            events_argv(self.c, f"rwb-{OTHER_RUN}-d", "/w"): None,           # another run
            events_argv(self.c, f"rwb-{RUN}-d-x", "/w"): None,               # not an exact project
            events_argv("/usr/local/bin/docker-compose", f"rwb-{RUN}-d", "/w"): None,  # other binary
            events_argv(self.c + "-evil", f"rwb-{RUN}-d", "/w"): None,       # path-prefix collision
            f"{self.c} version": None,                                       # no project token
            f"bash -c '{self.c} --project-name rwb-{RUN}-d events'": None,   # mentions, not runs
            "/T/other/tools/tilt ci": None,
            "tilt ci --port 0": None,
        }
        for args, kind in cases.items():
            with self.subTest(args):
                self.assertEqual(self.m.kind(row(4242, args)), kind)

    def test_project_must_be_first_argument_and_unique(self):
        mine, other = f"rwb-{RUN}-a", "rwb-other-a"
        A = AMBIGUOUS
        cases = {
            f"{self.c} -p {mine} --project-directory /w -f /w/compose.yaml --env-file /w/.env "
            "--profile x --ansi never --progress plain --parallel 2 --compatibility --dry-run ps": "compose",
            f"{self.c} -p{mine} events": "compose",                        # attached short form
            f"{self.c} --project-name {mine}": "compose",                  # Compose runs as this project
            f"{self.c} -f compose.yaml -p": None,                          # names no project of this run
            f"{self.c} --project-name {other} exec -T app echo hi": None,  # never names this run
            # A leading option could swallow the project flag into its (spaced) value.
            f"{self.c} -f=/w/compose.yaml --project-name {mine} events --json": A,
            f"{self.c} --unknown -p {mine} events": A,
            # Another project's container payloads carrying this run's project flag.
            f"{self.c} --project-name {other} exec -T app echo --project-name {mine}": A,
            f"{self.c} -p {other} run --rm app sh -c 'x' -p {mine}": A,
            f"{self.c} -p {other} exec app docker-compose --project-name={mine} events": A,
            f"{self.c} exec -T app echo --project-name {mine}": A,         # no global project at all
            # Conflicting or duplicate declarations.
            f"{self.c} -p {other} --project-name {mine} events": A,
            f"{self.c} --project-name {mine} -p {other} events": A,
            f"{self.c} --project-name={mine} -p {mine} events": A,
            f"{self.c} -p {mine} events --json -Tp {other}": A,            # short cluster containing p
        }
        for m in (self.m, owned.Matcher("/shared/tools", f"rwb-{RUN}-", tilt_owned=False)):
            compose = self.c if m is self.m else "/shared/tools/docker-compose"
            for args, kind in cases.items():
                with self.subTest(args=args, tools=compose):
                    self.assertEqual(kind_or_ambiguous(m, row(4242, args.replace(self.c, compose))), kind)

    def test_space_split_display_never_claims_or_omits(self):
        """Round-2 counterexamples: ps joins argv with spaces, so values containing spaces are
        undecidable. Exact probe rows from /tmp/astra-tilt-r2-probes.log."""
        m = owned.Matcher("/shared/tools", f"rwb-{RUN}-", tilt_owned=False)
        c, mine = "/shared/tools/docker-compose", f"rwb-{RUN}-a"
        cases = {
            # argv [-p mine, -f '/tmp/my project/compose.yaml', -p rwb-other-a, events, --json]
            f"{c} -p {mine} -f /tmp/my project/compose.yaml -p rwb-other-a events --json": AMBIGUOUS,
            # argv [--env-file '/tmp/env -p mine events.env', --project-name rwb-other-a, events, --json]
            f"{c} --env-file /tmp/env -p {mine} events.env --project-name rwb-other-a events --json": AMBIGUOUS,
            # Previously silent omissions (false clean): spaced value / unknown option before the project.
            f"{c} --project-directory /tmp/my work -p {mine} events": AMBIGUOUS,
            f"{c} --new-option -p {mine} events": AMBIGUOUS,
            # Spaces after a leading project cannot change it: no later token can select a project.
            f"{c} -p {mine} --project-directory /tmp/my work events": "compose",
            events_argv(c, mine, "/tmp/my work"): "compose",
            # A harness exec whose payload has a -p flag is undecidable too (visible, not signalled).
            f"{c} -p {mine} -f compose.yaml exec -T app bash -c mkdir -p /x": AMBIGUOUS,
            f"{c} -p {mine} -f compose.yaml exec -T app bash -c true": "compose",
            # The confirmed wrapper behaviour: mentions the private path, does not run it.
            f"bash -c '{c} --project-name {mine} events'": None,
        }
        for args, kind in cases.items():
            with self.subTest(args):
                self.assertEqual(kind_or_ambiguous(m, row(4242, args)), kind)
        with self.assertRaises(owned.ProbeError):  # Ambiguous is a visible ProbeError
            m.kind(row(4242, f"{c} --new-option -p {mine} events"))

    def test_shared_tools_dir_never_claims_tilt(self):
        m = owned.Matcher("/T/shared/tools", f"rwb-{RUN}-", tilt_owned=False)
        self.assertIsNone(m.kind(row(1, "/T/shared/tools/tilt ci")))
        self.assertEqual(m.kind(row(2, events_argv("/T/shared/tools/docker-compose", f"rwb-{RUN}-d", "/w"))), "compose")

    def test_unparsable_ps_fails_closed(self):
        with self.assertRaises(owned.ProbeError):
            owned.parse_ps("garbage\n")


class FakeTable:
    """Process table seam: pid -> Row; kill() applies a per-PID reaction."""

    def __init__(self, rows, react=None):
        self.rows = {r.pid: r for r in rows}
        self.react = react or {}
        self.signals = []
        self.now = 0.0

    def probe(self, pid=None):
        if pid is None:
            return list(self.rows.values())
        return [self.rows[pid]] if pid in self.rows else []

    def kill(self, pid, sig):
        self.signals.append((pid, sig))
        if pid not in self.rows:
            raise ProcessLookupError(pid)
        self.react.get(pid, lambda t, p, s: t.rows.pop(p))(self, pid, sig)

    def sleep(self, seconds):
        self.now += seconds

    def run(self, matcher):
        out = []
        code = owned.terminate(matcher, probe=self.probe, kill=self.kill, sleep=self.sleep,
                               clock=lambda: self.now, grace=1.0, out=out.append)
        return code, out


class Terminate(unittest.TestCase):
    def setUp(self):
        self.m = owned.Matcher("/T/run/tools", f"rwb-{RUN}-", tilt_owned=True)
        self.leak = row(100, events_argv("/T/run/tools/docker-compose", f"rwb-{RUN}-d", "/T/run"))
        self.other = row(200, events_argv("/T/run/tools/docker-compose", f"rwb-{OTHER_RUN}-d", "/T/x"), ppid=50)
        self.plain = row(300, "sleep 300", ppid=50)

    def test_failed_ci_leak_terminated_unrelated_untouched(self):
        t = FakeTable([self.leak, self.other, self.plain])
        code, out = t.run(self.m)
        self.assertEqual(code, 0, out)
        self.assertEqual(t.signals, [(100, signal.SIGTERM)])
        self.assertEqual(set(t.rows), {200, 300})

    def test_other_projects_with_this_runs_flag_in_payload_never_signalled(self):
        c = "/T/run/tools/docker-compose"
        decoys = [
            row(401, f"{c} --project-name rwb-other-a exec -T app echo --project-name rwb-{RUN}-a", ppid=50),
            row(402, f"{c} -p rwb-other-a run --rm app sh -c true -p rwb-{RUN}-a", ppid=50),
            row(403, f"{c} -p rwb-other-a --project-name rwb-{RUN}-a events --json", ppid=50),
        ]
        t = FakeTable([self.leak, *decoys])
        code, out = t.run(self.m)
        self.assertEqual(code, 2, out)  # undecided rows: never signalled, never clean
        self.assertEqual(t.signals, [(100, signal.SIGTERM)])  # only the original events reader
        self.assertEqual(set(t.rows), {401, 402, 403})
        self.assertEqual([line.split()[:2] for line in out if line.startswith("ambiguous ")],
                         [["ambiguous", "401"], ["ambiguous", "402"], ["ambiguous", "403"]])

        t = FakeTable(decoys)
        code, out = t.run(self.m)
        self.assertEqual((code, t.signals), (2, []))
        self.assertEqual(len(out), 3, out)

    def test_round2_counterexamples_no_signal_and_not_clean(self):
        c, mine = "/shared/tools/docker-compose", f"rwb-{RUN}-a"
        m = owned.Matcher("/shared/tools", f"rwb-{RUN}-", tilt_owned=False)
        reader = row(100, events_argv(c, f"rwb-{RUN}-d", "/T/run"))
        rows = [
            row(501, f"{c} -p {mine} -f /tmp/my project/compose.yaml -p rwb-other-a events --json", ppid=50),
            row(502, f"{c} --env-file /tmp/env -p {mine} events.env --project-name rwb-other-a events --json", ppid=50),
            row(503, f"{c} --project-directory /tmp/my work -p {mine} events", ppid=50),
            row(504, f"{c} --new-option -p {mine} events", ppid=50),
            row(505, events_argv(c, "rwb-other-a", "/tmp/my work"), ppid=50),  # another project: untouched
        ]
        t = FakeTable([reader, *rows])
        code, out = t.run(m)
        self.assertEqual(code, 2, out)
        self.assertEqual(t.signals, [(100, signal.SIGTERM)])  # the real event reader is still terminated
        self.assertEqual(set(t.rows), {501, 502, 503, 504, 505})
        self.assertEqual(sorted(int(line.split()[1]) for line in out if line.startswith("ambiguous ")),
                         [501, 502, 503, 504])

    def test_list_exits_2_on_ambiguous_rows(self):
        c = "/shared/tools/docker-compose"
        table = [row(100, events_argv(c, f"rwb-{RUN}-d", "/w")),
                 row(501, f"{c} --project-directory /tmp/my work -p rwb-{RUN}-a events")]
        argv = ["list", "--tools", "/shared/tools", "--project-prefix", f"rwb-{RUN}-"]
        saved = owned.ps
        owned.ps = lambda pid=None: table
        try:
            import contextlib
            import io
            stdout, stderr = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                code = owned.main(argv)
            self.assertEqual(code, 2)
            self.assertEqual([line.split()[:2] for line in stdout.getvalue().splitlines()],
                             [["process", "100"], ["ambiguous", "501"]])
            self.assertIn("ambiguous", stderr.getvalue())
            table.pop()
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(owned.main(argv), 0)
        finally:
            owned.ps = saved

    def test_recycled_pid_is_skipped_at_recheck(self):
        t = FakeTable([self.leak])
        recycled = self.leak._replace(start="2026-10-06T12:30:00", args="sleep 1")
        real_probe = t.probe

        def probe(pid=None):
            listing = real_probe(pid)
            if pid is not None:  # the PID was recycled between the table scan and the signal
                t.rows[100] = recycled
                return [recycled]
            return listing
        code = owned.terminate(self.m, probe=probe, kill=t.kill, sleep=t.sleep,
                               clock=lambda: t.now, grace=1.0, out=lambda line: None)
        self.assertEqual(t.signals, [])
        self.assertEqual(code, 0)  # the recycled process is not owned

    def test_term_ignored_escalates_to_kill_same_pid_only(self):
        ignore_term = lambda tbl, pid, sig: tbl.rows.pop(pid) if sig == signal.SIGKILL else None
        t = FakeTable([self.leak, self.plain], react={100: ignore_term})
        code, out = t.run(self.m)
        self.assertEqual(code, 0, out)
        self.assertEqual(t.signals, [(100, signal.SIGTERM), (100, signal.SIGKILL)])
        self.assertIn(300, t.rows)

    def test_unkillable_owned_process_fails(self):
        t = FakeTable([self.leak], react={100: lambda tbl, pid, sig: None})
        code, out = t.run(self.m)
        self.assertEqual(code, 1)
        self.assertTrue(any(line.startswith("process 100 ") for line in out), out)
        self.assertTrue(all(pid == 100 for pid, _ in t.signals))


def wait_for(predicate, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    return predicate()


@unittest.skipUnless(Path("/bin/bash").exists() and sys.platform in ("darwin", "linux"), "needs bash + ps")
class LiveTeardown(unittest.TestCase):
    """Real processes, real ps/kill, real harness teardown(); Docker is a logging stub."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="rwb-tilt-proc-"))
        self.work = self.tmp / "work"
        (self.work / "w").mkdir(parents=True)
        self.tools = self.work / "tools"
        self.tools.mkdir()
        self.ad = TiltAdapter({}, None, RUN)
        self.ad.root = str(self.work / "w")
        self.compose = str(self.tools / "docker-compose")
        stub = self.tmp / "bin" / "docker"
        stub.parent.mkdir()
        stub.write_text(f"#!/bin/bash\necho \"$*\" >> {shlex.quote(str(self.tmp / 'docker.log'))}\n")
        stub.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{stub.parent}:{os.environ['PATH']}")
        self.children = []
        self.matcher = owned.Matcher(str(self.tools), f"rwb-{RUN}-", tilt_owned=True)

    def tearDown(self):
        # Safety net for a failing test: this test run's own fake processes only.
        owned.terminate(self.matcher, grace=2.0, out=lambda line: None)
        for proc in self.children:
            if proc.poll() is None:
                proc.kill()
            proc.wait()
        subprocess.run(["rm", "-rf", str(self.tmp)])

    def spawn(self, argv0):
        """A direct child whose ps args start with argv0 (sleep under that argv[0])."""
        proc = subprocess.Popen(["/bin/bash", "-c", 'exec -a "$0" sleep 60', argv0], start_new_session=True)
        self.children.append(proc)
        return proc

    def failed_ci_leaves_events_reader(self):
        """Fake `tilt ci` (private path) starts the Compose events reader and exits 1."""
        reader = events_argv(self.compose, f"rwb-{RUN}-d", str(self.work))
        body = f'(exec -a {shlex.quote(reader)} sleep 60 </dev/null >/dev/null 2>&1) & echo $!; exit 1'
        ci = subprocess.run(["/bin/bash", "-c", f'exec -a "$0" /bin/bash -c {shlex.quote(body)}',
                             f"{self.tools}/tilt"], capture_output=True, text=True, start_new_session=True)
        self.assertEqual(ci.returncode, 1)
        pid = int(ci.stdout.split()[-1])
        [r] = wait_for(lambda: [x for x in owned.ps(pid) if x.args.startswith(self.compose + " ")])
        if sys.platform == "darwin":
            self.assertEqual(r.ppid, 1, r)  # reparented, exactly as PID 68320 was
        return r

    def unrelated(self):
        return [
            self.spawn(events_argv(self.compose, f"rwb-{OTHER_RUN}-d", "/elsewhere")),     # other run's project
            self.spawn(events_argv("/usr/local/bin/docker-compose", f"rwb-{RUN}-d", "/x")),  # not our binary
            self.spawn(f"{self.tmp}/other-tools/tilt ci --port 0"),                         # other tools dir
            self.spawn("sleep"),                                                            # plain child
        ]

    def teardown(self, adapter):
        recorder = Recorder(self.tmp / "out")
        self.addCleanup(recorder.close)
        tx = HostTransport(recorder, self.work, env=self.env)
        scenario = type("S", (), dict(diagnose=lambda s: None, collect_artifacts=lambda s, d: [],
                                      cleanup=lambda s: []))()
        meta = {}
        return harness.teardown(scenario, adapter, tx, self.tmp / "out", meta), tx

    def test_failed_ci_reader_terminated_before_workdir_removal_unrelated_untouched(self):
        leak = self.failed_ci_leaves_events_reader()
        tilt = self.spawn(f"{self.tools}/tilt ci --port 0 --timeout 900s")
        others = self.unrelated()
        listed = subprocess.run(["/bin/bash", "-c", self.ad.supervisor_processes()], capture_output=True,
                                text=True, env=self.env)
        self.assertEqual(listed.returncode, 0, listed.stderr)
        pids = {int(line.split()[1]) for line in listed.stdout.splitlines()}
        self.assertEqual(pids, {leak.pid, tilt.pid}, listed.stdout)

        problems, _ = self.teardown(self.ad)
        self.assertEqual(problems, [])  # run.py removes the work dir only in this case
        self.assertEqual(owned.ps(leak.pid), [])
        self.assertIsNotNone(wait_for(lambda: tilt.poll() is not None))
        self.assertEqual(tilt.returncode, -signal.SIGTERM)
        for proc in others:
            self.assertIsNone(proc.poll(), proc.args)
        receipts = [p.read_text() for p in sorted((self.tmp / "out" / "logs").glob("*host-cleanup.stdout"))]
        self.assertIn(f"signal SIGTERM pid={leak.pid} ", receipts[0])
        self.assertNotIn("process ", receipts[0])  # nothing owned survived

    def test_live_leak_blocks_clean_teardown_when_not_terminated(self):
        # The pre-fix cleanup (Docker only) must no longer be able to report a clean teardown.
        leak = self.failed_ci_leaves_events_reader()
        docker_only = TiltAdapter({}, None, RUN)
        docker_only.root = self.ad.root
        docker_only.cleanup_host = lambda: remove_owned(docker_only.docker_resources())
        problems, _ = self.teardown(docker_only)
        self.assertEqual(problems, ["owned host resources remain"])
        [still] = owned.ps(leak.pid)
        self.assertEqual((still.start, still.args), (leak.start, leak.args))  # nothing was signalled


if __name__ == "__main__":
    unittest.main()

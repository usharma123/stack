"""Offline transport PID-registration regressions: no network, no Docker, no services.

The generated WRAPPER and TIMEOUT_KILL shells run under the local bash with stub commands;
Docker argv is checked through a stub runner. No live unrelated PID is ever signalled.
"""
from pathlib import Path
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb.record import Recorder  # noqa: E402
from rwb.transport import TIMEOUT_KILL, WRAPPER, DockerTransport, HostTransport, Runner  # noqa: E402

BASH = shutil.which("bash") or "/bin/bash"
ERROR = "RWB-TRANSPORT-ERROR: pid registration failed"
# Inner timing markers need bash >= 5 (EPOCHREALTIME); macOS /bin/bash 3.2 has none.
HAS_EPOCH = subprocess.run([BASH, "-c", '[ -n "$EPOCHREALTIME" ]']).returncode == 0
IS_ROOT = hasattr(os, "geteuid") and os.geteuid() == 0


def scratch(test, name="rwb transport "):
    """Temporary directory whose path contains a space; made writable again before removal."""
    tmp = tempfile.mkdtemp(prefix=name)

    def cleanup():
        for root, dirs, _ in os.walk(tmp):
            for d in dirs:
                p = os.path.join(root, d)
                if not os.path.islink(p):
                    os.chmod(p, 0o700)
        shutil.rmtree(tmp)
    test.addCleanup(cleanup)
    return Path(tmp)


def wrap(body, pid_dir, path_prefix=None):
    env = dict(os.environ)
    if path_prefix:
        env["PATH"] = f"{path_prefix}{os.pathsep}{env['PATH']}"
    p = subprocess.run([BASH, "-c", WRAPPER, "rwb", body, str(pid_dir)], capture_output=True,
                       env=env, timeout=30)
    return p.returncode, p.stdout.decode(), p.stderr.decode()


def stub(directory, name, script):
    directory.mkdir(exist_ok=True)
    path = directory / name
    path.write_text("#!/bin/sh\n" + script)
    path.chmod(0o755)
    return directory


class WrapperRegistrationTest(unittest.TestCase):
    def setUp(self):
        self.tmp = scratch(self)
        self.marker = self.tmp / "body ran"
        # The body leaves a file behind, so "did not start" is checked beyond stdout.
        self.body = f"touch {json.dumps(str(self.marker))}; printf BODY_RAN"

    def assert_refused(self, result):
        code, out, err = result
        self.assertEqual(code, 126)
        self.assertEqual(out, "")
        self.assertIn(ERROR, err)
        self.assertNotIn("@@RWB-INNER", err)
        self.assertFalse(self.marker.exists(), "body started after failed registration")

    def test_success_records_session_pid_and_runs_body(self):
        d = self.tmp / "step 6"
        code, out, err = wrap('printf "BODY_RAN %s %s" "$$" "$(umask)"', d)
        self.assertEqual(code, 0, err)
        _, pid, body_umask = out.split()
        self.assertEqual((d / "pid").read_text(), f"{pid}\n")
        self.assertGreaterEqual(int(pid), 2)
        self.assertEqual(stat.S_IMODE(d.stat().st_mode), 0o700)
        # umask 077 is confined to registration; the body keeps the caller's policy.
        outer = subprocess.run([BASH, "-c", "umask"], capture_output=True).stdout.decode().strip()
        self.assertEqual(body_umask, outer)
        self.assertEqual("@@RWB-INNER" in err, HAS_EPOCH)

    def test_nonzero_body_exit_is_preserved(self):
        code, out, err = wrap("printf BODY_RAN; exit 13", self.tmp / "step 7")
        self.assertEqual((code, out), (13, "BODY_RAN"))
        if HAS_EPOCH:
            self.assertRegex(err, r"@@RWB-INNER \d+ \d+ 13\n$")

    def test_distinct_steps_get_distinct_directories(self):
        for seq in (3, 5):
            self.assertEqual(wrap(self.body, self.tmp / f"rwb-pid-c-{seq}")[0], 0)
        self.assertTrue((self.tmp / "rwb-pid-c-3" / "pid").is_file())
        self.assertTrue((self.tmp / "rwb-pid-c-5" / "pid").is_file())

    @unittest.skipIf(IS_ROOT, "root bypasses directory permissions")
    def test_unwritable_parent_reproduction_stops_before_body(self):
        # The archived failure: a parent the executing user cannot write into.
        locked = self.tmp / "locked"
        locked.mkdir(mode=0o500)
        self.assert_refused(wrap(self.body, locked / "6"))
        self.assertFalse((locked / "6").exists())

    def test_existing_directory_is_not_adopted(self):
        d = self.tmp / "step"
        d.mkdir(mode=0o755)
        self.assert_refused(wrap(self.body, d))
        self.assertFalse((d / "pid").exists())
        self.assertEqual(stat.S_IMODE(d.stat().st_mode), 0o755)

    def test_symlink_is_not_followed(self):
        target = self.tmp / "elsewhere"
        target.mkdir()
        (self.tmp / "step").symlink_to(target)
        self.assert_refused(wrap(self.body, self.tmp / "step"))
        self.assertEqual(list(target.iterdir()), [])

    def test_failed_directory_creation_stops_before_body(self):
        bin_dir = stub(self.tmp / "bin", "mkdir", "exit 1\n")
        self.assert_refused(wrap(self.body, self.tmp / "step", path_prefix=bin_dir))

    def test_failed_pid_write_stops_before_body(self):
        # mkdir "succeeds" but leaves a directory where the PID file must go (fails even as root).
        bin_dir = stub(self.tmp / "bin", "mkdir",
                       'for a; do last=$a; done\n/bin/mkdir "$last" && /bin/mkdir "$last/pid"\n')
        self.assert_refused(wrap(self.body, self.tmp / "step", path_prefix=bin_dir))


class TimeoutCleanupScriptTest(unittest.TestCase):
    """TIMEOUT_KILL with `kill` replaced by a logging shell function."""

    def setUp(self):
        self.tmp = scratch(self)
        self.log = self.tmp / "kill log"

    def cleanup(self, contents=None, probe=0, kill=0, make_dir=True):
        d = self.tmp / "rwb-pid-c-9"
        if make_dir:
            d.mkdir(mode=0o700)
            if contents is not None:
                (d / "pid").write_text(contents)
        stub_kill = (f'kill() {{ echo "$*" >> {json.dumps(str(self.log))}; '
                     f'case "$1" in -0) return {probe};; *) return {kill};; esac; }}\n')
        p = subprocess.run([BASH, "-c", stub_kill + TIMEOUT_KILL, "rwb-timeout", str(d)],
                           capture_output=True, timeout=30)
        calls = self.log.read_text().splitlines() if self.log.exists() else []
        return p.returncode, p.stdout.decode() + p.stderr.decode(), calls

    def test_invalid_registrations_never_invoke_kill(self):
        cases = dict(empty="", newline="\n", zero="0\n", one="1\n", negative="-5\n", alpha="abc\n",
                     spaced="12 34\n", leading_zero="0042\n", glob="*\n", huge="99999999999\n",
                     option="-KILL\n")
        for name, contents in cases.items():
            with self.subTest(name):
                shutil.rmtree(self.tmp / "rwb-pid-c-9", ignore_errors=True)
                code, text, calls = self.cleanup(contents)
                self.assertEqual(code, 4, text)
                self.assertEqual(calls, [])
                self.assertIn("RWB-TIMEOUT-CLEANUP", text)

    def test_absent_registration_never_invokes_kill(self):
        for make_dir in (False, True):
            with self.subTest(make_dir=make_dir):
                shutil.rmtree(self.tmp / "rwb-pid-c-9", ignore_errors=True)
                code, text, calls = self.cleanup(None, make_dir=make_dir)
                self.assertEqual(code, 3)
                self.assertIn("no pid registration", text)
                self.assertEqual(calls, [])

    def test_symlinked_pid_file_is_rejected(self):
        d = self.tmp / "rwb-pid-c-9"
        d.mkdir()
        (self.tmp / "other").write_text("4242\n")
        (d / "pid").symlink_to(self.tmp / "other")
        code, _, calls = self.cleanup(None, make_dir=False)
        self.assertEqual((code, calls), (3, []))

    @unittest.skipIf(IS_ROOT, "root can read a mode-000 file")
    def test_unreadable_pid_file_is_rejected(self):
        d = self.tmp / "rwb-pid-c-9"
        d.mkdir(mode=0o700)
        pid = d / "pid"
        pid.write_text("4242\n")
        pid.chmod(0)
        try:
            code, text, calls = self.cleanup(None, make_dir=False)
        finally:
            pid.chmod(0o600)
        self.assertEqual((code, calls), (3, []))
        self.assertIn("no pid registration", text)

    def test_valid_pid_kills_only_that_negative_group(self):
        code, text, calls = self.cleanup("4242\n")
        self.assertEqual(code, 0, text)
        self.assertEqual(calls, ["-0 -- -4242", "-KILL -- -4242"])
        self.assertIn("killed process group 4242", text)

    def test_already_exited_group_is_reported_without_kill(self):
        code, text, calls = self.cleanup("4242\n", probe=1)
        self.assertEqual(code, 0)
        self.assertEqual(calls, ["-0 -- -4242"])
        self.assertIn("already exited", text)

    def test_failed_group_kill_is_visible(self):
        code, text, calls = self.cleanup("4242\n", kill=1)
        self.assertEqual(code, 5)
        self.assertIn("could not kill process group 4242", text)
        self.assertEqual(calls[-1], "-KILL -- -4242")


class StubDocker:
    """Records argv; exec bodies return the scripted primary result, cleanup the scripted helper."""

    def __init__(self, primary=(124, b"", b"", True), cleanup=(0, b"RWB-TIMEOUT-CLEANUP: killed\n", b"")):
        self.calls, self.primary, self.cleanup = [], primary, cleanup

    def __call__(self, argv, timeout, env=None, cwd=None):
        self.calls.append(argv)
        if argv[1:3] == ["image", "inspect"]:
            return 0, b"sha256:img\n", b"", False, 1
        if argv[1] == "exec" and "rwb-timeout" in argv:
            return (*self.cleanup[:3], False, 1)
        if argv[1] == "exec":
            return (*self.primary, 1)
        return 0, b"", b"", False, 1


class DockerRegistrationTest(unittest.TestCase):
    def make(self, runner, root=None):
        tmp = scratch(self)
        rec = Recorder(tmp / "r")
        self.addCleanup(rec.close)
        tx = DockerTransport(rec, "run1", "toy", "ev-base", [], runner=runner)
        if root:
            tx.PID_ROOT = str(root)
        tx.start()
        return tx, rec

    def steps(self, rec):
        return [json.loads(line) for line in (rec.out / "steps.jsonl").read_text().splitlines()]

    def test_root_and_agent_steps_use_distinct_exact_directories(self):
        runner = StubDocker(primary=(0, b"", b"", False))
        tx, _ = self.make(runner)
        a = tx.exec("preflight", "setup", "true", user="root")
        b = tx.exec("canary", "setup", "true")
        execs = [c for c in runner.calls if c[1] == "exec"]
        self.assertEqual([c[3] for c in execs], ["root", "agent"])
        dirs = [c[-1] for c in execs]
        self.assertEqual(dirs, [f"/tmp/rwb-pid-rwb-toy-run1-{a.seq}", f"/tmp/rwb-pid-rwb-toy-run1-{b.seq}"])
        for c in execs:
            self.assertEqual(c[4:12], ["-w", "/tmp", "rwb-toy-run1", "setsid", "-w", "bash", "-c", WRAPPER])
            self.assertEqual(os.path.dirname(c[-1]), "/tmp")
        self.assertFalse(any("/tmp/rwb-pids" in " ".join(c) for c in runner.calls))

    def test_timeout_cleans_up_exact_step_path_as_root_and_records_evidence(self):
        runner = StubDocker(cleanup=(5, b"", b"RWB-TIMEOUT-CLEANUP: could not kill process group 77\n"))
        tx, rec = self.make(runner)
        tx.exec("before", "warm", "true")
        r = tx.exec("slow", "warm", "sleep 999", timeout=1, user="agent")
        primary, kill = runner.calls[-2], runner.calls[-1]
        pid_dir = f"/tmp/rwb-pid-rwb-toy-run1-{r.seq}"
        self.assertEqual(primary[-1], pid_dir)
        self.assertEqual(kill, ["docker", "exec", "-u", "root", "rwb-toy-run1", "bash", "-c", TIMEOUT_KILL,
                                "rwb-timeout", pid_dir])
        # A failed group kill stays visible; the primary step remains a timed-out failure.
        self.assertTrue(r.timed_out)
        self.assertFalse(r.ok)
        self.assertEqual(r.code, 124)
        evidence = r.extra["timeout_cleanup"]
        self.assertEqual((evidence["exit"], evidence["timed_out"]), (5, False))
        self.assertIn("could not kill process group 77", evidence["stderr"])
        step = self.steps(rec)[-1]
        self.assertEqual(step["seq"], r.seq)
        self.assertTrue(step["timed_out"])
        self.assertEqual(step["exit"], 124)
        self.assertEqual(step["timeout_cleanup"]["exit"], 5)
        self.assertEqual(step["timeout_cleanup"]["argv"][-1], pid_dir)

    def test_successful_cleanup_does_not_turn_timeout_into_success(self):
        runner = StubDocker()
        tx, _ = self.make(runner)
        r = tx.exec("slow", "warm", "sleep 999", timeout=1)
        self.assertEqual(r.extra["timeout_cleanup"]["exit"], 0)
        self.assertTrue(r.timed_out)
        self.assertFalse(r.ok)

    def test_completed_steps_keep_their_exit_and_skip_cleanup(self):
        for code in (0, 13):
            with self.subTest(code=code):
                runner = StubDocker(primary=(code, b"", b"", False))
                tx, _ = self.make(runner)
                r = tx.exec("x", "warm", "true")
                self.assertEqual((r.code, r.timed_out), (code, False))
                self.assertNotIn("timeout_cleanup", r.extra)
                self.assertFalse(any("rwb-timeout" in c for c in runner.calls))

    def test_generated_argv_registration_failure_stops_body(self):
        # Run the real generated wrapper argv locally (setsid is Linux-only; drop the docker prefix).
        tmp = scratch(self)
        marker = tmp / "ran"
        local = Runner()

        def runner(argv, timeout, env=None, cwd=None):
            if argv[1] == "exec":
                return local(argv[argv.index("bash"):], timeout)
            return StubDocker()(argv, timeout)

        locked = tmp / "pid root"
        locked.mkdir()
        tx, _ = self.make(runner, root=locked)
        ok = tx.exec("ok", "warm", "printf BODY_RAN")
        self.assertEqual((ok.code, ok.stdout), (0, "BODY_RAN"))
        self.assertTrue((locked / f"rwb-pid-rwb-toy-run1-{ok.seq}" / "pid").is_file())
        # Pre-existing step directory (or, unprivileged, an unwritable parent): fail closed.
        (locked / f"rwb-pid-rwb-toy-run1-{ok.seq + 1}").mkdir()
        bad = tx.exec("bad", "warm", f"touch {json.dumps(str(marker))}; printf BODY_RAN")
        self.assertEqual((bad.code, bad.stdout), (126, ""))
        self.assertIn(ERROR, bad.stderr)
        self.assertFalse(marker.exists())
        if not IS_ROOT:
            locked.chmod(0o500)
            bad = tx.exec("locked", "warm", f"touch {json.dumps(str(marker))}; printf BODY_RAN")
            self.assertEqual((bad.code, bad.stdout), (126, ""))
            self.assertFalse(marker.exists())


class HostRegistrationTest(unittest.TestCase):
    def make(self):
        tmp = scratch(self)
        work = tmp / "work dir"
        work.mkdir()
        rec = Recorder(tmp / "r")
        self.addCleanup(rec.close)
        return HostTransport(rec, work, bash=BASH), work

    def test_host_uses_run_owned_step_path_without_linux_commands(self):
        tx, work = self.make()
        r = tx.exec("x", "warm", 'printf "BODY_RAN %s" "$$"')
        self.assertEqual(r.code, 0, r.stderr)
        argv = json.loads((tx.recorder.out / "steps.jsonl").read_text().splitlines()[-1])["argv"]
        self.assertEqual(argv, [BASH, "-c", WRAPPER, "rwb", 'printf "BODY_RAN %s" "$$"',
                                str(work / "pids" / str(r.seq))])
        self.assertNotIn("setsid", argv)
        self.assertEqual((work / "pids" / str(r.seq) / "pid").read_text(), r.stdout.split()[1] + "\n")
        self.assertEqual(stat.S_IMODE((work / "pids" / str(r.seq)).stat().st_mode), 0o700)
        r2 = tx.exec("y", "warm", "exit 13")
        self.assertEqual(r2.code, 13)

    def test_host_registration_failure_stops_before_body(self):
        tx, work = self.make()
        marker = work / "ran"
        (work / "pids").mkdir()
        (work / "pids" / "1").mkdir()
        r = tx.exec("x", "warm", f"touch {json.dumps(str(marker))}")
        self.assertEqual(r.seq, 1)
        self.assertEqual(r.code, 126)
        self.assertIn(ERROR, r.stderr)
        self.assertFalse(marker.exists())

    def test_host_refuses_symlinked_pid_parent(self):
        tx, work = self.make()
        (work / "elsewhere").mkdir()
        (work / "pids").symlink_to(work / "elsewhere")
        with self.assertRaises(RuntimeError):
            tx.exec("x", "warm", "true")


if __name__ == "__main__":
    unittest.main()

"""Offline checks that cleanup signals only the exact processes a trial owns."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest import mock

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE))
import arms  # noqa: E402
import run as bench  # noqa: E402

ORIGINAL = {'pid': 12345, 'started': 'Mon Oct 19 10:00:00 2026', 'command': '/tmp/at.x/app/node server.mjs'}
REUSED = {'started': 'Mon Oct 19 10:00:07 2026', 'command': 'unrelated --daemon'}


class SignalTest(unittest.TestCase):
    """PID reuse is simulated; no real process is signalled here."""

    def run_reclaim(self, identities, owned=True):
        sent = []
        with mock.patch.object(arms, 'process_identity', side_effect=identities), \
                mock.patch.object(arms, '_pidfd_open', return_value=None), \
                mock.patch.object(arms, 'owns', return_value=owned) as owns, \
                mock.patch.object(arms, 'RECLAIM_GRACE_SECONDS', 0), \
                mock.patch.object(arms.os, 'kill', side_effect=lambda pid, signum: sent.append((pid, signum))):
            result = arms.reclaim([ORIGINAL], Path('/tmp/at.x'))
        return sent, result, owns

    def test_reused_pid_gets_no_escalation(self):
        # Same process at SIGTERM; by the SIGKILL check the PID belongs to another program.
        sent, result, owns = self.run_reclaim([dict(ORIGINAL), dict(ORIGINAL, **REUSED)])
        self.assertEqual(sent, [(12345, signal.SIGTERM)])
        self.assertEqual(result, {'terminated': [12345], 'killed': [], 'not_signalled': []})

    def test_identity_is_rechecked_before_every_signal(self):
        sent, result, owns = self.run_reclaim([dict(ORIGINAL)] * 3)
        self.assertEqual(sent, [(12345, signal.SIGTERM), (12345, signal.SIGKILL)])
        self.assertEqual(owns.call_count, 2)  # ownership asked again right before each signal

    def test_reused_before_first_signal_gets_nothing(self):
        sent, result, _ = self.run_reclaim([dict(ORIGINAL, **REUSED)])
        self.assertEqual(sent, [])
        self.assertEqual(result['not_signalled'], [12345])

    def test_same_identity_no_longer_owned_gets_nothing(self):
        sent, result, _ = self.run_reclaim([dict(ORIGINAL)], owned=False)
        self.assertEqual(sent, [])

    def test_linux_start_ticks_distinguish_reuse_within_one_second(self):
        recorded = dict(ORIGINAL, start_ticks=100)
        with mock.patch.object(arms, 'process_identity', return_value=dict(ORIGINAL, start_ticks=101)):
            self.assertFalse(arms.same_process(recorded))
        with mock.patch.object(arms, 'process_identity', return_value=dict(ORIGINAL, start_ticks=100)):
            self.assertTrue(arms.same_process(recorded))


@unittest.skipIf(sys.platform == 'darwin' and not shutil.which('lsof'), 'lsof unavailable')
class ReclaimProcessTest(unittest.TestCase):
    """Real processes: an owned one that ignores SIGTERM, and an unrelated sentinel."""

    def test_owned_process_is_stopped_and_sentinel_survives(self):
        root = Path(tempfile.mkdtemp(prefix='aut.', dir='/tmp')).resolve()
        elsewhere = Path(tempfile.mkdtemp(prefix='aus.', dir='/tmp')).resolve()
        stubborn = subprocess.Popen([sys.executable, '-c', 'import signal,time\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                                     'time.sleep(60)'], cwd=root, start_new_session=True)
        sentinel = subprocess.Popen(['sleep', '60'], cwd=elsewhere, start_new_session=True)
        try:
            time.sleep(0.5)
            owned = arms.owned_processes(root)
            self.assertEqual([process['pid'] for process in owned], [stubborn.pid])
            stale = dict(owned[0], pid=sentinel.pid)  # a record whose PID now names someone else
            with mock.patch.object(arms, 'RECLAIM_GRACE_SECONDS', 1):
                result = arms.reclaim(owned + [stale], root)
            stubborn.wait(timeout=5)
            self.assertEqual(result['terminated'], [stubborn.pid])
            self.assertEqual(result['killed'], [stubborn.pid])  # it ignored SIGTERM
            self.assertEqual(result['not_signalled'], [sentinel.pid])
            self.assertIsNone(sentinel.poll())
        finally:
            for process in [stubborn, sentinel]:
                if process.poll() is None:
                    process.kill()
                    process.wait()
            shutil.rmtree(root)
            shutil.rmtree(elsewhere)


# The leader exits on SIGTERM; its child ignores SIGTERM and, optionally, leaves the group.
CHILD = """import os, pathlib, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
if sys.argv[1] == 'own-group':
    os.setpgrp()
pathlib.Path('child.pid').write_text(str(os.getpid()))
time.sleep(60)
"""
LEADER = """import subprocess, sys, time
subprocess.Popen([sys.executable, 'child.py', sys.argv[1]])
time.sleep(60)
"""


class StopTreeTest(unittest.TestCase):
    """Timeout and external cancellation, each with an unrelated sentinel that must survive."""

    def setUp(self):
        self.directory = Path(tempfile.mkdtemp(prefix='aui.', dir='/tmp')).resolve()
        (self.directory / 'child.py').write_text(CHILD)
        (self.directory / 'leader.py').write_text(LEADER)
        self.sentinel = subprocess.Popen(['sleep', '60'], start_new_session=True)
        grace = mock.patch.object(bench, 'STOP_GRACE_SECONDS', 1)
        grace.start()
        self.addCleanup(grace.stop)

    def tearDown(self):
        pidfile = self.directory / 'child.pid'
        if pidfile.exists() and arms.alive(int(pidfile.read_text())):
            os.kill(int(pidfile.read_text()), signal.SIGKILL)
        self.sentinel.kill()
        self.sentinel.wait()
        shutil.rmtree(self.directory)

    def child(self):
        return int((self.directory / 'child.pid').read_text())

    def run_leader(self, mode, timeout):
        return bench.invoke([sys.executable, 'leader.py', mode], self.directory, os.environ, timeout, self.directory / 'step')

    def assert_stopped(self, record):
        child = self.child()
        self.assertFalse(arms.alive(child), 'the child that ignored SIGTERM outlived the step')
        self.assertIn(child, record['stopped']['killed'])
        self.assertEqual(record['stopped']['survivors'], [])
        self.assertIsNone(self.sentinel.poll(), 'an unrelated process was signalled')

    def test_timeout_stops_a_child_that_outlives_its_leader(self):
        record = self.run_leader('same-group', 1.5)
        self.assertTrue(record['timed_out'])
        self.assert_stopped(record)

    def test_timeout_finds_a_child_that_left_the_group(self):
        record = self.run_leader('own-group', 1.5)
        self.assertTrue(record['timed_out'])
        self.assert_stopped(record)

    def test_external_cancellation_stops_the_tree(self):
        previous = signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(bench.Cancelled('test')))
        try:
            threading.Timer(1.5, os.kill, [os.getpid(), signal.SIGTERM]).start()
            with self.assertRaises(bench.Cancelled):
                self.run_leader('same-group', 30)
        finally:
            signal.signal(signal.SIGTERM, previous)
        record = json.loads((self.directory / 'step.json').read_text())
        self.assertEqual(record['cancelled'], 'Cancelled')
        self.assert_stopped(record)

    def test_a_reaped_leaders_reused_group_is_left_alone(self):
        # A finished command whose PID now leads someone else's group: nothing is signalled.
        finished = SimpleNamespace(pid=self.sentinel.pid, returncode=0, poll=lambda: 0, wait=lambda timeout=None: 0)
        self.assertEqual(bench.command_tree(finished, {}), {})
        log = bench.stop_tree(finished)
        self.assertEqual(log, {'terminated': [], 'killed': [], 'survivors': []})
        self.assertIsNone(self.sentinel.poll())


if __name__ == '__main__':
    unittest.main()

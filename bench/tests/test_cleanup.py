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

    def test_empty_root_scan_cannot_erase_down_or_supervisor_uncertainty(self):
        empty = {'pids': [], 'ports': [], 'owned_processes': []}
        for role in ['down', 'supervisor-stop']:
            with self.subTest(role=role), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / 'context.json').write_text('{"task":"slug-unicode"}')
                (root / 'pf/state').mkdir(parents=True)
                (root / 'pf/state/fixture').write_text('')

                def command(argv, cwd, env, timeout, path):
                    record = {'exit_code': 0, 'receipt': path.name}
                    (root / (path.name + '.stdout')).write_text('{"data":{"confirmed":true}}')
                    if path.name.endswith(role):
                        record['stopped'] = {'survivors': [], 'unresolved': [{'pid': 42}]}
                    return record

                with mock.patch.object(bench, 'invoke', side_effect=command), \
                        mock.patch.object(arms, 'residue', return_value=empty), \
                        mock.patch.object(arms, 'disk_usage', return_value={}):
                    result = bench.teardown('stack', root, root, {}, root / 'stack', root, 'cleanup')
                self.assertFalse(result['ok'])
                self.assertFalse(bench.clean(result))
                self.assertFalse(bench.restored(result))
                self.assertIn('cleanup-' + role, result['command_cleanup'])

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

    def test_pidfd_checks_identity_and_ownership_without_numeric_pid_signals(self):
        recorded = dict(ORIGINAL, start_ticks=100)
        for identity, owned, expected in [(recorded, True, True), (dict(recorded, start_ticks=101), True, False),
                                          (recorded, False, False)]:
            with self.subTest(identity=identity, owned=owned), \
                    mock.patch.object(arms, '_pidfd_open', return_value=57), \
                    mock.patch.object(arms, 'process_identity', return_value=identity), \
                    mock.patch.object(arms.signal, 'pidfd_send_signal', create=True) as send, \
                    mock.patch.object(arms.os, 'close') as close, mock.patch.object(arms.os, 'kill') as kill:
                self.assertEqual(arms.signal_if_same(recorded, signal.SIGKILL, lambda _: owned), expected)
                self.assertEqual(send.call_count, int(expected))
                close.assert_called_once_with(57)
                kill.assert_not_called()

    def test_tree_does_not_adopt_a_reused_child_pid(self):
        leader = SimpleNamespace(pid=1, returncode=None)
        old = dict(ORIGINAL, pgid=1, ppid=1, start_ticks=100)
        replacement = dict(old, start_ticks=101)
        with mock.patch.object(arms, 'process_table', return_value={old['pid']: replacement}), \
                mock.patch.object(arms, 'same_process', return_value=False):
            self.assertEqual(bench.command_tree(leader, {old['pid']: old}), {})


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
# 'exec-on-term' replaces itself with /bin/sleep when sent SIGTERM (review round 2, B);
# 'escape-on-term' also leaves the session first, after its leader has gone, so the tree
# can no longer vouch for it.
CHILD = """import os, pathlib, signal, sys, time
def replace(*_):
    if sys.argv[1] == 'escape-on-term':
        time.sleep(0.5)
        os.setsid()
    os.execv('/bin/sleep', ['sleep', '30'])
signal.signal(signal.SIGTERM, replace if sys.argv[1].endswith('-on-term') else signal.SIG_IGN)
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

    def test_cancellation_before_launch_records_no_child_and_reraises(self):
        for index, error in enumerate([bench.Cancelled('test'), KeyboardInterrupt()]):
            with self.subTest(error=type(error).__name__), \
                    mock.patch.object(bench.subprocess, 'Popen', side_effect=error), \
                    mock.patch.object(bench, 'stop_tree') as stop:
                with self.assertRaises(type(error)):
                    self.run_leader('same-group', 30)
                name = 'step.json' if index == 0 else 'step-attempt-2.json'
                record = json.loads((self.directory / name).read_text())
                self.assertEqual(record['cancelled'], type(error).__name__)
                self.assertFalse(record['process_started'])
                self.assertNotIn('stopped', record)
                self.assertIsNone(record['exit_code'])
                self.assertTrue(bench.command_schema(record))
                stop.assert_not_called()

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
        self.assertTrue(record['process_started'])
        self.assertTrue(bench.command_schema(record))
        self.assert_stopped(record)

    def test_timeout_follows_a_child_that_execs_another_program(self):
        record = self.run_leader('exec-on-term', 1.5)
        self.assertTrue(record['timed_out'])
        self.assert_stopped(record)
        transition = [entry for entry in record['stopped']['exec_transitions'] if entry['pid'] == self.child()]
        self.assertEqual(len(transition), 1)
        self.assertIn('sleep 30', transition[0]['to'])
        self.assertEqual(record['stopped']['unresolved'], [])

    def test_cancellation_follows_a_child_that_execs_another_program(self):
        previous = signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(bench.Cancelled('test')))
        try:
            threading.Timer(1.5, os.kill, [os.getpid(), signal.SIGTERM]).start()
            with self.assertRaises(bench.Cancelled):
                self.run_leader('exec-on-term', 30)
        finally:
            signal.signal(signal.SIGTERM, previous)
        record = json.loads((self.directory / 'step.json').read_text())
        self.assertEqual(record['cancelled'], 'Cancelled')
        self.assertTrue(record['process_started'])
        self.assertTrue(bench.command_schema(record))
        self.assert_stopped(record)
        self.assertTrue(any(entry['pid'] == self.child() for entry in record['stopped']['exec_transitions']))

    def test_an_exec_the_tree_cannot_vouch_for_is_unresolved_not_signalled(self):
        # The child leaves the session and execs after its leader exits: the PID keeps its start
        # time but nothing ties it to the command any more. It is reported, never signalled.
        record = self.run_leader('escape-on-term', 1.5)
        child = self.child()
        self.assertTrue(arms.alive(child), 'an unverifiable process was signalled')
        self.assertNotIn(child, record['stopped']['killed'])
        self.assertEqual([entry['pid'] for entry in record['stopped']['unresolved']], [child])
        self.assertIn('sleep 30', record['stopped']['unresolved'][0]['now']['command'])
        self.assertIsNone(self.sentinel.poll(), 'an unrelated process was signalled')

    def test_a_reaped_leaders_reused_group_is_left_alone(self):
        # A finished command whose PID now leads someone else's group: nothing is signalled.
        finished = SimpleNamespace(pid=self.sentinel.pid, returncode=0, poll=lambda: 0, wait=lambda timeout=None: 0)
        self.assertEqual(bench.command_tree(finished, {}), {})
        log = bench.stop_tree(finished)
        self.assertEqual(log, {'terminated': [], 'killed': [], 'survivors': [], 'unresolved': [], 'exec_transitions': []})
        self.assertIsNone(self.sentinel.poll())


if __name__ == '__main__':
    unittest.main()

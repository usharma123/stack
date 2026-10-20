"""Offline regression for DevPod's auto-resuming entry (bench/results/smoke-devpod-1).

That run showed `devpod stop` exit 0 and every A container exited (steps 45/46), then the
after-stop `devpod ssh` restarted the workspace and returned identity (step 47), so stop.a
read `fail`. The fake world below reproduces that tool behaviour: any `devpod ssh` into a
stopped workspace starts it again. No network, Docker or DevPod binary is used.
"""
from pathlib import Path
import sys
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(BENCH / "tests"))

from rwb.adapters.devpod import DevpodAdapter  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder  # noqa: E402
from test_container_adapters import ContainerTransport, ContainerWorld, make  # noqa: E402


class ResumingTransport(ContainerTransport):
    """`devpod ssh <workspace>` into a stopped workspace starts it, as observed at step 47."""

    def exec(self, label, phase, body, timeout=600, user=None):
        scenario = self.world.scenario
        for name in "abcde":
            if (f"devpod ssh {scenario.ad.project(name)} " in body and name not in self.world.running
                    and name in self.world.generation):
                self.world.running[name] = True
                self.world.generation[name] += 1
                self.resumed.append((label, name))
        return super().exec(label, phase, body, timeout, user)


def run_resuming(adapter, configure=None):
    rec = FakeRecorder()
    world = ContainerWorld(adapter)
    tx = ResumingTransport(rec, adapter, world)
    tx.resumed = []
    scenario = Scenario(adapter, tx, rec, repeats=1, warmups=0)
    world.scenario = scenario
    if configure:
        configure(world)
    scenario.execute()
    scenario.cleanup()
    out = {o["check"]: o for o in scenario.out.as_list()}
    seqs = {r.label: r.seq for r in rec.results}
    return out, tx, seqs


class NonResumingDevpod(DevpodAdapter):
    """The pre-fix declaration, kept only to prove the fake reproduces smoke-devpod-1."""
    entry_auto_resumes = False


class DevpodAutoResume(unittest.TestCase):
    def test_declaration_and_required_hook(self):
        self.assertTrue(DevpodAdapter.entry_auto_resumes)
        adapter = make(DevpodAdapter)
        self.assertIn("entry_auto_resumes", " ".join(adapter.core_hooks_required()))
        self.assertIn("entry_auto_resumes", " ".join(adapter.pins["validity"]["core_hooks_required"]))

    def test_old_declaration_reproduces_the_smoke_failure(self):
        out, tx, _ = run_resuming(make(NonResumingDevpod))
        self.assertEqual(out["stop.a"]["status"], "fail")
        self.assertIn("app after stop exit 0", out["stop.a"]["detail"])
        self.assertIn(("a-after-stop", "a"), tx.resumed)

    def test_stop_is_decided_without_an_after_stop_entry(self):
        out, tx, seqs = run_resuming(make(DevpodAdapter))
        labels = [c[0] for c in tx.calls]
        self.assertNotIn("a-after-stop", labels)
        self.assertNotIn("a", [name for _, name in tx.resumed])  # nothing undid the stop
        stop = out["stop.a"]
        self.assertEqual(stop["status"], "pass")
        self.assertEqual(stop["mode"], "native")
        self.assertIn("after-stop entry not run (entry auto-resumes the project)", stop["detail"])
        self.assertEqual(stop["evidence"], [seqs["a-stop"], seqs["a-stopped-probe"]])
        bodies = dict((c[0], c[2]) for c in tx.calls)
        wid = make(DevpodAdapter).project("a")
        self.assertIn(f"devpod stop {wid} ", bodies["a-stop"])
        self.assertIn(f"compose_receipt.py stopped {wid} ", bodies["a-stopped-probe"])
        self.assertNotIn("devpod ssh", bodies["a-stopped-probe"])  # probe inspects, never enters

    def test_explicit_restart_and_persistence_receipts_remain(self):
        out, tx, seqs = run_resuming(make(DevpodAdapter))
        labels = [c[0] for c in tx.calls]
        for label in ("a-persist", "a-stop", "a-stopped-probe", "b-after-a-stop", "a-restart",
                      "a-persisted", "a-cache-after-restart"):
            self.assertIn(label, labels)
        order = [labels.index(l) for l in ("a-persist", "a-stop", "a-stopped-probe", "a-restart", "a-persisted")]
        self.assertEqual(order, sorted(order))
        self.assertIn("devpod up ", dict((c[0], c[2]) for c in tx.calls)["a-restart"])
        for check in ("b.survives", "restart.a", "persist.pg", "persist.redis", "cache.after_restart"):
            self.assertEqual(out[check]["status"], "pass", check)
        self.assertEqual(out["persist.pg"]["evidence"], [seqs["a-persisted"]])

    def test_stop_and_probe_failures_still_fail_or_block(self):
        for label in ("a-stop", "a-stopped-probe"):
            with self.subTest(failing=label):
                out, _, _ = run_resuming(make(DevpodAdapter), lambda w, l=label: w.codes.update({l: (1, False)}))
                self.assertEqual(out["stop.a"]["status"], "fail")
        out, _, _ = run_resuming(make(DevpodAdapter), lambda w: w.codes.update({"a-stopped-probe": (124, True)}))
        self.assertEqual(out["stop.a"]["status"], "blocked")
        self.assertEqual(out["restart.a"]["status"], "blocked")


if __name__ == "__main__":
    unittest.main()

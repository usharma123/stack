"""Offline validity tests: no network, no Docker, no services.

python3 -m unittest discover -s bench/tests -v
"""
from pathlib import Path
import sys
import tempfile
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(BENCH / "fixtures" / "app"))

from rwb import verify  # noqa: E402
from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES, Adapter  # noqa: E402
from rwb.outcomes import Outcomes  # noqa: E402
from rwb.record import Recorder, split_inner  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.stats import nearest_rank, summarize_ns  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402
from rwb.transport import DockerTransport, OwnershipError  # noqa: E402


def run_fake(adapter, configure=None):
    rec = FakeRecorder()
    world = FakeWorld()
    tx = FakeTransport(rec, adapter, world)
    scenario = Scenario(adapter, tx, rec, repeats=3, warmups=1)
    world.scenario = scenario
    if configure:
        configure(world)
    scenario.execute()
    scenario.cleanup()
    return {o["check"]: o for o in scenario.out.as_list()}, tx


class Toy(Adapter):
    """Minimal fully-native adapter used to exercise the scenario."""
    name, title, image = "toy", "Toy", "ev-base"
    features = {k: "native" for k in FEATURES}
    lock_files = ("toy.lock",)

    def versions(self): return "toy --version"
    def setup(self, co): return f"toy setup {co.name}"
    def frozen_setup(self, co): return f"toy setup --locked {co.name}"
    def enter(self, co, body): return f"toy exec -- {body}"
    def start(self, co): return "toy up"
    def ready(self, co): return "toy wait"
    def stop(self, co): return "toy down"
    def status(self, co): return "toy status"
    def break_config(self, co): return "sed -i s/1/99/ toy.toml"


class StatsTest(unittest.TestCase):
    def test_nearest_rank(self):
        self.assertEqual(nearest_rank([5, 1, 3, 2, 4], 50), 3)
        self.assertEqual(nearest_rank(list(range(1, 101)), 95), 95)
        self.assertIsNone(nearest_rank([], 50))

    def test_failures_excluded_and_counted(self):
        s = summarize_ns([(True, 1_000_000), (False, 1), (True, 3_000_000)])
        self.assertEqual((s["n"], s["ok"], s["failed"]), (3, 2, 1))
        self.assertEqual(s["min_ms"], 1.0)


class RecordTest(unittest.TestCase):
    def test_inner_marker_is_stripped(self):
        err, ns, code = split_inner(b"warning\n\n@@RWB-INNER 1000 6000 3\n")
        self.assertEqual((err, ns, code), (b"warning\n", 5000, 3))
        self.assertEqual(split_inner(b"plain")[1], None)

    def test_refuses_existing_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(FileExistsError):
                Recorder(tmp)

    def test_raw_logs_and_jsonl(self):
        with tempfile.TemporaryDirectory() as tmp:
            rec = Recorder(Path(tmp) / "run")
            r = rec.write(rec.next_seq(), "x/y", "warm", ["a"], "docker-exec", 0, 10, b"out", b"e\n@@RWB-INNER 1 4 0\n", False, "t")
            rec.close()
            self.assertEqual((r.inner_ns, r.stderr), (3, "e"))
            self.assertIn('"phase": "warm"', (Path(tmp) / "run/steps.jsonl").read_text())
            self.assertEqual((Path(tmp) / "run/logs/0001-x_y.stdout").read_text(), "out")
            with self.assertRaises(ValueError):
                rec.write(rec.next_seq(), "x", "bogus", [], "h", 0, 0, b"", b"", False, "t")


def ident(sysid="s1", run="r1", port=5432, rport=6379, db=0, database="postgres", token="t", module="/w/a/rwbapp"):
    return dict(pg=dict(system_identifier=sysid, data_directory="/var/lib/postgresql/data", port=port,
                        started="x", database=database),
                redis=dict(run_id=run, port=rport, db=db, dir="/data"),
                urls=dict(database=f"postgresql://u@h:{port}/d", redis=f"redis://h:{rport}/{db}"),
                declared={}, source=dict(token=token, module=module))


class VerifyTest(unittest.TestCase):
    def test_wrong_code_detected(self):
        self.assertTrue(verify.identity_problems(ident(token="other"), "t", "/w/a"))
        self.assertTrue(verify.identity_problems(ident(module="/w/root/rwbapp"), "t", "/w/a"))
        self.assertFalse(verify.identity_problems(ident(), "t", "/w/a"))
        self.assertFalse(verify.identity_problems(ident(module="/app/rwbapp"), "t", None))

    def test_url_port_truth(self):
        bad = ident()
        bad["urls"]["database"] = "postgresql://u@h:9999/d"
        self.assertTrue(verify.identity_problems(bad))

    def test_container_boundary_allows_identical_paths(self):
        self.assertEqual(verify.distinct_instances(ident("s1", "r1"), ident("s2", "r2"), "container"), [])
        self.assertTrue(verify.distinct_instances(ident("s1", "r1"), ident("s2", "r2"), "container", ({"c": 1}, {"c": 1})))

    def test_database_boundary_allows_shared_servers(self):
        a = ident(database="db_a", db=0)
        b = ident(database="db_b", db=1)
        self.assertEqual(verify.distinct_instances(a, b, "database"), [])
        self.assertTrue(verify.distinct_instances(a, b, "service-instance"))
        self.assertTrue(verify.distinct_instances(a, ident(database="db_a", db=1), "database"))

    def test_restart_semantics(self):
        before, after = ident(run="r1"), ident(run="r2")
        after["pg"]["started"] = "y"
        self.assertEqual(verify.restart_changes(before, after), [])
        self.assertTrue(verify.restart_changes(before, before))
        self.assertEqual(verify.restart_changes(before, before, "database"), [])

    def test_conflict_classification(self):
        self.assertEqual(verify.classify_conflict((1, False), None, None, {1}), ("refused-at-start", True))
        self.assertEqual(verify.classify_conflict((0, False), None, None, {1}), ("reported-ready-but-unreachable", False))
        self.assertEqual(verify.classify_conflict((0, False), (0, False), ident(port=1), {1})[1], False)
        # Timeouts and missing commands are infrastructure faults, never a detected conflict.
        self.assertFalse(verify.classify_conflict((124, True), None, None, {1})[1])
        self.assertFalse(verify.classify_conflict((127, False), None, None, {1})[1])
        self.assertFalse(verify.classify_conflict((0, False), (127, False), None, {1})[1])
        self.assertFalse(verify.classify_conflict((0, False), (1, True), None, {1})[1])

    def test_app_result_fails_closed(self):
        ok = dict(command="crud", ok=True, result=dict(steps=["create", "read", "update", "list", "delete"]))
        self.assertTrue(verify.app_result(ok, "crud"))
        with self.assertRaises(ValueError):
            verify.app_result(ok, "crud", code=1)          # ok receipt but failed process
        with self.assertRaises(ValueError):
            verify.app_result(ok, "crud", timed_out=True)
        with self.assertRaises(ValueError):
            verify.app_result(dict(command="crud", ok=True, result={}), "crud")   # empty result
        with self.assertRaises(ValueError):
            verify.app_result(dict(command="crud", ok=True), "crud")              # absent result
        with self.assertRaises(ValueError):
            verify.app_result(dict(command="persisted", ok=True, result=dict(pg_keeper=True)), "persisted")

    def test_app_result_rejects_failures(self):
        with self.assertRaises(ValueError):
            verify.app_result(dict(command="crud", ok=False, error={}), "crud")
        with self.assertRaises(ValueError):
            verify.app_result(dict(command="read", ok=True, result={}), "crud")


class ScenarioTest(unittest.TestCase):
    def test_well_behaved_tool_passes_everything(self):
        out, _ = run_fake(Toy())
        failing = {k: v for k, v in out.items() if v["status"] not in ("pass", "observed")}
        self.assertEqual(failing, {})

    def test_wrong_instance_fails_isolation(self):
        out, _ = run_fake(Toy(), lambda w: w.share_with.update(b="a"))
        self.assertEqual(out["isolation"]["status"], "fail")

    def test_wrong_code_fails_start(self):
        out, _ = run_fake(Toy(), lambda w: w.source_of.update(b="a"))
        self.assertEqual(out["start.b"]["status"], "fail")
        self.assertIn("source token", out["start.b"]["detail"])

    def test_lock_drift_fails_frozen_copy(self):
        out, _ = run_fake(Toy(), lambda w: setattr(w, "lock_changes", True))
        self.assertEqual(out["lock.frozen_copy"]["status"], "fail")

    def test_shared_server_needs_database_boundary(self):
        shared = lambda w: setattr(w, "shared_server", True)
        out, _ = run_fake(Toy(), shared)
        self.assertEqual(out["isolation"]["status"], "fail")
        db = Toy()
        db.isolation_boundary = "database"
        out, _ = run_fake(db, shared)
        self.assertEqual(out["isolation"]["status"], "pass")

    def test_unsupported_is_not_failure(self):
        class NoFrozen(Toy):
            features = dict(Toy.features, frozen_setup="unsupported", wrong_instance_guard="unsupported")
            def frozen_setup(self, co): return None
        out, _ = run_fake(NoFrozen())
        self.assertEqual(out["lock.frozen_copy"]["status"], "unsupported")
        self.assertFalse(any(v["status"] == "fail" for v in out.values()))

    def test_tool_failure_is_result_not_error(self):
        out, _ = run_fake(Toy(), lambda w: w.fail.add("a-setup-cold"))
        self.assertEqual(out["setup.a"]["status"], "fail")
        self.assertNotIn("error", {v["status"] for v in out.values()})

    def test_semantic_read_failure_counts(self):
        out, _ = run_fake(Toy(), lambda w: w.fail.add("repeat.app_read-sample-002"))
        self.assertEqual(out["repeat.app_read"]["status"], "fail")

    def test_empty_crud_receipt_fails(self):
        out, _ = run_fake(Toy(), lambda w: w.bad_receipt.update(crud={}))
        self.assertEqual(out["crud_cache.a"]["status"], "fail")

    def test_ok_stdout_with_failed_exit_fails(self):
        out, _ = run_fake(Toy(), lambda w: w.codes.update({"a-crud": (1, False)}))
        self.assertEqual(out["crud_cache.a"]["status"], "fail")

    def test_redis_loss_detected_from_receipt_not_exit(self):
        out, _ = run_fake(Toy(), lambda w: w.bad_receipt.update(persisted=dict(pg_keeper=True, redis_durable=False)))
        self.assertEqual(out["persist.redis"]["status"], "fail")
        self.assertEqual(out["persist.pg"]["status"], "pass")

    def test_missing_lock_hashes_never_pass(self):
        for co in ("a", "c"):
            out, _ = run_fake(Toy(), lambda w: w.no_hash.add(co))
            self.assertNotEqual(out["lock.frozen_copy"]["status"], "pass", co)

    def test_timed_out_leftover_probe_is_error(self):
        out, _ = run_fake(Toy(), lambda w: w.codes.update({"leftover-processes": (124, True)}))
        self.assertEqual(out["cleanup.processes"]["status"], "error")

    def test_bad_config_infra_fault_is_blocked(self):
        out, _ = run_fake(Toy(), lambda w: w.codes.update({"d-setup-invalid": (127, False)}))
        self.assertEqual(out["bad_config"]["status"], "blocked")

    def test_occupied_port_timeouts_never_pass(self):
        for label, code in (("e-start", (124, True)), ("e-ready", (127, False)), ("e-ready", (1, True))):
            out, _ = run_fake(Toy(), lambda w: w.codes.update({label: code}))
            self.assertEqual(out["occupied_port"]["status"], "fail", (label, code))

    def test_stop_probe_timeout_is_blocked_not_pass(self):
        out, _ = run_fake(Toy(), lambda w: w.codes.update({"a-stopped-probe": (124, True)}))
        self.assertEqual(out["stop.a"]["status"], "blocked")
        self.assertEqual(out["persist.pg"]["status"], "blocked")

    # ---- Astra round-1 regressions (bench/reviews/round-1-core.md) ----------------
    @staticmethod
    def inject(fn):
        return lambda w: w.mutators.append(fn)

    def test_r1_1_good_stdout_with_failed_or_timed_out_process(self):
        def fault(r):
            if r.label in ("a-start-identity", "a-crud"):
                r.code, r.timed_out = 17, True
        out, _ = run_fake(Toy(), self.inject(fault))
        self.assertEqual(out["start.a"]["status"], "fail")

    def test_r1_1_repeated_start_failure_fails_even_if_identity_is_fine(self):
        def fault(r):
            if r.label == "a-start-again":
                r.code = 1
        out, _ = run_fake(Toy(), self.inject(fault))
        self.assertEqual(out["start.repeat"]["status"], "fail")

    def test_r1_2_identity_without_source_or_urls_fails(self):
        import json
        for field in ("source", "urls"):
            def fault(r, field=field):
                if r.label == "a-start-identity":
                    p = json.loads(r.stdout)
                    del p["result"][field]
                    r.stdout = json.dumps(p)
            out, _ = run_fake(Toy(), self.inject(fault))
            self.assertEqual(out["start.a"]["status"], "fail", field)

    def test_r1_4_occupied_port_wrong_source_never_relocated(self):
        import json
        def fault(r):
            if r.label == "e-identity":
                p = json.loads(r.stdout)
                p["result"]["source"]["token"] = "wrong-checkout"
                r.stdout = json.dumps(p)
        out, _ = run_fake(Toy(), self.inject(fault))
        self.assertEqual(out["occupied_port"]["status"], "fail")

    def test_r1_4_occupied_port_failed_deps_is_blocked(self):
        out, _ = run_fake(Toy(), lambda w: w.codes.update({"e-deps": (1, False)}))
        self.assertEqual(out["occupied_port"]["status"], "blocked")

    def test_r1_5_failed_hash_commands_never_pass(self):
        def fault(r):
            if r.label.endswith("-lock-hash"):
                r.code, r.stdout = 1, ""
        out, _ = run_fake(Toy(), self.inject(fault))
        self.assertEqual(out["lock.created"]["status"], "fail")
        self.assertEqual(out["lock.frozen_copy"]["status"], "blocked")

    def test_r1_7_no_listener_after_failed_setup_and_listener_always_released(self):
        out, tx = run_fake(Toy(), lambda w: w.codes.update({"e-setup": (1, False)}))
        self.assertEqual(out["occupied_port"]["status"], "blocked")
        self.assertNotIn("e-occupy-port", [c[0] for c in tx.calls])
        out, tx = run_fake(Toy(), lambda w: w.codes.update({"e-start": (124, True)}))
        labels = [c[0] for c in tx.calls]
        self.assertIn("e-occupy-port", labels)
        self.assertIn("release-listener", labels[labels.index("e-occupy-port"):])

    def test_r1_8_failed_preparation_blocks_dependents(self):
        for co, check in (("a", "setup.a"), ("b", "setup.b"), ("c", "lock.frozen_copy"),
                          ("d", "bad_config"), ("e", "occupied_port")):
            out, tx = run_fake(Toy(), lambda w, co=co: w.codes.update({f"{co}-prepare": (1, False)}))
            self.assertEqual(out[f"prepare.{co}"]["status"], "fail", co)
            self.assertEqual(out[check]["status"], "blocked", co)
            self.assertFalse(any(c[0] == f"{co}-setup-cold" or c[0] == f"{co}-setup-warm" for c in tx.calls), co)

    def test_r1_9_missing_container_receipt_fails_isolation(self):
        class Containers(Toy):
            isolation_boundary = "container"
            def instance_identity(self, co): return f"echo instance {co.name}"
        def fault(r):
            if r.label == "a-instance-identity":
                r.code, r.stdout = 1, ""
            elif r.label == "b-instance-identity":
                r.stdout = '{"containers": ["b-pg", "b-redis"]}'
        out, _ = run_fake(Containers(), self.inject(fault))
        self.assertEqual(out["isolation"]["status"], "fail")
        def good(r):
            if r.label.endswith("-instance-identity"):
                r.stdout = '{"containers": ["%s"]}' % r.label[0]
        out, _ = run_fake(Containers(), self.inject(good))
        self.assertEqual(out["isolation"]["status"], "pass")

    def test_outcome_validation(self):
        o = Outcomes()
        o.add("x", "pass")
        with self.assertRaises(ValueError):
            o.add("x", "pass")
        with self.assertRaises(ValueError):
            o.add("y", "great")


class FakeRunner:
    def __init__(self, inspect_labels):
        self.calls, self.inspect_labels = [], inspect_labels

    def __call__(self, argv, timeout, env=None, cwd=None):
        self.calls.append(argv)
        if argv[1] == "inspect":
            return 0, self.inspect_labels.encode(), b"", False, 1
        if argv[1:3] == ["image", "inspect"]:
            return 0, b"sha256:img\n", b"", False, 1
        if argv[1] == "exec":
            return 124, b"", b"", True, 1
        return 0, b"", b"", False, 1


class TransportSafetyTest(unittest.TestCase):
    def make(self, labels):
        tmp = tempfile.mkdtemp()
        rec = Recorder(Path(tmp) / "r")
        runner = FakeRunner(labels)
        self.addCleanup(rec.close)
        return DockerTransport(rec, "run1", "toy", "ev-base", [], runner=runner), runner

    def test_refuses_to_remove_unowned_container(self):
        tx, runner = self.make("someone-else other-run")
        tx.start()
        with self.assertRaises(OwnershipError):
            tx.destroy()
        self.assertFalse(any(c[1] == "rm" for c in runner.calls))

    def test_removes_only_its_own_container(self):
        tx, runner = self.make("stack-realworld-bench run1")
        tx.start()
        tx.destroy()
        removals = [c for c in runner.calls if c[1] == "rm"]
        self.assertEqual(removals, [["docker", "rm", "-f", "rwb-toy-run1"]])

    def test_timeout_kills_only_step_group_in_own_container(self):
        tx, runner = self.make("stack-realworld-bench run1")
        tx.start()
        r = tx.exec("slow", "warm", "sleep 999", timeout=1)
        self.assertTrue(r.timed_out)
        kill = runner.calls[-1]
        self.assertEqual(kill[:6], ["docker", "exec", "-u", "root", "rwb-toy-run1", "bash"])
        self.assertIn(f"/tmp/rwb-pids/{r.seq}", kill[-1])

    def test_no_global_docker_operations(self):
        tx, runner = self.make("stack-realworld-bench run1")
        tx.start()
        tx.exec("x", "warm", "true")
        tx.destroy()
        flat = [" ".join(c) for c in runner.calls]
        for forbidden in ("prune", "docker kill", "docker stop", "rm -f ev-", "compose down"):
            self.assertFalse(any(forbidden in c for c in flat), forbidden)
        # The only kill is the step-scoped one inside our own container (after a timeout).
        kills = [c for c in flat if "kill" in c]
        self.assertTrue(all(c.startswith("docker exec -u root rwb-toy-run1 ") for c in kills))


class FixtureTest(unittest.TestCase):
    def test_migrations_ordered_without_gaps(self):
        from rwbapp import core
        self.assertEqual([v for v, _ in core.migration_files()], ["0001", "0002"])
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "0001_a.sql").write_text("")
            (Path(tmp) / "0003_c.sql").write_text("")
            with self.assertRaises(core.ConfigError):
                core.migration_files(tmp)

    def test_cli_requires_checkout_name(self):
        from rwbapp import core
        with self.assertRaises(SystemExit):
            core.parse(["crud"])
        self.assertEqual(core.parse(["wait", "--timeout", "3"]).timeout, 3.0)

    def test_lock_pins_match_pyproject(self):
        text = (BENCH / "fixtures/app/uv.lock").read_text()
        for pin in ('name = "psycopg"\nversion = "3.3.6"', 'name = "redis"\nversion = "8.1.0"',
                    'name = "pytest"\nversion = "9.1.1"'):
            self.assertIn(pin, text)


class AdapterContractTest(unittest.TestCase):
    def test_every_available_adapter_completes_fake_scenario(self):
        for name, cls in registry.available().items():
            for variant in cls.variants:
                with self.subTest(adapter=name, variant=variant):
                    adapter = cls({}, variant, "contract")
                    if adapter.transport == "host":
                        adapter.root = "/tmp/rwb-contract"
                    self.assertEqual(set(adapter.features), set(FEATURES))
                    self.assertIn(adapter.isolation_boundary, verify.BOUNDARIES)
                    for rel in adapter.config_files:
                        self.assertTrue((adapter.config_dir() / rel).is_file(), rel)
                    out, tx = run_fake(adapter)
                    self.assertNotIn("error", {v["status"] for v in out.values()})
                    for label, _, body in tx.calls:
                        self.assertIsInstance(body, str, label)
                        for forbidden in ("docker system prune", "pkill", "killall", "--all-projects"):
                            self.assertNotIn(forbidden, body, label)


if __name__ == "__main__":
    unittest.main()

"""Offline validity tests: no network, no Docker, no services.

python3 -m unittest discover -s bench/tests -v
"""
from pathlib import Path
import json
import shutil
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

    def test_app_result_malformed_error_is_value_error(self):
        for error in ("broken", ["broken"], None, 7):
            with self.assertRaises(ValueError, msg=repr(error)):
                verify.app_result(dict(command="crud", ok=False, error=error), "crud", 1)
        with self.assertRaises(ValueError):  # ok but no error key at all
            verify.app_result(dict(command="crud", ok=False), "crud", 1)


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
        out, _ = run_fake(Toy(), lambda w: w.fail.add("a-setup"))
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

    def test_malformed_failure_receipt_fails_check_without_crashing(self):
        def fault(r):
            if r.label == "a-crud":
                r.code, r.stdout = 1, json.dumps(dict(command="crud", ok=False, error="broken"))
        out, _ = run_fake(Toy(), self.inject(fault))
        self.assertEqual(out["crud_cache.a"]["status"], "fail")
        self.assertIn("malformed error", out["crud_cache.a"]["detail"])

    def test_lock_hash_receipt_must_be_exact(self):
        good = "5" * 64
        cases = {
            "nonhex": lambda p: f"not-a-hash  {p}/toy.lock\n",
            "short": lambda p: f"{'5' * 63}  {p}/toy.lock\n",
            "upper": lambda p: f"{'A' * 64}  {p}/toy.lock\n",
            "wrong file": lambda p: f"{good}  {p}/wrong.lock\n",
            "other root": lambda p: f"{good}  /elsewhere/toy.lock\n",
            "duplicate": lambda p: f"{good}  {p}/toy.lock\n{good}  {p}/toy.lock\n",
            "extra": lambda p: f"{good}  {p}/toy.lock\n{good}  {p}/wrong.lock\n",
            "trailing junk": lambda p: f"{good}  {p}/toy.lock\nsha256sum: warning\n",
            "missing": lambda p: "",
        }
        for name, make in cases.items():
            def configure(w, make=make):
                for co in ("a", "c"):
                    w.outputs[f"{co}-lock-hash"] = (make(w.checkout_path(co)), "")
            out, _ = run_fake(Toy(), configure)
            self.assertEqual(out["lock.created"]["status"], "fail", name)
            self.assertEqual(out["lock.frozen_copy"]["status"], "blocked", name)
        # Positive: exact declared set, 64-hex digests, binary-mode marker accepted.
        def exact(w):
            for co in ("a", "c"):
                w.outputs[f"{co}-lock-hash"] = (f"{good} *{w.checkout_path(co)}/toy.lock\n\n", "")
        out, _ = run_fake(Toy(), exact)
        self.assertEqual(out["lock.created"]["status"], "pass")
        self.assertEqual(out["lock.frozen_copy"]["status"], "pass")

    def test_parse_lock_hashes_reports_missing_as_absent(self):
        from rwb.scenario import parse_lock_hashes
        good = "a" * 64
        hashes, problems = parse_lock_hashes(f"{good}  /w/a/x.lock\n", "/w/a", ("x.lock", "y.lock"))
        self.assertEqual((hashes, problems), ({"x.lock": good}, []))
        hashes, problems = parse_lock_hashes(f"{good}  /w/a/x.lock\n{good}  /w/a/x.lock\n", "/w/a", ("x.lock",))
        self.assertTrue(problems)

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
            self.assertFalse(any(c[0] == f"{co}-setup" for c in tx.calls), co)

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


class ParentFindingsTest(unittest.TestCase):
    """Parent findings after Astra round 1 (IMPLEMENTATION.md, "Parent findings P1-P4")."""

    def test_p1_nat_port_requires_valid_mapping_receipt(self):
        nat = ident(port=5432, rport=6379)
        nat["urls"] = dict(database="postgresql://u@127.0.0.1:55001/d", redis="redis://127.0.0.1:56001/0")
        self.assertTrue(verify.identity_problems(nat))                      # no receipt: rejected
        good = dict(pg=dict(published=55001, target=5432), redis=dict(published=56001, target=6379),
                    evidence="container 1234abcd")
        self.assertEqual(verify.identity_problems(nat, port_map=good), [])
        wrong = dict(good, pg=dict(published=55002, target=5432))         # maps a different port
        self.assertTrue(verify.identity_problems(nat, port_map=wrong))
        for broken in (None, "exit 1", {}, dict(good, evidence=""), dict(good, redis=dict(published="56001", target=6379))):
            self.assertTrue(verify.identity_problems(nat, port_map=broken if broken is not None else {}), broken)

    def test_p1_scenario_runs_port_map_and_fails_closed(self):
        class Nat(Toy):
            def port_map(self, co): return f"inspect {co.name}"
        def fault(r):
            if r.label == "a-port-map":
                r.code, r.stdout = 1, ""
        out, tx = run_fake(Nat(), ScenarioTest.inject(fault))
        self.assertIn("a-port-map", [c[0] for c in tx.calls])
        self.assertEqual(out["start.a"]["status"], "fail")

    def test_p1_declared_port_map_without_receipt_fails_even_if_ports_match(self):
        class Nat(Toy):
            def port_map(self, co): return f"inspect {co.name}"
        out, _ = run_fake(Nat())
        self.assertEqual(out["start.a"]["status"], "pass")          # fake prints a valid receipt
        def silent(r):
            if r.label == "a-port-map":
                r.stdout = ""
        out, _ = run_fake(Nat(), ScenarioTest.inject(silent))
        self.assertEqual(out["start.a"]["status"], "fail")
        self.assertIn("port map", out["start.a"]["detail"])

    def test_p1_nat_lane_with_real_mapping_passes_full_scenario(self):
        """URL port != server port, proven by a valid Docker mapping: every checkpoint passes."""
        import json as _json
        class Nat(Toy):
            def port_map(self, co): return f"docker inspect {co.name}"
        def nat(r):
            if r.label.endswith("-identity") or r.label.endswith("-check") or "after-a-stop" in r.label:
                try:
                    p = _json.loads(r.stdout)
                except ValueError:
                    return
                res = p.get("result") or {}
                if "urls" in res:
                    res["urls"]["database"] = res["urls"]["database"].replace(f":{res['pg']['port']}/", f":{res['pg']['port'] + 10000}/")
                    res["urls"]["redis"] = res["urls"]["redis"].replace(f":{res['redis']['port']}/", f":{res['redis']['port'] + 10000}/")
                    r.stdout = _json.dumps(p)
            if r.label.endswith("-port-map") and r.stdout:
                m = _json.loads(r.stdout)
                m["pg"]["published"] += 10000
                m["redis"]["published"] += 10000
                r.stdout = _json.dumps(m)
        out, _ = run_fake(Nat(), ScenarioTest.inject(nat))
        self.assertEqual(out["start.a"]["status"], "pass", out["start.a"]["detail"])
        self.assertEqual(out["isolation"]["status"], "pass")
        def wrong_target(r):
            nat(r)
            if r.label == "a-port-map":
                m = _json.loads(r.stdout)
                m["pg"]["target"] += 1
                r.stdout = _json.dumps(m)
        out, _ = run_fake(Nat(), ScenarioTest.inject(wrong_target))
        self.assertEqual(out["start.a"]["status"], "fail")

    def test_p2_listener_pidfile_is_run_unique_and_released_by_owner_check(self):
        out, tx = run_fake(Toy())
        occupy = next(b for label, _, b in tx.calls if label == "e-occupy-port")
        release = next(b for label, _, b in tx.calls if label == "release-listener")
        self.assertNotIn("/tmp/rwb-squat-45999.pid", occupy)
        self.assertIn("rwb-squat-dryrun-e", occupy)
        self.assertIn("ps -o args=", release)

    def test_p2_legacy_occupy_override_still_owner_checked_by_port(self):
        class Legacy(Toy):
            def occupy(self, port, pidfile): return f"listen {port} {pidfile}"
        out, tx = run_fake(Legacy())
        release = next(b for label, _, b in tx.calls if label == "release-listener")
        self.assertIn(str(Toy().checkout("e", 4).pg_port), release)
        self.assertNotIn("error", {v["status"] for v in out.values()})

    @unittest.skipUnless(shutil.which("bash") and shutil.which("ps"), "needs bash and ps")
    def test_p2_release_never_signals_a_foreign_pid(self):
        import subprocess
        foreign = subprocess.Popen(["bash", "-c", "exec -a someone-else sleep 30"], start_new_session=True)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                pidfile = f"{tmp}/p.pid"
                Path(pidfile).write_text(str(foreign.pid))
                body = Toy().release(pidfile, "rwb-squat-run1-e")
                r = subprocess.run(["bash", "-c", body], capture_output=True, text=True, timeout=30)
                self.assertEqual(r.returncode, 0)
                self.assertIn("not our listener", r.stdout)
                self.assertIsNone(foreign.poll())              # still alive
        finally:
            foreign.kill()
            foreign.wait()

    @unittest.skipUnless(shutil.which("shasum") or shutil.which("sha256sum"), "needs a sha256 tool")
    def test_p3_hash_helpers_work_without_sha256sum(self):
        import hashlib
        import subprocess
        from rwb.adapters.base import SHA256_FN, sha256_check
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "lock"
            target.write_bytes(b"pinned\n")
            digest = hashlib.sha256(b"pinned\n").hexdigest()
            bindir = Path(tmp) / "bin"
            bindir.mkdir()
            for tool in ("shasum", "cut", "perl"):   # macOS-like PATH: no sha256sum
                if shutil.which(tool):
                    (bindir / tool).symlink_to(shutil.which(tool))
            if not (bindir / "shasum").exists():
                self.skipTest("no shasum on this host")
            env = {"PATH": str(bindir)}
            bash = shutil.which("bash")
            r = subprocess.run([bash, "-c", f"{SHA256_FN}; rwb_sha256 {target}"], env=env, capture_output=True, text=True)
            self.assertEqual(r.stdout.split()[0], digest, r.stderr)
            ok = subprocess.run([bash, "-c", sha256_check(digest, str(target))], env=env, capture_output=True)
            bad = subprocess.run([bash, "-c", sha256_check("0" * 64, str(target))], env=env, capture_output=True)
            self.assertEqual((ok.returncode, bad.returncode != 0), (0, True))

    def test_p4_bad_config_unrelated_failure_is_not_a_pass(self):
        out, _ = run_fake(Toy(), lambda w: w.outputs.update({"d-setup-invalid": ("", "network unreachable\n")}))
        self.assertEqual(out["bad_config"]["status"], "blocked")
        out, _ = run_fake(Toy())
        self.assertEqual(out["bad_config"]["status"], "pass")

    def test_p4_occupied_port_needs_conflict_diagnostic(self):
        unrelated = lambda w: (w.codes.update({"e-start": (1, False)}),
                               w.outputs.update({"e-start": ("", "cannot resolve postgres@99.99.99\n")}))
        out, _ = run_fake(Toy(), unrelated)
        self.assertEqual(out["occupied_port"]["status"], "fail")
        self.assertIn("without-conflict-diagnostic", out["occupied_port"]["detail"])
        port = Toy().checkout("e", 4).pg_port
        named = lambda w: (w.codes.update({"e-start": (1, False)}),
                           w.outputs.update({"e-start": ("", f"port {port} is already in use\n")}))
        out, _ = run_fake(Toy(), named)
        self.assertEqual(out["occupied_port"]["status"], "pass")
        silent_ready = lambda w: (w.codes.update({"e-ready": (1, False)}), w.outputs.update({"e-ready": ("", "")}))
        out, _ = run_fake(Toy(), silent_ready)
        self.assertEqual(out["occupied_port"]["status"], "fail")

    def test_p4_port_listed_in_an_unrelated_failure_is_not_a_conflict_diagnostic(self):
        # smoke-stack-5 seq 66: `stack up` failed at stop_previous; its receipt lists the planned
        # port in compile details. That must not read as detecting the occupied port.
        receipt = ('{"ok":false,"error":{"code":"stop_failed","message":"mise daemons stop failed","details":'
                   '[{"output":"mise ERROR no matching project daemons"},{"steps":[{"step":"compile","status":"ok",'
                   '"detail":{"ports":{"postgres":43560,"redis":48122}}}]}]}}')
        self.assertFalse(verify.relevant(receipt, verify.conflict_pattern(43560)))
        self.assertTrue(verify.relevant("could not bind IPv4 address: Address already in use",
                                        verify.conflict_pattern(43560)))

    def test_p4_scripted_readiness_timeout_needs_conflict_evidence(self):
        class Scripted(Toy):
            def ready(self, co): return None
            def conflict_logs(self, co): return "tool logs"
        port = Scripted().checkout("e", 4).pg_port
        def unreachable(w):
            w.share_with["e"] = "zz"  # e's app reaches nothing
        out, _ = run_fake(Scripted(), unreachable)
        self.assertEqual(out["occupied_port"]["status"], "fail")
        def logged(w):
            unreachable(w)
            w.outputs["e-conflict-logs"] = (f"FATAL: could not bind IPv4 address 127.0.0.1:{port}: Address already in use", "")
        out, _ = run_fake(Scripted(), logged)
        self.assertEqual(out["occupied_port"]["status"], "pass")
        self.assertEqual(out["occupied_port"]["detail"], "detected-at-readiness")

    def test_first_task_timing_only_when_verified(self):
        rec, world = FakeRecorder(), FakeWorld()
        sc = Scenario(Toy(), FakeTransport(rec, Toy(), world), rec, repeats=1, warmups=0)
        world.scenario = sc
        sc.execute()
        task = sc.timings["first_task.a"]
        self.assertTrue(task["ok"])
        labels = {r.label for r in rec.results if r.seq in task["steps"]}
        self.assertTrue({"a-setup", "a-start", "a-deps", "a-migrate", "a-crud", "a-pytest"} <= labels)
        self.assertFalse(labels & {"a-lock-hash", "a-tool-versions", "a-migrate-again"})
        self.assertIn("setup.first_checkout", sc.timings)
        self.assertNotIn("setup.cold", sc.timings)
        rec, world = FakeRecorder(), FakeWorld()
        sc = Scenario(Toy(), FakeTransport(rec, Toy(), world), rec, repeats=1, warmups=0)
        world.scenario = sc
        world.fail.add("a-pytest")
        sc.execute()
        self.assertFalse(sc.timings["first_task.a"]["ok"])

    def test_frozen_copy_needs_nonempty_matching_version_lines(self):
        def empty(r):
            if r.label == "c-tool-versions":
                r.stdout = ""
        out, _ = run_fake(Toy(), ScenarioTest.inject(empty))
        self.assertEqual(out["lock.frozen_copy"]["status"], "fail")
        def other_path(r):
            if r.label == "c-tool-versions":
                r.stdout = "/nix/store/other-python3-env/bin/python3\nPython 3.13.16\n"
        out, _ = run_fake(Toy(), ScenarioTest.inject(other_path))
        self.assertEqual(out["lock.frozen_copy"]["status"], "pass")
        self.assertIn("paths differ", out["lock.frozen_copy"]["detail"])

    def test_c_cleaned_only_when_frozen_setup_starts_services(self):
        _, tx = run_fake(Toy())
        self.assertNotIn("c-cleanup", [c[0] for c in tx.calls])
        class Starts(Toy):
            def frozen_setup(self, co): return f"toy setup --locked {co.name} && {self.start(co)}"
        _, tx = run_fake(Starts())
        self.assertIn("c-cleanup", [c[0] for c in tx.calls])

    def test_first_task_starts_from_prepared_checkout_with_declared_scope(self):
        class PrepareCreates(Toy):
            prepare_scope = "native worktree creation"
        class SetupCreates(Toy):
            prepare_scope = "fixture repository only; worktree created inside setup"
        for cls in (PrepareCreates, SetupCreates):
            rec, world = FakeRecorder(), FakeWorld()
            sc = Scenario(cls(), FakeTransport(rec, cls(), world), rec, repeats=1, warmups=0)
            world.scenario = sc
            sc.execute()
            task = sc.timings["first_task.a"]
            labels = {r.label for r in rec.results if r.seq in task["steps"]}
            self.assertNotIn("a-prepare", labels)
            self.assertIn("a-setup", labels)
            self.assertEqual(task["starts_from"], "prepared checkout")
            self.assertEqual(task["excluded_prepare"], cls.prepare_scope)
            self.assertIsInstance(sc.timings["prepare.a"], int)

    def test_stop_keeps_data_endpoints_hook(self):
        class Shared(Toy):
            stop_keeps_data_endpoints = True
        def reachable(r):
            if r.label == "a-after-stop":
                r.code = 0
        out, _ = run_fake(Shared(), ScenarioTest.inject(reachable))
        self.assertEqual(out["stop.a"]["status"], "pass")
        out, _ = run_fake(Toy(), ScenarioTest.inject(reachable))
        self.assertEqual(out["stop.a"]["status"], "fail")

    def test_provision_exit_77_is_blocked_everywhere(self):
        from rwb.scenario import ProvisionBlocked
        class Gated(Toy):
            def provision(self): return [("provision-x", "probe", None)]
        rec, world = FakeRecorder(), FakeWorld()
        sc = Scenario(Gated(), FakeTransport(rec, Gated(), world), rec, repeats=1, warmups=0)
        world.scenario = sc
        world.codes["provision-x"] = (77, False)
        world.outputs["provision-x"] = ("", "RWB-BLOCKED: no verified digest\n")
        with self.assertRaises(ProvisionBlocked) as ctx:
            sc.execute()
        self.assertIn("no verified digest", str(ctx.exception))
        sc.block_all(str(ctx.exception))
        statuses = {o["check"]: o["status"] for o in sc.out.as_list()}
        self.assertEqual(statuses["provision"], "blocked")
        self.assertTrue(all(v == "blocked" for v in statuses.values()))
        self.assertNotIn("a-setup", [r.label for r in rec.results])


class ProcessProbeTest(unittest.TestCase):
    """The default leftover-process probe fails when `ps` fails; no matches is a clean pass."""

    @staticmethod
    def probe(ps_body, body=None):
        import subprocess
        body = body or Toy().service_processes()
        return subprocess.run(["bash", "-c", f"ps() {{ {ps_body}; }}; {body}"], capture_output=True, text=True)

    def test_failing_ps_fails_the_probe_and_cleanup(self):
        for body in (Toy().service_processes(), Toy().supervisor_processes()):
            r = self.probe('echo "ps unavailable" >&2; return 127', body)
            self.assertNotEqual(r.returncode, 0)
            self.assertEqual(r.stdout, "")
        r = self.probe('echo "ps unavailable" >&2; return 127')
        rec, world = FakeRecorder(), FakeWorld()
        sc = Scenario(Toy(), FakeTransport(rec, Toy(), world), rec, repeats=1, warmups=0)
        world.scenario = sc
        world.codes["leftover-processes"] = (r.returncode, False)
        sc.execute()
        problems = sc.cleanup()
        statuses = {o["check"]: o["status"] for o in sc.out.as_list()}
        self.assertEqual(statuses["cleanup.processes"], "error")
        self.assertIn("leftover process probe failed", problems)

    def test_no_matches_is_exit_zero_and_matches_are_filtered(self):
        r = self.probe('printf "  9 agent S bash\\n  8 root S postgres -D /x\\n  7 agent Z redis-server\\n"')
        self.assertEqual((r.returncode, r.stdout), (0, ""))
        out, _ = run_fake(Toy(), lambda w: w.outputs.update({"leftover-processes": (r.stdout, "")}))
        self.assertEqual(out["cleanup.processes"]["status"], "pass")
        r = self.probe('printf "  1 agent S postgres -D /x\\n  4 agent S redis-server *:6379\\n  5 agent S postgresql-x\\n"')
        self.assertEqual(r.returncode, 0)
        self.assertEqual([l.split()[0] for l in r.stdout.splitlines()], ["1", "4"])


class StackFrozenSetupTest(unittest.TestCase):
    def test_failed_partial_c_setup_is_still_cleaned_up(self):
        from rwb.adapters.stack import StackAdapter
        self.assertIs(StackAdapter.frozen_setup_starts_services, True)
        ad, rec, world = StackAdapter(), FakeRecorder(), FakeWorld()
        tx = FakeTransport(rec, ad, world)
        sc = Scenario(ad, tx, rec, repeats=1, warmups=0)
        world.scenario = sc
        sc.prepare("a")
        sc.bring_up("a", True)
        def partial(r):  # `up` started C's services, then the recipe failed
            if r.label == "c-frozen-setup":
                world.running["c"] = True
                world.generation["c"] = 1
                r.code = 1
        world.mutators.append(partial)
        sc.frozen_copy()
        self.assertIn("c", sc.started)
        sc.cleanup()
        self.assertIn("c-cleanup", [c[0] for c in tx.calls])
        self.assertNotIn("c", world.running)


class TeardownTest(unittest.TestCase):
    """Artifacts are copied before teardown; no copy/diagnostic failure can skip teardown."""

    class Logged(Toy):
        transport = "host"
        def artifacts(self, co): return ("svc.log",)
        def diagnostics(self, co): return [("logs", "toy logs")]
        def cleanup_host(self): return "toy host cleanup"

    def scenario(self, raises=False):
        import run
        adapter = self.Logged()
        adapter.root = "/tmp/rwb-teardown"
        rec, world = FakeRecorder(), FakeWorld()
        tx = FakeTransport(rec, adapter, world)
        sc = Scenario(adapter, tx, rec, repeats=1, warmups=0)
        world.scenario = sc
        sc.execute()
        world.files = {f"{sc.co[c].path}/svc.log" for c in "abcde"}
        world.copy_raises = raises
        meta = {}
        problems = run.teardown(sc, adapter, tx, Path(tempfile.mkdtemp()), meta)
        return tx, meta, problems

    def test_artifact_copied_before_its_checkout_is_cleaned(self):
        tx, meta, problems = self.scenario()
        labels = [c[0] for c in tx.calls]
        first_cleanup = next(i for i, l in enumerate(labels) if l.endswith("-cleanup"))
        copies = [i for i, l in enumerate(labels) if l == "artifact-copy"]
        self.assertTrue(copies and max(copies) < first_cleanup)
        self.assertLess(labels.index("a-diag-logs"), min(copies))
        self.assertTrue(any(a["checkout"] == "a" and a["ok"] for a in meta["artifacts"]))
        self.assertEqual(problems, [])

    def test_copy_failure_never_skips_teardown(self):
        tx, meta, problems = self.scenario(raises=True)
        labels = [c[0] for c in tx.calls]
        self.assertIn("a-cleanup", labels)
        self.assertIn("host-cleanup", labels)
        self.assertTrue(any("artifact copy raised" in e for e in meta["artifact_errors"]))


class HookBehaviourTest(unittest.TestCase):
    def test_entry_auto_resumes_never_calls_app_after_stop(self):
        class Resumes(Toy):
            entry_auto_resumes = True
        def guard(r):
            if r.label == "a-after-stop":
                raise AssertionError("after-stop entry would restart the project")
        out, tx = run_fake(Resumes(), ScenarioTest.inject(guard))
        self.assertEqual(out["stop.a"]["status"], "pass")
        self.assertNotIn("a-after-stop", [c[0] for c in tx.calls])
        for label, code in (("a-stop", (1, False)), ("a-stopped-probe", (1, False))):
            out, _ = run_fake(Resumes(), lambda w, l=label, c=code: w.codes.update({l: c}))
            self.assertEqual(out["stop.a"]["status"], "fail", label)
        out, _ = run_fake(Resumes(), lambda w: w.codes.update({"a-stopped-probe": (124, True)}))
        self.assertEqual(out["stop.a"]["status"], "blocked")

    def test_declared_frozen_start_tracks_c_even_without_start_substring(self):
        class Helper(Toy):
            frozen_setup_starts_services = True
            def frozen_setup(self, co): return f"helper-that-starts {co.name}"
        _, tx = run_fake(Helper())
        self.assertIn("c-cleanup", [c[0] for c in tx.calls])
        _, tx = run_fake(Helper(), lambda w: w.codes.update({"c-frozen-setup": (1, False)}))
        self.assertIn("c-cleanup", [c[0] for c in tx.calls])


class RosterTest(unittest.TestCase):
    FROZEN = ("stack", "mise", "flox", "devbox", "devenv", "nix", "pixi", "compose",
              "devcontainers", "devpod", "ddev", "lando", "process-compose", "services-flake", "pkgx",
              "dnvr", "guix", "workz", "worktrunk", "git-grove", "isola", "berth", "branchbox",
              "tilt", "organist", "vagrant")

    def test_all_26_frozen_adapters_import_and_run_the_contract(self):
        """SCOPE.md: Stack + 25. available() silently skips missing modules; this does not."""
        self.assertEqual(len(self.FROZEN), 26)
        self.assertEqual(set(registry.ADAPTERS), set(self.FROZEN))
        loaded = {name: registry.load(name)[0] for name in self.FROZEN}  # ImportError fails here
        self.assertEqual(set(registry.available()), set(self.FROZEN))
        for name, cls in loaded.items():
            for variant in cls.variants:
                adapter = cls({}, variant, "roster")
                if adapter.transport == "host":
                    adapter.root = "/tmp/rwb-roster"
                out, _ = run_fake(adapter)
                self.assertIn("cleanup.processes", out, name)


class SummaryTest(unittest.TestCase):
    def test_blocked_run_shows_no_timings(self):
        import run
        meta = dict(title="T", tool="t", variant="d", run_id="r", valid=True, completed=True, transport="docker",
                    isolation_boundary="service-instance", errors=[], blocked="provision-x: no digest",
                    measurement="blocked-prerequisite", timings={})
        text = run.render_summary(meta, [dict(check="provision", status="blocked", mode="n/a", detail="x")])
        self.assertIn("BLOCKED", text)
        self.assertNotIn("first_task", text)


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
        self.assertEqual(kill[-1], f"/tmp/rwb-pid-rwb-toy-run1-{r.seq}")

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
                    # A well-behaved fake tool must carry every lane through every checkpoint.
                    bad = {k: (v["status"], v["detail"][:120]) for k, v in out.items()
                           if v["status"] in ("fail", "blocked")}
                    self.assertEqual(bad, {})
                    for check in ("isolation", "repeat.entry", "stop.a", "lock.frozen_copy", "bad_config",
                                  "occupied_port", "cleanup.processes"):
                        self.assertIn(check, out)
                    for label, _, body in tx.calls:
                        self.assertIsInstance(body, str, label)
                        for forbidden in ("docker system prune", "pkill", "killall", "--all-projects"):
                            self.assertNotIn(forbidden, body, label)


if __name__ == "__main__":
    unittest.main()

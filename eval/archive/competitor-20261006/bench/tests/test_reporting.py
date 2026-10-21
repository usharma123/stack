"""Offline tests for the manifest-selected final report (bench/report.py) and run.py's
public timing suppression. No network, Docker or services.

python3 -m unittest discover -s bench/tests -v
"""
from pathlib import Path
import copy
import hashlib
import json
import sys
import tempfile
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(BENCH / "tests"))

import report  # noqa: E402
import run as runner  # noqa: E402
from rwb.adapters import registry  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402
from test_core import Toy  # noqa: E402

PLAN_NS = 1_800_000_000 * 10**9
PLAN_UTC = "2027-01-15T08:00:00Z"  # == PLAN_NS
HARNESS = dict(commit="c0ffee", files={"run.py": "1" * 64})
FIXTURE = {"app.py": "2" * 64}
GLUE = {"rwb-services.sh": "3" * 64}
CONFIG = {"toy.toml": "4" * 64}
REPEATS, WARMUPS = 3, 1


def write_run(out, tool="mise", start_ns=PLAN_NS + 10**9, duration_ns=10**9, configure=None, **meta_overrides):
    """Drive the real Scenario with the fake tool and write a run directory shaped like run.py's."""
    rec, world = FakeRecorder(), FakeWorld()
    adapter = Toy()
    tx = FakeTransport(rec, adapter, world)
    scenario = Scenario(adapter, tx, rec, repeats=REPEATS, warmups=WARMUPS)
    world.scenario = scenario

    def raw(result):  # vary timings; keep the exit code as the real Recorder writes it
        result.outer_ns = 1_000_000 + result.seq * 1000
        result.extra["raw_exit"] = result.code
    world.mutators.append(raw)
    if configure:
        configure(world)
    scenario.execute()
    scenario.cleanup()
    out = Path(out)
    (out / "logs").mkdir(parents=True)
    with (out / "steps.jsonl").open("w") as f:
        for r in rec.results:
            base = f"logs/{r.seq:04d}-{r.label}"
            (out / f"{base}.stdout").write_text(r.stdout)
            (out / f"{base}.stderr").write_text(r.stderr)
            f.write(json.dumps(dict(seq=r.seq, label=r.label, phase=r.phase, exit=r.extra.get("raw_exit", r.code),
                                    timed_out=r.timed_out, outer_ns=r.outer_ns, inner_ns=r.inner_ns,
                                    stdout=f"{base}.stdout", stderr=f"{base}.stderr")) + "\n")
    meta = dict(run_id=out.name, tool=tool, variant="default", title="Toy", valid=True, completed=True,
                started_unix_ns=start_ns, finished_unix_ns=start_ns + duration_ns, harness=dict(HARNESS, dirty=True),
                fixture=FIXTURE, shared_glue=GLUE, config=CONFIG, host=dict(system="Linux"), transport="docker",
                isolation_boundary="service-instance", options={}, pins={}, repeats=REPEATS, warmups=WARMUPS,
                keep=False, resources=dict(cpus=None, memory=None), errors=[], cleanup_problems=[],
                timings=scenario.timings, reportable=False, measurement="diagnostic")
    meta.update(meta_overrides)
    (out / "meta.json").write_text(json.dumps(meta, indent=2))
    (out / "outcomes.json").write_text(json.dumps(scenario.out.as_list(), indent=2))
    return out


def edit_json(path, fn):
    data = json.loads(Path(path).read_text())
    fn(data)
    Path(path).write_text(json.dumps(data))


def manifest(attempts, **session):
    roster = [dict(entry=e, tool=t, variant="default") for e, t in report.ROSTER]
    s = dict(id="test-session", purpose="final-measurement", plan_created_utc=PLAN_UTC, roster=roster,
             sample_policy=dict(repeats=REPEATS, warmups=WARMUPS), timing_policy="serial",
             serialization_declaration="parent ran each attempt synchronously; no builds", review="review-1")
    s.update(session)
    lanes = {t: dict(config=CONFIG, version_contains=[], deviations=[]) for _, t in report.ROSTER}
    return dict(schema=report.SCHEMA, session=s, attempts=attempts,
                protocol=dict(harness=HARNESS, fixture=FIXTURE, shared_glue=GLUE, lanes=lanes,
                              platform=dict(host=None, note="Linux containers"),
                              resources=dict(cpus=None, memory=None), cache_policy="images as built"))


def selected(run_dir, **extra):
    block = report.attempt_block(run_dir)
    block.update(result_review="result-review-1", **extra)
    return block


class ReportCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())

    def run_dir(self, name="r1", **kw):
        return write_run(self.tmp / name, **kw)

    def build(self, attempts, **session):
        path = self.tmp / f"manifest-{len(list(self.tmp.glob('manifest-*')))}.json"
        path.write_text(json.dumps(manifest(attempts, **session)))
        return report.build(json.loads(path.read_text()), path)

    def metrics(self, rep, tool="mise"):
        return {t["metric"]: t for t in rep["reported_timings"] if t["tool"] == tool}

    def omitted(self, rep, tool="mise"):
        return {o["metric"]: o["reason"] for o in rep["omitted"] if o["tool"] == tool}


class EligibleTest(ReportCase):
    def test_serial_reviewed_attempt_reports_recomputed_metrics(self):
        d = self.run_dir()
        rep = self.build([selected(d)])
        got = self.metrics(rep)
        self.assertEqual(set(got), set(report.METRICS), self.omitted(rep))
        steps = [json.loads(x) for x in (d / "steps.jsonl").read_text().splitlines()]
        by_seq = {s["seq"]: s for s in steps}
        task = got["first_task.a"]
        self.assertEqual(task["steps_ns"], sum(by_seq[s]["outer_ns"] for s in task["evidence"]))
        entry = got["repeat.entry"]
        samples = [s for s in steps if s["label"].startswith("repeat.entry-sample-")]
        warm = [s for s in steps if s["label"].startswith("repeat.entry-warmup-")]
        self.assertEqual((entry["n"], entry["warmups"], len(warm)), (REPEATS, WARMUPS, WARMUPS))
        self.assertEqual(entry["evidence"], [s["seq"] for s in samples])
        self.assertNotIn(warm[0]["seq"], entry["evidence"])
        ordered = sorted(s["outer_ns"] for s in samples)
        self.assertEqual(entry["outer"]["p50_ms"], round(ordered[1] / 1e6, 3))
        self.assertEqual(entry["outer"]["p95_ms"], round(ordered[-1] / 1e6, 3))
        self.assertEqual(len(rep["roster"]), 26)
        row = next(r for r in rep["roster"] if r["tool"] == "mise")
        self.assertEqual(row["coverage"], "completed")
        self.assertTrue(next(a for a in rep["attempts"] if a["tool"] == "mise")["reportable"])
        self.assertFalse(rep["coverage_complete"])
        self.assertTrue(all(r["coverage"].startswith("missing") for r in rep["roster"] if r["tool"] != "mise"))
        self.assertFalse(json.loads((d / "meta.json").read_text())["reportable"])  # source untouched

    def test_cli_writes_report_and_require_complete_fails_on_missing_lanes(self):
        d = self.run_dir()
        m = self.tmp / "m.json"
        m.write_text(json.dumps(manifest([selected(d)])))
        self.assertEqual(report.main(["--manifest", str(m), "--out", str(self.tmp / "rep")]), 0)
        self.assertIn("| mise | ", (self.tmp / "rep" / "report.md").read_text())
        self.assertEqual(report.main(["build", "--manifest", str(m), "--out", str(self.tmp / "rep2"),
                                      "--require-complete"]), 1)
        with self.assertRaises(FileExistsError):
            report.main(["--manifest", str(m), "--out", str(self.tmp / "rep")])

    def test_absent_inner_timing_stays_null(self):
        def no_inner(world):
            world.mutators.append(lambda r: setattr(r, "inner_ns", None))
        rep = self.build([selected(self.run_dir(configure=no_inner))])
        entry = self.metrics(rep)["repeat.entry"]
        self.assertEqual(entry["inner"], dict(available=0, p50_ms=None, p95_ms=None))
        self.assertIsNotNone(entry["outer"]["p50_ms"])

    def test_rerun_selects_only_declared_attempt(self):
        first = self.run_dir("try1", start_ns=PLAN_NS + 10**9)
        second = self.run_dir("try2", start_ns=PLAN_NS + 5 * 10**9)
        old = report.attempt_block(first)
        old.update(state="excluded", reason="rerun after network flake")
        rep = self.build([old, selected(second)])
        self.assertEqual({t["run_id"] for t in rep["reported_timings"]}, {"try2"})
        self.assertEqual([a["state"] for a in rep["attempts"]], ["excluded", "selected"])
        self.assertTrue(first.is_dir() and second.is_dir())


class ExclusionTest(ReportCase):
    def assert_no_timings(self, rep, needle, tool="mise"):
        self.assertEqual(self.metrics(rep, tool), {})
        reasons = self.omitted(rep, tool)
        self.assertEqual(set(reasons), set(report.METRICS))
        self.assertTrue(all(needle in r for r in reasons.values()), reasons)

    def test_unselected_smoke_stays_diagnostic(self):
        smoke = self.run_dir("smoke", start_ns=PLAN_NS - 10**12, repeats=20)
        block = report.attempt_block(smoke)
        block.update(state="excluded", reason="smoke")
        rep = self.build([block])
        self.assertEqual(rep["reported_timings"], [])
        # Selecting a pre-plan smoke anyway: refused.
        rep = self.build([selected(self.run_dir("smoke2", start_ns=PLAN_NS - 10**12))])
        self.assert_no_timings(rep, "before the session plan")

    def test_no_review_no_timings(self):
        d = self.run_dir()
        self.assert_no_timings(self.build([selected(d)], review=None), "no review reference")
        block = selected(d)
        block["result_review"] = None
        self.assert_no_timings(self.build([block]), "no result-review")

    def test_overlapping_and_misordered_runs(self):
        a = self.run_dir("a", start_ns=PLAN_NS + 10**9, duration_ns=10 * 10**9)
        b = self.run_dir("b", tool="flox", start_ns=PLAN_NS + 5 * 10**9)
        rep = self.build([selected(a), selected(b)])
        self.assert_no_timings(rep, "overlaps run", "mise")
        self.assert_no_timings(rep, "overlaps run", "flox")
        c = self.run_dir("c", tool="flox", start_ns=PLAN_NS + 100 * 10**9)
        d = self.run_dir("d", start_ns=PLAN_NS + 200 * 10**9)
        self.assert_no_timings(self.build([selected(d), selected(c)]), "declared after", "flox")

    def test_identity_and_protocol_mismatches(self):
        cases = [(dict(run_id="other"), "run_id"), (dict(tool="flox"), "meta.tool"),
                 (dict(harness=dict(commit="c0ffee", files={"run.py": "9" * 64})), "harness file hashes"),
                 (dict(config={"toy.toml": "9" * 64}), "lane config"),
                 (dict(shared_glue=None), "shared glue"), (dict(fixture={}), "fixture"),
                 (dict(repeats=20), "sample policy"), (dict(keep=True), "--keep"),
                 (dict(resources=dict(cpus="2", memory=None)), "resources"),
                 (dict(reportable=True), "reportable"), (dict(started_unix_ns=None), "nanosecond")]
        for i, (override, needle) in enumerate(cases):
            with self.subTest(needle):
                d = self.run_dir(f"m{i}", **override)
                block = selected(d)
                block.update(run_id=f"m{i}", tool="mise")
                self.assert_no_timings(self.build([block]), needle)

    def test_tampered_evidence(self):
        d = self.run_dir()
        block = selected(d)
        log = sorted((d / "logs").glob("*repeat.entry-sample-001*.stdout"))[0]
        log.write_text("edited")
        rep = self.build([block])
        self.assert_no_timings(rep, "tree digest")
        self.assertTrue(next(r for r in rep["roster"] if r["tool"] == "mise")["coverage"].startswith("evidence invalid"))
        d2 = self.run_dir("r2")
        block = selected(d2)
        edit_json(d2 / "meta.json", lambda m: m["timings"]["first_task.a"].update(steps_ns=1))
        self.assert_no_timings(self.build([block]), "meta.json hash differs")

    def test_steps_and_outcome_integrity(self):
        def dup(d):
            lines = (d / "steps.jsonl").read_text().splitlines()
            (d / "steps.jsonl").write_text("\n".join(lines + [lines[0]]) + "\n")

        def missing_log(d):
            next((d / "logs").glob("*-a-setup.stderr")).unlink()

        def incomplete(d):
            edit_json(d / "outcomes.json", lambda o: o.remove(next(x for x in o if x["check"] == "occupied_port")))
        for name, damage, needle in (("dup", dup, "duplicate step seq"), ("log", missing_log, "missing raw stderr"),
                                     ("out", incomplete, "incomplete outcomes")):
            with self.subTest(name):
                d = self.run_dir(name)
                damage(d)
                self.assert_no_timings(self.build([selected(d)]), needle)  # hashes taken after damage

    def test_invalid_and_blocked_runs_never_yield_timings(self):
        d = self.run_dir("inv", valid=False, errors=["harness: boom"])
        rep = self.build([selected(d)])
        self.assert_no_timings(rep, "run invalid")
        self.assertEqual(next(r for r in rep["roster"] if r["tool"] == "mise")["coverage"],
                         "invalid run (harness error, interruption or unclean teardown)")
        g = self.run_dir("guix", tool="guix", blocked="provision-guix: no verified digest")
        rep = self.build([selected(g, reason="evidence only")])
        row = next(r for r in rep["roster"] if r["tool"] == "guix")
        self.assertTrue(row["coverage"].startswith("blocked (provisioning)"))
        self.assert_no_timings(rep, "provisioning blocked", "guix")
        rep = self.build([dict(tool="vagrant", variant="default", state="blocked", reason="no hypervisor")])
        self.assertEqual(next(r for r in rep["roster"] if r["tool"] == "vagrant")["coverage"],
                         "blocked (declared): no hypervisor")

    def test_stack_needs_product_identity(self):
        d = self.run_dir("st", tool="stack", options=dict(stack_sha256="a" * 64), pins=dict(stack_sha256="a" * 64))
        self.assert_no_timings(self.build([selected(d)]), "stack attempt needs", "stack")
        receipt = self.tmp / "build.txt"
        receipt.write_text("cargo build --release ok\n")
        good = dict(source_revision="abc123", binary_sha256="a" * 64,
                    build_receipt=dict(path=str(receipt), sha256=report.sha256_file(receipt)))
        self.assertEqual(len(self.metrics(self.build([selected(d, stack=good)]), "stack")), 4)
        self.assert_no_timings(self.build([selected(d, stack=dict(good, binary_sha256="b" * 64))]),
                               "binary SHA256", "stack")

    def test_version_output_checked(self):
        def versioned(world):
            world.outputs["tool-version"] = ("toy 1.2.3\n", "")
        d = self.run_dir(configure=versioned)
        m = manifest([selected(d)])
        m["protocol"]["lanes"]["mise"]["version_contains"] = ["toy 1.2.3"]
        path = self.tmp / "mv.json"
        path.write_text(json.dumps(m))
        self.assertEqual(len(self.metrics(report.build(m, path))), 4)
        m["protocol"]["lanes"]["mise"]["version_contains"] = ["toy 9"]
        path.write_text(json.dumps(m))
        self.assert_no_timings(report.build(m, path), "lacks 'toy 9'")


class MetricTest(ReportCase):
    def test_failed_sample_omits_only_that_metric(self):
        d = self.run_dir(configure=lambda w: w.codes.update({"repeat.entry-sample-002": (1, False)}))
        rep = self.build([selected(d)])
        self.assertIn("outcome fail", self.omitted(rep)["repeat.entry"])
        self.assertEqual(set(self.metrics(rep)), {"first_task.a", "first_task.b", "repeat.app_read"})
        outcome = next(o for a in rep["attempts"] for o in a["outcomes"] if o["check"] == "repeat.entry")
        self.assertEqual(outcome["status"], "fail")  # the failure remains visible

    def test_exit_zero_wrong_keeper(self):
        wrong = json.dumps(dict(command="read", ok=True, result=dict(item=dict(sku="keeper-b")))) + "\n"
        d = self.run_dir(configure=lambda w: w.outputs.update({"repeat.app_read-sample-002": (wrong, "")}))
        step = next(json.loads(x) for x in (d / "steps.jsonl").read_text().splitlines()
                    if json.loads(x)["label"] == "repeat.app_read-sample-002")
        self.assertEqual(step["exit"], 0)
        rep = self.build([selected(d)])
        self.assertNotIn("repeat.app_read", self.metrics(rep))
        self.assertIn("repeat.entry", self.metrics(rep))
        # Even if the outcome had been recorded as pass, the receipt reparse refuses it.
        d2 = self.run_dir("forced", configure=lambda w: w.outputs.update({"repeat.app_read-sample-002": (wrong, "")}))
        edit_json(d2 / "outcomes.json",
                  lambda o: next(x for x in o if x["check"] == "repeat.app_read").update(status="pass"))
        self.assertIn("receipts", self.omitted(self.build([selected(d2)]))["repeat.app_read"])

    def test_failed_warmup_and_missing_sample(self):
        d = self.run_dir(configure=lambda w: w.codes.update({"repeat.entry-warmup-000": (1, False)}))
        self.assertIn("warmup receipts", self.omitted(self.build([selected(d)]))["repeat.entry"])
        d2 = self.run_dir("gap")
        lines = (d2 / "steps.jsonl").read_text().splitlines()
        gone = next(json.loads(x)["seq"] for x in lines if json.loads(x)["label"] == "repeat.entry-sample-003")
        (d2 / "steps.jsonl").write_text("\n".join(x for x in lines if json.loads(x)["seq"] != gone) + "\n")
        self.assertIn("cites missing steps", self.omitted(self.build([selected(d2)]))["repeat.entry"])
        # Also drop the citation: the policy gate still refuses the short sample set.
        edit_json(d2 / "outcomes.json",
                  lambda o: next(x for x in o if x["check"] == "repeat.entry")["evidence"].remove(gone))
        rep = self.build([selected(d2)])
        self.assertIn("sample receipts differ from policy", self.omitted(rep)["repeat.entry"])

    def test_two_checkout_condition_required_for_repeats(self):
        d = self.run_dir()
        edit_json(d / "outcomes.json", lambda o: next(x for x in o if x["check"] == "isolation").update(status="fail"))
        rep = self.build([selected(d)])
        self.assertIn("two-checkout condition", self.omitted(rep)["repeat.app_read"])
        self.assertIn("first_task.a", self.metrics(rep))

    def test_first_task_gate_and_sum(self):
        d = self.run_dir()
        edit_json(d / "outcomes.json", lambda o: next(x for x in o if x["check"] == "tests.a").update(status="fail"))
        rep = self.build([selected(d)])
        self.assertIn("tests.a", self.omitted(rep)["first_task.a"])  # meta still says ok=True
        self.assertIn("first_task.b", self.metrics(rep))
        d2 = self.run_dir("sum")
        edit_json(d2 / "meta.json", lambda m: m["timings"]["first_task.b"].update(steps_ns=5))
        self.assertIn("receipt sum", self.omitted(self.build([selected(d2)]))["first_task.b"])
        d3 = self.run_dir("pct")
        edit_json(d3 / "meta.json", lambda m: m["timings"]["repeat.entry"]["outer"].update(p50_ms=0.001))
        self.assertIn("stored summary disagrees", self.omitted(self.build([selected(d3)]))["repeat.entry"])

    def test_first_task_wall_and_prepare_must_be_durations(self):
        cases = [("wall_ns", -900_000_000), ("wall_ns", 1.5), ("wall_ns", "bad"), ("wall_ns", None),
                 ("wall_ns", True), ("wall_ns", float("nan")), ("wall_ns", "<missing>"),
                 ("prepare_ns", -1), ("prepare_ns", 2.5), ("prepare_ns", "1"), ("prepare_ns", False)]
        for i, (field, value) in enumerate(cases):
            with self.subTest(f"{field}={value!r}"):
                d = self.run_dir(f"wall{i}")

                def damage(m):
                    task = m["timings"]["first_task.a"]
                    task.pop(field, None) if value == "<missing>" else task.update({field: value})
                edit_json(d / "meta.json", damage)
                rep = self.build([selected(d)])
                self.assertIn(f"stored {field}", self.omitted(rep)["first_task.a"])
                self.assertEqual(set(self.metrics(rep)), set(report.METRICS) - {"first_task.a"})
                self.assertIn("first_task.a", report.render(rep))  # renders as an omitted row, no TypeError
                m = self.tmp / f"wall{i}.json"
                m.write_text(json.dumps(manifest([selected(d)])))
                out = self.tmp / f"wall{i}-rep"
                self.assertEqual(report.main(["--manifest", str(m), "--out", str(out)]), 0)
                self.assertTrue((out / "report.md").is_file())
        d = self.run_dir("noprep")  # an absent (null) prepare_ns is allowed
        edit_json(d / "meta.json", lambda m: m["timings"]["first_task.a"].update(prepare_ns=None, wall_ns=0))
        got = self.metrics(self.build([selected(d)]))["first_task.a"]
        self.assertEqual((got["prepare_ns"], got["wall_ns"]), (None, 0))


class ManifestTest(ReportCase):
    def test_schema_errors(self):
        d = self.run_dir()
        bad = [manifest([selected(d)], roster=[dict(entry="Stack", tool="stack", variant="default")]),
               manifest([selected(d)], purpose="smoke"),
               manifest([selected(d), selected(d)]),
               manifest([dict(tool="mise", variant="default", state="untested")])]
        for m in bad:
            with self.subTest(m["session"].get("purpose")):
                with self.assertRaises(report.ManifestError):
                    report.check_manifest(m)
        path = self.tmp / "bad.json"
        path.write_text(json.dumps(bad[0]))
        self.assertEqual(report.main(["--manifest", str(path), "--out", str(self.tmp / "x")]), 2)
        self.assertFalse((self.tmp / "x").exists())

    def test_roster_is_frozen_registry(self):
        self.assertEqual(sorted(t for _, t in report.ROSTER), sorted(registry.ADAPTERS))
        self.assertEqual(len(report.ROSTER), 26)

    def test_template_uses_current_bytes_and_no_review(self):
        m = report.template("s1")
        self.assertIsNone(m["session"]["review"])
        self.assertEqual(m["protocol"]["harness"]["files"]["run.py"],
                         hashlib.sha256((BENCH / "run.py").read_bytes()).hexdigest())
        self.assertEqual(m["protocol"]["shared_glue"], runner.tree_hashes(BENCH / "adapters" / "_shared"))
        self.assertEqual((len(m["session"]["roster"]), m["attempts"]), (26, []))
        m["session"]["serialization_declaration"] = "x"
        m["protocol"]["cache_policy"] = "x"
        m["protocol"]["platform"]["note"] = "x"
        report.check_manifest(copy.deepcopy(m))  # a filled template is structurally valid
        path = self.tmp / "t.json"
        path.write_text(json.dumps(m))
        rep = report.build(m, path)
        self.assertFalse(rep["coverage_complete"])  # nothing declared yet: every row is missing
        self.assertEqual({r["coverage"] for r in rep["roster"]}, {"missing: no attempt declared"})


def edit_step(run_dir, label, fn):
    path = Path(run_dir) / "steps.jsonl"
    records = [json.loads(x) for x in path.read_text().splitlines()]
    fn(next(r for r in records if r["label"] == label))
    path.write_text("".join(json.dumps(r) + "\n" for r in records))


class ReceiptSafetyTest(ReportCase):
    LABEL = "repeat.app_read-sample-001"

    def roster_row(self, rep, tool="mise"):
        return next(r for r in rep["roster"] if r["tool"] == tool)

    def test_logs_inside_run_tree_accepted(self):
        d = self.run_dir()
        sub = d / "logs" / "nested"
        sub.mkdir()
        edit_step(d, self.LABEL, lambda r: (sub / "x.stdout").write_text((d / r["stdout"]).read_text())
                  or r.update(stdout="logs/nested/x.stdout"))
        rep = self.build([selected(d)])
        self.assertEqual(set(self.metrics(rep)), set(report.METRICS), self.omitted(rep))
        self.assertTrue(self.roster_row(rep)["covered"])

    def test_logs_outside_hashed_tree_refused(self):
        outside = self.tmp / "outside.stdout"

        def absolute(d, r):
            outside.write_text((d / r["stdout"]).read_text())
            r["stdout"] = str(outside)

        def traversal(d, r):
            outside.write_text((d / r["stdout"]).read_text())
            r["stdout"] = f"../{outside.name}"

        def dotdot_back_in(d, r):  # resolves inside, but `..` is refused outright
            r["stdout"] = "logs/../" + r["stdout"]

        def absolute_inside(d, r):
            r["stdout"] = str((d / r["stdout"]).resolve())

        def symlink_escape(d, r):
            outside.write_text((d / r["stdout"]).read_text())
            (d / "logs" / "link.stdout").symlink_to(outside)
            r["stdout"] = "logs/link.stdout"

        def symlinked_dir(d, r):
            target = self.tmp / f"{d.name}-ext"
            target.mkdir()
            (target / "x.stdout").write_text((d / r["stdout"]).read_text())
            (d / "elsewhere").symlink_to(target, target_is_directory=True)
            r["stdout"] = "elsewhere/x.stdout"
        cases = (absolute, traversal, dotdot_back_in, absolute_inside, symlink_escape, symlinked_dir)
        for i, damage in enumerate(cases):
            with self.subTest(damage.__name__):
                d = self.run_dir(f"esc{i}")
                edit_step(d, self.LABEL, lambda r: damage(d, r))
                block = selected(d)
                before = report.tree_digest(d)
                if outside.exists():  # mutating the external copy must not slip past the hashes
                    outside.write_text(outside.read_text() + "\nchanged outside the integrity set\n")
                if damage in (absolute, traversal, symlinked_dir):  # the edit is invisible to the tree digest
                    self.assertEqual(report.tree_digest(d), before)
                rep = self.build([block])
                self.assert_no_timings_for(rep, "missing raw stdout log inside the hashed run tree")
                self.assertFalse(self.roster_row(rep)["covered"])
                self.assertTrue(self.roster_row(rep)["coverage"].startswith("evidence invalid"))
                outside.unlink(missing_ok=True)

    def assert_no_timings_for(self, rep, needle):
        self.assertEqual(self.metrics(rep), {})
        self.assertTrue(all(needle in r for r in self.omitted(rep).values()), self.omitted(rep))

    def test_selected_but_missing_evidence_is_not_coverage(self):
        attempts = [dict(tool=t, variant="default", state="selected", path=str(self.tmp / "absent" / t), run_id=t,
                         hashes={}, result_review="review") for _, t in report.ROSTER]
        rep = self.build(attempts)
        self.assertFalse(rep["coverage_complete"])
        self.assertEqual(len(rep["roster"]), 26)
        self.assertTrue(all(r["coverage"].startswith("evidence missing") and not r["covered"] for r in rep["roster"]))
        m = self.tmp / "absent.json"
        m.write_text(json.dumps(manifest(attempts)))
        self.assertEqual(report.main(["--manifest", str(m), "--out", str(self.tmp / "absent-rep"),
                                      "--require-complete"]), 1)

    def complete_attempts(self, mise, tag=""):
        guix = self.run_dir(f"guix{tag}", tool="guix", start_ns=PLAN_NS + 100 * 10**9,
                            blocked="provision-guix: no verified digest")
        rest = [dict(tool=t, variant="default", state="untested", reason="declared out of reach")
                for _, t in report.ROSTER if t not in ("mise", "guix")]
        return [selected(mise, tool="mise"), selected(guix, reason="blocked receipt")] + rest

    def test_require_complete_accepts_intact_evidence_and_declarations_only(self):
        attempts = self.complete_attempts(self.run_dir())
        rep = self.build(attempts)
        self.assertTrue(rep["coverage_complete"], [r["coverage"] for r in rep["roster"] if not r["covered"]])
        self.assertTrue(self.roster_row(rep, "guix")["coverage"].startswith("blocked (provisioning)"))
        self.assertEqual(len(self.metrics(rep)), 4)
        m = self.tmp / "complete.json"
        m.write_text(json.dumps(manifest(attempts)))
        self.assertEqual(report.main(["--manifest", str(m), "--out", str(self.tmp / "c1"), "--require-complete"]), 0)

        def tampered(d):
            next((d / "logs").glob("*.stdout")).write_text("edited")

        def mislabeled(d):  # intact hashes, but the evidence is another lane's run
            edit_json(d / "meta.json", lambda meta: meta.update(tool="flox"))

        def invalid(d):
            edit_json(d / "meta.json", lambda meta: meta.update(valid=False, errors=["harness: boom"]))

        def incomplete(d):
            edit_json(d / "meta.json", lambda meta: meta.update(completed=False))
        for i, damage in enumerate((tampered, mislabeled, invalid, incomplete)):
            with self.subTest(damage.__name__):
                d = self.run_dir(f"bad{i}")
                if damage is tampered:
                    attempts = self.complete_attempts(d, i)
                    damage(d)
                else:
                    damage(d)
                    attempts = self.complete_attempts(d, i)
                rep = self.build(attempts)
                self.assertFalse(rep["coverage_complete"], damage.__name__)
                self.assertFalse(self.roster_row(rep)["covered"])
                self.assertEqual(len(rep["roster"]), 26)

    def test_negative_inner_withheld_outer_still_reported(self):
        d = self.run_dir(configure=lambda w: w.mutators.append(lambda r: setattr(r, "inner_ns", -900_000)))
        rep = self.build([selected(d)])
        got = self.metrics(rep)
        self.assertEqual(set(got), set(report.METRICS), self.omitted(rep))
        for key in ("repeat.entry", "repeat.app_read"):
            inner = got[key]["inner"]
            self.assertEqual((inner["available"], inner["p50_ms"], inner["p95_ms"]), (0, None, None))
            self.assertIn("negative inner_ns", inner["reason"])
            self.assertEqual(got[key]["n"], REPEATS)
            self.assertGreaterEqual(got[key]["outer"]["p50_ms"], 0)
        self.assertIn("withheld", report.render(rep))
        # One negative sample among valid ones also withholds the inner summary.
        d2 = self.run_dir("one")
        edit_step(d2, "repeat.entry-sample-001", lambda r: r.update(inner_ns=-1))
        edit_json(d2 / "meta.json", lambda m: m["timings"]["repeat.entry"].update(
            inner=report.summarize_ns([(True, r["inner_ns"]) for r in map(json.loads, (d2 / "steps.jsonl")
                                       .read_text().splitlines()) if r["label"].startswith("repeat.entry-sample-")])))
        entry = self.metrics(self.build([selected(d2)]))["repeat.entry"]
        self.assertEqual(entry["inner"]["p50_ms"], None)
        self.assertNotIn("-", json.dumps(entry["outer"]))

    def test_malformed_durations_make_the_receipt_malformed(self):
        cases = [("outer_ns", -1), ("outer_ns", True), ("outer_ns", 1.5), ("outer_ns", float("inf")),
                 ("outer_ns", float("nan")), ("outer_ns", "100"), ("outer_ns", None),
                 ("inner_ns", True), ("inner_ns", 1.5), ("inner_ns", float("inf")), ("inner_ns", float("nan")),
                 ("inner_ns", "100")]
        for i, (field, value) in enumerate(cases):
            with self.subTest(f"{field}={value!r}"):
                d = self.run_dir(f"dur{i}")
                edit_step(d, "repeat.entry-sample-001", lambda r: r.update({field: value}))
                rep = self.build([selected(d)])
                self.assert_no_timings_for(rep, "malformed step record")
                self.assertFalse(self.roster_row(rep)["covered"])


def ok_seq(run_dir):
    return next(r["seq"] for r in map(json.loads, (Path(run_dir) / "steps.jsonl").read_text().splitlines())
                if r["exit"] == 0 and not r["timed_out"])


def failed_seq(run_dir):
    return next(r["seq"] for r in map(json.loads, (Path(run_dir) / "steps.jsonl").read_text().splitlines())
                if r["exit"] != 0)


class ArtifactReceiptTest(ReportCase):
    """meta.artifacts as written by Scenario.collect_artifacts: copies land at artifacts/<checkout>/<path>."""

    def with_artifacts(self, name, make, receipts):
        d = self.run_dir(name)
        make(d)
        edit_json(d / "meta.json", lambda m: m.update(artifacts=receipts(d)))
        return d

    def test_present_copies_and_failed_best_effort_copies_accepted(self):
        def make(d):
            (d / "artifacts" / "a" / ".rwb-state" / "logs").mkdir(parents=True)
            (d / "artifacts" / "a" / ".rwb-state" / "logs" / "pc.log").write_text("log\n")
            (d / "artifacts" / "a" / ".rwb-state" / "logs" / "same.log").symlink_to("pc.log")  # stays inside
            (d / "artifacts" / "b").mkdir()
            (d / "artifacts" / "b" / "service.log").write_text("log\n")
        d = self.with_artifacts("arts", make, lambda d: [
            dict(checkout="a", path=".rwb-state/logs", ok=True, seq=ok_seq(d)),
            dict(checkout="b", path="service.log", ok=True, seq=ok_seq(d)),
            dict(checkout="b", path=".devenv/processes.log", ok=False, seq=failed_seq(d))])  # never copied
        rep = self.build([selected(d)])
        self.assertEqual(rep["attempts"][0]["evidence_problems"], [])
        self.assertEqual(set(self.metrics(rep)), set(report.METRICS), self.omitted(rep))

    def test_missing_or_escaping_successful_copies_rejected(self):
        outside = self.tmp / "outside.log"
        outside.write_text("external\n")

        def file_at(rel):
            def make(d):
                (d / "artifacts" / "a").mkdir(parents=True)
                if rel:
                    (d / "artifacts" / "a" / rel).write_text("log\n")
            return make

        def symlinked_file(d):
            (d / "artifacts" / "a").mkdir(parents=True)
            (d / "artifacts" / "a" / "service.log").symlink_to(outside)

        def dir_with_escaping_link(d):
            (d / "artifacts" / "a" / "logs").mkdir(parents=True)
            (d / "artifacts" / "a" / "logs" / "x.log").symlink_to(outside)

        def receipt(path="service.log", **kw):
            return lambda d: [dict(dict(checkout="a", path=path, ok=True, seq=ok_seq(d)), **kw)]
        cases = [
            ("deleted before hashing", file_at(None), receipt(), "missing inside the hashed run tree"),
            ("symlink to outside", symlinked_file, receipt(), "missing inside the hashed run tree"),
            ("directory with escaping symlink", dir_with_escaping_link, receipt("logs"), "symlink leaving"),
            ("traversal", file_at(None), lambda d: [dict(checkout="a", path="../../meta.json", ok=True,
                                                         seq=ok_seq(d))], "missing inside the hashed run tree"),
            ("absolute", file_at(None), receipt(str(outside)), "missing inside the hashed run tree"),
            ("failed copy step marked ok", file_at("service.log"), lambda d: [dict(
                checkout="a", path="service.log", ok=True, seq=failed_seq(d))], "without a successful copy receipt"),
            ("missing step", file_at("service.log"), receipt(seq=99_999), "cites missing step"),
            ("ok not boolean", file_at("service.log"), receipt(ok="yes"), "malformed artifact receipt"),
            ("bad checkout", file_at("service.log"), receipt(checkout="a/../a"), "malformed artifact receipt"),
        ]
        for i, (name, make, receipts, needle) in enumerate(cases):
            with self.subTest(name):
                d = self.with_artifacts(f"bad{i}", make, receipts)
                rep = self.build([selected(d)])  # hashes are taken after the damage: they cannot catch it
                problems = rep["attempts"][0]["evidence_problems"]
                self.assertTrue(any(needle in p for p in problems), problems)
                self.assertEqual(self.metrics(rep), {})
                row = next(r for r in rep["roster"] if r["tool"] == "mise")
                self.assertFalse(row["covered"])


class RunSummaryTest(unittest.TestCase):
    META = dict(title="T", tool="t", variant="d", run_id="r", completed=True, transport="docker",
                isolation_boundary="service-instance", errors=[], measurement="x",
                timings={"first_task.a": dict(ok=True, steps_ns=1_234_000_000, wall_ns=2_000_000_000, steps=[1],
                                              checkout="first"),
                         "repeat.entry": dict(outer=dict(p50_ms=777.0, p95_ms=888.0, ok=1, n=1),
                                              inner=dict(p50_ms=1, p95_ms=1))})

    def test_invalid_or_blocked_meta_with_numbers_renders_none(self):
        for extra in (dict(valid=False, errors=["harness: x"]), dict(valid=True, blocked="provision: x")):
            text = runner.render_summary(dict(self.META, **extra), [])
            self.assertNotIn("777", text)
            self.assertNotIn("1.23", text)
            self.assertIn("No timings", text)
        self.assertIn("777", runner.render_summary(dict(self.META, valid=True), []))


if __name__ == "__main__":
    unittest.main()

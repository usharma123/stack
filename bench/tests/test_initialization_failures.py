"""Initialization failures in run.py: truthful receipt, exact tempdir cleanup, original error kept.

Offline only: no Docker, no network, no services, no transport command is executed.
python3 -m unittest discover -s bench/tests -v
"""
from pathlib import Path
import json
import os
import shutil
import signal
import sys
import tempfile
import unittest
from unittest import mock

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

import run  # noqa: E402
from rwb import transport  # noqa: E402
from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES, Adapter  # noqa: E402
from rwb.adapters.git_grove import GitGroveAdapter  # noqa: E402
from rwb.record import Recorder  # noqa: E402


class HostToy(Adapter):
    name, title, transport = "inittoy", "Init toy", "host"
    features = {k: "native" for k in FEATURES}


class ShadowedImage(HostToy):
    """The smoke-git-grove-1 defect shape: a per-checkout helper named like Adapter.image."""
    def image(self, co):
        return f"{co.name}-app"


class DockerShadowedImage(ShadowedImage):
    transport = "docker"


class WritesHomeThenFails(HostToy):
    """host_env writes harness files into the tempdir; Scenario() construction then fails."""
    def host_env(self, workdir):
        (Path(workdir) / "home" / ".config").mkdir(parents=True)
        (Path(workdir) / "home" / ".config" / "x").write_text("harness-written\n")
        return None

    def checkout(self, name, index, token=""):
        raise RuntimeError("checkout model exploded")


class CyclicPins(HostToy):
    pins = {"tool": "1"}
    pins["self"] = pins


class TupleKeyPins(HostToy):
    pins = {("tool", "version"): "1", "plain": [1, ("a", "b")]}


class WidePins(HostToy):
    """Wide malformed metadata: 60,000 nulls, then a value the strict serializer rejects."""
    pins = {"wide": [None] * 60000 + [object()]}


class UntrustedTitle(ShadowedImage):
    title = object()


class InterruptedInit(HostToy):
    def host_env(self, workdir):
        raise KeyboardInterrupt("signal 15")


class InitHarness(unittest.TestCase):
    def setUp(self):
        self.base = Path(tempfile.mkdtemp(prefix="rwb-init-test-")).resolve()
        self.addCleanup(shutil.rmtree, self.base, ignore_errors=True)
        self.tmp = self.base / "tmp"
        self.tmp.mkdir()
        # Route mkdtemp's default directory into the test sandbox (restored afterwards).
        saved = tempfile.tempdir
        tempfile.tempdir = str(self.tmp)
        self.addCleanup(setattr, tempfile, "tempdir", saved)
        handler = signal.getsignal(signal.SIGTERM)
        self.addCleanup(signal.signal, signal.SIGTERM, handler)
        self.out = self.base / "results" / "attempt-1"

    def main(self, cls):
        with mock.patch.object(run.registry, "load", return_value=(cls, "default")):
            return run.main(["--tool", cls.name, "--out", str(self.out)])

    def meta(self):
        return json.loads((self.out / "meta.json").read_text())

    def leftover_tempdirs(self):
        return sorted(p.name for p in self.tmp.iterdir())


class InitialMetadataTest(unittest.TestCase):
    def test_every_adapter_initial_metadata_is_json_serializable(self):
        """The real initial meta shape, per adapter and variant, after host_env as in main()."""
        args = run.parse(["--tool", "x"])
        for name in registry.ADAPTERS:
            cls = registry.load(name)[0]
            for variant in cls.variants:
                with self.subTest(adapter=name, variant=variant), tempfile.TemporaryDirectory() as work:
                    adapter = cls({}, variant, "meta-check")
                    if adapter.transport == "host":
                        adapter.root = str(Path(work) / "w")
                        adapter.host_env(Path(work))
                    init = dict(run_id="meta-check", tool=adapter.name, variant=adapter.variant)
                    meta = run.initial_meta(adapter, args, {}, init)
                    json.dumps(meta)
                    self.assertFalse(callable(meta["image"]))

    def test_git_grove_image_metadata_is_not_the_per_checkout_helper(self):
        adapter = GitGroveAdapter({}, "default", "r1")
        adapter.root = "/tmp/rwb-grove"
        self.assertIsNone(adapter.image)
        co = adapter.checkout("a", 0, "tok")
        tag = adapter.app_image(co)
        self.assertEqual(tag, f"{adapter.project(co)}-app")
        self.assertIn(f"-t {tag} .", adapter.setup(co))
        self.assertIn(f"docker run --rm --network none {tag} ", adapter.tool_versions(co))
        self.assertIn(f"docker image rm {tag}", adapter.cleanup(co))


class InitializationFailureTest(InitHarness):
    def test_shadowed_image_failure_is_recorded_and_tempdir_removed(self):
        with self.assertRaises(TypeError) as caught:
            self.main(ShadowedImage)
        self.assertIn("Object of type method is not JSON serializable", str(caught.exception))
        meta = self.meta()
        self.assertEqual((meta["valid"], meta["completed"], meta["reportable"]), (False, False, False))
        self.assertEqual(meta["errors"], ["initialization: TypeError: Object of type method is not JSON serializable"])
        self.assertTrue(meta["image"].startswith("<unserializable method: "))
        failure = meta["initialization_failure"]
        self.assertIn("TypeError", failure["traceback"])
        self.assertFalse(failure["initial_meta_written"])
        self.assertEqual(failure["steps_run"], 0)
        self.assertEqual(failure["workdir"]["action"], "removed")
        self.assertTrue(failure["workdir"]["verified_absent"])
        self.assertTrue(failure["workdir"]["path"].startswith(str(self.tmp)))
        self.assertEqual(meta["cleanup_problems"], [])
        self.assertEqual(self.leftover_tempdirs(), [])
        outcomes = json.loads((self.out / "outcomes.json").read_text())
        self.assertEqual([(o["check"], o["status"]) for o in outcomes], [("harness", "error")])
        self.assertIn("No timings", (self.out / "summary.md").read_text())
        self.assertEqual((self.out / "steps.jsonl").read_text(), "")
        self.assertEqual(list((self.out / "logs").iterdir()), [])

    def test_harness_written_tempdir_contents_are_removed_after_late_failure(self):
        with self.assertRaisesRegex(RuntimeError, "checkout model exploded"):
            self.main(WritesHomeThenFails)
        meta = self.meta()
        self.assertTrue(meta["initialization_failure"]["initial_meta_written"])
        self.assertEqual(meta["errors"], ["initialization: RuntimeError: checkout model exploded"])
        self.assertEqual(meta["initialization_failure"]["workdir"]["action"], "removed")
        self.assertEqual(self.leftover_tempdirs(), [])

    def test_interrupt_during_initialization_is_recorded_and_reraised(self):
        with self.assertRaises(KeyboardInterrupt):
            self.main(InterruptedInit)
        self.assertEqual(self.meta()["errors"], ["initialization: KeyboardInterrupt: signal 15"])
        self.assertEqual(self.leftover_tempdirs(), [])

    def test_recording_failure_never_replaces_the_original_error(self):
        real, calls = Recorder.write_json, []

        def write_json(rec, name, payload):
            calls.append(name)
            if len(calls) == 1:
                return real(rec, name, payload)  # the initial meta write raises the TypeError
            raise OSError("disk full")
        with mock.patch.object(Recorder, "write_json", write_json):
            with self.assertRaises(TypeError) as caught:
                self.main(ShadowedImage)
        self.assertEqual(calls, ["meta.json", "outcomes.json", "outcomes.json"])  # full, then minimal
        notes = "\n".join(getattr(caught.exception, "__notes__", []))
        self.assertIn("recording the initialization failure", notes)
        self.assertIn("recording the minimal initialization receipt", notes)
        self.assertIn("disk full", notes)
        self.assertIn("receipt NOT completed", notes)
        self.assertEqual(self.leftover_tempdirs(), [])

    def test_cleanup_fault_never_replaces_the_original_error(self):
        with mock.patch.object(run, "remove_init_workdir", side_effect=RuntimeError("stat broke")):
            with self.assertRaises(TypeError):
                self.main(ShadowedImage)
        self.assertEqual(self.meta()["cleanup_problems"], ["tempdir cleanup raised RuntimeError: stat broke"])
        # Not removed when its ownership could not be established.
        self.assertEqual(len(self.leftover_tempdirs()), 1)

    def test_docker_initialization_failure_runs_no_command(self):
        with mock.patch.object(transport.Runner, "__call__", side_effect=AssertionError("command ran")):
            with self.assertRaises(TypeError):
                self.main(DockerShadowedImage)
        meta = self.meta()
        self.assertEqual(meta["initialization_failure"]["workdir"]["action"], "none created")
        self.assertEqual(meta["initialization_failure"]["steps_run"], 0)
        self.assertEqual(self.leftover_tempdirs(), [])

    def test_existing_output_fails_before_any_tempdir(self):
        self.out.mkdir(parents=True)
        (self.out / "keep").write_text("old attempt\n")
        with self.assertRaises(FileExistsError):
            self.main(ShadowedImage)
        self.assertEqual(sorted(p.name for p in self.out.iterdir()), ["keep"])
        self.assertEqual(self.leftover_tempdirs(), [])


class FallbackSerializationTest(InitHarness):
    """Metadata the strict serializer rejects is still recorded, with the defect marked."""

    def assertFullReceipt(self, caught):
        meta = self.meta()
        self.assertEqual((meta["valid"], meta["completed"], meta["reportable"]), (False, False, False))
        self.assertIn("traceback", meta["initialization_failure"])
        self.assertEqual(meta["initialization_failure"]["workdir"]["action"], "removed")
        self.assertEqual(self.leftover_tempdirs(), [])
        outcomes = json.loads((self.out / "outcomes.json").read_text())
        self.assertEqual([(o["check"], o["status"]) for o in outcomes], [("harness", "error")])
        self.assertIn("No timings", (self.out / "summary.md").read_text())
        self.assertIn("receipt in", "\n".join(caught.exception.__notes__))
        return meta

    def test_circular_metadata_end_to_end(self):
        with self.assertRaisesRegex(ValueError, "Circular reference detected") as caught:
            self.main(CyclicPins)
        meta = self.assertFullReceipt(caught)
        self.assertEqual(meta["pins"], {"tool": "1", "self": "<circular reference: dict>"})
        self.assertEqual(meta["errors"], ["initialization: ValueError: Circular reference detected"])

    def test_unsupported_mapping_key_end_to_end(self):
        with self.assertRaises(TypeError) as caught:  # write_json sorts keys: str vs tuple
            self.main(TupleKeyPins)
        meta = self.assertFullReceipt(caught)
        self.assertEqual(meta["pins"], {"<invalid key tuple: ('tool', 'version')>": "1", "plain": [1, ["a", "b"]]})

    def test_wide_metadata_receipt_is_bounded_end_to_end(self):
        real_failed, real_close = run.initialization_failed, Recorder.close
        passed, closed = [], []

        def failed(*args):
            passed.append(args[-1])
            return real_failed(*args)

        def close(rec):
            real_close(rec)
            closed.append(rec)
        with mock.patch.object(run, "initialization_failed", failed), \
                mock.patch.object(Recorder, "close", close):
            with self.assertRaises(TypeError) as caught:
                self.main(WidePins)
        self.assertEqual(len(passed), 1)
        self.assertIs(caught.exception, passed[0])
        self.assertEqual(len(closed), 1)
        self.assertTrue(closed[0]._steps.closed)
        meta = self.assertFullReceipt(caught)
        wide = meta["pins"]["wide"]
        self.assertLess(len(wide), run.JSON_NODES)
        self.assertEqual(wide[-1], f"<omitted: more than {run.JSON_NODES} values>")
        self.assertEqual(wide.count(wide[-1]), 1)
        self.assertLess((self.out / "meta.json").stat().st_size, 40 * run.JSON_NODES)

    def test_minimal_trusted_receipt_when_full_copy_fails(self):
        with mock.patch.object(run, "jsonable", side_effect=RecursionError("too deep")):
            with self.assertRaises(TypeError) as caught:
                self.main(UntrustedTitle)
        meta = self.meta()
        self.assertEqual(meta["title"], "<omitted object>")
        self.assertEqual((meta["tool"], meta["transport"], meta["valid"]), ("inittoy", "host", False))
        self.assertIn("RecursionError: too deep", meta["initialization_failure"]["receipt"])
        self.assertIn("TypeError", meta["initialization_failure"]["traceback"])
        self.assertEqual(meta["initialization_failure"]["workdir"], {"action": "removed", "problems": []})
        self.assertNotIn("image", meta)
        self.assertIn("No timings", (self.out / "summary.md").read_text())
        self.assertIn("minimal receipt in", "\n".join(caught.exception.__notes__))
        self.assertEqual(self.leftover_tempdirs(), [])


class JsonableTest(unittest.TestCase):
    def test_cycles_keys_bounds_and_subclasses(self):
        class Weird(dict):
            def items(self):
                raise AssertionError("override called")
        shared = ["s"]
        loop = []
        loop.append(loop)
        value = {1: "int", "1": "str", None: 0, False: 1.5, frozenset(): "fs", "nan": float("nan"),
                 "shared": [shared, shared], "loop": loop, "weird": Weird(a=1), "obj": object()}
        out = run.jsonable(value)
        json.dumps(out, allow_nan=False)
        self.assertEqual(out["1"], "int")
        self.assertEqual(out["1 <duplicate key>"], "str")
        self.assertEqual((out["null"], out["false"]), (0, 1.5))
        self.assertEqual(out["<invalid key frozenset: frozenset()>"], "fs")
        self.assertEqual(out["nan"], "<non-finite float: nan>")
        self.assertEqual(out["shared"], [["s"], ["s"]])  # repeated, not circular
        self.assertEqual(out["loop"], ["<circular reference: list>"])
        self.assertEqual(out["weird"], {"a": 1})
        self.assertTrue(out["obj"].startswith("<unserializable object: "))

    def test_depth_and_size_are_bounded(self):
        deep = cur = []
        for _ in range(run.JSON_DEPTH + 5):
            cur.append([])
            cur = cur[0]
        text = json.dumps(run.jsonable(deep))
        self.assertIn("nested deeper than", text)
        wide = run.jsonable(list(range(run.JSON_NODES + 10)))
        self.assertEqual(wide[-1], f"<omitted: more than {run.JSON_NODES} values>")

    def test_copied_entries_stay_bounded_as_width_grows(self):
        marker = f"<omitted: more than {run.JSON_NODES} values>"
        for width in (run.JSON_NODES + 1, 60000, 200000):
            with self.subTest(width=width):
                out = run.jsonable([None] * width)
                self.assertEqual(len(out), run.JSON_NODES)  # the list itself is one value
                self.assertEqual(out[-1], marker)
                self.assertEqual(out.count(marker), 1)
                out = run.jsonable({f"k{i}": None for i in range(width)})
                self.assertEqual(len(out), run.JSON_NODES)
                self.assertEqual(out[marker], marker)
                self.assertEqual(sum(v == marker for v in out.values()), 1)
                out = run.jsonable({"a": [[None] * width, [None] * width], "b": {"c": [None] * width}})
                self.assertLessEqual(len(json.dumps(out)), 8 * run.JSON_NODES)
                self.assertLessEqual(json.dumps(out).count(marker), 4)  # one per open container (a dict's is key and value)

    def test_keys_past_the_cutoff_are_not_rendered(self):
        rendered = []

        class Key:
            def __init__(self, n):
                self.n = n

            def __repr__(self):
                rendered.append(self.n)
                return f"Key({self.n})"
        value = {i: None for i in range(run.JSON_NODES - 2)}
        value[Key("inside")] = None
        value.update({Key(i): None for i in range(50000)})
        out = run.jsonable(value)
        self.assertEqual(rendered, ["inside"])
        self.assertIn("<invalid key Key: Key(inside)>", out)
        self.assertEqual(len(out), run.JSON_NODES)
        self.assertEqual(out[f"<omitted: more than {run.JSON_NODES} values>"],
                         f"<omitted: more than {run.JSON_NODES} values>")

    def test_lazy_iteration_bypasses_overrides(self):
        class Lst(list):
            def __iter__(self):
                raise AssertionError("override called")

        class Tup(tuple):
            def __iter__(self):
                raise AssertionError("override called")
        self.assertEqual(run.jsonable([Lst([1, 2]), Tup((3,))]), [[1, 2], [3]])


class SecondaryInterruptTest(InitHarness):
    """A second interrupt during recovery never displaces the original error, in any phase."""

    @staticmethod
    def sigterm():
        signal.getsignal(signal.SIGTERM)(signal.SIGTERM, None)  # the installed callback, no OS signal

    def run_phase(self, phase, fire):
        real_remove, real_write, real_close = run.remove_init_workdir, Recorder.write_json, Recorder.close
        closed = []

        def remove(*args):
            fire()
            return real_remove(*args)

        def write_json(rec, name, payload):
            if name == "outcomes.json" and not getattr(rec, "fired", False):
                rec.fired = True
                fire()
            return real_write(rec, name, payload)

        def close(rec):
            real_close(rec)
            closed.append(rec)
            if phase == "close":
                fire()
        target, attr, replacement = dict(cleanup=(run, "remove_init_workdir", remove),
                                         record=(Recorder, "write_json", write_json),
                                         close=(Recorder, "close", close))[phase]
        with mock.patch.object(Recorder, "close", close), mock.patch.object(target, attr, replacement):
            with self.assertRaises(TypeError) as caught:
                self.main(ShadowedImage)
        self.assertEqual(len(closed), 1)
        self.assertTrue(closed[0]._steps.closed)
        self.assertEqual(signal.getsignal(signal.SIGINT), signal.default_int_handler)
        self.assertEqual(signal.getsignal(signal.SIGTERM).__name__, "on_signal")
        return "\n".join(caught.exception.__notes__)

    def test_deferred_sigterm_in_each_phase(self):
        for phase in ("cleanup", "record", "close"):
            with self.subTest(phase=phase):
                self.setUp()
                notes = self.run_phase(phase, self.sigterm)
                self.assertIn("signal 15 arrived during initialization-failure recovery", notes)
                self.assertIn("receipt in", notes)
                self.assertEqual(self.meta()["initialization_failure"]["workdir"]["action"], "removed")
                self.assertEqual(self.leftover_tempdirs(), [])

    def test_undeferred_interrupt_in_each_phase(self):
        def interrupt():
            raise KeyboardInterrupt("second interrupt")
        expected = dict(cleanup="tempdir cleanup raised KeyboardInterrupt: second interrupt",
                        record="recording the initialization failure in",
                        close="steps.jsonl raised KeyboardInterrupt: second interrupt")
        for phase in ("cleanup", "record", "close"):
            with self.subTest(phase=phase):
                self.setUp()
                notes = self.run_phase(phase, interrupt)
                self.assertIn(expected[phase], notes)
                self.assertIn("receipt in", notes)  # full (cleanup/close) or minimal (record)
                meta = self.meta()
                self.assertEqual(meta["valid"], False)
                if phase == "cleanup":
                    self.assertEqual(meta["cleanup_problems"], [expected[phase]])
                    self.assertEqual(len(self.leftover_tempdirs()), 1)  # ownership unproven: left
                else:
                    self.assertEqual(self.leftover_tempdirs(), [])


class RemoveInitWorkdirTest(unittest.TestCase):
    """remove_init_workdir may delete only the exact, unchanged directory it created."""

    def setUp(self):
        self.base = Path(tempfile.mkdtemp(prefix="rwb-init-rm-")).resolve()
        self.addCleanup(shutil.rmtree, self.base, ignore_errors=True)
        self.victim = self.base / "victim"
        self.victim.mkdir()
        (self.victim / "sentinel").write_text("not owned\n")
        self.work = self.base / "rwb-run-x"
        self.work.mkdir()
        self.ident = run.dir_identity(self.work)

    def assertVictimIntact(self):
        self.assertEqual((self.victim / "sentinel").read_text(), "not owned\n")

    def test_symlink_substitution_is_refused(self):
        self.work.rmdir()
        self.work.symlink_to(self.victim, target_is_directory=True)
        receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual(receipt["action"], "left")
        self.assertTrue(self.work.is_symlink())
        self.assertVictimIntact()

    def test_replaced_directory_is_refused(self):
        self.work.rename(self.base / "moved")  # keeps the original inode alive
        self.work.mkdir()
        (self.work / "sentinel").write_text("someone else's\n")
        receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual(receipt["action"], "left")
        self.assertIn("identity changed", receipt["problems"][0])
        self.assertTrue((self.work / "sentinel").exists())

    def test_file_in_place_of_directory_is_refused(self):
        self.work.rmdir()
        self.work.write_text("file\n")
        self.assertEqual(run.remove_init_workdir(self.work, self.ident, 0)["action"], "left")
        self.assertTrue(self.work.is_file())

    def test_left_when_commands_ran_or_identity_unknown(self):
        for ident, steps in ((self.ident, 1), (None, 0)):
            receipt = run.remove_init_workdir(self.work, ident, steps)
            self.assertEqual(receipt["action"], "left")
            self.assertTrue(self.work.is_dir())

    def test_absent_and_none(self):
        self.assertEqual(run.remove_init_workdir(None, None, 0)["action"], "none created")
        self.work.rmdir()
        receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual((receipt["action"], receipt["problems"]), ("already absent", []))

    def test_owned_contents_removed_without_following_inner_symlinks(self):
        (self.work / "home").mkdir()
        (self.work / "home" / "f").write_text("x")
        (self.work / "escape").symlink_to(self.victim, target_is_directory=True)
        receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual((receipt["action"], receipt["problems"]), ("removed", []))
        self.assertTrue(receipt["verified_absent"])
        self.assertFalse(os.path.lexists(self.work))
        self.assertVictimIntact()

    def swap_in_replacement(self):
        """Move the owned directory away and put an unrelated real directory at its path."""
        self.work.rename(self.base / "original")
        self.victim.rename(self.work)

    def test_replacement_between_check_and_open_is_left(self):
        real = run.remove_verified_dir

        def swap_then_remove(*args):
            self.swap_in_replacement()
            return real(*args)
        with mock.patch.object(run, "remove_verified_dir", swap_then_remove):
            receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual(receipt["action"], "left")
        self.assertIn("identity changed", receipt["problems"][0])
        self.assertNotIn("verified_absent", receipt)
        self.assertEqual((self.work / "sentinel").read_text(), "not owned\n")
        self.assertTrue((self.base / "original").is_dir())

    def test_replacement_between_verified_open_and_delete_is_left(self):
        (self.work / "owned").write_text("harness\n")
        real = run.remove_dir_contents

        def swap_then_remove(fd, depth):
            if depth == 0:
                self.swap_in_replacement()
            return real(fd, depth)
        with mock.patch.object(run, "remove_dir_contents", swap_then_remove):
            receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual(receipt["action"], "left")
        self.assertIn("replaced during removal", receipt["problems"][0])
        self.assertEqual((self.work / "sentinel").read_text(), "not owned\n")
        self.assertEqual(list((self.base / "original").iterdir()), [])  # owned contents, via the fd

    def test_nested_subdirectory_swapped_for_symlink_is_not_followed(self):
        (self.work / "a" / "b").mkdir(parents=True)
        real = run.remove_dir_contents

        def swap_then_remove(fd, depth):
            if depth == 0:
                shutil.rmtree(self.work / "a")
                (self.work / "a").symlink_to(self.victim, target_is_directory=True)
            return real(fd, depth)
        with mock.patch.object(run, "remove_dir_contents", swap_then_remove):
            receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual((receipt["action"], receipt["problems"]), ("removed", []))
        self.assertVictimIntact()

    def test_depth_bound_leaves_with_problem(self):
        with mock.patch.object(run, "WORKDIR_DEPTH", 1):
            (self.work / "a" / "b").mkdir(parents=True)
            receipt = run.remove_init_workdir(self.work, self.ident, 0)
        self.assertEqual(receipt["action"], "failed")
        self.assertIn("nested deeper than 1", receipt["problems"][0])
        self.assertTrue(self.work.is_dir())


if __name__ == "__main__":
    unittest.main()

"""Offline tests for the worktree-manager adapters (workz, Worktrunk, GitGrove).

No network, Docker or services: bodies are generated and syntax-checked with the host's
/bin/bash (3.2 on macOS, the shell the host transport uses), and the scenario runs on fakes
that also answer the Docker receipts these adapters add.

    python3 -m unittest bench/tests/test_worktree_adapters.py -v
"""
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest

try:
    import tomllib
except ImportError:  # Python < 3.11: TOML checks are skipped
    tomllib = None

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb.adapters import git_grove, registry, worktree_common  # noqa: E402
from rwb.adapters.base import FEATURES  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402

NAMES = ("workz", "worktrunk", "git-grove")
RUN_ID = "20261006t120000-abc123"


def make(name):
    cls, _ = registry.load(name)
    adapter = cls({}, None, RUN_ID)
    adapter.root = "/tmp/rwb-worktree-test/w"
    return adapter


class ContainerWorld(FakeWorld):
    """App module path as the containerised GitGrove app reports it (baked into /app)."""

    def __init__(self, in_container=False):
        super().__init__()
        self.in_container = in_container

    def identity(self, co):
        ident = super().identity(co)
        if ident is not None and self.in_container:
            ident["source"]["module"] = "/app/rwbapp"
        return ident


class DockerFakeTransport(FakeTransport):
    """Answers the Docker receipts (port map, compose project receipt) from the fake world."""

    def exec(self, label, phase, body, timeout=600, user=None):
        result = super().exec(label, phase, body, timeout, user)
        m = re.match(r"([a-e])-(port-map|instance-identity)$", label)
        if m and result.code == 0:
            co = m.group(1)
            ident = self.world.identity(co)
            if ident is None:
                result.code = 1
            elif m.group(2) == "port-map":
                result.stdout = json.dumps(dict(
                    pg=dict(published=ident["pg"]["port"], target=ident["pg"]["port"]),
                    redis=dict(published=ident["redis"]["port"], target=ident["redis"]["port"]),
                    evidence=f"docker inspect fake-{co}"))
            else:
                result.stdout = json.dumps(dict(project=f"rwb-{co}", containers=[f"c-{co}"], volumes=[f"v-{co}"]))
        return result


def run(adapter, configure=None):
    rec = FakeRecorder()
    world = ContainerWorld(in_container=adapter.name == "git-grove")
    tx = DockerFakeTransport(rec, adapter, world)
    # What uv / the registry print for the pinned-but-nonexistent CPython 3.13.99.
    world.outputs["d-setup-invalid"] = ("", "error: No interpreter found for Python 3.13.99 in search path\n")
    scenario = Scenario(adapter, tx, rec, repeats=2, warmups=1)
    world.scenario = scenario
    if configure:
        configure(world)
    scenario.execute()
    scenario.cleanup()
    return {o["check"]: o for o in scenario.out.as_list()}, tx


class ScenarioOnFakes(unittest.TestCase):
    def test_well_behaved_fake_passes(self):
        for name in NAMES:
            with self.subTest(name):
                out, _ = run(make(name))
                bad = {k: v["detail"] for k, v in out.items() if v["status"] not in ("pass", "observed")}
                self.assertEqual(bad, {})

    def test_wrong_worktree_source_is_detected(self):
        for name in NAMES:
            with self.subTest(name):
                out, _ = run(make(name), lambda w: w.source_of.update(b="a"))
                self.assertEqual(out["start.b"]["status"], "fail")
                self.assertIn("source token", out["start.b"]["detail"])

    def test_shared_services_fail_isolation(self):
        for name in NAMES:
            with self.subTest(name):
                out, _ = run(make(name), lambda w: w.share_with.update(b="a"))
                self.assertNotEqual(out["isolation"]["status"], "pass")


class Bodies(unittest.TestCase):
    def bodies(self, name):
        _, tx = run(make(name))
        adapter = make(name)
        extra = [("provision-" + label, body) for label, body, _ in adapter.provision()]
        extra += [("cleanup-host", adapter.cleanup_host()), ("host-resources", adapter.host_resources())]
        return [(label, body) for label, _, body in tx.calls] + extra

    def test_bash32_syntax(self):
        for name in NAMES:
            for label, body in self.bodies(name):
                with self.subTest(name=name, label=label):
                    r = subprocess.run(["/bin/bash", "-n", "-c", body], capture_output=True, text=True)
                    self.assertEqual(r.returncode, 0, r.stderr)

    def test_no_global_or_masking_operations(self):
        forbidden = ("docker system", " prune", "pkill", "killall", "--all-projects", "|| true", "docker stop ",
                     "docker kill", "sudo ", "brew ", "npm install -g", "shell install", "~/.docker")
        for name in NAMES:
            for label, body in self.bodies(name):
                with self.subTest(name=name, label=label):
                    for word in forbidden:
                        self.assertNotIn(word, body)

    def test_app_commands_rely_on_tool_working_directory(self):
        # No harness `cd <checkout>` in app commands: a tool that runs another worktree's code
        # (or the wrong directory) must surface as a source-token/module mismatch.
        for name in NAMES:
            adapter = make(name)
            co = adapter.checkout("b", 1, "tok")
            body = adapter.app(co, "identity")
            with self.subTest(name):
                self.assertNotIn(f"cd {co.path} &&", body)
                self.assertNotIn(f"cd '{co.path}' &&", body)
        self.assertIn("workz switch rwb-20261006t120000-abc123-b", make("workz").app(make("workz").checkout("b", 1), "identity"))
        wt = make("worktrunk")
        self.assertIn(" -y cmd bash -c", wt.app(wt.checkout("b", 1), "identity"))
        grove = make("git-grove")
        self.assertIn("exec -T app bash -c", grove.app(grove.checkout("b", 1), "identity"))

    def test_stop_keeps_data_cleanup_destroys_it(self):
        for name in NAMES:
            adapter = make(name)
            co = adapter.checkout("a", 0, "tok")
            with self.subTest(name):
                self.assertNotIn("volume rm", adapter.stop(co))
                self.assertNotIn("--volumes", adapter.stop(co))
                cleanup = adapter.cleanup(co)
                self.assertTrue("volume rm" in cleanup or (name == "worktrunk" and "remove" in cleanup))
                self.assertIn("still exists", cleanup)
        if tomllib is None:
            return
        hook = tomllib.loads((BENCH / "adapters/worktrunk/wt.toml").read_text())["pre-remove"]["services"]
        self.assertIn("down --volumes", hook)

    def test_checkout_paths_follow_tool_layout(self):
        root = "/private/tmp/rwb-worktree-test/w"
        self.assertEqual(make("workz").checkout("a", 0).path, f"{root}/rwbz--rwb-{RUN_ID}-a")
        self.assertEqual(make("worktrunk").checkout("a", 0).path, f"{root}/rwbt.rwb-{RUN_ID}-a")
        self.assertEqual(make("git-grove").checkout("a", 0).path, f"{root}/trees/rwb-{RUN_ID}-a")


class Ownership(unittest.TestCase):
    def test_project_names_contain_run_id(self):
        for name in NAMES:
            adapter = make(name)
            for i, c in enumerate("abcde"):
                project = adapter.project(adapter.checkout(c, i))
                with self.subTest(name=name, co=c):
                    self.assertTrue(any(p in project for p in adapter.owned_patterns()))
                    self.assertRegex(project, r"^[a-z0-9][a-z0-9_-]*$")  # valid Compose name

    def test_cleanup_project_regex_is_exact(self):
        adapter = make("workz")
        regex = re.search(r"re='([^']+)'", adapter.cleanup_host()).group(1)
        owned = [f"rwb-{RUN_ID}-a", f"rwb_{RUN_ID.replace('-', '_')}_e"]
        foreign = ["ev-nix", f"rwb-{RUN_ID}-a-extra", "rwb-20261006t120000-zzzzzz-a", "x" + owned[0]]
        for project in owned:
            self.assertRegex(project, regex)
        for project in foreign:
            self.assertNotRegex(project, regex)

    def test_private_environment(self):
        adapter = make("worktrunk")
        env = adapter.host_env("/tmp/rwb-worktree-test")
        home = "/private/tmp/rwb-worktree-test/w/home"
        self.assertEqual(env["HOME"], home)
        self.assertEqual(env["DOCKER_CONFIG"], f"{home}/.docker")
        self.assertEqual(env["GIT_CONFIG_GLOBAL"], f"{home}/.gitconfig")
        self.assertTrue(env["PATH"].startswith("/private/tmp/rwb-worktree-test/w/tools/bin:"))
        self.assertNotIn("homebrew", env["PATH"])
        self.assertNotIn("COMPOSE_PROJECT_NAME", env)
        self.assertEqual(make("git-grove").host_env("/x")["GROVE_WORKTREE_ROOT"],
                         "/private/tmp/rwb-worktree-test/w/trees")


class PinsAndConfig(unittest.TestCase):
    def test_features_declared_honestly(self):
        for name in NAMES:
            adapter = make(name)
            with self.subTest(name):
                self.assertEqual(set(adapter.features), set(FEATURES))
                self.assertEqual(adapter.features["wrong_instance_guard"], "unsupported")
                self.assertEqual(adapter.features["services"], "scripted")
                self.assertEqual(adapter.isolation_boundary, "container")
                for rel in adapter.config_files:
                    self.assertTrue((adapter.config_dir() / rel).is_file(), rel)

    def test_images_pinned_consistently(self):
        for name in ("workz", "worktrunk"):
            text = (BENCH / f"adapters/{name}/compose.yaml").read_text()
            self.assertIn(worktree_common.POSTGRES_IMAGE, text)
            self.assertIn(worktree_common.REDIS_IMAGE, text)
        env = (BENCH / "adapters/git-grove/env.example").read_text()
        self.assertIn(f"RWB_POSTGRES_IMAGE={worktree_common.POSTGRES_IMAGE}", env)
        self.assertIn(f"RWB_REDIS_IMAGE={worktree_common.REDIS_IMAGE}", env)
        for line in env.splitlines():
            self.assertRegex(line, r"^RWB_[A-Z]+_IMAGE=\S+@sha256:[0-9a-f]{64}$")
        self.assertIn("python:3.13.16-", env)
        self.assertIn(f"uv:{worktree_common.UV_VERSION}@", env)

    def test_compose_files_have_no_global_names(self):
        for name in NAMES:
            text = (BENCH / f"adapters/{name}/compose.yaml").read_text()
            with self.subTest(name):
                for word in ("container_name", "external:", "\n    name:", "network_mode: host"):
                    self.assertNotIn(word, text)
                self.assertIn("127.0.0.1:", text)
                self.assertIn("--appendfsync, always", text)

    def test_python_pin_matches_provider(self):
        for name in ("workz", "worktrunk"):
            self.assertEqual((BENCH / f"adapters/{name}/python-version").read_text().strip(),
                             worktree_common.PYTHON_VERSION)

    @unittest.skipIf(tomllib is None, "tomllib needs Python 3.11+")
    def test_tool_configs_parse(self):
        wt = tomllib.loads((BENCH / "adapters/worktrunk/wt.toml").read_text())
        self.assertEqual(set(wt["aliases"]), {"up", "stop-services", "services-status", "ports", "cmd"})
        self.assertIn("uv sync --frozen", wt["pre-start"]["deps"])
        workz = tomllib.loads((BENCH / "adapters/workz/workz.toml").read_text())
        self.assertEqual(workz["sync"], {"symlink": [], "copy": []})
        grove = json.loads((BENCH / "adapters/git-grove/grove-config.json").read_text())
        self.assertIs(grove["enabled"], True)  # alpha.1.8 ignores configs without it
        self.assertEqual(grove["providers"]["app"]["type"], "custom-shell")
        lock = json.loads((BENCH / "adapters/git-grove/tool/package-lock.json").read_text())
        entry = lock["packages"]["node_modules/@gitgrove/cli"]
        self.assertEqual(entry["version"], "0.1.0-alpha.1.8")
        self.assertEqual(entry["integrity"], git_grove.GROVE_INTEGRITY)

    def test_glue_scripts_parse(self):
        for path in ("workz/rwb-workz-env.sh", "git-grove/bin/start.sh", "git-grove/bin/stop.sh"):
            with self.subTest(path):
                r = subprocess.run(["/bin/bash", "-n", str(BENCH / "adapters" / path)], capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stderr)
        # Compose output must not pollute `grove start --json` on stdout.
        self.assertIn(">&2", (BENCH / "adapters/git-grove/bin/start.sh").read_text())


STATEFUL_DOCKER = r'''#!/usr/bin/env python3
"""Test-only docker stub over $STUB_DIR/state.json. FAIL_KIND=ps|volume|network makes that
kind's project discovery listing fail. Every call is logged. Never real Docker."""
import json, os, sys
d = os.environ["STUB_DIR"]; path = os.path.join(d, "state.json")
st = json.load(open(path)); a = sys.argv[1:]
open(os.path.join(d, "calls.log"), "a").write("docker " + " ".join(a) + "\n")
kind = {"ps": "containers", "volume": "volumes", "network": "networks"}.get(a[0])
def save(): json.dump(st, open(path, "w"))
if "--format" in a and a[0] in ("ps", "volume", "network") and "-q" not in a:
    if os.environ.get("FAIL_KIND") == a[0]:
        sys.exit("Cannot connect to the Docker daemon")
    for r in st[kind]: print(r["project"])
elif "-q" in a and kind:
    want = a[a.index("--filter") + 1].split("=", 2)[2]
    for r in st[kind]:
        if r["project"] == want: print(r["id"])
elif a[:2] == ["rm", "-f"] or a[:2] in (["network", "rm"], ["volume", "rm"]):
    k = "containers" if a[0] == "rm" else kind
    ids = a[2:]; st[k] = [r for r in st[k] if r["id"] not in ids]; save()
elif a[:2] == ["image", "ls"]:
    for i in st["images"]: print(i)
elif a[:2] == ["image", "rm"]:
    st["images"] = [i for i in st["images"] if i not in a[2:]]; save()
else:
    sys.exit("unsupported " + " ".join(a))
'''


class SharedCleanupBodies(unittest.TestCase):
    """Astra P2: no discovery or verification failure may be hidden by a later success."""

    OWNED = ("rwb-20261006t120000-abc123-a", "rwb_20261006t120000_abc123_b")
    FOREIGN = ("cit-observability-c", "rwb-20261006t120000-abc123-z", "xrwb-20261006t120000-abc123-a",
               "rwb-20261006t120000-abc124-a")

    def setUp(self):
        import os
        import tempfile
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        bindir = self.dir / "bin"
        bindir.mkdir()
        (bindir / "docker").write_text(STATEFUL_DOCKER)
        (bindir / "docker").chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bindir}:/usr/bin:/bin", STUB_DIR=str(self.dir))

    def tearDown(self):
        self.tmp.cleanup()

    def state(self, projects=OWNED + FOREIGN):
        st = {k: [dict(id=f"{k[0]}{i}", project=p) for i, p in enumerate(projects)]
              for k in ("containers", "volumes", "networks")}
        st["images"] = [f"{self.OWNED[0]}-app:latest", "postgres:17.6-alpine"]
        (self.dir / "state.json").write_text(json.dumps(st))
        (self.dir / "calls.log").write_text("")

    def run_body(self, body, fail_kind=None):
        env = dict(self.env, **({"FAIL_KIND": fail_kind} if fail_kind else {}))
        proc = subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=env, timeout=60)
        calls = (self.dir / "calls.log").read_text().splitlines()
        return proc, calls, json.loads((self.dir / "state.json").read_text())

    def test_cleanup_fails_before_any_removal_when_a_discovery_query_fails(self):
        for name in ("workz", "worktrunk", "git-grove"):
            for kind in ("ps", "volume", "network"):
                self.state()
                proc, calls, _ = self.run_body(make(name).cleanup_host(), kind)
                self.assertNotEqual(proc.returncode, 0, (name, kind))
                self.assertFalse([c for c in calls if " rm " in f" {c} "], (name, kind, calls))

    def test_cleanup_removes_exactly_the_owned_projects(self):
        for name in ("workz", "worktrunk", "git-grove"):
            self.state()
            proc, _, st = self.run_body(make(name).cleanup_host())
            self.assertEqual(proc.returncode, 0, (name, proc.stderr))
            for k in ("containers", "volumes", "networks"):
                self.assertEqual(sorted(r["project"] for r in st[k]), sorted(self.FOREIGN), (name, k))
            self.assertEqual(st["images"], ["postgres:17.6-alpine"])

    def test_verification_fails_on_query_failure_or_any_leftover(self):
        project = self.OWNED[0]
        for name in ("workz", "worktrunk", "git-grove"):
            body = "set -euo pipefail\n" + make(name).verify_project_gone(project)
            self.state(projects=())
            proc, _, _ = self.run_body(body)
            self.assertEqual(proc.returncode, 0, (name, proc.stderr))
            for kind in ("ps", "volume", "network"):
                # -q listings: make that kind fail by removing its key (stub exits on KeyError)
                self.state(projects=())
                st = json.loads((self.dir / "state.json").read_text())
                del st[{"ps": "containers", "volume": "volumes", "network": "networks"}[kind]]
                (self.dir / "state.json").write_text(json.dumps(st))
                proc, _, _ = self.run_body(body)
                self.assertNotEqual(proc.returncode, 0, (name, kind))
            for k in ("containers", "volumes", "networks"):
                self.state(projects=())
                st = json.loads((self.dir / "state.json").read_text())
                st[k] = [dict(id="left", project=project)]
                (self.dir / "state.json").write_text(json.dumps(st))
                proc, _, _ = self.run_body(body)
                self.assertNotEqual(proc.returncode, 0, (name, k))


if __name__ == "__main__":
    unittest.main()

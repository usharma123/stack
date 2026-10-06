"""Offline checks for the agent-environment adapters (isola, Berth, BranchBox).

No network, no Docker, no services: bodies are generated and syntax-checked, pins are
cross-checked against the checked-in configs, and the receipt scripts are exercised on
synthetic inputs.

python3 -m unittest bench/tests/test_agent_env_adapters.py -v
"""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb import verify  # noqa: E402
from rwb.adapters import agent_env_common as common  # noqa: E402
from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES  # noqa: E402
from rwb.adapters.berth import BerthAdapter  # noqa: E402
from rwb.adapters.branchbox import START_RECEIPT_PY, BranchboxAdapter  # noqa: E402
from rwb.adapters.isola import SHARED_PG_PORT, SHARED_REDIS_PORT, IsolaAdapter  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402

RUN = "20261006t120000-abc123"
ADAPTERS = (IsolaAdapter, BerthAdapter, BranchboxAdapter)


def make(cls):
    adapter = cls({}, None, RUN)
    if adapter.transport == "host":
        adapter.root = "/tmp/rwb-agentenv-test/w"
    return adapter


def fake_run(adapter):
    rec = FakeRecorder()
    world = FakeWorld()
    tx = FakeTransport(rec, adapter, world)
    scenario = Scenario(adapter, tx, rec, repeats=2, warmups=0)
    world.scenario = scenario
    if hasattr(adapter, "container_root"):
        # The app runs in a container and reports its module under the container root.
        world.path = lambda co: adapter.container_root(scenario.co[co])
    if adapter.isolation_boundary == "database":
        world.shared_server = True
    scenario.execute()
    scenario.cleanup()
    return {o["check"]: o for o in scenario.out.as_list()}, tx


def bodies(adapter):
    """Every body the adapter can produce, including host cleanup and per-checkout hooks."""
    out = [("versions", adapter.versions())]
    out += [(label, body) for label, body, _ in adapter.provision()]
    for i, name in enumerate("abcde"):
        co = adapter.checkout(name, i, f"tok{name}")
        for hook in ("prepare", "setup", "start", "stop", "status", "deps", "pytest", "tool_versions",
                     "instance_identity", "cleanup", "frozen_setup"):
            body = getattr(adapter, hook)(co)
            if body:
                out.append((f"{name}-{hook}", body))
        out.append((f"{name}-break", adapter.prepare(co) + "\n" + adapter.break_config(co)))
        out.append((f"{name}-enter", adapter.enter(co, "true")))
        out.append((f"{name}-app", adapter.app(co, "identity")))
        ident = dict(pg=dict(port=5432), redis=dict(port=6379))
        out.append((f"{name}-stopped", adapter.stopped_probe(co, ident)))
    for hook in ("cleanup_host", "host_resources", "service_processes", "supervisor_processes"):
        body = getattr(adapter, hook)()
        if body:
            out.append((hook, body))
    return out


class RegistryAndContract(unittest.TestCase):
    def test_registered_under_fixed_names(self):
        for key, cls in (("isola", IsolaAdapter), ("berth", BerthAdapter), ("branchbox", BranchboxAdapter)):
            self.assertIs(registry.load(key)[0], cls)

    def test_declarations(self):
        for cls in ADAPTERS:
            with self.subTest(cls.name):
                adapter = make(cls)
                self.assertEqual(set(adapter.features), set(FEATURES))
                self.assertIn(adapter.isolation_boundary, verify.BOUNDARIES)
                self.assertEqual(adapter.features["wrong_instance_guard"], "unsupported")
                for rel in adapter.config_files:
                    self.assertTrue((adapter.config_dir() / rel).is_file(), rel)

    def test_boundaries_match_the_tools(self):
        self.assertEqual(IsolaAdapter.isolation_boundary, "database")
        self.assertEqual(BerthAdapter.isolation_boundary, "container")
        self.assertEqual(BranchboxAdapter.isolation_boundary, "container")

    def test_fake_scenario_has_no_errors(self):
        for cls in ADAPTERS:
            with self.subTest(cls.name):
                out, tx = fake_run(make(cls))
                self.assertNotIn("error", {v["status"] for v in out.values()}, out)
                self.assertEqual(out["occupied_port"]["status"], "not_applicable")
                # The fake transport returns no adapter instance receipt, so isolation can only
                # fail closed here; the receipts themselves are tested below.
                self.assertIn(out["isolation"]["status"], ("pass", "fail"))
                if out["isolation"]["status"] == "fail":
                    self.assertIn("receipt", out["isolation"]["detail"])

    def test_every_body_is_valid_bash(self):
        for cls in ADAPTERS:
            adapter = make(cls)
            for label, body in bodies(adapter):
                with self.subTest(adapter=cls.name, body=label):
                    r = subprocess.run(["bash", "-n", "-c", body], capture_output=True, text=True)
                    self.assertEqual(r.returncode, 0, r.stderr)

    def test_bodies_are_scoped(self):
        forbidden = ("prune", "pkill", "killall", "--all-projects", "docker rm -f $(docker ps -aq)",
                     "docker volume rm $(docker volume ls -q)", "isola proxy stop", "down --all", "rm -rf ~", "rm -rf $HOME")
        for cls in ADAPTERS:
            adapter = make(cls)
            for label, body in bodies(adapter):
                with self.subTest(adapter=cls.name, body=label):
                    for word in forbidden:
                        self.assertNotIn(word, body)
                    self.assertNotRegex(body, r"rm -rf /(\s|$)")

    def test_no_global_installs(self):
        for cls in ADAPTERS:
            adapter = make(cls)
            for label, body in bodies(adapter):
                with self.subTest(adapter=cls.name, body=label):
                    for word in ("brew install", "npm install -g", "sudo ", "cargo install", "/usr/local/bin/berth",
                                 ".bashrc", ".zshrc", ".profile"):
                        self.assertNotIn(word, body)


class Pins(unittest.TestCase):
    def test_canonical_matches_stack_recipe(self):
        text = (BENCH / "adapters/stack/stack.toml").read_text()
        for key in ("python", "uv"):
            self.assertIn(f'{key} = "{common.CANONICAL[key]}"', text)
        for key in ("postgres", "redis"):
            self.assertIn(f'version = "{common.CANONICAL[key]}"', text)

    def test_image_digests_in_every_container_config(self):
        files = [BENCH / "adapters/berth/Dockerfile", BENCH / "adapters/berth/compose.yaml",
                 BENCH / "adapters/branchbox/devcontainer/Dockerfile",
                 BENCH / "adapters/branchbox/devcontainer/compose.yaml"]
        text = {f.name + str(f.parent.name): f.read_text() for f in files}
        for key, image in common.IMAGES.items():
            version = common.CANONICAL[key]
            self.assertIn(version, image)
            where = [k for k, t in text.items() if image in t]
            expected = 2  # each image appears in both lanes
            self.assertEqual(len(where), expected, f"{key} pinned in {where}")
        for t in text.values():
            for line in t.splitlines():
                if line.strip().startswith(("image:", "FROM")):
                    self.assertIn("@sha256:", line, line)

    def test_isola_toolchain_is_canonical(self):
        import tomllib
        tools = tomllib.loads((BENCH / "adapters/isola/toolchain.toml").read_text())["tools"]
        self.assertEqual(tools, common.CANONICAL)

    def test_fixture_python_constraint(self):
        text = (BENCH / "fixtures/app/pyproject.toml").read_text()
        self.assertIn('requires-python = ">=3.13,<3.14"', text)


class Isola(unittest.TestCase):
    def setUp(self):
        self.ad = make(IsolaAdapter)

    def test_config_renders_run_unique_names(self):
        import tomllib
        template = (BENCH / "adapters/isola/isola.toml").read_text()
        rendered = (template.replace("@PROJECT@", self.ad.project).replace("@DB_PREFIX@", self.ad.db_prefix)
                    .replace("@PG_PORT@", str(SHARED_PG_PORT)).replace("@REDIS_PORT@", str(SHARED_REDIS_PORT)))
        cfg = tomllib.loads(rendered)
        self.assertEqual(cfg["project"], "rwb-isola-20261006t120000abc123")
        self.assertFalse(cfg["proxy"]["enabled"])
        self.assertTrue(cfg["accessories"]["database"]["name"].startswith("rwb_20261006t120000abc123_"))
        self.assertIn(":25440/", cfg["accessories"]["database"]["server_url"])
        self.assertEqual(cfg["services"]["fixture"]["env"]["DATABASE_URL"], "${accessories.database.url}")
        self.assertEqual(cfg["services"]["fixture"]["env"]["REDIS_URL"], "${accessories.cache.url}")
        self.assertNotRegex(rendered, r"@[A-Z_]+@")
        # The sed in render_config uses the same four placeholders.
        body = self.ad.render_config(self.ad.checkout("a", 0))
        for token in ("@PROJECT@", "@DB_PREFIX@", "@PG_PORT@", "@REDIS_PORT@"):
            self.assertIn(token, body)

    def test_postgres_name_budget(self):
        self.assertLessEqual(len(self.ad.db_prefix) + 1 + 8, 63)

    def test_shared_ports_never_collide_with_checkout_ports(self):
        ports = {p for i, n in enumerate("abcde") for p in (self.ad.checkout(n, i).pg_port, self.ad.checkout(n, i).redis_port)}
        self.assertNotIn(SHARED_PG_PORT, ports)
        self.assertNotIn(SHARED_REDIS_PORT, ports)

    def test_linked_worktrees_from_main(self):
        a = self.ad.prepare(self.ad.checkout("a", 0, "ta"))
        b = self.ad.prepare(self.ad.checkout("b", 1, "tb"))
        self.assertIn("init -q -b a", a)
        self.assertIn("worktree add -q -b b /home/agent/rwb/b", b)
        self.assertIn("ta", a)
        self.assertIn("tb", b)

    def test_entry_is_the_generated_env_file(self):
        body = self.ad.enter(self.ad.checkout("a", 0), "true")
        self.assertIn("uv run --no-project --env-file .env.isola", body)
        self.assertNotIn("source .env.isola", body)
        self.assertNotIn(". .env.isola", body)

    def test_stop_is_down_and_destroy_only_in_cleanup(self):
        co = self.ad.checkout("a", 0)
        self.assertIn("isola down", self.ad.stop(co))
        self.assertNotIn("destroy", self.ad.stop(co))
        self.assertIn("receipt.py destroy a", self.ad.cleanup(co))
        self.assertIn("shared-servers.sh stop", self.ad.cleanup(co))
        self.assertNotIn("shared-servers.sh stop", self.ad.cleanup(self.ad.checkout("b", 1)))
        self.assertTrue(self.ad.stop_keeps_data_endpoints)

    def test_break_config_targets_isola_owned_failure(self):
        body = self.ad.break_config(self.ad.checkout("d", 3))
        self.assertIn("127.0.0.1:1/postgres", body)

    def test_scripts_parse(self):
        self.assertEqual(subprocess.run(["bash", "-n", str(BENCH / "adapters/isola/shared-servers.sh")]).returncode, 0)
        subprocess.run([sys.executable, "-m", "py_compile", str(BENCH / "adapters/isola/receipt.py")], check=True)

    def load_receipt(self):
        spec = importlib.util.spec_from_file_location("isola_receipt", BENCH / "adapters/isola/receipt.py")
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    def run_identity(self, env_text, resources, marker):
        mod = self.load_receipt()

        def fake(*argv):
            if argv[:3] == ("isola", "accessory", "ls"):
                return json.dumps(resources)
            if argv[:2] == ("isola", "ls"):
                return json.dumps([dict(worktree="a", service="fixture", status="running", pid=7)])
            if argv[0] == "redis-cli":
                return marker + "\n"
            raise AssertionError(argv)
        mod.run = fake
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / ".env.isola").write_text(env_text)
            cwd = Path.cwd()
            try:
                import os
                os.chdir(tmp)
                mod.identity("a", "rwb-isola-x")
                return 0
            except SystemExit as exit_:
                return exit_.code
            finally:
                os.chdir(cwd)

    def test_identity_receipt_cross_checks_env_and_owner(self):
        resources = [dict(worktree="a", accessory="database", kind="postgres", provisioned=True,
                          resource=dict(database="rwb_x_a")),
                     dict(worktree="a", accessory="cache", kind="redis", provisioned=True,
                          resource=dict(db=3, owner="rwb-isola-x:a"))]
        env = "DATABASE_URL=postgres://bench@127.0.0.1:25440/rwb_x_a\nREDIS_URL=redis://127.0.0.1:26390/3\n"
        self.assertEqual(self.run_identity(env, resources, "rwb-isola-x:a"), 0)
        # Env file pointing at another worktree's logical DB, or a foreign owner, fails.
        self.assertEqual(self.run_identity(env.replace("/3", "/4"), resources, "rwb-isola-x:a"), 1)
        self.assertEqual(self.run_identity(env, resources, "rwb-isola-x:b"), 1)
        self.assertEqual(self.run_identity(env.replace("rwb_x_a", "rwb_x_b"), resources, "rwb-isola-x:a"), 1)


class ComposeLanes(unittest.TestCase):
    def test_project_names_are_run_owned_and_unique(self):
        for cls in (BerthAdapter, BranchboxAdapter):
            ad = make(cls)
            names = [ad.project(ad.checkout(n, i)) for i, n in enumerate("abcde")]
            self.assertEqual(len(set(names)), 5)
            for name in names:
                self.assertIn("20261006t120000abc123", name.replace("-", ""))
            with self.assertRaises(ValueError):
                common.require_owned("berth-somebody-else", RUN)

    def test_cleanup_only_by_exact_project_label(self):
        for cls in (BerthAdapter, BranchboxAdapter):
            ad = make(cls)
            body = ad.cleanup_host()
            for line in body.splitlines():
                if "docker ps" in line or "docker network ls" in line or "docker volume ls" in line:
                    self.assertIn("label=com.docker.compose.project=", line)
                    self.assertIn("20261006t120000abc123", line.replace("-", ""))

    def test_berth_paths_follow_berth_worktree_naming(self):
        ad = make(BerthAdapter)
        a, d = ad.checkout("a", 0), ad.checkout("d", 3)
        self.assertEqual(a.path, "/tmp/rwb-agentenv-test/w/main/app-rwb20261006t120000abc123a")
        self.assertTrue(d.path.startswith("/tmp/rwb-agentenv-test/w/d-repo/app-"))
        compose = (BENCH / "adapters/berth/compose.yaml").read_text()
        self.assertIn("context: ../app-${BERTH_NAME", compose)
        self.assertIn("source: ../app-${BERTH_NAME", compose)
        self.assertEqual(ad.app_source_path(a), "/workspace")
        self.assertIn("cd /workspace && RWB_CHECKOUT=a /opt/venv/bin/python -m rwbapp identity", ad.app(a, "identity"))

    def test_berth_lifecycle_verbs(self):
        ad = make(BerthAdapter)
        a = ad.checkout("a", 0, "tok")
        self.assertIn(" up rwb", ad.setup(a))
        self.assertIn("tok", ad.setup(a))
        self.assertIn(" start rwb", ad.start(a))
        self.assertIn(" stop rwb", ad.stop(a))
        self.assertIn(" down rwb", ad.cleanup(a))
        self.assertIn("exec -T app bash -c", ad.enter(a, "true"))
        self.assertIn(f"--env-file {a.path.rsplit('-', 1)[0]}/.berth/", ad.enter(a, "true"))

    def test_branchbox_lifecycle_verbs(self):
        ad = make(BranchboxAdapter)
        a = ad.checkout("a", 0, "tok")
        self.assertIn("devcontainer build", ad.setup(a))
        self.assertIn("devcontainer up", ad.start(a))
        self.assertIn("devcontainer down", ad.stop(a))
        self.assertNotIn("--volumes", ad.stop(a))
        self.assertIn("--volumes", ad.cleanup(a))
        self.assertIn("devcontainer exec --workspace-folder", ad.enter(a, "true"))
        self.assertIn("feature start", ad.prepare(a))
        self.assertIn("--runtime container", ad.prepare(a))
        self.assertNotIn("feature exec", "\n".join(b for _, b in bodies(ad)))
        self.assertEqual(ad.app_source_path(a), f"/workspaces/{ad.feature(a)}")
        cfg = json.loads((BENCH / "adapters/branchbox/devcontainer/devcontainer.json").read_text())
        self.assertEqual(cfg["workspaceFolder"], "/workspaces/${localWorkspaceFolderBasename}")
        self.assertEqual(cfg["service"], "app")

    def test_host_env_is_private_and_scrubbed(self):
        import os
        with tempfile.TemporaryDirectory() as tmp:
            saved = {k: os.environ.get(k) for k in ("BERTH_NAME", "COMPOSE_PROJECT_NAME", "PGPORT")}
            os.environ.update(BERTH_NAME="x", COMPOSE_PROJECT_NAME="y", PGPORT="1")
            try:
                env = make(BerthAdapter).host_env(tmp)
            finally:
                for k, v in saved.items():
                    if v is None:
                        os.environ.pop(k, None)
                    else:
                        os.environ[k] = v
            for key in ("BERTH_NAME", "COMPOSE_PROJECT_NAME", "PGPORT"):
                self.assertNotIn(key, env)
            self.assertTrue(env["HOME"].startswith(tmp))
            self.assertTrue(env["GIT_CONFIG_GLOBAL"].startswith(tmp))
            self.assertTrue(env["PATH"].startswith(str(Path(tmp) / "tools" / "bin")))


class Receipts(unittest.TestCase):
    def test_snippets_compile(self):
        for src in (common.COMPOSE_RECEIPT_PY, common.CODE_DIGEST_PY, START_RECEIPT_PY):
            compile(src, "<receipt>", "exec")

    def test_code_digest_tracks_code_and_token(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "rwbapp").mkdir()
            (root / "migrations").mkdir()
            (root / "rwbapp/core.py").write_text("x = 1\n")
            (root / "rwbapp/SOURCE_TOKEN").write_text("a\n")
            (root / "migrations/0001_x.sql").write_text("select 1;\n")
            (root / "pyproject.toml").write_text("[project]\n")
            (root / "uv.lock").write_text("lock\n")

            def digest():
                return subprocess.run([sys.executable, "-I", "-c", common.CODE_DIGEST_PY, tmp],
                                      capture_output=True, text=True, check=True).stdout.strip()
            first = digest()
            (root / "rwbapp/__pycache__").mkdir()
            (root / "rwbapp/__pycache__/core.pyc").write_bytes(b"\0")
            self.assertEqual(digest(), first)
            (root / "rwbapp/SOURCE_TOKEN").write_text("b\n")
            self.assertNotEqual(digest(), first)

    def test_start_receipt_requires_expected_worktree(self):
        with tempfile.TemporaryDirectory() as tmp:
            receipt = Path(tmp) / "r.json"
            receipt.write_text(json.dumps(dict(worktree_path=f"{tmp}/w", branch_name="feature/x",
                                               runtime="container")))
            ok = subprocess.run([sys.executable, "-I", "-c", START_RECEIPT_PY, str(receipt), f"{tmp}/w"],
                                capture_output=True, text=True)
            bad = subprocess.run([sys.executable, "-I", "-c", START_RECEIPT_PY, str(receipt), f"{tmp}/other"],
                                 capture_output=True, text=True)
            self.assertEqual(ok.returncode, 0, ok.stderr)
            self.assertNotEqual(bad.returncode, 0)


if __name__ == "__main__":
    unittest.main()

"""Offline tests for the container adapters (Dev Containers, DevPod, DDEV, Lando).

No network, no Docker daemon: scenario runs use rwb.testing fakes with a container-shaped
world (identical in-container paths/ports across checkouts, distinct cluster ids and container
receipts), and compose_receipt.py runs against a fake `docker` executable.
"""
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb import verify  # noqa: E402
from rwb.adapters.base import FEATURES  # noqa: E402
from rwb.adapters.ddev import DdevAdapter  # noqa: E402
from rwb.adapters.devcontainers import IMAGES, RECEIPT_REL, DevcontainersAdapter  # noqa: E402
from rwb.adapters.devpod import DevpodAdapter  # noqa: E402
from rwb.adapters.lando import LANDO_IMAGES, LandoAdapter  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402

ADAPTERS = (DevcontainersAdapter, DevpodAdapter, DdevAdapter, LandoAdapter)
RUN = "20261006t120000-abc123"
RECEIPT = BENCH / RECEIPT_REL


def make(cls, run_id=RUN):
    adapter = cls({"tools_dir": "/opt/rwb-tools"}, None, run_id)
    adapter.root = "/tmp/rwb-test/w"
    return adapter


class ContainerWorld(FakeWorld):
    """Each checkout is its own set of containers: same in-container paths and ports."""

    def __init__(self, adapter):
        super().__init__()
        self.adapter = adapter
        self.shared_cluster = False

    def identity(self, co):
        ident = super().identity(co)
        if ident is None:
            return None
        target = self.share_with.get(co, co)
        cluster = "shared" if self.shared_cluster else target
        ident["pg"].update(data_directory="/var/lib/postgresql/data", port=5432,
                           system_identifier=f"sys-{cluster}")
        ident["redis"].update(port=6379, dir="/data", run_id=f"run-{cluster}-{self.generation[target]}")
        ident["urls"] = dict(database="postgresql://bench:bench@postgres:5432/bench", redis="redis://redis:6379/0")
        src = self.source_of.get(co, co)
        ident["source"]["module"] = f"{self.adapter.workspace}/rwbapp"
        ident["source"]["token"] = self.token(src)
        return ident


class ContainerTransport(FakeTransport):
    """Adds per-checkout container receipts for instance_identity bodies."""

    def __init__(self, recorder, adapter, world, same_receipts=False):
        super().__init__(recorder, adapter, world)
        self.same_receipts = same_receipts

    def exec(self, label, phase, body, timeout=600, user=None):
        result = super().exec(label, phase, body, timeout, user)
        m = re.match(r"([a-e])-instance-identity$", label)
        if m:
            co = "x" if self.same_receipts else self.world.share_with.get(m.group(1), m.group(1))
            result.stdout = json.dumps(dict(project=f"p-{co}", containers=[f"c-{co}"], volumes=[f"v-{co}"]))
        return result


def run_fake(adapter, configure=None, same_receipts=False):
    rec = FakeRecorder()
    world = ContainerWorld(adapter)
    tx = ContainerTransport(rec, adapter, world, same_receipts)
    scenario = Scenario(adapter, tx, rec, repeats=2, warmups=1)
    world.scenario = scenario
    if configure:
        configure(world)
    scenario.execute()
    scenario.cleanup()
    return {o["check"]: o for o in scenario.out.as_list()}, tx


class ScenarioWithContainerWorld(unittest.TestCase):
    def test_well_behaved_tools_pass_every_applicable_check(self):
        for cls in ADAPTERS:
            with self.subTest(adapter=cls.name):
                out, _ = run_fake(make(cls))
                bad = {k: v for k, v in out.items()
                       if v["status"] not in ("pass", "observed", "not_applicable", "unsupported")}
                # D's fake setup always fails without the tool's diagnostic text: no evidence.
                bad.pop("bad_config", None)
                self.assertEqual(bad, {})
                self.assertEqual(out["occupied_port"]["status"], "not_applicable")
                self.assertEqual(out["isolation"]["status"], "pass")
                self.assertIn("boundary=container", out["isolation"]["detail"])

    def test_shared_cluster_is_caught_even_with_distinct_containers(self):
        def configure(world):
            world.shared_cluster = True
        out, _ = run_fake(make(DdevAdapter), configure)
        self.assertEqual(out["isolation"]["status"], "fail")

    def test_identical_container_receipts_fail_isolation(self):
        out, _ = run_fake(make(DevcontainersAdapter), same_receipts=True)
        self.assertEqual(out["isolation"]["status"], "fail")
        self.assertIn("instance receipts", out["isolation"]["detail"])

    def test_wrong_checkout_code_is_caught_by_token(self):
        def configure(world):
            world.source_of["b"] = "a"
        out, _ = run_fake(make(LandoAdapter), configure)
        self.assertEqual(out["start.b"]["status"], "fail")
        self.assertIn("source token", out["start.b"]["detail"])

    def test_same_internal_paths_are_valid_under_container_boundary(self):
        world = ContainerWorld(make(DevpodAdapter))
        world.running = {"a": True, "b": True}
        world.generation = {"a": 1, "b": 1}
        a, b = world.identity("a"), world.identity("b")
        self.assertEqual(a["pg"]["data_directory"], b["pg"]["data_directory"])
        self.assertEqual(verify.distinct_instances(a, b, "container", ({"c": 1}, {"c": 2})), [])


class AdapterDeclarations(unittest.TestCase):
    def test_features_and_boundary(self):
        for cls in ADAPTERS:
            with self.subTest(adapter=cls.name):
                adapter = make(cls)
                self.assertEqual(set(adapter.features), set(FEATURES))
                self.assertEqual(adapter.transport, "host")
                self.assertEqual(adapter.isolation_boundary, "container")
                self.assertEqual(adapter.features["wrong_instance_guard"], "unsupported")
                self.assertEqual(adapter.src, str(BENCH))
                self.assertTrue(adapter.setup_scope)
                for rel in adapter.config_files:
                    self.assertTrue((adapter.config_dir() / rel).is_file(), rel)
                self.assertIn("uv.lock", adapter.lock_files)

    def test_readiness_claims_match_tool_behaviour(self):
        self.assertTrue(DevcontainersAdapter.start_waits_ready)
        self.assertTrue(DevpodAdapter.start_waits_ready)
        self.assertTrue(DdevAdapter.start_waits_ready)
        # Lando records failed healthchecks as warnings, so start is not readiness.
        self.assertFalse(LandoAdapter.start_waits_ready)
        self.assertEqual(LandoAdapter.features["readiness"], "scripted")

    def test_entry_resume_declarations(self):
        self.assertTrue(DdevAdapter.entry_auto_resumes)      # exec -> StartAppIfNotRunning
        self.assertFalse(DevpodAdapter.entry_auto_resumes)   # ssh -> startWait(create=false)
        self.assertIn("entry_auto_resumes", " ".join(make(DdevAdapter).core_hooks_required()))

    def test_checkout_names_are_distinct_run_owned_and_tool_valid(self):
        for cls in ADAPTERS:
            with self.subTest(adapter=cls.name):
                adapter = make(cls)
                names = [adapter.project(n) for n in "abcde"]
                self.assertEqual(len(set(names)), 5)
                for name in names:
                    self.assertTrue(re.fullmatch(r"[a-z0-9][a-z0-9-]*", name), name)
                    self.assertIn(re.sub("[^a-z0-9]", "", RUN), re.sub("[^a-z0-9]", "", name))
        lando = make(LandoAdapter)
        normalized = {re.sub(r"[-_.]", "", lando.project(n)) for n in "abcde"}
        self.assertEqual(len(normalized), 5)
        self.assertIn(RUN, make(DevpodAdapter).project("a"))

    def test_pins_are_hashes_and_digests(self):
        for cls in ADAPTERS:
            with self.subTest(adapter=cls.name):
                pins = make(cls).pins
                json.dumps(pins)
                for value in pins.get("assets", {}).values():
                    self.assertRegex(value, r"^[0-9a-f]{64}$")
                for key, ref in pins["images"].items():
                    if key != "uv" or cls is not LandoAdapter:
                        self.assertRegex(ref, r"@sha256:[0-9a-f]{64}$", key)
                self.assertTrue(pins["validity"]["core_hooks_required"])


class CheckedInConfig(unittest.TestCase):
    def read(self, name, rel):
        return (BENCH / "adapters" / name / rel).read_text()

    def test_devcontainer_json_parses_and_is_cli_only(self):
        for name in ("devcontainers", "devpod"):
            config = json.loads(self.read(name, ".devcontainer/devcontainer.json"))
            self.assertEqual(config["service"], "app")
            self.assertEqual(config["workspaceFolder"], "/workspace")
            self.assertEqual(config["postCreateCommand"], ["uv", "sync", "--frozen"])
            self.assertNotIn("features", config)

    def test_compose_files_publish_no_ports_and_pin_digests(self):
        for name, rel in (("devcontainers", ".devcontainer/compose.yaml"), ("devpod", ".devcontainer/compose.yaml"),
                          ("ddev", ".ddev/docker-compose.workload.yaml")):
            text = self.read(name, rel)
            self.assertNotRegex(text, r"(?m)^\s+ports:", f"{name} publishes ports")
            for ref in re.findall(r"image:\s*(\S+)", text):
                if "${" not in ref:
                    self.assertRegex(ref, r"@sha256:[0-9a-f]{64}$", f"{name}: {ref}")
        for name in ("devcontainers", "devpod"):
            text = self.read(name, ".devcontainer/compose.yaml")
            self.assertNotRegex(text, r"(?m)^\s+container_name:")
            self.assertIn(IMAGES["postgres"], text)
            self.assertIn(IMAGES["redis"], text)
            dockerfile = self.read(name, ".devcontainer/Dockerfile")
            self.assertIn(IMAGES["python"], dockerfile)
            self.assertIn(IMAGES["uv"], dockerfile)

    def test_ddev_uses_debian_postgres_and_unique_endpoints(self):
        text = self.read("ddev", ".ddev/docker-compose.workload.yaml")
        self.assertIn("postgres:17.6-bookworm@sha256:", text)
        self.assertIn("@ddev-${DDEV_SITENAME}-db:5432", text)
        self.assertIn("redis://ddev-${DDEV_SITENAME}-redis:6379", text)
        self.assertIn('version: "17"', self.read("ddev", ".ddev/config.yaml"))
        self.assertIn("ddev-router", self.read("ddev", "global_config.yaml"))

    def test_lando_pins_match_module(self):
        text = self.read("lando", ".lando.yml")
        for key in ("python", "postgres", "redis"):
            self.assertIn(LANDO_IMAGES[key], text)
        self.assertIn("persist: true", text)
        self.assertIn("type: volume", text)
        reqs = self.read("lando", ".lando/uv-requirements.txt")
        self.assertEqual(len(re.findall(r"--hash=sha256:[0-9a-f]{64}", reqs)), 2)
        self.assertNotIn("orchestratorBin", self.read("lando", "config.yml").split("#")[-1])

    def test_cli_package_lock_pins_release(self):
        lock = json.loads(self.read("devcontainers", "cli/package-lock.json"))
        entry = lock["packages"]["node_modules/@devcontainers/cli"]
        self.assertEqual(entry["version"], "0.89.0")
        self.assertTrue(entry["integrity"].startswith("sha512-LzaoOGKQ"))
        self.assertNotIn("hasInstallScript", entry)

    def test_break_config_edits_apply_to_committed_files(self):
        for cls in ADAPTERS:
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                adapter = make(cls)
                co = adapter.checkout("d", 3)
                co.path = tmp
                for rel in adapter.config_files:
                    dest = Path(tmp) / rel
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    dest.write_text((adapter.config_dir() / rel).read_text())
                proc = subprocess.run(["bash", "-c", adapter.break_config(co)], capture_output=True, text=True)
                self.assertEqual(proc.returncode, 0, proc.stderr)
                again = subprocess.run(["bash", "-c", adapter.break_config(co)], capture_output=True, text=True)
                self.assertNotEqual(again.returncode, 0, "edit must fail when the pinned text is absent")


FORBIDDEN = ("prune", "poweroff", "stop --all", "delete --all", "destroy --all", "stop -a", "docker kill", "pkill", "killall",
             "rm -f $(docker", "~/.ddev", "~/.lando", "$HOME/.ddev", "$HOME/.lando", "--force")


class PlannedBodies(unittest.TestCase):
    def bodies(self, cls):
        adapter = make(cls)
        _, tx = run_fake(adapter)
        extra = [("provision:" + label, body) for label, body, _ in adapter.provision()]
        extra += [("versions", adapter.versions()), ("cleanup_host", adapter.cleanup_host()),
                  ("host_resources", adapter.host_resources())]
        return adapter, [(label, body) for label, _, body in tx.calls] + extra

    def test_bodies_are_valid_bash_and_never_global(self):
        for cls in ADAPTERS:
            adapter, bodies = self.bodies(cls)
            for label, body in bodies:
                with self.subTest(adapter=cls.name, step=label):
                    proc = subprocess.run(["bash", "-n", "-c", body], capture_output=True, text=True)
                    self.assertEqual(proc.returncode, 0, proc.stderr)
                    for word in FORBIDDEN:
                        self.assertNotIn(word, body)

    def test_tool_calls_use_private_state_and_owned_names(self):
        for cls in ADAPTERS:
            adapter, bodies = self.bodies(cls)
            for label, body in bodies:
                if label.startswith(("provision:", "versions")) or "leftover-supervisors" in label:
                    continue
                with self.subTest(adapter=cls.name, step=label):
                    if re.search(rf"\b(devcontainer|devpod|ddev|lando) ", body):
                        self.assertIn(adapter.env().strip(), body)
            cleanup = adapter.cleanup_host()
            self.assertIn(f" remove {RUN} ", cleanup)
            for name in "abcde":
                self.assertIn(adapter.project(name), cleanup)

    def test_tool_specific_safety_flags(self):
        _, devpod = self.bodies(DevpodAdapter)
        ups = [b for _, b in devpod if "devpod up " in b]
        self.assertTrue(ups)
        for body in ups:
            self.assertIn("--ide none", body)
            self.assertIn("--configure-ssh=false", body)
            self.assertIn("COMPOSE_PROJECT_NAME=", body)
        for _, body in devpod:
            if "devpod ssh " in body:
                self.assertIn("--agent-forwarding=false", body)
        _, ddev = self.bodies(DdevAdapter)
        for _, body in ddev:
            if "ddev delete" in body:
                self.assertIn("--clean-containers=false", body)
                self.assertIn("--omit-snapshot", body)
            if "ddev " in body:
                self.assertIn("DDEV_XDG_CONFIG_HOME=", body)
        _, lando = self.bodies(LandoAdapter)
        for _, body in lando:
            self.assertNotIn("lando setup", body)
            if "lando " in body and "LANDO_CORE_USERCONFROOT" not in body:
                self.fail(f"lando call without private state: {body[:120]}")


FAKE_DOCKER = textwrap.dedent('''\
    #!/usr/bin/env python3
    """Fake docker CLI over a JSON state file (RWB_FAKE_STATE); logs every call."""
    import json, os, sys
    path = os.environ["RWB_FAKE_STATE"]
    state = json.load(open(path))
    args = sys.argv[1:]
    with open(path + ".log", "a") as log:
        log.write(" ".join(args) + "\\n")
    def save():
        json.dump(state, open(path, "w"))
    def label_filter(items, flt):
        key, _, value = flt[len("label="):].partition("=")
        return [i for i in items if key in i["labels"] and (not value or i["labels"][key] == value)]
    def arg(name):
        return [args[i + 1] for i, a in enumerate(args) if a == name]
    cmd = args[0]
    if cmd == "ps":
        items = state["containers"]
        for flt in arg("--filter"):
            items = label_filter(items, flt) if flt.startswith("label=") else items
        fmt = (arg("--format") or [""])[0]
        for c in items:
            if "-q" in args:
                print(c["id"])
            else:
                print(c["labels"].get("com.docker.compose.project", "") + "\\t" +
                      c["labels"].get("com.docker.compose.project.working_dir", ""))
    elif cmd == "container" and args[1] == "inspect":
        out = []
        for cid in args[2:]:
            c = next(x for x in state["containers"] if x["id"] == cid)
            out.append(dict(Id=c["id"], Name="/" + c["id"], Image="sha256:img",
                            Config=dict(Labels=c["labels"], Image="img"),
                            State=dict(Status=c["status"], StartedAt="t"),
                            Mounts=[dict(Type="volume", Name=v, Destination="/d") for v in c.get("volumes", [])],
                            NetworkSettings=dict(Networks={}, Ports={})))
        print(json.dumps(out))
    elif cmd == "volume" and args[1] == "ls":
        items = state["volumes"]
        for flt in arg("--filter"):
            items = label_filter(items, flt)
        for v in items:
            print(v["name"])
    elif cmd == "network" and args[1] == "ls":
        items = label_filter(state["networks"], arg("--filter")[0])
        for n in items:
            print(n["id"] + "\\t" + n["name"])
    elif cmd == "image" and args[1] == "ls":
        for ref in state["images"]:
            print(ref)
    elif cmd == "rm":
        state["containers"] = [c for c in state["containers"] if c["id"] != args[-1]]; save()
    elif cmd == "network" and args[1] == "rm":
        state["networks"] = [n for n in state["networks"] if n["id"] != args[2]]; save()
    elif cmd == "volume" and args[1] == "rm":
        state["volumes"] = [v for v in state["volumes"] if v["name"] != args[2]]; save()
    elif cmd == "image" and args[1] == "rm":
        state["images"] = [i for i in state["images"] if i != args[2]]; save()
    else:
        sys.exit(f"fake docker: unsupported {args}")
''')


class ComposeReceiptScript(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        tmp = Path(self.tmp.name)
        self.docker = tmp / "docker"
        self.docker.write_text(FAKE_DOCKER)
        self.docker.chmod(self.docker.stat().st_mode | stat.S_IXUSR)
        self.state = tmp / "state.json"
        mine, other = f"rwb-{RUN}-a", "cit-observability-c"
        self.state.write_text(json.dumps(dict(
            containers=[
                dict(id="c1", status="running", volumes=[f"{mine}_pgdata"],
                     labels={"com.docker.compose.project": mine, "com.docker.compose.service": "postgres",
                             "com.docker.compose.project.working_dir": "/w/a/.devcontainer"}),
                dict(id="c2", status="exited", labels={"com.docker.compose.project": mine,
                                                       "com.docker.compose.service": "app"}),
                dict(id="u1", status="running", labels={"com.docker.compose.project": other,
                                                        "com.docker.compose.service": "postgres"}),
            ],
            volumes=[dict(name=f"{mine}_pgdata", labels={"com.docker.compose.project": mine}),
                     dict(name=f"rwb-{RUN}-a-postgres", labels={}),
                     dict(name=f"{other}_data", labels={"com.docker.compose.project": other})],
            networks=[dict(id="n1", name=f"{mine}_default", labels={"com.docker.compose.project": mine}),
                      dict(id="n2", name=f"{other}_default", labels={"com.docker.compose.project": other})],
            images=[f"{mine}-app:latest", "postgres:17.6-alpine", f"{other}-api:latest"])))

    def tearDown(self):
        self.tmp.cleanup()

    def receipt(self, *args):
        env = dict(os.environ, RWB_DOCKER=str(self.docker), RWB_FAKE_STATE=str(self.state))
        return subprocess.run([sys.executable, "-I", str(RECEIPT), *args], capture_output=True, text=True, env=env)

    def test_identity_receipt(self):
        proc = self.receipt("identity", f"rwb-{RUN}-a")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        receipt = json.loads(proc.stdout)
        self.assertEqual(receipt["containers"], ["c1", "c2"])
        self.assertEqual(receipt["volumes"], [f"rwb-{RUN}-a_pgdata"])
        self.assertEqual(self.receipt("project", "dir:/w/a/.devcontainer").stdout.strip(), f"rwb-{RUN}-a")

    def test_health_and_stopped(self):
        health = self.receipt("health", f"rwb-{RUN}-a", "postgres", "app")
        self.assertEqual(health.returncode, 1)
        self.assertEqual(json.loads(health.stdout)["unhealthy"], ["app"])
        stopped = self.receipt("stopped", f"rwb-{RUN}-a", "0")
        self.assertEqual(stopped.returncode, 1, "a running container must fail the stopped probe")

    def test_refuses_unowned_projects(self):
        for args in (("remove", RUN, "cit-observability-c"), ("remove", "abc", f"rwb-{RUN}-a"),
                     ("resources", RUN, "rwb-other-run-a")):
            proc = self.receipt(*args)
            self.assertNotEqual(proc.returncode, 0, args)
        log = Path(str(self.state) + ".log")
        calls = log.read_text().splitlines() if log.exists() else []
        self.assertFalse([c for c in calls if c.split()[:1] == ["rm"] or " rm " in f" {c} "], calls)

    def test_remove_touches_only_owned_resources(self):
        listed = self.receipt("resources", RUN, f"rwb-{RUN}-a").stdout
        self.assertIn(f"rwb-{RUN}-a-postgres", listed)
        self.assertNotIn("cit-observability", listed)
        proc = self.receipt("remove", RUN, f"rwb-{RUN}-a")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        state = json.loads(self.state.read_text())
        self.assertEqual([c["id"] for c in state["containers"]], ["u1"])
        self.assertEqual([v["name"] for v in state["volumes"]], ["cit-observability-c_data"])
        self.assertEqual([n["id"] for n in state["networks"]], ["n2"])
        self.assertEqual(state["images"], ["postgres:17.6-alpine", "cit-observability-c-api:latest"])
        self.assertEqual(self.receipt("resources", RUN, f"rwb-{RUN}-a").stdout, "")
        self.assertEqual(self.receipt("running", RUN, f"rwb-{RUN}-a").stdout, "")


if __name__ == "__main__":
    unittest.main()

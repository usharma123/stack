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
            # Lando owns names with the normalized run id; the others with the raw run id.
            self.assertIn(f" remove {adapter.owner_token} ", cleanup)
            self.assertEqual(adapter.owner_token, RUN if cls is not LandoAdapter else RUN.replace("-", ""))
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
    for fail in filter(None, os.environ.get("RWB_FAKE_FAIL", "").split("|")):
        if " ".join(args).startswith(fail):
            sys.exit("Cannot connect to the Docker daemon (simulated)")
    cmd = args[0]
    if cmd == "ps":
        items = state["containers"]
        for flt in arg("--filter"):
            if flt.startswith("label="):
                items = label_filter(items, flt)
            elif flt.startswith("volume="):
                items = [c for c in items if flt[len("volume="):] in c.get("volumes", [])]
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
    elif cmd in ("network", "volume") and args[1] == "inspect":
        found = [x for x in state[cmd + "s"] if args[2] in (x.get("id"), x["name"])]
        if not found:
            sys.exit(f"Error: No such {cmd}: {args[2]}")
        print(json.dumps([dict(Name=found[0]["name"], Containers={})]))
    elif cmd == "rm":
        state["containers"] = [c for c in state["containers"] if c["id"] != args[-1]]; save()
    elif cmd == "network" and args[1] == "rm":
        state["networks"] = [n for n in state["networks"] if args[2] not in (n["id"], n["name"])]; save()
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


class LandoOwnershipReceipts(unittest.TestCase):
    """Astra P1: Lando's normalized project names must pass the receipt ownership guard."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        tmp = Path(self.tmp.name)
        self.docker = tmp / "docker"
        self.docker.write_text(FAKE_DOCKER)
        self.docker.chmod(self.docker.stat().st_mode | stat.S_IXUSR)
        self.state = tmp / "state.json"
        self.lando = make(LandoAdapter)
        mine, other = self.lando.project("a"), "cit-observability-c"
        self.mine = mine
        self.state.write_text(json.dumps(dict(
            containers=[dict(id="c1", status="running", volumes=[f"{mine}_data_database"],
                             labels={"com.docker.compose.project": mine, "com.docker.compose.service": "database"}),
                        dict(id="u1", status="running", labels={"com.docker.compose.project": other,
                                                                "com.docker.compose.service": "postgres"})],
            volumes=[dict(name=f"{mine}_data_database", labels={"com.docker.compose.project": mine}),
                     dict(name=f"{other}_data", labels={"com.docker.compose.project": other})],
            networks=[dict(id="n1", name=f"{mine}_default", labels={"com.docker.compose.project": mine}),
                      dict(id="n2", name=f"{other}_default", labels={"com.docker.compose.project": other})],
            images=[f"{mine}-appserver:latest", f"{other}-api:latest"])))

    def tearDown(self):
        self.tmp.cleanup()

    def run_receipt_lines(self, body):
        """Execute only the generated compose_receipt invocations of a body (never real Docker)."""
        lines = [l.strip() for l in body.splitlines() if "compose_receipt.py" in l]
        self.assertTrue(lines, body)
        env = dict(os.environ, RWB_DOCKER=str(self.docker), RWB_FAKE_STATE=str(self.state))
        return [subprocess.run(["bash", "-c", l], capture_output=True, text=True, env=env) for l in lines]

    def test_normalized_token_is_used_and_accepted(self):
        token = self.lando.owner_token
        self.assertNotIn("-", token)
        self.assertEqual(self.lando.project("a"), f"rwb{token}a")
        for body in (self.lando.service_processes(), self.lando.host_resources()):
            for proc in self.run_receipt_lines(body):
                self.assertEqual(proc.returncode, 0, proc.stderr)
        listed = self.run_receipt_lines(self.lando.host_resources())[0].stdout
        self.assertIn(self.mine, listed)
        self.assertNotIn("cit-observability", listed)

    def test_fallback_removal_touches_only_owned_resources(self):
        removal = [l for l in self.lando.cleanup_host().splitlines() if " remove " in l]
        for proc in self.run_receipt_lines("\n".join(removal)):
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        state = json.loads(self.state.read_text())
        self.assertEqual([c["id"] for c in state["containers"]], ["u1"])
        self.assertEqual([v["name"] for v in state["volumes"]], ["cit-observability-c_data"])
        self.assertEqual([n["id"] for n in state["networks"]], ["n2"])
        self.assertEqual(state["images"], ["cit-observability-c-api:latest"])

    def test_other_run_and_short_tokens_still_refused(self):
        env = dict(os.environ, RWB_DOCKER=str(self.docker), RWB_FAKE_STATE=str(self.state))
        for args in (("remove", self.lando.owner_token, "rwbotherrun999999a"),
                     ("remove", "abc12", "rwbabc12a")):
            proc = subprocess.run([sys.executable, "-I", str(RECEIPT), *args], capture_output=True, text=True, env=env)
            self.assertNotEqual(proc.returncode, 0, args)
        self.assertEqual(len(json.loads(self.state.read_text())["containers"]), 2)


class FullCleanupBody(unittest.TestCase):
    """Astra R3 P2: run the whole generated cleanup_host body (native teardown, owned-resource
    receipt removal, shared-infra cleanup) against the fake Docker CLI. Removal and shared
    cleanup must both be attempted, and either failing must fail the body."""

    SHARED = {LandoAdapter: [("networks", "lando_bridge_network")],
              DdevAdapter: [("networks", "ddev_default"), ("volumes", "ddev-global-cache")]}

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.docker = self.dir / "docker"
        self.docker.write_text(FAKE_DOCKER)
        self.docker.chmod(self.docker.stat().st_mode | stat.S_IXUSR)
        self.state = self.dir / "state.json"

    def tearDown(self):
        self.tmp.cleanup()

    def adapter(self, cls):
        adapter = cls({"tools_dir": str(self.dir / "tools")}, None, RUN)
        adapter.root = str(self.dir / "w")
        Path(adapter.state).mkdir(parents=True, exist_ok=True)
        # Snapshot as preflight records it: the shared infra did not exist before this run.
        Path(adapter.state, "shared-infra.json").write_text(json.dumps(
            {f"{k[:-1]}:{n}": dict(existed_before=False) for k, n in self.SHARED[cls]}))
        mine, other = adapter.project("a"), "cit-observability-c"
        world = dict(
            containers=[dict(id="owned", status="running", volumes=[f"{mine}_data"],
                             labels={"com.docker.compose.project": mine, "com.docker.compose.service": "postgres"}),
                        dict(id="foreign", status="running", volumes=[f"{other}_data"],
                             labels={"com.docker.compose.project": other, "com.docker.compose.service": "postgres"})],
            volumes=[dict(name=f"{mine}_data", labels={"com.docker.compose.project": mine}),
                     dict(name=f"{other}_data", labels={"com.docker.compose.project": other})],
            networks=[dict(id="n1", name=f"{mine}_default", labels={"com.docker.compose.project": mine}),
                      dict(id="n2", name=f"{other}_default", labels={"com.docker.compose.project": other})],
            images=[f"{mine}-app:latest", f"{other}-api:latest"])
        for kind, name in self.SHARED[cls]:
            world[kind].append(dict(id=f"shared-{name}", name=name, labels={}))
        self.state.write_text(json.dumps(world))
        return adapter

    def run_cleanup(self, adapter, fail="", docker=None):
        env = dict(os.environ, RWB_DOCKER=str(docker or self.docker), RWB_FAKE_STATE=str(self.state),
                   RWB_FAKE_FAIL=fail)
        return subprocess.run(["/bin/bash", "-c", adapter.cleanup_host()], cwd=self.dir,
                              capture_output=True, text=True, env=env)

    def left(self):
        state = json.loads(self.state.read_text())
        return dict(containers=[c["id"] for c in state["containers"]],
                    volumes=[v["name"] for v in state["volumes"]],
                    networks=[n["name"] for n in state["networks"]], images=state["images"])

    def assert_foreign_kept(self):
        left = self.left()
        self.assertIn("foreign", left["containers"])
        self.assertIn("cit-observability-c_data", left["volumes"])
        self.assertIn("cit-observability-c_default", left["networks"])
        self.assertIn("cit-observability-c-api:latest", left["images"])

    def shared_names(self, cls):
        return [n for _, n in self.SHARED[cls]]

    def test_both_succeed(self):
        for cls in self.SHARED:
            with self.subTest(adapter=cls.name):
                proc = self.run_cleanup(self.adapter(cls))
                self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
                self.assertEqual(self.left(), dict(
                    containers=["foreign"], volumes=["cit-observability-c_data"],
                    networks=["cit-observability-c_default"], images=["cit-observability-c-api:latest"]))

    def test_owned_query_failure_fails_cleanup_but_shared_cleanup_runs(self):
        for cls in self.SHARED:
            with self.subTest(adapter=cls.name):
                adapter = self.adapter(cls)
                proc = self.run_cleanup(adapter, fail="ps -a -q --no-trunc")
                self.assertNotEqual(proc.returncode, 0, proc.stdout + proc.stderr)
                self.assertIn("owned-resource removal failed", proc.stderr)
                left = self.left()
                self.assertIn("owned", left["containers"])
                for name in self.shared_names(cls):
                    self.assertNotIn(name, left["networks"] + left["volumes"], "shared cleanup must still run")
                self.assert_foreign_kept()

    def test_refused_removal_leaves_owned_running_and_fails_cleanup(self):
        refusing = self.dir / "docker-refuse"
        refusing.write_text(FAKE_DOCKER.replace(
            'cmd = args[0]\n', 'cmd = args[0]\nif cmd == "rm":\n    sys.exit("simulated removal refusal")\n', 1))
        refusing.chmod(refusing.stat().st_mode | stat.S_IXUSR)
        for cls in self.SHARED:
            with self.subTest(adapter=cls.name):
                proc = self.run_cleanup(self.adapter(cls), docker=refusing)
                self.assertNotEqual(proc.returncode, 0, proc.stdout + proc.stderr)
                self.assertIn("remaining container owned", proc.stdout)
                self.assertEqual(self.left()["containers"], ["owned", "foreign"])
                self.assert_foreign_kept()

    def test_shared_cleanup_failure_fails_cleanup_after_removal(self):
        for cls in self.SHARED:
            with self.subTest(adapter=cls.name):
                # `network rm` of the run-created shared network fails (owned networks are
                # removed by id with tolerated exit codes, so only shared cleanup sees this).
                proc = self.run_cleanup(self.adapter(cls), fail="network rm lando_bridge_network|network rm ddev_default")
                self.assertNotEqual(proc.returncode, 0, proc.stdout + proc.stderr)
                self.assertIn("shared-infra cleanup failed", proc.stderr)
                self.assertNotIn("owned-resource removal failed", proc.stderr)
                left = self.left()
                self.assertEqual(left["containers"], ["foreign"])
                self.assertIn("lando_bridge_network" if cls is LandoAdapter else "ddev_default", left["networks"])
                self.assert_foreign_kept()

    def test_native_teardown_failure_stays_best_effort(self):
        for cls in self.SHARED:
            with self.subTest(adapter=cls.name):
                adapter = self.adapter(cls)
                marker = ".lando.local.yml" if cls is LandoAdapter else ".ddev/config.local.yaml"
                path = Path(adapter.workdir(), "a", marker)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("")
                # The private tool dir has no lando/ddev binary, so native teardown fails.
                proc = self.run_cleanup(adapter)
                self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
                self.assertIn("native teardown of a failed", proc.stderr)
                self.assertEqual(self.left()["containers"], ["foreign"])
                self.assert_foreign_kept()

    def test_devcontainers_without_shared_infra_fails_on_removal_failure(self):
        adapter = make(DevcontainersAdapter)
        adapter.root = str(self.dir / "w")
        self.state.write_text(json.dumps(dict(containers=[], volumes=[], networks=[], images=[])))
        self.assertEqual(self.run_cleanup(adapter).returncode, 0)
        proc = self.run_cleanup(adapter, fail="volume ls")
        self.assertNotEqual(proc.returncode, 0, proc.stdout + proc.stderr)


if __name__ == "__main__":
    unittest.main()

"""Offline checks for the Tilt, Organist and Vagrant adapters: no network, no Docker, no Nix.

python3 -m unittest bench/tests/test_additional_adapters.py -v
"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb import verify  # noqa: E402
from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES, Checkout  # noqa: E402
from rwb.adapters.organist import NIXPKGS_REV, ORGANIST_REV, OrganistAdapter  # noqa: E402
from rwb.adapters.tilt import IMAGES, TiltAdapter, remove_owned  # noqa: E402
from rwb.adapters.vagrant import VagrantAdapter  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402

TOOLS = {"tilt": TiltAdapter, "organist": OrganistAdapter, "vagrant": VagrantAdapter}
RUN = "20261006t000000-abc123"


def make(cls, run_id=RUN):
    adapter = cls({}, None, run_id)
    if adapter.transport == "host":
        adapter.root = "/tmp/rwb-test-work/w"
    return adapter


def run_fake(adapter):
    rec = FakeRecorder()
    world = FakeWorld()
    # Container-boundary adapters must print distinct per-checkout instance receipts.
    for co in "abcde":
        world.outputs[f"{co}-instance-identity"] = (json.dumps({"project": f"rwb-x-{co}", "postgres": co}), "")
    tx = FakeTransport(rec, adapter, world)
    scenario = Scenario(adapter, tx, rec, repeats=2, warmups=1)
    world.scenario = scenario
    scenario.execute()
    scenario.cleanup()
    return {o["check"]: o for o in scenario.out.as_list()}, tx


def bash_n(body, bash="bash"):
    return subprocess.run([bash, "-n", "-c", body], capture_output=True, text=True)


class Registration(unittest.TestCase):
    def test_registry_resolves_fixed_class_names(self):
        for name, cls in TOOLS.items():
            loaded, variant = registry.load(name)
            self.assertIs(loaded, cls)
            self.assertIsNone(variant)

    def test_declarations(self):
        for name, cls in TOOLS.items():
            with self.subTest(name):
                ad = make(cls)
                self.assertEqual(set(ad.features), set(FEATURES))
                self.assertIn(ad.isolation_boundary, verify.BOUNDARIES)
                for rel in ad.config_files:
                    self.assertTrue((ad.config_dir() / rel).is_file(), rel)
                for rel in ad.shared_files:
                    self.assertTrue((BENCH / "adapters" / "_shared" / rel).is_file(), rel)
                self.assertTrue(ad.setup_scope and ad.cache_note)
                self.assertEqual(ad.features["wrong_instance_guard"], "unsupported")

    def test_transports_and_boundaries(self):
        self.assertEqual((TiltAdapter.transport, TiltAdapter.isolation_boundary), ("host", "container"))
        self.assertEqual((VagrantAdapter.transport, VagrantAdapter.isolation_boundary), ("host", "container"))
        self.assertEqual((OrganistAdapter.transport, OrganistAdapter.image), ("docker", "ev-nix"))
        self.assertEqual(OrganistAdapter.isolation_boundary, "service-instance")

    def test_organist_modes_separate_native_from_scripted(self):
        f = OrganistAdapter.features
        self.assertEqual([f[k] for k in ("lockfile", "frozen_setup", "services")], ["native"] * 3)
        for key in ("detached_services", "readiness", "per_checkout_ports", "per_checkout_data",
                    "stop_confirmation", "structured_status"):
            self.assertEqual(f[key], "scripted", key)


class FakeScenario(unittest.TestCase):
    def test_identical_instance_receipts_fail_isolation(self):
        ad = make(TiltAdapter)
        rec, world = FakeRecorder(), FakeWorld()
        tx = FakeTransport(rec, ad, world)
        for co in "ab":
            world.outputs[f"{co}-instance-identity"] = ('{"postgres": "same"}', "")
        scenario = Scenario(ad, tx, rec, repeats=1, warmups=0)
        world.scenario = scenario
        scenario.execute()
        scenario.cleanup()
        self.assertEqual(scenario.out.status("isolation"), "fail")

    EXPECT = {
        "tilt": {"start.a": "pass", "isolation": "pass", "stop.a": "pass", "persist.pg": "pass",
                 "bad_config": "pass", "lock.created": "unsupported", "lock.frozen_copy": "unsupported",
                 "occupied_port": "not_applicable", "status": "observed"},
        "vagrant": {"start.a": "pass", "isolation": "pass", "stop.a": "pass", "persist.pg": "pass",
                    "bad_config": "pass", "lock.created": "unsupported", "lock.frozen_copy": "unsupported",
                    "occupied_port": "not_applicable", "status": "observed"},
        "organist": {"start.a": "pass", "isolation": "pass", "stop.a": "pass", "persist.pg": "pass",
                     "bad_config": "pass", "lock.created": "pass", "lock.frozen_copy": "pass",
                     "occupied_port": "pass", "status": "observed"},
    }

    def test_full_fake_scenario(self):
        for name, cls in TOOLS.items():
            with self.subTest(name):
                out, tx = run_fake(make(cls))
                self.assertNotIn("error", {v["status"] for v in out.values()})
                for check, status in self.EXPECT[name].items():
                    self.assertEqual(out[check]["status"], status, (check, out[check]))
                labels = [label for label, _, _ in tx.calls]
                if name != "organist":  # not applicable: nothing is started for E
                    self.assertFalse([x for x in labels if x.startswith("e-")])

    def test_every_body_parses(self):
        for name, cls in TOOLS.items():
            ad = make(cls)
            _, tx = run_fake(ad)
            bodies = [(label, body) for label, _, body in tx.calls]
            bodies += [(label, body) for label, body, _ in ad.provision()]
            if ad.transport == "host":
                bodies += [("host-cleanup", ad.cleanup_host()), ("host-leftovers", ad.host_resources())]
            # Host bodies run under macOS /bin/bash (3.2); container bodies under the image's bash.
            shell = "/bin/bash" if ad.transport == "host" and Path("/bin/bash").exists() else "bash"
            for label, body in bodies:
                with self.subTest(tool=name, step=label):
                    r = bash_n(body, shell)
                    self.assertEqual(r.returncode, 0, r.stderr)

    def test_host_bodies_only_touch_this_run(self):
        for cls in (TiltAdapter, VagrantAdapter):
            ad = make(cls)
            _, tx = run_fake(ad)
            for label, _, body in tx.calls + [("host-cleanup", None, ad.cleanup_host())]:
                for forbidden in ("prune", "pkill", "killall", "docker rm -f $(docker ps", "--all-projects"):
                    self.assertNotIn(forbidden, body, label)
                for m in re.finditer(r"label=rwb\.run=(\S+)", body):
                    self.assertEqual(m.group(1).strip("'\""), RUN, label)
            listing = ad.host_resources()
            self.assertIn(f"rwb-{RUN}-", listing)
            self.assertEqual(listing.count(f"label=rwb.run={RUN}") + listing.count(f"\"rwb-{RUN}-\""),
                             listing.count("docker "), "every docker query is filtered by this run")


class Pins(unittest.TestCase):
    def test_container_images_are_digest_pinned_and_consistent(self):
        lock = json.loads((BENCH / "adapters/vagrant/images.lock.json").read_text())
        self.assertEqual(lock, IMAGES)
        compose = (BENCH / "adapters/tilt/compose.yaml").read_text()
        tilt_docker = (BENCH / "adapters/tilt/Dockerfile").read_text()
        for key in ("postgres", "redis"):
            self.assertIn(f"image: {IMAGES[key]}", compose)
        for key in ("python", "uv"):
            self.assertIn(IMAGES[key], tilt_docker)
        for ref in IMAGES.values():
            self.assertRegex(ref, r"@sha256:[0-9a-f]{64}$")

    def test_organist_flake_pins(self):
        flake = (BENCH / "adapters/organist/flake.nix").read_text()
        self.assertIn(f"github:nickel-lang/organist/{ORGANIST_REV}", flake)
        self.assertIn(f"github:NixOS/nixpkgs/{NIXPKGS_REV}", flake)
        project = (BENCH / "adapters/organist/project.ncl").read_text()
        for pkg in ("python313", "uv", "postgresql_17", "redis"):
            self.assertIn(f'"nixpkgs#{pkg}"', project)
        self.assertIn("organist.services", project)

    def test_redis_durability_policy_matches_shared_glue(self):
        files = [BENCH / "adapters/tilt/compose.yaml", BENCH / "adapters/vagrant/Vagrantfile",
                 BENCH / "adapters/organist/organist-services.sh"]
        for path in files:
            text = path.read_text()
            self.assertRegex(text, r"appendonly.{0,6}yes", path.name)
            self.assertRegex(text, r"appendfsync.{0,6}always", path.name)


class BreakConfig(unittest.TestCase):
    def test_breakers_request_nonexistent_versions(self):
        pattern = re.compile(TiltAdapter.bad_config_pattern)
        for cls, rel in ((TiltAdapter, "compose.yaml"), (VagrantAdapter, "images.lock.json")):
            with self.subTest(cls.name), tempfile.TemporaryDirectory() as tmp:
                shutil.copy(BENCH / "adapters" / cls.name / rel, Path(tmp) / rel)
                co = Checkout("d", tmp, 1, 2, "t")
                r = subprocess.run(["bash", "-c", "set -e\n" + make(cls).break_config(co)],
                                   capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stderr)
                text = (Path(tmp) / rel).read_text()
                self.assertTrue(pattern.search(text))
                self.assertNotIn("postgres:17.6-alpine", text)
        body = make(OrganistAdapter).break_config(Checkout("d", "/w/d", 1, 2))
        self.assertIn("s/nixpkgs#postgresql_17/nixpkgs#postgresql_99/g", body)
        self.assertTrue(re.search(OrganistAdapter.bad_config_pattern, "Missing input \"nixpkgs#postgresql_99\""))


class LocalEnv(unittest.TestCase):
    def test_per_checkout_names_are_run_unique(self):
        for cls, key in ((TiltAdapter, "RWB_PROJECT"), (VagrantAdapter, "RWB_INSTANCE")):
            with self.subTest(cls.name), tempfile.TemporaryDirectory() as tmp:
                ad = make(cls)
                ad.root = f"{tmp}/w"
                co = ad.checkout("b", 1, "t")
                os.makedirs(co.path)
                r = subprocess.run(["bash", "-c", "set -e\n" + "\n".join(ad.local_env(co)) +
                                    f"\n. {co.path}/{cls.name}.local.env && echo ${key}:$RWB_SRC:$RWB_RUN"],
                                   capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(r.stdout.strip(), f"rwb-{RUN}-b:{co.path}:{RUN}")
                self.assertTrue(Path(ad.uv_cache()).is_dir())


class HostCleanup(unittest.TestCase):
    def fake_docker(self, tmp, listing):
        """A `docker` stub that answers the listing queries and logs removals."""
        stub = Path(tmp) / "docker"
        stub.write_text("#!/bin/bash\n"
                        f"log={tmp}/calls.log\n"
                        'case "$1 $2" in\n'
                        "  'ps -a') printf '%s' \"$LIST_CONTAINERS\" ;;\n"
                        "  'volume ls') printf '%s' \"$LIST_VOLUMES\" ;;\n"
                        "  'network ls') printf '%s' \"$LIST_NETWORKS\" ;;\n"
                        "  'images --format') printf '%s' \"$LIST_IMAGES\" ;;\n"
                        '  *) echo "$*" >> "$log" ;;\n'
                        "esac\n")
        stub.chmod(0o755)
        env = dict(os.environ, PATH=f"{tmp}:{os.environ['PATH']}", **listing)
        return env

    def test_removes_only_listed_resources(self):
        ad = make(TiltAdapter)
        p = f"rwb-{RUN}-a"
        listing = dict(LIST_CONTAINERS=f"container {p}-app-1\n", LIST_VOLUMES=f"{p}_pgdata\nother_vol\n",
                       LIST_NETWORKS=f"{p}_default\nbridge\n",
                       LIST_IMAGES=f"{p}-app:tilt-1 sha256:aa\n{p}-app:<none> sha256:bb\npython:3 sha256:cc\n")
        with tempfile.TemporaryDirectory() as tmp:
            r = subprocess.run(["bash", "-c", ad.cleanup_host()], cwd=tmp, env=self.fake_docker(tmp, listing),
                               capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            calls = (Path(tmp) / "calls.log").read_text().splitlines()
        self.assertEqual(calls, [f"rm -f -v {p}-app-1", f"network rm {p}_default", f"volume rm {p}_pgdata",
                                 f"image rm {p}-app:tilt-1 sha256:bb"])

    def test_nothing_owned_runs_no_removal(self):
        with tempfile.TemporaryDirectory() as tmp:
            # Vagrant filters volumes by label server-side; the stub returns what the daemon would.
            empty = dict(LIST_CONTAINERS="", LIST_VOLUMES="", LIST_NETWORKS="", LIST_IMAGES="python:3 sha256:cc\n")
            r = subprocess.run(["bash", "-c", remove_owned(make(VagrantAdapter).host_resources())], cwd=tmp,
                               env=self.fake_docker(tmp, empty), capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertFalse((Path(tmp) / "calls.log").exists())


class OrganistHolder(unittest.TestCase):
    def checkout(self, tmp):
        for rel in ("organist-holder.sh",):
            shutil.copy(BENCH / "adapters/organist" / rel, Path(tmp) / rel)
        shutil.copy(BENCH / "adapters/_shared/rwb-env.sh", Path(tmp) / "rwb-env.sh")
        (Path(tmp) / "bench.local.env").write_text("PGPORT=1\nREDIS_PORT=2\nRWB_INSTANCE=rwb-a\n")

    def test_stop_never_signals_a_pid_that_is_not_its_honcho(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.checkout(tmp)
            other = subprocess.Popen(["sleep", "30"])
            try:
                os.makedirs(f"{tmp}/.rwb-state", exist_ok=True)
                Path(f"{tmp}/.rwb-state/honcho.pid").write_text(f"{other.pid}\n")
                r = subprocess.run(["bash", "organist-holder.sh", "stop"], cwd=tmp, capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("not running", r.stdout)
                self.assertIsNone(other.poll())
                s = subprocess.run(["bash", "organist-holder.sh", "status"], cwd=tmp, capture_output=True, text=True)
                self.assertEqual(s.returncode, 1)
                self.assertEqual(json.loads(s.stdout)["honcho"], "stopped")
            finally:
                other.kill()
                other.wait()

    def test_scripts_parse(self):
        for path in [*(BENCH / "adapters/organist").glob("*.sh"), *(BENCH / "adapters/vagrant").glob("*.sh")]:
            r = subprocess.run(["bash", "-n", str(path)], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, (path.name, r.stderr))


COUNTING_DOCKER = r'''#!/bin/bash
# Test-only docker stub. Call N (1-based) behaves per $STUB_DIR/plan, one line per call:
#   ok | fail | failout | empty | busy   (default ok). Every call is logged. Never real Docker.
dir="$STUB_DIR"; n=$(( $(cat "$dir/n" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$dir/n"
echo "docker $*" >> "$dir/calls.log"
mode=$(sed -n "${n}p" "$dir/plan" 2>/dev/null); mode=${mode:-ok}
case "$mode" in
  fail) echo "Cannot connect to the Docker daemon" >&2; exit 1;;
  failout) echo "partial-$n"; exit 1;;
  empty) exit 0;;
  busy) echo "ctr-busy"; exit 0;;
esac
case "$1 $2" in
  "volume ls") echo "vol-b-$n"; echo "vol-a-$n";;
  "volume inspect") echo "${@: -1}";;
  "ps -aq") ;;   # stopped probe: nothing left
  *) echo "id-$n-$(printf '%s' "$*" | cksum | cut -d' ' -f1)";;
esac
'''


class StubDocker(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        bindir = self.dir / "bin"
        bindir.mkdir()
        for name, text in (("docker", COUNTING_DOCKER), ("sleep", "#!/bin/sh\nexit 0\n")):
            (bindir / name).write_text(text)
            (bindir / name).chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bindir}:/usr/bin:/bin", STUB_DIR=str(self.dir))

    def tearDown(self):
        self.tmp.cleanup()

    def run_body(self, body, plan=(), cwd=None, extra_env=None):
        (self.dir / "plan").write_text("\n".join(plan) + "\n")
        (self.dir / "n").write_text("0")
        (self.dir / "calls.log").write_text("")
        env = dict(self.env, **(extra_env or {}))
        proc = subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=env, cwd=cwd, timeout=60)
        return proc, (self.dir / "calls.log").read_text().splitlines()


class TiltReceipts(StubDocker):
    """Astra P1/P2: Tilt identity and stopped probe must fail closed on Docker failures."""

    def setUp(self):
        super().setUp()
        self.tilt = make(TiltAdapter)
        self.co = {n: self.tilt.checkout(n, i, "t") for i, n in enumerate("ab")}

    def test_identity_fails_on_any_failed_or_empty_query(self):
        body = self.tilt.instance_identity(self.co["a"])
        for index in range(4):
            for mode in ("fail", "failout", "empty"):
                plan = ["ok"] * 4
                plan[index] = mode
                proc, _ = self.run_body(body, plan)
                self.assertNotEqual(proc.returncode, 0, (index, mode))
                self.assertNotIn("{", proc.stdout, (index, mode))

    def test_identity_valid_receipt_schema_and_exact_filters(self):
        receipts = {}
        for name in "ab":
            proc, calls = self.run_body(self.tilt.instance_identity(self.co[name]))
            self.assertEqual(proc.returncode, 0, proc.stderr)
            receipts[name] = json.loads(proc.stdout)
            self.assertEqual(set(receipts[name]), {"project", "postgres", "redis", "volumes", "network"})
            self.assertTrue(all(isinstance(v, str) and v for v in receipts[name].values()))
            label = f"label=com.docker.compose.project={self.tilt.project(self.co[name])}"
            self.assertEqual(len(calls), 4)
            self.assertTrue(all(label in c for c in calls), calls)
            self.assertFalse([c for c in calls if re.search(r" (rm|create|stop|kill|down)\b", c)], calls)
        self.assertNotEqual(receipts["a"], receipts["b"])
        self.assertTrue(receipts["a"]["volumes"].startswith("vol-a-"))  # sorted, deterministic

    def test_stopped_probe_never_reads_a_failed_query_as_absence(self):
        body = self.tilt.stopped_probe(self.co["a"], None)
        label = f"label=com.docker.compose.project={self.tilt.project(self.co['a'])}"
        for plan, ok in ((["fail"], False), (["failout"], False), (["ok"], True),
                         (["busy"] * 150, False), (["busy", "busy", "ok"], True)):
            proc, calls = self.run_body(body, plan)
            self.assertEqual(proc.returncode == 0, ok, (plan[:3], proc.stderr))
            self.assertTrue(calls and all(label in c and " ps -aq " in f" {c} " for c in calls), calls)


class VagrantIdentity(StubDocker):
    """Astra P1: vagrant-resources.sh identity fails on any failed or empty Docker result."""

    SCRIPT = BENCH / "adapters" / "vagrant" / "vagrant-resources.sh"

    def identity(self, plan=(), instance="rwb-20261006t000000-abc123-a"):
        return self.run_body(f"bash {self.SCRIPT} identity", plan,
                             extra_env=dict(RWB_INSTANCE=instance, RWB_RUN=RUN))

    def test_each_query_failure_or_empty_result_fails(self):
        for index in range(5):  # pg, redis, network, volume 1 (first!), volume 2
            for mode in ("fail", "failout", "empty"):
                plan = ["ok"] * 5
                plan[index] = mode
                proc, _ = self.identity(plan)
                self.assertNotEqual(proc.returncode, 0, (index, mode))
                self.assertNotIn("{", proc.stdout, (index, mode))

    def test_valid_receipt_schema_names_and_no_mutation(self):
        a, calls = self.identity()
        self.assertEqual(a.returncode, 0, a.stderr)
        ra = json.loads(a.stdout)
        self.assertEqual(set(ra), {"instance", "pg", "redis", "network", "volumes"})
        inst = "rwb-20261006t000000-abc123-a"
        self.assertEqual(ra["volumes"], f"{inst}-pgdata {inst}-redisdata ")
        for name in (f"{inst}-pg", f"{inst}-redis", f"{inst}-net", f"{inst}-pgdata", f"{inst}-redisdata"):
            self.assertTrue(any(c.endswith(" " + name) for c in calls), name)
        self.assertFalse([c for c in calls if re.search(r" (rm|create)\b", c)], calls)
        b, _ = self.identity(instance="rwb-20261006t000000-abc123-b")
        self.assertNotEqual(ra, json.loads(b.stdout))


if __name__ == "__main__":
    unittest.main()

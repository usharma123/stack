"""Offline checks for the native-extra adapters: Process Compose, services-flake, pkgx/dev,
dnvr and GNU Guix. No network, no Docker, no services.

python3 -m unittest discover -s bench/tests -v
"""
from pathlib import Path
import re
import shutil
import subprocess
import sys
import unittest

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(BENCH / "fixtures" / "app"))

from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES  # noqa: E402
from rwb.adapters import dnvr, guix, pkgx, process_compose, services_flake  # noqa: E402
from rwb.scenario import Scenario  # noqa: E402
from rwb.testing import FakeRecorder, FakeTransport, FakeWorld  # noqa: E402

NAMES = ("process-compose", "services-flake", "pkgx", "dnvr", "guix")
CONFIG = BENCH / "adapters"
SCRIPTS = [CONFIG / "process-compose/rwb-pc.sh", CONFIG / "services-flake/rwb-sf.sh",
           CONFIG / "dnvr/rwb-dnvr.sh", CONFIG / "guix/provision.sh"]
HEX64 = re.compile(r"^[0-9a-f]{64}$")


def run_fake(adapter):
    rec = FakeRecorder()
    world = FakeWorld()
    tx = FakeTransport(rec, adapter, world)
    scenario = Scenario(adapter, tx, rec, repeats=2, warmups=1)
    world.scenario = scenario
    scenario.execute()
    scenario.cleanup()
    return {o["check"]: o for o in scenario.out.as_list()}, tx, scenario


def bash_syntax(text):
    proc = subprocess.run(["bash", "-n"], input=text.encode(), capture_output=True)
    return proc.returncode, proc.stderr.decode()


class RegistrationTest(unittest.TestCase):
    def test_registered_and_importable(self):
        found = registry.available()
        for name in NAMES:
            with self.subTest(name=name):
                self.assertIn(name, found)
                cls = found[name]
                self.assertEqual(cls.name, name)
                self.assertEqual(set(cls.features), set(FEATURES))
                for rel in cls.config_files:
                    self.assertTrue((CONFIG / name / rel).is_file(), rel)
                for rel in cls.shared_files:
                    self.assertTrue((CONFIG / "_shared" / rel).is_file(), rel)

    def test_no_stack_only_guard_claimed(self):
        for name in NAMES:
            self.assertEqual(registry.load(name)[0].features["wrong_instance_guard"], "unsupported", name)


class FakeScenarioTest(unittest.TestCase):
    def test_every_body_is_valid_bash_and_scoped(self):
        for name in NAMES:
            cls = registry.load(name)[0]
            for variant in cls.variants:
                with self.subTest(name=name, variant=variant):
                    adapter = cls({}, variant, "native-extra-test")
                    out, tx, _ = run_fake(adapter)
                    self.assertNotIn("error", {v["status"] for v in out.values()})
                    # Core-owned process listings legitimately end in `|| true` (empty = clean).
                    listings = {adapter.service_processes(), adapter.supervisor_processes()}
                    for label, _, body in tx.calls:
                        code, err = bash_syntax(body)
                        self.assertEqual(code, 0, f"{label}: {err}")
                        if body in listings:
                            continue
                        for forbidden in ("pkill", "killall", "prune", "kill-server", "| tail", "|| true"):
                            self.assertNotIn(forbidden, body, label)

    def test_pkgx_has_no_lock_and_scripted_services(self):
        out, _, _ = run_fake(pkgx.PkgxAdapter({}, None, "t"))
        self.assertEqual(out["lock.created"]["status"], "unsupported")
        self.assertEqual(out["lock.frozen_copy"]["status"], "unsupported")
        self.assertEqual(out["start.a"]["mode"], "scripted")

    def test_nix_lanes_lock_inside_their_flake_dir(self):
        for cls, flake_dir in ((process_compose.ProcessComposeAdapter, "toolchain"),
                               (services_flake.ServicesFlakeAdapter, "services"),
                               (dnvr.DnvrAdapter, "dnvr")):
            adapter = cls({}, None, "t")
            self.assertEqual(adapter.lock_files, (f"{flake_dir}/flake.lock",))
            co = adapter.checkout("c", 2)
            self.assertIn("--no-update-lock-file", adapter.frozen_setup(co))
            self.assertNotIn("flake lock", adapter.frozen_setup(co))

    def test_dnvr_start_claims_readiness_and_uses_runner(self):
        adapter = dnvr.DnvrAdapter({}, None, "t")
        self.assertTrue(adapter.start_waits_ready)
        self.assertIsNone(adapter.ready(adapter.checkout("a", 0)))
        script = (CONFIG / "dnvr/rwb-dnvr.sh").read_text()
        self.assertIn('script -qfec "dnvr up"', script)      # the real runner on a PTY
        self.assertIn("printf '\\007'", script)              # dnvr's Ctrl-G detach binding
        self.assertIn("dnvr-state wait pg.url", script)


class ConfigurationTest(unittest.TestCase):
    def test_scripts_parse(self):
        for path in SCRIPTS + [CONFIG / "_shared/rwb-services.sh"]:
            with self.subTest(script=path.name):
                code, err = bash_syntax(path.read_text())
                self.assertEqual(code, 0, err)

    @unittest.skipUnless(shutil.which("shellcheck"), "shellcheck not installed")
    def test_scripts_shellcheck(self):
        for path in SCRIPTS:
            with self.subTest(script=path.name):
                proc = subprocess.run(["shellcheck", "-S", "warning", str(path)], capture_output=True)
                self.assertEqual(proc.returncode, 0, proc.stdout.decode())

    def test_digests_are_pinned(self):
        for digest in (process_compose.PC_LINUX_ARM64_TGZ_SHA256, pkgx.PKGX_LINUX_ARM64_TXZ_SHA256):
            self.assertRegex(digest, HEX64)
        for rev in (pkgx.PANTRY_REV, dnvr.DNVR_REV, guix.CHANNEL_COMMIT, process_compose.NIXPKGS_REV):
            self.assertRegex(rev, r"^[0-9a-f]{40}$")

    def test_flakes_share_the_nixpkgs_revision(self):
        for rel in ("process-compose/toolchain/flake.nix", "services-flake/services/flake.nix", "dnvr/dnvr/flake.nix"):
            text = (CONFIG / rel).read_text()
            self.assertIn(f"NixOS/nixpkgs/{process_compose.NIXPKGS_REV}", text, rel)
            self.assertIn("postgresql_17", text, rel)
        self.assertIn(f"dialohq/dnvr/{dnvr.DNVR_REV}", (CONFIG / "dnvr/dnvr/flake.nix").read_text())
        self.assertIn('dnvr.inputs.nixpkgs.follows = "nixpkgs"', (CONFIG / "dnvr/dnvr/flake.nix").read_text())

    def test_services_flake_enables_api_on_own_socket(self):
        text = (CONFIG / "services-flake/services/flake.nix").read_text()
        self.assertIn("no-server = false;", text)
        self.assertIn("use-uds = true;", text)
        self.assertIn("unix-socket = local.socket;", text)

    def test_control_sockets_unique_and_short(self):
        run_id = "20261006T235959-" + "x" * 40
        for cls in (process_compose.ProcessComposeAdapter, services_flake.ServicesFlakeAdapter):
            adapter = cls({}, None, run_id)
            sockets = {adapter.socket(adapter.checkout(n, i)) for i, n in enumerate("abcde")}
            self.assertEqual(len(sockets), 5)
            self.assertTrue(all(len(s) < 100 for s in sockets), sockets)
            other = cls({}, None, run_id + "-other")
            self.assertNotEqual(other.socket(other.checkout("a", 0)), adapter.socket(adapter.checkout("a", 0)))

    def test_process_compose_yaml_semantics(self):
        text = (CONFIG / "process-compose/process-compose.yaml").read_text()
        for needle in ("disable_env_expansion: true", "ordered_shutdown: true", "readiness_probe",
                       "condition: process_completed_successfully", "--appendfsync always",
                       "signal: 2, parent_only: true"):
            self.assertIn(needle, text)
        try:
            import yaml  # optional
        except ImportError:
            return
        doc = yaml.safe_load(text)
        self.assertEqual(set(doc["processes"]), {"init-postgres", "init-redis", "postgres", "redis"})

    def test_pkgx_yaml_matches_adapter_expectations(self):
        text = (CONFIG / "pkgx/pkgx.yaml").read_text()
        for project, version in (("python.org", pkgx.EXPECTED["python"]), ("postgresql.org", "17.2.0"),
                                 ("redis.io", pkgx.EXPECTED["redis"]), ("astral.sh/uv", pkgx.EXPECTED["uv"])):
            self.assertIn(f"{project}: '={version}'", text)
        adapter = pkgx.PkgxAdapter({}, None, "t")
        self.assertIn("'=17.2.0'", adapter.break_config(adapter.checkout("d", 3)))

    def test_guix_manifest_declares_its_deviation(self):
        manifest = (CONFIG / "guix/manifest.scm").read_text()
        for spec in ("python@3.13.13", "postgresql@16.14", "redis@7.2.6", "uv@0.10.12"):
            self.assertIn(spec, manifest)
        self.assertIn(guix.CHANNEL_COMMIT, (CONFIG / "guix/channels.scm").read_text())
        adapter = guix.GuixAdapter({}, None, "t")
        self.assertIn("deviation", adapter.pins)
        self.assertNotIn("--disable-chroot", " ".join(b for _, b, _ in adapter.provision()))
        labelled = guix.GuixAdapter({"guix_daemon_flags": "--disable-chroot"}, None, "t")
        self.assertIn("--disable-chroot", labelled.title)

    def test_guix_preflight_blocks_without_verified_digest(self):
        """Runs only the local, network-free part: exits 77 with RWB-BLOCKED before any download."""
        proc = subprocess.run(["bash", str(CONFIG / "guix/provision.sh"), "preflight"], capture_output=True,
                              env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "RWB_GUIX_URL": "https://invalid.example/x",
                                   "RWB_GUIX_SHA256": ""}, timeout=30)
        self.assertEqual(proc.returncode, 77, proc.stderr.decode())
        self.assertIn("RWB-BLOCKED:", proc.stdout.decode())

    def test_guix_provision_users(self):
        steps = guix.GuixAdapter({}, None, "t").provision()
        self.assertEqual([(label, user) for label, _, user in steps],
                         [("provision-guix-preflight", "root"), ("provision-guix-install", "root"),
                          ("provision-guix-canary", None)])


if __name__ == "__main__":
    unittest.main()

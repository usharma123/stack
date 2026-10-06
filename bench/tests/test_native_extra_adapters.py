"""Offline checks for the native-extra adapters: Process Compose, services-flake, pkgx/dev,
dnvr and GNU Guix. No network, no Docker, no services.

python3 -m unittest discover -s bench/tests -v
"""
from pathlib import Path
import re
import os
import shutil
import subprocess
import sys
import tempfile
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


def stub(bin_dir, name, body):
    path = Path(bin_dir) / name
    path.write_text("#!/usr/bin/env bash\n" + body)
    path.chmod(0o755)


def run_script(script, args, cwd, bin_dir, env=None, timeout=60):
    full = dict(PATH=f"{bin_dir}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                HOME=str(cwd), LC_ALL="C", **(env or {}))
    return subprocess.run(["bash", str(Path(cwd) / script), *args], cwd=cwd, env=full,
                          capture_output=True, text=True, timeout=timeout)


CONFLICT = ("could not bind IPv4 address \"127.0.0.1\": Address already in use; "
            "Is another postmaster already running on port 25436?")
# Native `process list -o json` shape for a manager whose postgres never became ready.
PC_LIST = '[{"name":"postgres","status":"Restarting","is_running":false,"is_ready":"-"}]'


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


class NativeReadinessFailureTest(unittest.TestCase):
    """rwb-pc.sh / rwb-sf.sh `ready` against stub CLIs: a failed native is-ready returns its
    exact status (124 = the 120 s deadline) and only adds bounded native status/log output."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.calls = self.tmp / "calls"

    def cli(self, name, ready_rc):
        # Records argv; `project is-ready` exits ready_rc; `process list` prints PC_LIST.
        stub(self.bin, name, f"""echo "$*" >> {self.calls}
case "$*" in
  *"project is-ready --wait"*) exit {ready_rc} ;;
  *"process list -o json"*) echo '{PC_LIST}' ;;
  *) exit 0 ;;
esac
""")

    def pc_checkout(self, ready_rc, log_dir=".rwb-state/logs"):
        co = self.tmp / "co"
        co.mkdir()
        shutil.copy(CONFIG / "process-compose/rwb-pc.sh", co)
        shutil.copy(CONFIG / "_shared/rwb-env.sh", co)
        (co / "bench.local.env").write_text("PGPORT=25436\nREDIS_PORT=26436\nRWB_INSTANCE=rwb-e\n")
        (co / "pc.local.env").write_text(f"RWB_PC_SOCKET={self.tmp}/sock/e.sock\n")
        (co / log_dir).mkdir(parents=True)
        (co / log_dir / "processes.log").write_text("noise\n" * 5 + CONFLICT + "\n")
        (co / log_dir / "process-compose.log").write_text('{"message":"Project exited"}\n')
        self.cli("process-compose", ready_rc)
        return co

    def sf_checkout(self, ready_rc):
        co = self.tmp / "co"
        co.mkdir()
        shutil.copy(CONFIG / "services-flake/rwb-sf.sh", co)
        (co / ".rwb-state/sf").mkdir(parents=True)
        (co / ".rwb-state/sf/processes.log").write_text("noise\n" * 5 + CONFLICT + "\n")
        (co / ".rwb-state/sf/process-compose.log").write_text('{"message":"Project exited"}\n')
        self.cli("services", ready_rc)
        return co

    def assert_failure_kept(self, proc, rc, script):
        self.assertEqual(proc.returncode, rc, proc.stderr)
        self.assertIn(f"native readiness exited {rc}; bounded diagnostics follow", proc.stderr)
        self.assertIn(CONFLICT, proc.stderr)               # retained native process output
        self.assertIn('"status":"Restarting"', proc.stderr)  # native status
        self.assertIn("Project exited", proc.stderr)        # manager log
        self.assertEqual(proc.stdout, "")
        # The deadline wrapper itself is unchanged: the native wait under exactly 120 s.
        self.assertRegex((CONFIG / script).read_text(), r"timeout 120 (process-compose --unix-socket \"\$RWB_PC_SOCKET\"|services) project is-ready --wait \|\| rc=\$\?")

    def test_process_compose_deadline_exit_124_preserved_with_logs(self):
        co = self.pc_checkout(124)
        proc = run_script("rwb-pc.sh", ["ready"], co, self.bin)
        self.assert_failure_kept(proc, 124, "process-compose/rwb-pc.sh")
        calls = self.calls.read_text()
        self.assertIn(f"--unix-socket {self.tmp}/sock/e.sock project is-ready --wait", calls)
        self.assertIn("process list -o json", calls)

    def test_process_compose_other_status_preserved(self):
        proc = run_script("rwb-pc.sh", ["ready"], self.pc_checkout(3), self.bin)
        self.assert_failure_kept(proc, 3, "process-compose/rwb-pc.sh")

    def test_services_flake_deadline_exit_124_preserved_with_logs(self):
        proc = run_script("rwb-sf.sh", ["ready"], self.sf_checkout(124), self.bin)
        self.assert_failure_kept(proc, 124, "services-flake/rwb-sf.sh")

    @unittest.skipUnless(shutil.which("jq"), "jq not installed")
    def test_ready_success_prints_no_diagnostics(self):
        co = self.pc_checkout(0)
        ok = '[{"name":"postgres","status":"Running","is_running":true,"is_ready":"Ready"},' \
             '{"name":"redis","status":"Running","is_running":true,"is_ready":"Ready"}]'
        stub(self.bin, "process-compose", f"case \"$*\" in *\"process list -o json\"*) echo '{ok}';; esac\n")
        proc = run_script("rwb-pc.sh", ["ready"], co, self.bin)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertNotIn("diagnostics", proc.stderr)

    def test_logs_are_bounded(self):
        co = self.pc_checkout(124)
        (co / ".rwb-state/logs/processes.log").write_text(("x" * 2000 + "\n") * 500)
        proc = run_script("rwb-pc.sh", ["logs"], co, self.bin)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertLess(len(proc.stdout), 8192 + 2 * 16384 + 1024)

    def test_logs_tolerate_absent_manager_and_files(self):
        co = self.pc_checkout(124)
        shutil.rmtree(co / ".rwb-state/logs")
        stub(self.bin, "process-compose", "echo 'no manager' >&2; exit 1\n")
        proc = run_script("rwb-pc.sh", ["logs"], co, self.bin)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn(".rwb-state/logs/processes.log: absent", proc.stdout)


class DnvrReadinessFailureTest(unittest.TestCase):
    """rwb-dnvr.sh `up` with stub runner tools: when pg.url is never published, the driver
    prints bounded native service logs and keeps its existing exit 1."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.co = self.tmp / "co"
        self.state = self.co / ".dnvr"
        (self.state / "logs/tmux-rwb-up").mkdir(parents=True)
        shutil.copy(CONFIG / "dnvr/rwb-dnvr.sh", self.co)
        (self.state / "logs/pg.json").write_text(
            '{"error_severity":"LOG","message":"starting PostgreSQL 17.11"}\n'
            f'{{"error_severity":"LOG","state_code":"XX000","message":{CONFLICT!r}}}\n'.replace("'", '"'))
        (self.state / "logs/tmux-rwb-up/redis.log").write_text("Ready to accept connections tcp\n")
        (self.state / "logs/rwb-pty-old.log").write_text("PTY-TRANSCRIPT-NOISE\n")
        # `script` holds the PTY until its keyboard (the FIFO) closes, like an attached client.
        stub(self.bin, "script", "cat > /dev/null\n")
        stub(self.bin, "tmux", 'case "$*" in *list-clients*) echo client0;; esac\n')
        stub(self.bin, "dnvr", 'echo "NAME PID STATE"; echo "pg - exited"; echo "redis 7 running"\n')
        stub(self.bin, "dnvr-state", f'echo "$*" >> {self.tmp}/state-calls; exit 1\n')

    def test_readiness_failure_keeps_exit_1_and_prints_native_logs(self):
        proc = run_script("rwb-dnvr.sh", ["up"], self.co, self.bin, env=dict(DNVR_STATE=str(self.state)))
        self.assertEqual(proc.returncode, 1, proc.stderr)
        self.assertIn("services did not publish readiness keys", proc.stderr)
        self.assertIn("pg - exited", proc.stderr)              # dnvr ps, as before
        self.assertIn("Address already in use", proc.stderr)   # PG preset jsonlog
        self.assertIn("port 25436", proc.stderr)
        self.assertIn("Ready to accept connections", proc.stderr)  # runner pane log
        self.assertNotIn("PTY-TRANSCRIPT-NOISE", proc.stderr)
        self.assertEqual((self.tmp / "state-calls").read_text(), "wait pg.url --timeout 120\n")

    def test_logs_are_bounded(self):
        (self.state / "logs/pg.json").write_text(("ERROR " + "y" * 3000 + "\n") * 300)
        proc = run_script("rwb-dnvr.sh", ["logs"], self.co, self.bin, env=dict(DNVR_STATE=str(self.state)))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertLess(len(proc.stdout), 2 * (2 * 8192 + 256))


class NativeRuntimeFixTest(unittest.TestCase):
    def test_services_flake_versions_use_the_services_wrapper(self):
        adapter = services_flake.ServicesFlakeAdapter({}, None, "t")
        body = adapter.tool_versions(adapter.checkout("a", 0))
        self.assertIn("command -v python3 uv postgres redis-server services;", body)
        self.assertIn("services version", body)
        self.assertNotIn("process-compose", body)  # absent from the devshell PATH (exit 127)
        self.assertTrue(body.startswith(adapter.nix_env()))
        self.assertIn("nix develop", body)

    def test_dnvr_devshell_exposes_redis_for_the_version_receipt(self):
        text = (CONFIG / "dnvr/dnvr/flake.nix").read_text()
        packages = re.search(r"^\s*packages = \[([^\]]*)\];", text, re.M).group(1).split()
        self.assertEqual(packages, ["pkgs.python313", "pkgs.uv", "pkgs.redis", "pkgs.tmux", "pkgs.util-linux"])
        adapter = dnvr.DnvrAdapter({}, None, "t")
        self.assertIn("redis-server --version", adapter.tool_versions(adapter.checkout("a", 0)))

    def test_process_output_logs_are_written_and_retained(self):
        yaml_text = (CONFIG / "process-compose/process-compose.yaml").read_text()
        self.assertRegex(yaml_text, r"(?m)^log_location: \.rwb-state/logs/processes\.log$")
        self.assertIn('settings.log_location = ".rwb-state/sf/processes.log";',
                      (CONFIG / "services-flake/services/flake.nix").read_text())
        expected = {process_compose.ProcessComposeAdapter: ((".rwb-state/logs",), "rwb-pc.sh logs"),
                    services_flake.ServicesFlakeAdapter: ((".rwb-state/sf/process-compose.log",
                                                           ".rwb-state/sf/processes.log"), "rwb-sf.sh logs"),
                    dnvr.DnvrAdapter: ((".dnvr/logs",), "rwb-dnvr.sh logs")}
        for cls, (paths, logs) in expected.items():
            with self.subTest(adapter=cls.name):
                adapter = cls({}, None, "t")
                co = adapter.checkout("e", 4)
                self.assertEqual(adapter.artifacts(co), paths)
                (name, body), = adapter.diagnostics(co)
                self.assertIn(logs, body)
                self.assertIn(logs, adapter.conflict_logs(co))

    def test_labels_and_pty_scope_unchanged(self):
        pc = process_compose.ProcessComposeAdapter.features
        sf = services_flake.ServicesFlakeAdapter.features
        dn = dnvr.DnvrAdapter.features
        self.assertEqual((pc["readiness"], sf["readiness"], dn["readiness"]), ("native", "native", "scripted"))
        self.assertTrue(dnvr.DnvrAdapter.start_waits_ready)
        self.assertIn("PTY driver", dnvr.DnvrAdapter.start_scope)
        self.assertEqual(process_compose.NixFlakeAdapter.timeouts["ready"], 120)


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

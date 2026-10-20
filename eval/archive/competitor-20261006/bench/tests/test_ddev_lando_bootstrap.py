"""Offline tests for DDEV/Lando host bootstrap: private Docker client config, Lando autosetup
(no CA install, no second orchestrator) and orchestrator identity.

No network, no Docker daemon, no real ddev/lando: provision bodies run under bash with fake
`docker`/`lando` executables that log every call. The user's Docker config is a fixture
directory holding fake credentials; tests check it is never written and never echoed.
"""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

BENCH = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(BENCH))

from rwb.adapters import lando as lando_mod  # noqa: E402
from rwb.adapters.ddev import DOCKER_OVERRIDES, PRIVATE_DOCKER_REL, DdevAdapter  # noqa: E402
from rwb.adapters.lando import CHECK_REL, COMPOSE_VERSION, LandoAdapter  # noqa: E402

RUN = "20261006t120000-abc123"
SECRET = "c2VjcmV0LXRva2VuLWRvLW5vdC1sZWFr"
BUILD = [sys.executable, "-I", str(BENCH / PRIVATE_DOCKER_REL)]
CHECK = [sys.executable, "-I", str(BENCH / CHECK_REL)]

# Fake docker: answers `context inspect` like docker/cli (cli/command/cli.go): DOCKER_HOST,
# then DOCKER_CONTEXT, then $DOCKER_CONFIG (or ~/.docker) currentContext ->
# contexts/meta/*/meta.json endpoint. Logs everything (with DOCKER_* env) and succeeds.
FAKE_DOCKER = textwrap.dedent('''\
    #!/usr/bin/env python3
    import glob, json, os, sys
    cfg = os.environ.get("DOCKER_CONFIG") or os.path.join(os.environ["HOME"], ".docker")
    with open(os.environ["RWB_FAKE_LOG"], "a") as fh:
        fh.write(json.dumps({"tool": "docker", "args": sys.argv[1:], "DOCKER_CONFIG": cfg,
                             "HOME": os.environ.get("HOME"),
                             "env": sorted(k for k in os.environ if k.startswith("DOCKER_"))}) + "\\n")
    if sys.argv[1:3] == ["context", "inspect"]:
        try:
            name = json.load(open(os.path.join(cfg, "config.json"))).get("currentContext") or "default"
        except FileNotFoundError:
            name = "default"
        name = os.environ.get("DOCKER_CONTEXT") or name
        host = "unix:///var/run/docker.sock"
        if os.environ.get("DOCKER_HOST"):
            name, host = "default", os.environ["DOCKER_HOST"]
        for meta in glob.glob(os.path.join(cfg, "contexts", "meta", "*", "meta.json")):
            data = json.load(open(meta))
            if data["Name"] == name:
                host = data["Endpoints"]["docker"]["Host"]
        print(name, host)
    elif sys.argv[1:2] == ["info"]:
        print("docker server 28.0.0 Docker Desktop aarch64")
''')

# Fake lando 3.26.9: emulates the parts of utils/build-config.js the adapter relies on
# (config.yml `setup:` merged over defaults; orchestratorBin kept only if absolute and
# existing) and lib/formatters.js `_.get(data, path, data)` (unset path -> whole config,
# including process.env).
FAKE_LANDO = textwrap.dedent('''\
    #!/usr/bin/env python3
    import json, os, sys
    root = os.environ["LANDO_CORE_USERCONFROOT"]
    with open(os.environ["RWB_FAKE_LOG"], "a") as fh:
        fh.write(json.dumps({"tool": "lando", "args": sys.argv[1:], "HOME": os.environ.get("HOME")}) + "\\n")
    args = sys.argv[1:]
    if args == ["version"]:
        print("v3.26.9"); sys.exit(0)
    if args[:1] != ["config"]:
        sys.exit(0)
    setup = {"skipInstallCa": False, "skipCommonPlugins": False, "installPlugins": True,
             "buildEngine": "4.85.0", "buildx": "0.30.1"}
    config = {"userConfRoot": root, "env": dict(os.environ), "setup": setup, "orchestratorVersion": "2.40.3"}
    section = None
    for line in open(os.path.join(root, "config.yml")):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        key, _, value = line.strip().partition(":")
        value = value.strip()
        if not line.startswith(" "):
            section = key if not value else None
            if value:
                config[key] = value
        elif section == "setup":
            setup[key] = {"true": True, "false": False}.get(value, value)
    bin_ = config.get("orchestratorBin")
    if isinstance(bin_, str) and os.path.isabs(bin_) and os.path.exists(bin_):
        config.pop("orchestratorVersion")
    else:
        config.pop("orchestratorBin", None)
    path = args[args.index("--path") + 1]
    print(json.dumps(config.get(path, config)))
''')


def write_exe(path, text):
    path.write_text(text)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def tree_digest(root):
    out = {}
    for path in sorted(Path(root).rglob("*")):
        if path.is_file() and not path.is_symlink():
            out[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
        elif path.is_symlink():
            out[str(path.relative_to(root))] = "link:" + os.readlink(path)
    return out


DESKTOP_SOCK = "/Users/u/.docker/run/docker.sock"


def user_docker_dir(root, context="desktop-linux", host="unix://" + DESKTOP_SOCK, tls=False, skip_verify=None):
    """A Docker Desktop-shaped user config with credentials that must never be copied: the
    active context `abc` (TLS material only if `tls`), plus an inactive TLS context `def`."""
    src = Path(root) / "user-home" / ".docker"
    (src / "contexts" / "meta" / "abc").mkdir(parents=True)
    (src / "contexts" / "meta" / "def").mkdir(parents=True)
    (src / "contexts" / "tls" / "def" / "docker").mkdir(parents=True)
    (src / "cli-plugins").mkdir()
    (src / "config.json").write_text(json.dumps({
        "auths": {"https://index.docker.io/v1/": {"auth": SECRET}},
        "credsStore": "desktop", "credHelpers": {"gcr.io": "gcloud"},
        "currentContext": context, "proxies": {"default": {"httpProxy": "http://user:pw@proxy"}}}))
    endpoint = {"Host": host} if skip_verify is None else {"Host": host, "SkipTLSVerify": skip_verify}
    (src / "contexts" / "meta" / "abc" / "meta.json").write_text(json.dumps(
        {"Name": context, "Endpoints": {"docker": endpoint}}))
    (src / "contexts" / "meta" / "def" / "meta.json").write_text(json.dumps(
        {"Name": "remote-tls", "Endpoints": {"docker": {"Host": "tcp://remote.invalid:2376"}}}))
    (src / "contexts" / "tls" / "def" / "docker" / "key.pem").write_text("PRIVATE KEY " + SECRET)
    if tls:
        (src / "contexts" / "tls" / "abc" / "docker").mkdir(parents=True)
        (src / "contexts" / "tls" / "abc" / "docker" / "key.pem").write_text("PRIVATE KEY " + SECRET)
    return src


class PrivateDockerConfig(unittest.TestCase):
    def test_keeps_context_and_plugins_drops_credentials(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = user_docker_dir(tmp)
            before = tree_digest(src)
            dst = Path(tmp) / "run" / "docker-config"
            proc = subprocess.run(BUILD + [str(src), str(dst)], capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            config = json.loads((dst / "config.json").read_text())
            self.assertEqual(config, {"currentContext": "desktop-linux",
                                      "cliPluginsExtraDirs": [os.path.realpath(src / "cli-plugins")]})
            self.assertTrue((dst / "contexts" / "meta" / "abc" / "meta.json").exists())
            self.assertFalse((dst / "contexts" / "meta" / "def").exists(), "only the active context is copied")
            self.assertFalse((dst / "contexts" / "tls").exists())
            copied = "".join(p.read_text() for p in dst.rglob("*") if p.is_file())
            self.assertNotIn(SECRET, copied)
            self.assertNotIn("credsStore", copied)
            self.assertNotIn(SECRET, proc.stdout + proc.stderr)
            self.assertNotIn("user:pw", proc.stdout + proc.stderr)
            self.assertIn("dropped-keys=auths,credHelpers,credsStore,proxies", proc.stdout)
            self.assertEqual(tree_digest(src), before, "user Docker config must only be read")

    def test_missing_user_config_gives_empty_private_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            dst = Path(tmp) / "dst"
            proc = subprocess.run(BUILD + [str(Path(tmp) / "absent"), str(dst)], capture_output=True, text=True)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual(json.loads((dst / "config.json").read_text()), {})

    def test_active_tls_context_is_rejected_before_writing(self):
        for user in (dict(tls=True), dict(skip_verify=True)):
            with self.subTest(**user), tempfile.TemporaryDirectory() as tmp:
                src = user_docker_dir(tmp, context="tls-prod", host="tcp://tls.example.invalid:2376", **user)
                dst = Path(tmp) / "dst"
                proc = subprocess.run(BUILD + [str(src), str(dst)], capture_output=True, text=True)
                self.assertEqual(proc.returncode, 77, proc.stderr)
                self.assertIn("RWB-BLOCKED:", proc.stderr)
                self.assertFalse(dst.exists())
                self.assertNotIn(SECRET, proc.stdout + proc.stderr)

    def test_env_overrides_are_rejected_by_the_helper_too(self):
        from importlib import util
        spec = util.spec_from_file_location("docker_client_config", BENCH / PRIVATE_DOCKER_REL)
        helper = util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        self.assertEqual(helper.ENV_OVERRIDES, DOCKER_OVERRIDES)
        with tempfile.TemporaryDirectory() as tmp:
            src = user_docker_dir(tmp)
            for var in DOCKER_OVERRIDES:
                dst = Path(tmp) / var
                proc = subprocess.run(BUILD + [str(src), str(dst)], capture_output=True, text=True,
                                      env={"PATH": "/usr/bin:/bin", var: "tcp://user:" + SECRET + "@x"})
                self.assertEqual(proc.returncode, 77, var)
                self.assertFalse(dst.exists())
                self.assertNotIn(SECRET, proc.stdout + proc.stderr)

    def test_refuses_to_write_into_user_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = user_docker_dir(tmp)
            before = tree_digest(src)
            for dst in (src, src / "nested"):
                proc = subprocess.run(BUILD + [str(src), str(dst)], capture_output=True, text=True)
                self.assertNotEqual(proc.returncode, 0)
            self.assertEqual(tree_digest(src), before)


class Harness:
    """A temp world: fake user home, fake docker/lando on PATH, adapter rooted in it."""

    def __init__(self, cls, tmp, **user):
        self.tmp = Path(tmp)
        self.user_home = self.tmp / "user-home"
        self.src = user_docker_dir(tmp, **user)
        self.fakes = self.tmp / "fakes"
        self.fakes.mkdir()
        write_exe(self.fakes / "docker", FAKE_DOCKER)
        self.log = self.tmp / "calls.jsonl"
        self.log.touch()
        self.adapter = cls({"tools_dir": str(self.tmp / "tools")}, None, RUN)
        self.adapter.root = str(self.tmp / "w")
        if cls is LandoAdapter:
            # Stands in for /var/run/docker.sock -> the Desktop socket (Lando's engine socket).
            (self.tmp / "engine.sock").symlink_to(DESKTOP_SOCK)
            self.adapter.engine_socket = str(self.tmp / "engine.sock")
        Path(self.adapter.tools, "bin").mkdir(parents=True)
        write_exe(Path(self.adapter.tools, "bin", "lando"), FAKE_LANDO)

    def run(self, body, **env):
        base = {"PATH": f"{self.fakes}:/usr/bin:/bin", "HOME": str(self.user_home),
                "RWB_FAKE_LOG": str(self.log), **env}
        return subprocess.run(["bash", "-c", body], capture_output=True, text=True, env=base)

    def calls(self, tool=None):
        rows = [json.loads(l) for l in self.log.read_text().splitlines() if l.strip()]
        return [r for r in rows if tool is None or r["tool"] == tool]


class PreflightUsesPrivateConfig(unittest.TestCase):
    def expected_config_dir(self, adapter):
        return adapter.docker_config if isinstance(adapter, DdevAdapter) else f"{adapter.private_home}/.docker"

    def test_every_docker_call_uses_private_config_on_the_user_context(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                before = tree_digest(h.src)
                body = h.adapter.provision()[0][1]
                proc = h.run(body)
                self.assertEqual(proc.returncode, 0, proc.stderr)
                private = self.expected_config_dir(h.adapter)
                self.assertEqual(json.loads(Path(private, "config.json").read_text())["currentContext"], "desktop-linux")
                self.assertIn("docker context user=[desktop-linux unix:///Users/u/.docker/run/docker.sock] "
                              "run=[desktop-linux unix:///Users/u/.docker/run/docker.sock]", proc.stdout)
                calls = h.calls("docker")
                user_reads = [c for c in calls if c["DOCKER_CONFIG"] == str(h.src)]
                # Only the read-only comparison reads the user config; info/compose/receipts don't.
                self.assertEqual([c["args"][:2] for c in user_reads], [["context", "inspect"]])
                others = [c for c in calls if c not in user_reads]
                self.assertTrue(any(c["args"][:1] == ["info"] for c in others))
                self.assertEqual({c["DOCKER_CONFIG"] for c in others}, {private})
                first_info = next(i for i, c in enumerate(calls) if c["args"][:1] == ["info"])
                self.assertGreater(first_info, calls.index(user_reads[0]), "config must exist before docker info")
                self.assertEqual(tree_digest(h.src), before)
                self.assertNotIn(SECRET, proc.stdout + proc.stderr)

    def test_lando_runs_with_private_home(self):
        with tempfile.TemporaryDirectory() as tmp:
            h = Harness(LandoAdapter, tmp)
            proc = h.run(h.adapter.provision()[0][1])
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertEqual({c["HOME"] for c in h.calls("docker")}, {h.adapter.private_home})

    def test_context_mismatch_fails_closed_before_docker_info(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                # A private config that cannot reproduce the user's context (e.g. context
                # TLS material it does not copy) must stop the run, not switch daemons.
                fake = (h.fakes / "docker").read_text().replace(
                    'print(name, host)', 'print(name, host if cfg.endswith("user-home/.docker") else "unix:///other.sock")')
                write_exe(h.fakes / "docker", fake)
                proc = h.run(h.adapter.provision()[0][1])
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("different daemon context", proc.stderr)
                self.assertFalse(any(c["args"][:1] == ["info"] for c in h.calls("docker")))
                self.assertFalse(Path(h.adapter.selection_marker).exists())

    def test_env_captures_user_config_once_and_overrides_it(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                self.assertEqual(h.run(h.adapter.provision()[0][1]).returncode, 0)
                show = 'printf "%s|%s|%s\\n" "$RWB_USER_DOCKER_CONFIG" "$DOCKER_CONFIG" "$HOME"'
                twice = h.adapter.env() + h.adapter.env() + show
                out = h.run(twice).stdout.strip().split("|")
                self.assertEqual(out[0], f"{h.user_home}/.docker")
                self.assertEqual(out[1], self.expected_config_dir(h.adapter))
                self.assertEqual(out[2], h.adapter.private_home if cls is LandoAdapter else str(h.user_home))
                custom = h.run(h.adapter.env() + show, DOCKER_CONFIG="/custom/docker").stdout.split("|")
                self.assertEqual(custom[0], "/custom/docker")


class UnsupportedDaemonSelectionFailsClosed(unittest.TestCase):
    """Daemon selection the private config cannot carry is rejected before any docker call,
    in provision and in every later body (receipts, cleanup), so nothing reaches another daemon."""

    OVERRIDES = {"DOCKER_HOST": "unix:///selected-by-env.sock", "DOCKER_CONTEXT": "env-selected",
                 "DOCKER_TLS": "1", "DOCKER_TLS_VERIFY": "1", "DOCKER_CERT_PATH": "/certs"}

    def assert_blocked(self, h, proc, *tools):
        self.assertEqual(proc.returncode, 77, proc.stdout + proc.stderr)
        self.assertIn("RWB-BLOCKED:", proc.stderr)
        self.assertEqual([c for c in h.calls() if c["tool"] in tools], [])

    def test_env_override_blocks_preflight_before_any_docker_call(self):
        self.assertEqual(set(self.OVERRIDES), set(DOCKER_OVERRIDES))
        for cls in (DdevAdapter, LandoAdapter):
            for var, value in [*self.OVERRIDES.items(), ("DOCKER_HOST", "")]:
                with self.subTest(adapter=cls.name, var=var, value=value), tempfile.TemporaryDirectory() as tmp:
                    h = Harness(cls, tmp)
                    proc = h.run(h.adapter.provision()[0][1], **{var: value})
                    self.assert_blocked(h, proc, "docker")
                    self.assertIn(var, proc.stderr)
                    if value:
                        self.assertNotIn(value, proc.stdout + proc.stderr)
                    self.assertFalse(Path(h.adapter.state).exists(), "nothing written before the guard")

    def test_without_the_guard_the_override_would_pass_the_comparison(self):
        # The regression the guard closes: both sides inherit DOCKER_HOST and agree, while
        # Lando's stripped environment resolves the persisted context instead.
        with tempfile.TemporaryDirectory() as tmp:
            h = Harness(LandoAdapter, tmp)
            body = h.adapter.provision()[0][1].replace(lando_mod.DOCKER_ENV_GUARD, "")
            proc = h.run(body.replace(f'python3 -I {h.adapter.src}/{PRIVATE_DOCKER_REL}',
                                      f'env -u DOCKER_HOST python3 -I {h.adapter.src}/{PRIVATE_DOCKER_REL}'),
                         DOCKER_HOST="unix:///selected-by-env.sock")
            self.assertIn("user=[default unix:///selected-by-env.sock] run=[default unix:///selected-by-env.sock]",
                          proc.stdout)
            self.assertNotIn("selection sealed", proc.stdout, "the private config does not select that endpoint")
            stripped = h.run(h.adapter.env(validated=False).replace(lando_mod.DOCKER_ENV_GUARD, "")
                             + "unset DOCKER_HOST DOCKER_CONFIG; docker context inspect",
                             DOCKER_HOST="unix:///selected-by-env.sock")
            self.assertEqual(stripped.stdout.strip(), "desktop-linux unix://" + DESKTOP_SOCK)

    def test_receipt_and_cleanup_bodies_never_reach_an_overridden_daemon(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                # A rejected provision, then the core's teardown sequence (run.py teardown,
                # Scenario.cleanup) and the remaining receipt bodies.
                self.assert_blocked(h, h.run(h.adapter.provision()[0][1], DOCKER_CONTEXT="env-selected"), "docker")
                co = mock.Mock(path=str(h.tmp / "w" / "a"))
                co.name = "a"
                bodies = [h.adapter.service_processes(), h.adapter.cleanup_host(), h.adapter.host_resources(),
                          h.adapter.versions(), h.adapter.instance_identity(co), h.adapter.cleanup(co),
                          *[b for _, b, _ in h.adapter.provision()[1:]]]
                for body in bodies:
                    for var, value in self.OVERRIDES.items():
                        self.assert_blocked(h, h.run(body, **{var: value}), "docker", "lando", "ddev")

    def test_active_tls_context_with_matching_name_and_host_blocks_before_info(self):
        for cls in (DdevAdapter, LandoAdapter):
            for user in (dict(tls=True), dict(skip_verify=True)):
                with self.subTest(adapter=cls.name, **user), tempfile.TemporaryDirectory() as tmp:
                    h = Harness(cls, tmp, context="tls-prod", host="tcp://tls.example.invalid:2376", **user)
                    proc = h.run(h.adapter.provision()[0][1])
                    self.assert_blocked(h, proc, "lando")
                    self.assertIn("tls-prod", proc.stderr)
                    self.assertFalse(any(c["args"][:1] in (["info"], ["compose"]) for c in h.calls("docker")))
                    copied = [p for p in Path(h.adapter.state).rglob("*") if p.is_file()]
                    self.assertFalse(any("tls" in p.parts for p in copied))
                    self.assertNotIn(SECRET, "".join(p.read_text() for p in copied) + proc.stdout + proc.stderr)

    def test_desktop_context_with_inactive_tls_context_still_works(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp, skip_verify=False)   # Docker Desktop's meta.json shape
                proc = h.run(h.adapter.provision()[0][1])
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertTrue(any(c["args"][:1] == ["info"] for c in h.calls("docker")))
                self.assertNotIn(SECRET, proc.stdout + proc.stderr)

    def test_lando_requires_the_context_socket_to_be_its_engine_socket(self):
        for case in ("other-socket", "tcp-endpoint"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                if case == "tcp-endpoint":
                    h = Harness(LandoAdapter, tmp, context="remote", host="tcp://10.0.0.5:2375")
                else:
                    h = Harness(LandoAdapter, tmp)
                    h.adapter.engine_socket = str(h.tmp / "elsewhere.sock")
                proc = h.run(h.adapter.provision()[0][1])
                self.assertEqual(proc.returncode, 77, proc.stderr)
                self.assertIn("Lando engine socket", proc.stderr)
                self.assertFalse(any(c["args"][:1] == ["info"] for c in h.calls("docker")))
        self.assertEqual(lando_mod.ENGINE_SOCKET, "/var/run/docker.sock")


TEARDOWN = ("service_processes", "cleanup_host", "host_resources")


def later_bodies(h):
    """Every body the core may run after the preflight: run.py teardown (cleanup_host,
    host_resources), Scenario.cleanup (service_processes), receipts, checkout and install."""
    co = mock.Mock(path=str(h.tmp / "w" / "a"))
    co.name = "a"
    named = {name: getattr(h.adapter, name)() for name in TEARDOWN}
    named.update(versions=h.adapter.versions(), identity=h.adapter.instance_identity(co),
                 stopped=h.adapter.stopped_probe(co, None), status=h.adapter.status(co),
                 cleanup=h.adapter.cleanup(co),
                 **{label: body for label, body, _ in h.adapter.provision()[1:]})
    return named


class DaemonSelectionSeal(unittest.TestCase):
    """Astra R2 P1: a rejected preflight leaves no (or an empty) private config, which docker/cli
    resolves to the default socket. Later bodies must not run on that unvalidated daemon, while
    failures after a validated selection must still get the full cleanup."""

    def assert_blocked_without_tools(self, h, proc, label):
        self.assertEqual(proc.returncode, 77, f"{label}: {proc.stdout}{proc.stderr}")
        self.assertIn("RWB-BLOCKED:", proc.stderr, label)
        self.assertEqual(h.calls(), [], f"{label} reached a tool")

    def test_active_tls_rejection_blocks_every_teardown_and_receipt_body(self):
        for cls in (DdevAdapter, LandoAdapter):
            for user in (dict(tls=True), dict(skip_verify=True)):
                with self.subTest(adapter=cls.name, **user), tempfile.TemporaryDirectory() as tmp:
                    h = Harness(cls, tmp, context="tls-prod", host="tcp://tls.example.invalid:2376", **user)
                    pre = h.run(h.adapter.provision()[0][1])
                    self.assertEqual(pre.returncode, 77, pre.stderr)
                    self.assertEqual(h.calls(), [])
                    self.assertFalse(Path(h.adapter.selection_marker).exists())
                    for label, body in later_bodies(h).items():
                        proc = h.run(body)
                        self.assert_blocked_without_tools(h, proc, label)
                        self.assertIn("no validated Docker daemon selection", proc.stderr)
                        self.assertNotIn(SECRET, proc.stdout + proc.stderr)

    def test_without_any_preflight_bodies_are_blocked(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                for label, body in later_bodies(h).items():
                    self.assert_blocked_without_tools(h, h.run(body), label)

    def test_failure_after_validated_selection_still_cleans_up(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                fake = (h.fakes / "docker").read_text().replace(
                    'print("docker server 28.0.0 Docker Desktop aarch64")', 'sys.exit("daemon unavailable")')
                write_exe(h.fakes / "docker", fake)
                pre = h.run(h.adapter.provision()[0][1])
                self.assertNotEqual(pre.returncode, 0)
                self.assertIn("selection sealed: [desktop-linux unix://" + DESKTOP_SOCK + "]", pre.stdout)
                self.assertTrue(any(c["args"][:1] == ["info"] for c in h.calls("docker")))
                private = Path(h.adapter.docker_config if cls is DdevAdapter else f"{h.adapter.private_home}/.docker")
                for name in TEARDOWN:
                    h.log.write_text("")
                    proc = h.run(getattr(h.adapter, name)())
                    self.assertEqual(proc.returncode, 0, f"{name}: {proc.stderr}")
                    self.assertNotIn("RWB-BLOCKED", proc.stderr)
                    calls = h.calls("docker")
                    self.assertTrue(calls, f"{name} must still query the validated daemon")
                    self.assertEqual({c["DOCKER_CONFIG"] for c in calls}, {str(private)})
                h.log.write_text("")
                self.assertTrue(h.run(h.adapter.cleanup_host()).returncode == 0)
                self.assertTrue(any(c["args"][:1] in (["ps"], ["container"]) for c in h.calls("docker")))

    def test_guard_is_silent_after_a_valid_preflight(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                self.assertEqual(h.run(h.adapter.provision()[0][1]).returncode, 0)
                proc = h.run(h.adapter.env() + "echo body-ran")
                self.assertEqual((proc.returncode, proc.stdout, proc.stderr), (0, "body-ran\n", ""))

    def tamper(self, h, how):
        private = Path(h.adapter.docker_config if isinstance(h.adapter, DdevAdapter)
                       else f"{h.adapter.private_home}/.docker")
        marker = Path(h.adapter.selection_marker)
        if how == "config-deleted":
            (private / "config.json").unlink()
        elif how == "config-emptied":
            (private / "config.json").write_text("{}\n")
        elif how == "context-switched":
            (private / "config.json").write_text(json.dumps({"currentContext": "default"}))
        elif how == "endpoint-rewritten":
            meta = private / "contexts" / "meta" / "abc" / "meta.json"
            meta.write_text(meta.read_text().replace(DESKTOP_SOCK, "/var/run/other.sock"))
        elif how == "tls-added":
            tls = private / "contexts" / "tls" / "abc" / "docker"
            tls.mkdir(parents=True)
            (tls / "key.pem").write_text("x")
        elif how == "marker-truncated":
            marker.write_text(marker.read_text()[:20])
        elif how == "marker-incomplete":
            marker.write_text(json.dumps({"docker_config": str(private.resolve())}))
        elif how == "marker-forged":
            # A marker that matches the files but claims an endpoint they do not select.
            data = json.loads(marker.read_text())
            data["context"] = "desktop-linux unix:///var/run/other.sock"
            marker.write_text(json.dumps(data))
        elif how == "marker-deleted":
            marker.unlink()

    def test_missing_incomplete_or_tampered_selection_is_rejected(self):
        cases = ("config-deleted", "config-emptied", "context-switched", "endpoint-rewritten", "tls-added",
                 "marker-truncated", "marker-incomplete", "marker-forged", "marker-deleted")
        for cls in (DdevAdapter, LandoAdapter):
            for how in cases:
                with self.subTest(adapter=cls.name, tamper=how), tempfile.TemporaryDirectory() as tmp:
                    h = Harness(cls, tmp)
                    self.assertEqual(h.run(h.adapter.provision()[0][1]).returncode, 0)
                    self.tamper(h, how)
                    h.log.write_text("")
                    for name in TEARDOWN:
                        self.assert_blocked_without_tools(h, h.run(getattr(h.adapter, name)()), name)

    def test_rejected_rerun_of_preflight_drops_an_earlier_seal(self):
        for cls in (DdevAdapter, LandoAdapter):
            with self.subTest(adapter=cls.name), tempfile.TemporaryDirectory() as tmp:
                h = Harness(cls, tmp)
                self.assertEqual(h.run(h.adapter.provision()[0][1]).returncode, 0)
                self.assertTrue(Path(h.adapter.selection_marker).exists())
                # The user switches to an unsupported TLS context before a second preflight.
                tls = h.src / "contexts" / "tls" / "abc" / "docker"
                tls.mkdir(parents=True)
                (tls / "key.pem").write_text("PRIVATE KEY " + SECRET)
                self.assertEqual(h.run(h.adapter.provision()[0][1]).returncode, 77)
                self.assertFalse(Path(h.adapter.selection_marker).exists())
                h.log.write_text("")
                for name in TEARDOWN:
                    self.assert_blocked_without_tools(h, h.run(getattr(h.adapter, name)()), name)

    def test_lando_engine_socket_rejection_leaves_no_seal(self):
        with tempfile.TemporaryDirectory() as tmp:
            h = Harness(LandoAdapter, tmp)
            h.adapter.engine_socket = str(h.tmp / "elsewhere.sock")
            self.assertEqual(h.run(h.adapter.provision()[0][1]).returncode, 77)
            self.assertFalse(Path(h.adapter.selection_marker).exists())
            h.log.write_text("")
            for name in TEARDOWN:
                self.assert_blocked_without_tools(h, h.run(getattr(h.adapter, name)()), name)

    def test_seal_refuses_an_endpoint_the_private_config_does_not_select(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = user_docker_dir(tmp)
            dst = Path(tmp) / "dst"
            marker = Path(tmp) / "marker.json"
            self.assertEqual(subprocess.run(BUILD + [str(src), str(dst)], capture_output=True).returncode, 0)
            bad = subprocess.run(BUILD + ["--seal", str(dst), str(marker), "default unix:///var/run/docker.sock"],
                                 capture_output=True, text=True)
            self.assertNotEqual(bad.returncode, 0)
            self.assertFalse(marker.exists())
            good = subprocess.run(BUILD + ["--seal", str(dst), str(marker), "desktop-linux unix://" + DESKTOP_SOCK],
                                  capture_output=True, text=True)
            self.assertEqual(good.returncode, 0, good.stderr)
            ok = subprocess.run(BUILD + ["--verify", str(dst), str(marker)], capture_output=True, text=True)
            self.assertEqual((ok.returncode, ok.stdout, ok.stderr), (0, "", ""))
            empty = Path(tmp) / "empty"
            empty.mkdir()
            self.assertNotEqual(subprocess.run(BUILD + ["--seal", str(empty), str(marker), "default x"],
                                               capture_output=True).returncode, 0)


class LandoAutosetupConfig(unittest.TestCase):
    def setup_block(self):
        text = (BENCH / "adapters" / "lando" / "config.yml").read_text()
        block, inside = {}, False
        for line in text.splitlines():
            if line.startswith("setup:"):
                inside = True
            elif inside and line.startswith("  ") and ":" in line:
                key, value = line.strip().split(":", 1)
                block[key] = {"true": True, "false": False}[value.strip()]
            elif inside and line.strip():
                inside = False
        return block

    def test_config_disables_every_host_mutating_setup_task(self):
        from importlib import util
        spec = util.spec_from_file_location("check_config", BENCH / CHECK_REL)
        check = util.module_from_spec(spec)
        spec.loader.exec_module(check)
        self.assertEqual(self.setup_block(), check.SETUP)
        self.assertIs(check.SETUP["skipInstallCa"], True)
        self.assertIs(check.SETUP["orchestrator"], False)

    def run_check(self, payload, *args):
        return subprocess.run(CHECK + list(args), input=payload, capture_output=True, text=True)

    def test_setup_check_rejects_lando_defaults(self):
        defaults = {"skipInstallCa": False, "buildEngine": "4.85.0", "buildx": "0.30.1",
                    "installPlugins": True, "skipCommonPlugins": False}
        proc = self.run_check(json.dumps(defaults), "setup")
        self.assertNotEqual(proc.returncode, 0)
        for key in ("skipInstallCa", "buildEngine", "buildx", "orchestrator", "installPlugins"):
            self.assertIn(key, proc.stderr)

    def test_checks_never_echo_the_whole_config(self):
        whole = json.dumps({"userConfRoot": "/x", "env": {"GITHUB_TOKEN": SECRET}, "setup": {}})
        for args in (("setup",), ("orchestrator", "/x/bin/docker-compose-v2.40.3")):
            proc = self.run_check("some banner\n" + whole + "\n", *args)
            self.assertNotEqual(proc.returncode, 0)
            self.assertNotIn(SECRET, proc.stdout + proc.stderr)

    def test_orchestrator_check_requires_the_expected_executable(self):
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / f"docker-compose-v{COMPOSE_VERSION}"
            write_exe(exe, "#!/bin/sh\n")
            ok = self.run_check(json.dumps(str(exe)), "orchestrator", str(exe))
            self.assertEqual(ok.returncode, 0, ok.stderr)
            self.assertIn(f"lando orchestratorBin {exe}", ok.stdout)
            desktop = self.run_check(json.dumps("/Applications/Docker.app/cli-plugins/docker-compose"),
                                     "orchestrator", str(exe))
            self.assertNotEqual(desktop.returncode, 0)


class LandoProvision(unittest.TestCase):
    """Provision bodies run against fake tools with a fake, locally hashed Compose asset."""

    def harness(self, tmp):
        h = Harness(LandoAdapter, tmp)
        compose = Path(h.adapter.tools) / "fake-compose"
        write_exe(compose, "#!/bin/sh\necho 'Docker Compose version v2.40.3'\n")
        digest = hashlib.sha256(compose.read_bytes()).hexdigest()
        assets = {p: ("fake-compose", digest) for p in lando_mod.COMPOSE_ASSETS}
        return h, assets

    def provision(self, h, assets, preflight=True):
        with mock.patch.dict(lando_mod.COMPOSE_ASSETS, assets):
            bodies = {label: body for label, body, _ in h.adapter.provision()}
        if preflight:   # later bodies require the sealed daemon selection
            self.assertEqual(h.run(bodies["preflight"]).returncode, 0)
        return bodies

    def test_verified_compose_is_the_one_lando_resolves(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, assets = self.harness(tmp)
            bodies = self.provision(h, assets, preflight=False)
            for label in ("preflight", "install-lando-orchestrator", "lando-private-config"):
                proc = h.run(bodies[label])
                self.assertEqual(proc.returncode, 0, f"{label}: {proc.stderr}")
            target = Path(h.adapter.orchestrator)
            # The exact path utils/get-compose-x.js resolves after autosetup resets orchestratorBin.
            self.assertEqual(target, Path(h.adapter.state, "lando", "bin", f"docker-compose-v{COMPOSE_VERSION}"))
            self.assertFalse(target.is_symlink())
            self.assertEqual(hashlib.sha256(target.read_bytes()).hexdigest(), assets["darwin-arm64"][1])
            config = Path(h.adapter.conf, "config.yml").read_text()
            self.assertTrue(config.endswith(f"orchestratorBin: {target}\n"))
            self.assertIn("lando setup skipInstallCa=true buildEngine=false buildx=false orchestrator=false",
                          proc.stdout)
            self.assertIn(f"lando orchestratorBin {target}", proc.stdout)
            self.assertEqual({c["HOME"] for c in h.calls("lando")}, {h.adapter.private_home})

    def test_tampered_private_copy_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, assets = self.harness(tmp)
            bodies = self.provision(h, assets)
            # The private copy differs from the verified download: its own hash check fails.
            body = bodies["install-lando-orchestrator"].replace('cp "$asset" ', '{ cat "$asset"; printf x; } > ', 1)
            self.assertNotEqual(body, bodies["install-lando-orchestrator"])
            self.assertNotEqual(h.run(body).returncode, 0)
            self.assertFalse(Path(h.adapter.orchestrator).exists())

    def test_missing_orchestrator_fails_before_any_lando_call(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, assets = self.harness(tmp)
            bodies = self.provision(h, assets)
            proc = h.run(bodies["lando-private-config"])
            self.assertNotEqual(proc.returncode, 0)
            self.assertEqual(h.calls("lando"), [])

    def test_unsafe_setup_config_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, assets = self.harness(tmp)
            bodies = self.provision(h, assets)
            self.assertEqual(h.run(bodies["install-lando-orchestrator"]).returncode, 0)
            # The original adapter config: no setup block, so Lando keeps its defaults
            # (host CA install via sudo, orchestrator re-download).
            unsafe = bodies["lando-private-config"].replace(
                f"cp {h.adapter.src}/adapters/lando/config.yml",
                "printf 'proxy: \"OFF\"\\nscanner: false\\n' >", 1)
            self.assertNotEqual(unsafe, bodies["lando-private-config"])
            proc = h.run(unsafe)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("would act on the host", proc.stderr)

    def test_version_receipt_shows_resolved_orchestrator_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            h, assets = self.harness(tmp)
            bodies = self.provision(h, assets)
            for label in ("install-lando-orchestrator", "lando-private-config"):
                self.assertEqual(h.run(bodies[label]).returncode, 0)
            proc = h.run(h.adapter.versions())
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn(f"lando orchestratorBin {h.adapter.orchestrator}", proc.stdout)
            self.assertIn(f"{assets['darwin-arm64'][1]} docker-compose-v{COMPOSE_VERSION}", proc.stdout)
            self.assertIn("Docker Compose version v2.40.3", proc.stdout)


class PlannedBodiesStayLocal(unittest.TestCase):
    def test_no_sudo_setup_or_user_docker_writes(self):
        for cls in (DdevAdapter, LandoAdapter):
            adapter = cls({"tools_dir": "/opt/rwb-tools"}, None, RUN)
            adapter.root = "/tmp/rwb-test/w"
            bodies = [b for _, b, _ in adapter.provision()] + [adapter.versions(), adapter.cleanup_host()]
            for body in bodies:
                with self.subTest(adapter=cls.name, body=body[:60]):
                    for word in ("sudo", "lando setup", "ddev config global", "cli-plugins", "~/.docker",
                                 "$HOME/.docker/", "credsStore", "install-ca", "mkcert"):
                        self.assertNotIn(word, body)

    def test_pinned_hashes_unchanged(self):
        self.assertEqual(lando_mod.ASSETS["darwin-arm64"][1],
                         "8dd9b6306a05816c3d8cdb2f4c4b9fd6d9df76490992fb0f2e331a894fd92b95")
        self.assertEqual(lando_mod.COMPOSE_ASSETS["darwin-arm64"][1],
                         "8cd7eb5f95bacb536cc407111662e2c205d67d9abfea5dcb8400be8418db60d1")
        self.assertEqual(lando_mod.PLUGINS, ("@lando/python@1.4.3", "@lando/postgres@1.6.0", "@lando/redis@1.3.0"))
        pins = LandoAdapter({}, None, RUN).pins
        self.assertEqual(pins["orchestrator"], "docker-compose 2.40.3")


if __name__ == "__main__":
    unittest.main()

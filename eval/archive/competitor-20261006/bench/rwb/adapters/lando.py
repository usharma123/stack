"""Lando 3.26.9 (API 3 services): official python, postgres and redis service types.

Lando orchestrates Docker Compose from its Landofile; commands enter the app container with
`lando exec appserver --` (Lando's documented command routing). Build steps install a
hash-locked uv and run `uv sync --frozen` on first start. Lando's healthchecks only add
warnings when they fail (hooks/app-add-healthchecks.js), so `lando start` exiting 0 is not
readiness: readiness is the app's own retrying `wait` (scripted).

Global state is private (LANDO_CORE_USERCONFROOT in the run's state directory); `lando
setup` is never run. Lando's name normalizer strips -, _ and ., so checkout names differ in
alphanumerics (rwb<run><checkout>). Source: lando/core 7a87f80576c5cdb5c7d616108bc9aff81150d463.

Autosetup (source facts at that commit): start/stop/destroy call hooks/lando-run-setup.js,
which runs lando.setup(config.setup). adapters/lando/config.yml sets setup.skipInstallCa
(no sudo system-CA install), buildEngine/buildx/orchestrator=false and installPlugins=false.
`setup.orchestrator: false` is required: with orchestratorBin set, utils/build-config.js
drops orchestratorVersion, the orchestrator task's hasRun is then false and it downloads
Compose again; after setup lando-run-setup.js resets orchestratorBin to
get-compose-x(<root>/bin/docker-compose-v2.40.3). The checksum-verified Compose is therefore
copied to exactly that path and orchestratorBin points at it, so both resolutions agree.
check_config.py proves the merged setup flags and the resolved orchestratorBin.

Docker client: utils/build-config.js deletes every DOCKER_* variable (strip-env) before
spawning Compose, so DOCKER_CONFIG cannot reach Lando's orchestrator. HOME is the run's
private home instead, whose .docker is the private client config (user's persisted current
context, no credential helper; ddev.private_docker_config). Because Lando strips them, an
inherited DOCKER_HOST/DOCKER_CONTEXT/TLS variable would make the adapter's preflight, receipts
and cleanup address a different daemon than Lando's Compose; ddev.DOCKER_ENV_GUARD (first in
env(), so in every body) blocks those before any docker call. Lando's own engine client
(dockerode, utils/get-engine-config.js) always uses /var/run/docker.sock, so the preflight
also requires the context endpoint to be a unix socket resolving to that same file. Only then
is the selection sealed; later bodies (receipts, cleanup) fail closed without that seal
(ddev.selection_guard), so a rejected preflight never falls back to the default socket.

Private HOME changes normal Lando behaviour (a benchmark validity condition, see validity()):
Lando mounts HOME at /user in its containers and scans HOME/.ssh for keys, so containers see
the run-private home, not the user's, and no host SSH key files reach /user/.ssh. That home is
not empty: it holds the private .docker metadata, and Lando core creates .ssh in it.
SSH_AUTH_SOCK is still inherited, so this is not a guarantee that SSH agent access is off.

Research: bench/research/lando.md. Handoff: bench/CONTAINER-ADAPTERS.md.
"""
import re

from .base import q
from .ddev import (DOCKER_ENV_GUARD, PRIVATE_DOCKER_REL, SELECTION_MARKER, private_docker_config,
                   seal_selection, selection_guard)
from .devcontainers import SHA_CHECK, ContainerAdapter, binary_hash

VERSION = "3.26.9"
RELEASE = f"https://github.com/lando/core/releases/download/v{VERSION}"
# From the release's sha256sum.txt (identical to GitHub's asset digests).
ASSETS = {
    "darwin-arm64": (f"lando-macos-arm64-v{VERSION}", "8dd9b6306a05816c3d8cdb2f4c4b9fd6d9df76490992fb0f2e331a894fd92b95"),
    "darwin-amd64": (f"lando-macos-x64-v{VERSION}", "16969dc627d1594a40ac62b8362f657b59309af1563a82057d58cc769bc70718"),
    "linux-arm64": (f"lando-linux-arm64-v{VERSION}", "5709cf237ccd7a23768b920b00e20cbf2067b184d2572f0945bb2ecf09be67a8"),
    "linux-amd64": (f"lando-linux-x64-v{VERSION}", "7a868b71efffb1f8ecc5cc3377bcbeea9926618a082c5138ba19803b9bba5011"),
}
COMPOSE_VERSION = "2.40.3"
COMPOSE_RELEASE = f"https://github.com/docker/compose/releases/download/v{COMPOSE_VERSION}"
# From docker/compose v2.40.3 checksums.txt.
COMPOSE_ASSETS = {
    "darwin-arm64": ("docker-compose-darwin-aarch64", "8cd7eb5f95bacb536cc407111662e2c205d67d9abfea5dcb8400be8418db60d1"),
    "darwin-amd64": ("docker-compose-darwin-x86_64", "53528ecff0182546d92d7cc3f50dc78f9b387c3da68b4a3fd0cf2c48dab77133"),
    "linux-arm64": ("docker-compose-linux-aarch64", "d26373b19e89160546d15407516cc59f453030d9bc5b43ba7faf16f7b4980137"),
    "linux-amd64": ("docker-compose-linux-x86_64", "dba9d98e1ba5bfe11d88c99b9bd32fc4a0624a30fafe68eea34d61a3e42fd372"),
}
CHECK_REL = "adapters/lando/check_config.py"
# Lando's dockerode engine socket (utils/get-engine-config.js default, no engineConfig set).
ENGINE_SOCKET = "/var/run/docker.sock"
# argv: "<context name> <endpoint host>" and the engine socket; no daemon contact.
ENGINE_CHECK = ("import os,sys; h=sys.argv[1].split()[-1]; e=sys.argv[2]; "
                "ok=h.startswith('unix://') and os.path.realpath(h[7:])==os.path.realpath(e); "
                "print('lando engine socket', e, '->', os.path.realpath(e), 'context endpoint', h); "
                "ok or print('RWB-BLOCKED: Docker context endpoint is not the Lando engine socket '+e, file=sys.stderr); "
                "sys.exit(0 if ok else 77)")
PLUGINS = ("@lando/python@1.4.3", "@lando/postgres@1.6.0", "@lando/redis@1.3.0")
LANDO_IMAGES = {
    "python": "python:3.13.16-bookworm@sha256:d79ba8693551488516799bfce4566d8cbcccb87c9d9dfaef5e3915f1451e1326",
    "postgres": "bitnamilegacy/postgresql:17.6.0-debian-12-r4@sha256:926356130b77d5742d8ce605b258d35db9b62f2f8fd1601f9dbaef0c8a710a8d",
    "redis": "redis:8.10.2-trixie@sha256:c94085d298b738be22c9ccdc0ac3761fa6649df7dd82ad1d42367f3cb9714935",
    "uv": "uv==0.12.23 wheel from PyPI, hash-locked in .lando/uv-requirements.txt",
}


class LandoAdapter(ContainerAdapter):
    name = "lando"
    title = "Lando"
    version = VERSION
    workspace = "/app"                 # Lando mounts the app root at /app
    config_files = (".lando.yml", ".lando/uv-requirements.txt")
    lock_files = ("uv.lock", ".lando.yml", ".lando/uv-requirements.txt")
    start_waits_ready = False
    setup_scope = "lando info: resolves the merged Landofile only; image pulls and build steps (uv install, uv sync) run on first `lando start`"
    bad_config_pattern = r"3\.13\.99"
    shared_infra = ("network:lando_bridge_network",)
    engine_socket = ENGINE_SOCKET
    features = {
        "lockfile": "scripted",           # digest pins + uv.lock; Lando has no environment lock
        "frozen_setup": "scripted",
        "services": "native",
        "detached_services": "native",
        "readiness": "scripted",          # failed healthchecks are warnings; app `wait` decides
        "per_checkout_ports": "native",
        "per_checkout_data": "native",
        "stop_confirmation": "native",
        "structured_status": "native",    # `lando info --format json`
        "wrong_instance_guard": "unsupported",
    }

    def images(self):
        return dict(LANDO_IMAGES)

    def extra_pins(self):
        return dict(assets={k: v[1] for k, v in ASSETS.items()}, asset_hash_source="release sha256sum.txt",
                    orchestrator=f"docker-compose {COMPOSE_VERSION}",
                    orchestrator_assets={k: v[1] for k, v in COMPOSE_ASSETS.items()}, plugins=list(PLUGINS),
                    source_commit="7a87f80576c5cdb5c7d616108bc9aff81150d463",
                    image_notes="Debian images (Lando v3 service scripts need bash): python full bookworm, "
                                "Bitnami PostgreSQL 17.6.0, Redis 8.10.2 trixie; Redis AOF appendfsync=everysec (plugin default)")

    @property
    def owner_token(self):
        # Lando strips non-alphanumerics from app names, so owned names carry the normalized
        # run id; the receipt guard must match that exact token (raw run_id is kept elsewhere).
        return re.sub(r"[^a-z0-9]", "", self.run_id.lower())

    def project(self, name):
        return "rwb" + self.owner_token + name

    @property
    def conf(self):
        return f"{self.state}/lando"

    @property
    def private_home(self):
        return f"{self.state}/home"

    @property
    def orchestrator(self):
        # The path get-compose-x resolves for orchestratorVersion 2.40.3 under this root.
        return f"{self.conf}/bin/docker-compose-v{COMPOSE_VERSION}"

    @property
    def selection_marker(self):
        return f"{self.state}/{SELECTION_MARKER}"

    def env(self, validated=True):
        # Lando strips DOCKER_*, so the private client config is reached through HOME; the
        # user's config dir is captured first (and kept across repeated env()s), only read.
        # Every body but the preflight (validated=False) requires the sealed daemon selection.
        return DOCKER_ENV_GUARD + super().env() + (
            f"export LANDO_CORE_USERCONFROOT={q(self.conf)} NO_COLOR=1 "
            'RWB_USER_DOCKER_CONFIG="${RWB_USER_DOCKER_CONFIG:-${DOCKER_CONFIG:-$HOME/.docker}}" '
            f"HOME={q(self.private_home)} DOCKER_CONFIG={q(self.private_home)}/.docker; ") + (
            selection_guard(f"{self.src}/{PRIVATE_DOCKER_REL}", self.selection_marker) if validated else "")

    def provision(self):
        return [("preflight", self.env(validated=False) + self.preflight(), None)] + self.install()

    def preflight(self):
        # The private client config must exist before the shared preflight's `docker info`;
        # the selection is sealed only after the engine-socket check too.
        script = f"{self.src}/{PRIVATE_DOCKER_REL}"
        return "\n".join(["set -eu", f"mkdir -p {q(self.private_home)}/.docker",
                          *private_docker_config("$RWB_USER_DOCKER_CONFIG", script, self.selection_marker),
                          f'python3 -I -c {q(ENGINE_CHECK)} "$rwb_run_ctx" {q(self.engine_socket)}',
                          seal_selection(script, self.selection_marker),
                          super().preflight()])

    def config_checks(self):
        """Fail closed unless Lando's merged config disables host-mutating autosetup and
        resolves the verified private orchestrator (values printed, config never dumped)."""
        check = f"python3 -I {q(self.src + '/' + CHECK_REL)}"
        return [f"lando config --format json --path setup < /dev/null | {check} setup",
                f"lando config --format json --path orchestratorBin < /dev/null | {check} orchestrator {q(self.orchestrator)}"]

    def install(self):
        lando = self.fetch_binary(RELEASE, ASSETS, "lando")
        # Copied (not linked) into the private root, then re-verified: the executable Lando
        # runs is this exact file, and nothing Lando writes can reach the shared tool cache.
        compose = "\n".join([
            self.fetch_binary(COMPOSE_RELEASE, COMPOSE_ASSETS, None),
            f"mkdir -p {q(self.conf)}/bin",
            f'cp "$asset" {q(self.orchestrator)}.part',
            f'{SHA_CHECK} {q(self.orchestrator)}.part "$sum"',
            f"chmod 755 {q(self.orchestrator)}.part && mv {q(self.orchestrator)}.part {q(self.orchestrator)}",
        ])
        init = "\n".join([
            "set -eu", f"mkdir -p {q(self.conf)}",
            f"cp {q(self.src)}/adapters/lando/config.yml {q(self.conf)}/config.yml",
            # An absolute, existing orchestratorBin makes Lando use it (utils/build-config.js).
            f"test -x {q(self.orchestrator)}",
            f"printf 'orchestratorBin: %s\\n' {q(self.orchestrator)} >> {q(self.conf)}/config.yml",
            f'test "$(lando version)" = v{VERSION}',
            "lando plugin-add " + " ".join(PLUGINS) + " < /dev/null",
            "lando version --all",
            *self.config_checks(),
        ])
        return [("install-lando", self.env() + lando, None),
                ("install-lando-orchestrator", self.env() + compose, None),
                ("lando-private-config", self.env() + init, None)]

    def version_commands(self):
        # Identity of the executables Lando actually runs: resolved orchestratorBin, its hash.
        return ["lando version", binary_hash(f"{self.tools}/bin/lando"), *self.config_checks(),
                binary_hash(self.orchestrator), f"{q(self.orchestrator)} version", "lando version --all"]

    def validity(self):
        return dict(super().validity(), docker_client_config=(
            f"HOME={self.private_home} (Lando strips DOCKER_*): its .docker holds the user's persisted current context "
            "only, no credsStore/auths (anonymous public pulls); preflight fails closed on a different daemon endpoint, "
            f"on DOCKER_HOST/CONTEXT/TLS* env overrides, on TLS/SkipTLSVerify contexts, and unless the endpoint is "
            f"the unix socket {self.engine_socket} that Lando's engine uses; every later body (receipts, cleanup) "
            f"exits 77 before any docker/lando call unless {self.selection_marker} seals that validated selection"),
            private_home=(f"Lando's /user mount and .ssh key scan use {self.private_home} (holds private .docker "
                          "metadata and the .ssh Lando creates), not the user's home: no host SSH key files in "
                          "containers; SSH_AUTH_SOCK is still inherited, so agent access is not guaranteed off"),
            autosetup="setup.skipInstallCa, buildEngine/buildx/orchestrator/installPlugins=false; "
                      "verified by check_config.py before start")

    def local_env(self, co):
        return [f"printf 'name: %s\\n' {q(self.project(co.name))} > {q(co.path)}/.lando.local.yml"]

    def setup(self, co):
        # Lando has no build-only step: images pull and build steps run on first `lando start`.
        # Setup resolves and validates the merged Landofile with the pinned plugins.
        return self.in_checkout(co, "lando info --format json < /dev/null")

    def start(self, co):
        return self.in_checkout(co, "lando start < /dev/null")

    def enter(self, co, body):
        return self.in_checkout(co, f"lando exec appserver -- bash -c {q(body)} < /dev/null")

    def server_versions(self, co):
        return self.in_checkout(co, "lando exec database -- postgres --version < /dev/null && "
                                    "lando exec cache -- redis-server --version < /dev/null")

    def status(self, co):
        return self.in_checkout(co, "lando info --format json < /dev/null")

    def stop(self, co):
        return self.in_checkout(co, "lando stop < /dev/null")

    def cleanup(self, co):
        return self.in_checkout(co, "lando destroy --yes < /dev/null")

    def native_teardown(self, name):
        path = f"{self.workdir()}/{name}"
        return f"if [ -f {q(path)}/.lando.local.yml ]; then cd {q(path)} && lando destroy --yes < /dev/null; fi"

    def break_config(self, co):
        return self.edit(f"{co.path}/.lando.yml", LANDO_IMAGES["python"], "python:3.13.99-bookworm")


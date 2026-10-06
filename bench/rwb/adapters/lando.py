"""Lando 3.26.9 (API 3 services): official python, postgres and redis service types.

Lando orchestrates Docker Compose from its Landofile; commands enter the app container with
`lando exec appserver --` (Lando's documented command routing). Build steps install a
hash-locked uv and run `uv sync --frozen` on first start. Lando's healthchecks only add
warnings when they fail (hooks/app-add-healthchecks.js), so `lando start` exiting 0 is not
readiness: readiness is the app's own retrying `wait` (scripted).

Global state is private (LANDO_CORE_USERCONFROOT in the run's state directory); `lando
setup` is never run. Lando's name normalizer strips -, _ and ., so checkout names differ in
alphanumerics (rwb<run><checkout>). Source: lando/core 7a87f80576c5cdb5c7d616108bc9aff81150d463.

Research: bench/research/lando.md. Handoff: bench/CONTAINER-ADAPTERS.md.
"""
import re

from .base import q
from .devcontainers import ContainerAdapter, binary_hash

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

    def env(self):
        return super().env() + f"export LANDO_CORE_USERCONFROOT={q(self.state)}/lando NO_COLOR=1; "

    def install(self):
        lando = self.fetch_binary(RELEASE, ASSETS, "lando")
        compose = self.fetch_binary(COMPOSE_RELEASE, COMPOSE_ASSETS, None) + "\n" + \
            f'chmod 755 "$asset" && ln -sf "../$asset" bin/docker-compose-{COMPOSE_VERSION}'
        conf = f"{self.state}/lando"
        init = "\n".join([
            "set -eu", f"mkdir -p {q(conf)}",
            f"cp {q(self.src)}/adapters/lando/config.yml {q(conf)}/config.yml",
            # An absolute, existing orchestratorBin makes Lando use it and skip its own install.
            f"printf 'orchestratorBin: %s\\n' {q(self.orchestrator)} >> {q(conf)}/config.yml",
            f'test "$(lando version)" = v{VERSION}',
            "lando plugin-add " + " ".join(PLUGINS) + " < /dev/null",
            "lando version --all",
        ])
        return [("install-lando", self.env() + lando, None),
                ("install-lando-orchestrator", self.env() + compose, None),
                ("lando-private-config", self.env() + init, None)]

    @property
    def orchestrator(self):
        return f"{self.tools}/bin/docker-compose-{COMPOSE_VERSION}"

    def version_commands(self):
        return ["lando version", binary_hash(f"{self.tools}/bin/lando"), binary_hash(self.orchestrator),
                f"{q(self.orchestrator)} version", "lando version --all"]

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


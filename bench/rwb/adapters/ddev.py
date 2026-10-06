"""DDEV v1.25.4: native PostgreSQL 17 database, Python as a custom Compose service.

DDEV is PHP-first, so its PHP web container is part of this workflow's cost. PostgreSQL is
DDEV's own `database: postgres:17` service, built on the Debian `postgres:17.6-bookworm` base
(DDEV customizes it with apt), pinned by digest through BASE_IMAGE. Redis and the Python app
are custom services in .ddev/docker-compose.workload.yaml. URLs use the project-unique
container names (`ddev-<name>-db`) because DDEV attaches every service to the shared
`ddev_default` network as well, where generic aliases are ambiguous.

Global state is private: DDEV_XDG_CONFIG_HOME points into the run's state directory, whose
global_config.yaml omits the shared router and SSH agent, so the run binds no fixed host
ports and starts no host-wide DDEV containers. Source facts (5da91aeb9ebab0b0e66171c450b72099308d332c):
`ddev exec` calls StartAppIfNotRunning (the entry resumes a stopped project); `ddev stop`
removes containers and keeps volumes; `ddev delete` removes the project's volumes.

Research: bench/research/ddev.md. Handoff: bench/CONTAINER-ADAPTERS.md.
"""
from .base import q
from .devcontainers import ContainerAdapter, IMAGES, binary_hash

VERSION = "1.25.4"
RELEASE = f"https://github.com/ddev/ddev/releases/download/v{VERSION}"
# From the release's checksums.txt (also GitHub's asset digests).
ASSETS = {
    "darwin-arm64": (f"ddev_macos-arm64.v{VERSION}.tar.gz", "af68d362bf006d86e582ccd4f7e40926ba19a451b394cb375f93971272fb41ea"),
    "darwin-amd64": (f"ddev_macos-amd64.v{VERSION}.tar.gz", "05aa309c0e8cd7a14696da93e99efd7b91ec7a1994dd8218674fbfad283aa832"),
    "linux-arm64": (f"ddev_linux-arm64.v{VERSION}.tar.gz", "41b1412c83e7e2ae04887f02a9b4bf6d441c6f662773d97979e9fe23acf93c0a"),
    "linux-amd64": (f"ddev_linux-amd64.v{VERSION}.tar.gz", "65fb822f0d2874220c8f9a6b2dfec095d37c0fdc555e34dc5b0e5f4177beeb93"),
}
PG_IMAGE = "postgres:17.6-bookworm@sha256:f3bd19c606e442c3d7bdfa8002e03fe260a1023351e0ea4598032022b68dd6e3"


class DdevAdapter(ContainerAdapter):
    name = "ddev"
    title = "DDEV"
    version = VERSION
    config_files = (".ddev/config.yaml", ".ddev/docker-compose.workload.yaml", ".ddev/app/Dockerfile")
    lock_files = ("uv.lock", ".ddev/config.yaml", ".ddev/docker-compose.workload.yaml", ".ddev/app/Dockerfile")
    start_waits_ready = True
    entry_auto_resumes = True
    setup_scope = "ddev utility download-images: renders the project and pulls its images; builds and the app's uv sync happen at start/deps"
    bad_config_pattern = r"postgres:99"
    shared_infra = ("network:ddev_default", "volume:ddev-global-cache")
    features = {
        "lockfile": "scripted",           # digest pins + uv.lock; DDEV has no environment lock
        "frozen_setup": "scripted",
        "services": "native",
        "detached_services": "native",
        "readiness": "native",            # `ddev start` waits for every labelled container's healthcheck
        "per_checkout_ports": "native",
        "per_checkout_data": "native",
        "stop_confirmation": "native",
        "structured_status": "native",    # `ddev describe --json-output`
        "wrong_instance_guard": "unsupported",
    }

    def images(self):
        return dict(IMAGES, postgres=PG_IMAGE)

    def extra_pins(self):
        return dict(assets={k: v[1] for k, v in ASSETS.items()}, asset_hash_source="release checksums.txt",
                    source_commit="5da91aeb9ebab0b0e66171c450b72099308d332c",
                    image_notes="PostgreSQL 17.6 on Debian bookworm (DDEV customizes it with apt, so the "
                                "derived image is not byte-frozen); web container ddev/ddev-webserver as chosen by DDEV")

    def env(self):
        return super().env() + (f"export DDEV_XDG_CONFIG_HOME={q(self.state)}/ddev-xdg DDEV_NONINTERACTIVE=true "
                                "DDEV_NO_INSTRUMENTATION=true NO_COLOR=1; ")

    def selectors(self, name):
        # Compose project (containers, networks, compose volumes) and DDEV's own named
        # resources (<name>-postgres volume, <image>-<name>-built images).
        return [f"ddev-{self.project(name)}", self.project(name)]

    def install(self):
        fetch = self.fetch_binary(RELEASE, ASSETS, None)
        unpack = "\n".join([
            f'mkdir -p ddev-{VERSION} && tar -xzf "$asset" -C ddev-{VERSION} ddev',
            f"ln -sf ../ddev-{VERSION}/ddev bin/ddev",
            f"mkdir -p {q(self.state)}/ddev-xdg/ddev",
            f"cp {q(self.src)}/adapters/ddev/global_config.yaml {q(self.state)}/ddev-xdg/ddev/global_config.yaml",
            f'test "$(ddev --version)" = "ddev version v{VERSION}"',
        ])
        return [("install-ddev", self.env() + fetch + "\n" + unpack, None)]

    def version_commands(self):
        return ["ddev --version", binary_hash(f"{self.tools}/bin/ddev"), "ddev version --json-output"]

    def local_env(self, co):
        # DDEV merges .ddev/config.*.yaml; config.local.yaml is its conventional uncommitted file.
        return [f"printf 'name: %s\\n' {q(self.project(co.name))} > {q(co.path)}/.ddev/config.local.yaml"]

    def setup(self, co):
        # Native: renders this project's Compose config and pulls every image it needs.
        return self.in_checkout(co, "ddev utility download-images < /dev/null")

    def start(self, co):
        return self.in_checkout(co, "ddev start < /dev/null")

    def enter(self, co, body):
        return self.in_checkout(co, f"ddev exec --service app --raw -- bash -c {q(body)} < /dev/null")

    def server_versions(self, co):
        return self.in_checkout(co, "ddev exec --service db --raw -- postgres --version < /dev/null && "
                                    "ddev exec --service redis --raw -- redis-server --version < /dev/null")

    def status(self, co):
        return self.in_checkout(co, "ddev describe --json-output < /dev/null")

    def stop(self, co):
        return self.in_checkout(co, "ddev stop < /dev/null")

    def cleanup(self, co):
        return self.in_checkout(co, "ddev delete --yes --omit-snapshot --clean-containers=false < /dev/null")

    def native_teardown(self, name):
        path = f"{self.workdir()}/{name}"
        return (f"if [ -f {q(path)}/.ddev/config.local.yaml ]; then cd {q(path)} && "
                "ddev delete --yes --omit-snapshot --clean-containers=false < /dev/null; fi")

    def break_config(self, co):
        return self.edit(f"{co.path}/.ddev/config.yaml", 'version: "17"', 'version: "99"')

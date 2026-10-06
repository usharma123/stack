"""DevPod v0.6.15 with its local Docker provider, consuming the committed Dev Container.

DevPod owns workspace identity, the agent injected into the app container, SSH command entry
and stop/start/delete; Docker Compose runs PostgreSQL and Redis. Source facts used here
(release commit 33d20ff8806a3fee86d8f56ed50db6108b945fc2):

- The Compose project is named after the workspace UID unless COMPOSE_PROJECT_NAME is set
  (pkg/compose/helper.go GetProjectName, pkg/devcontainer/run.go GetRunnerIDFromWorkspace).
  Every call exports COMPOSE_PROJECT_NAME=<workspace id>, and start fails closed unless the
  project found by the checkout's Compose working directory equals that id.
- `devpod ssh` calls startWait(create=false): a stopped workspace returns "DevPod workspace
  is stopped" instead of resuming (cmd/ssh.go). The research note and brief said it
  auto-resumes; the stop probe uses Docker inspection either way.
- `devpod delete` runs `compose down` without --volumes: named volumes survive. Cleanup
  removes this project's volumes afterwards (scripted, ownership-checked).

Research: bench/research/devpod.md. Handoff: bench/CONTAINER-ADAPTERS.md.
"""
from .base import q
from .devcontainers import IMAGES, ContainerAdapter, binary_hash

VERSION = "0.6.15"
RELEASE = f"https://github.com/loft-sh/devpod/releases/download/v{VERSION}"
# DevPod publishes no checksum file and GitHub reports no asset digest for this release.
# These are hashes observed on 2026-10-06 from the official release URLs (trust on first
# use); darwin-arm64 matches the researcher's independent download.
ASSETS = {
    "darwin-arm64": ("devpod-darwin-arm64", "0c50934f4199732ff37e89708bc5c028ecda75d854a151cb2644e4ba39f4d7f7"),
    "darwin-amd64": ("devpod-darwin-amd64", "1205fc8626d9daa011479ded3ce7271359714f0d011acfcf739adf90901a6ee8"),
    "linux-arm64": ("devpod-linux-arm64", "9226161e0c9f5a45d0f8d1778f940498e787b650f0e0fcf3c29f1f67e7a3f272"),
    "linux-amd64": ("devpod-linux-amd64", "cc50bce09229d5a6d448ac1d4494327f4b8f7a20321e5fcee3bfec1aef0d20c5"),
}
SSH_FLAGS = "--user root --agent-forwarding=false --start-services=false"


class DevpodAdapter(ContainerAdapter):
    name = "devpod"
    title = "DevPod (Docker provider)"
    version = VERSION
    config_files = (".devcontainer/devcontainer.json", ".devcontainer/compose.yaml", ".devcontainer/Dockerfile")
    lock_files = ("uv.lock", ".devcontainer/Dockerfile", ".devcontainer/compose.yaml")
    start_waits_ready = True
    setup_scope = "Compose model validation only; DevPod builds/pulls images, injects its agent and runs uv sync (postCreateCommand) in `up`"
    bad_config_pattern = r"3\.13\.99"
    features = {
        # No DevPod environment lock or frozen mode; .workspace.lock files are mutexes.
        "lockfile": "scripted",
        "frozen_setup": "scripted",
        "services": "native",
        "detached_services": "native",
        "readiness": "native",           # Compose depends_on: service_healthy inside `up`
        "per_checkout_ports": "native",
        "per_checkout_data": "native",
        "stop_confirmation": "native",   # `devpod stop` -> compose stop
        "structured_status": "native",   # `devpod status --output json`
        "wrong_instance_guard": "unsupported",
    }

    def extra_pins(self):
        return dict(assets={k: v[1] for k, v in ASSETS.items()}, asset_hash_source="observed (no publisher checksum)",
                    source_commit="33d20ff8806a3fee86d8f56ed50db6108b945fc2",
                    provider="bench/adapters/devpod/provider-docker.yaml (docker provider at the release commit)",
                    agent="DevPod injects its linux agent into the app container (downloaded from the release by DevPod)")

    def env(self):
        return super().env() + f"export DEVPOD_HOME={q(self.state)}/devpod DEVPOD_DISABLE_TELEMETRY=true; "

    def install(self):
        fetch = self.fetch_binary(RELEASE, ASSETS, "devpod")
        provider = f"{self.src}/adapters/{self.name}/provider-docker.yaml"
        init = "\n".join([
            "set -eu", f"test \"$(devpod version)\" = v{VERSION}",
            f"devpod provider add {q(provider)} --name docker < /dev/null",
            "devpod context set-options -o TELEMETRY=false -o SSH_ADD_PRIVATE_KEYS=false < /dev/null",
            "devpod provider list --output json",
        ])
        return [("install-devpod", self.env() + fetch, None), ("devpod-private-home", self.env() + init, None)]

    def version_commands(self):
        return ["devpod version", binary_hash(f"{self.tools}/bin/devpod")]

    def workspace_env(self, co):
        return f"export COMPOSE_PROJECT_NAME={q(self.project(co.name))}; "

    def run(self, co, body):
        return self.in_checkout(co, self.workspace_env(co) + body)

    def setup(self, co):
        # DevPod has no build-only step for a local Compose workspace (`devpod build` creates
        # and deletes a temporary workspace to push an image); image build/pull happens in
        # `up`. Setup only validates the committed Compose model (scripted).
        return self.run(co, f"docker compose -p {q(self.project(co.name))} -f .devcontainer/compose.yaml config --quiet")

    def start(self, co):
        wid = self.project(co.name)
        return self.run(co, (
            f"mkdir -p {q(self.state)} && devpod up {q(co.path)} --id {q(wid)} --provider docker --ide none "
            f"--configure-ssh=false --ssh-config {q(self.state)}/ssh_config < /dev/null && "
            # Fail closed if DevPod did not honour the run-owned project name.
            f"test \"$({self.receipt('project', 'dir:' + co.path + '/.devcontainer')})\" = {q(wid)}"))

    def enter(self, co, body):
        return self.run(co, f"devpod ssh {q(self.project(co.name))} {SSH_FLAGS} --workdir {q(self.workspace)} "
                            f"--command {q('bash -c ' + q(body))} < /dev/null")

    def server_versions(self, co):
        compose = f"docker compose -p {q(self.project(co.name))} -f .devcontainer/compose.yaml"
        return self.run(co, f"{compose} exec -T postgres postgres --version && {compose} exec -T redis redis-server --version")

    def status(self, co):
        return self.run(co, f"devpod status {q(self.project(co.name))} --output json < /dev/null")

    def stop(self, co):
        return self.run(co, f"devpod stop {q(self.project(co.name))} < /dev/null")

    def cleanup(self, co):
        # Native delete keeps named volumes; remove this project's remaining resources too.
        return self.run(co, f"devpod delete {q(self.project(co.name))} < /dev/null && "
                            + self.receipt("remove", self.run_id, self.project(co.name)))

    def native_teardown(self, name):
        wid = self.project(name)
        return (f"if devpod list --output json < /dev/null | python3 -I -c 'import json,sys; "
                f"sys.exit(0 if any(w.get(\"id\") == sys.argv[1] for w in json.load(sys.stdin)) else 1)' {q(wid)}; "
                f"then COMPOSE_PROJECT_NAME={q(wid)} devpod delete {q(wid)} < /dev/null; fi")

    def break_config(self, co):
        return self.edit(f"{co.path}/.devcontainer/Dockerfile", IMAGES["python"], "python:3.13.99-slim-bookworm")


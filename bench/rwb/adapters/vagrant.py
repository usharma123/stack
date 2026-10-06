"""Vagrant 2.4.9 with its built-in Docker provider (no box, no VM). See bench/research/vagrant.md.

Host transport on Docker Desktop (force_host_vm = false). Each checkout defines three
machines, `pg`, `redis` and `app`, whose containers are named `rwb-<run id>-<checkout>-*` on
a per-checkout network with named volumes (container boundary). App commands use the
provider's native `vagrant docker-exec`, which runs in the app container. The checkout is
bind-mounted there at its own absolute path.

Vagrant is not installed on the host, and the official installer writes to /opt and
/usr/local. Provisioning instead verifies the official darwin DMG checksum, mounts it
read-only at a private mount point, expands the package payload into the run's tools
directory and detaches the image. The relocated launcher runs from there with a run-owned
VAGRANT_HOME. Nothing is installed system-wide.

Native `docker-exec` behaviour that the checks account for: stdout and stderr are relayed
as one stream on success. A failing command makes Vagrant exit 1 with the output in its
error message, so the app's own exit code is lost. A *stopped* target is skipped with exit
0. The app container is therefore never halted while checks run (stop halts only `pg` and
`redis`), and every app check parses the JSON receipt instead of trusting the exit code.
"""
import os
import posixpath

from .base import Adapter, q
from .tilt import darwin_arm64_preflight, remove_owned

VAGRANT_VERSION = "2.4.9"
# vagrant_2.4.9_darwin_arm64.dmg from releases.hashicorp.com (universal; same bytes as amd64).
VAGRANT_DMG_SHA256 = "8de08bd435ef8ae0fc5fbd6acefa9c68e62fb898c5ae0fbdacd26853bea9d4d6"
VAGRANT_LAUNCHER_SHA256 = "102bbe8336c246c3a647c2374d5ed9ecad42c8cd7fb886ee7f8aec4d14290d41"  # bin/vagrant (universal)


class VagrantAdapter(Adapter):
    name = "vagrant"
    title = "Vagrant (Docker provider)"
    transport = "host"
    isolation_boundary = "container"
    features = dict(
        lockfile="unsupported",          # no Vagrant lock; images.lock.json digests + uv.lock are project files
        frozen_setup="unsupported",
        services="native",               # multi-machine up/halt/destroy
        detached_services="native",      # containers keep running between commands
        readiness="scripted",            # provider waits for State.Running only; the app's `wait` checks SQL/Redis
        per_checkout_ports="native",     # per-checkout networks; no host ports
        per_checkout_data="scripted",    # named volumes created by vagrant-resources.sh
        stop_confirmation="native",      # halt returns after `docker stop`
        structured_status="native",      # `vagrant status --machine-readable`
        wrong_instance_guard="unsupported")
    not_applicable = {
        "occupied_port": "services publish no host ports; each checkout reaches pg:5432 and redis:6379 "
                         "on its own Docker network",
    }
    config_files = ("Vagrantfile", "Dockerfile", ".dockerignore", "images.lock.json", "vagrant-resources.sh")
    timeouts = dict(setup=600, start=1200, step=300, ready=120)
    setup_scope = ("`vagrant validate` only; network/volume creation, image pulls, the toolchain image "
                   "build and container creation happen in `vagrant up` (start)")
    cache_note = ("host Docker image/build cache not cleared; uv cache is run-owned and shared by the "
                  "run's checkouts (A fills it, B is warm)")
    pins = dict(vagrant=VAGRANT_VERSION, vagrant_dmg_sha256=VAGRANT_DMG_SHA256,
                vagrant_launcher_sha256=VAGRANT_LAUNCHER_SHA256, provider="docker (force_host_vm=false)",
                images="adapters/vagrant/images.lock.json", uv_lock="fixtures/app/uv.lock (uv sync --frozen)")

    def tools(self):
        return self.options.get("tools_dir") or posixpath.join(posixpath.dirname(self.workdir()), "tools")

    def host_env(self, workdir):
        tools = self.options.get("tools_dir") or str(workdir / "tools")
        return {**os.environ, "PATH": f"{tools}/vagrant/bin:{os.environ.get('PATH', '/usr/bin:/bin')}",
                "VAGRANT_HOME": str(workdir / "vagrant-home"), "VAGRANT_CHECKPOINT_DISABLE": "1",
                "VAGRANT_DEFAULT_PROVIDER": "docker", "VAGRANT_NO_COLOR": "1"}

    def provision(self):
        t = q(self.tools())
        url = f"https://releases.hashicorp.com/vagrant/{VAGRANT_VERSION}/vagrant_{VAGRANT_VERSION}_darwin_arm64.dmg"
        install = [
            f"mkdir -p {t}/dl {t}/mnt",
            f"curl -fsSL -o {t}/dl/vagrant.dmg {url}",
            f'echo "{VAGRANT_DMG_SHA256}  {t}/dl/vagrant.dmg" | shasum -a 256 -c -',
            # Private read-only mount; detached again whatever happens next.
            f"hdiutil attach -readonly -nobrowse -noautoopen -mountpoint {t}/mnt {t}/dl/vagrant.dmg >/dev/null",
            f"trap 'hdiutil detach {t}/mnt >/dev/null 2>&1 || true' EXIT",
            f"pkgutil --expand-full {t}/mnt/vagrant.pkg {t}/pkg",
            f"hdiutil detach {t}/mnt >/dev/null",
            "trap - EXIT",
            f"mv {t}/pkg/core.pkg/Payload {t}/vagrant",
            f"rm -rf {t}/pkg {t}/dl/vagrant.dmg",
        ] if not self.options.get("tools_dir") else []
        return [("provision-vagrant", darwin_arm64_preflight(
            "set -eu",
            *([] if self.options.get("tools_dir") else [
                'for c in hdiutil pkgutil; do command -v "$c" >/dev/null || '
                '{ echo "RWB-BLOCKED: $c (needed to unpack the official DMG privately) not found" >&2; exit 77; }; done']),
            *install,
            f'echo "{VAGRANT_LAUNCHER_SHA256}  {t}/vagrant/bin/vagrant" | shasum -a 256 -c -',
            f'test "$({t}/vagrant/bin/vagrant --version)" = "Vagrant {VAGRANT_VERSION}"'), None)]

    def versions(self):
        t = q(self.tools())
        return (f"set -e; {t}/vagrant/bin/vagrant --version; {t}/vagrant/embedded/bin/ruby --version; "
                "docker version --format 'docker {{.Client.Version}} / daemon {{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'; "
                f"shasum -a 256 {t}/vagrant/bin/vagrant")

    # ---- checkout ------------------------------------------------------------------------
    def instance_name(self, co):
        return f"rwb-{self.run_id}-{co.name}"

    def uv_cache(self):
        return posixpath.join(self.workdir(), ".uv-cache")

    def local_env(self, co):
        env = dict(RWB_INSTANCE=self.instance_name(co), RWB_RUN=self.run_id, RWB_CHECKOUT=co.name,
                   RWB_SRC=co.path, RWB_UV_CACHE=self.uv_cache())
        lines = "".join(f"export {k}={q(v)}\n" for k, v in env.items())
        return [f"mkdir -p {q(self.uv_cache())}",
                f"printf '%s' {q(lines)} > {q(co.path)}/vagrant.local.env"]

    def _in(self, co, body):
        return f"set -e; cd {q(co.path)} && . ./vagrant.local.env && {body}"

    def break_config(self, co):
        path = f"{q(co.path)}/images.lock.json"
        return (f"perl -pi -e 's{{\"postgres:17\\.6-alpine\\@sha256:[0-9a-f]+\"}}{{\"postgres:99.99.99-alpine\"}}' {path}\n"
                f"grep -q '\"postgres:99.99.99-alpine\"' {path}")

    # ---- the tool's own operations -----------------------------------------------------
    def setup(self, co):
        # Vagrant's Docker provider has no separate install step; validate the configuration.
        # Image pulls/builds happen in A's first `vagrant up` (cached for B).
        return self._in(co, "vagrant validate")

    def start(self, co):
        return self._in(co, "bash ./vagrant-resources.sh create && "
                            "vagrant up pg redis app --provider=docker --no-parallel")

    def status(self, co):
        return self._in(co, "vagrant status --machine-readable")

    def stop(self, co):
        return self._in(co, "vagrant halt pg redis")

    def cleanup(self, co):
        return self._in(co, "vagrant destroy -f app redis pg && bash ./vagrant-resources.sh remove")

    def enter(self, co, body):
        return self._in(co, f"vagrant docker-exec --no-prefix app -- bash -c {q(body)}")

    def tool_versions(self, co):
        return self._in(co, " && ".join([
            "vagrant docker-exec --no-prefix app -- bash -c 'command -v python3 uv; python3 --version; uv --version'",
            "vagrant docker-exec --no-prefix pg -- postgres --version",
            "vagrant docker-exec --no-prefix redis -- redis-server --version"]))

    def artifacts(self, co):
        return (".vagrant/machines",)  # machine IDs, matched against the inspected containers

    def instance_identity(self, co):
        return self._in(co, "bash ./vagrant-resources.sh identity")

    def stopped_probe(self, co, identity):
        n = self.instance_name(co)
        return (f"for i in $(seq 1 150); do "
                f"pg=$(docker inspect -f '{{{{.State.Running}}}}' {n}-pg) && "
                f"rd=$(docker inspect -f '{{{{.State.Running}}}}' {n}-redis) && "
                f'test "$pg/$rd" = false/false && exit 0; sleep 0.2; done; '
                f"echo '{n} service containers still running' >&2; exit 1")

    # ---- leftovers and host cleanup (only this run's labels/names) ---------------------
    def service_processes(self):
        return (f"docker ps --filter label=rwb.run={self.run_id} --filter label=rwb.service=1 "
                "--format '{{.ID}} {{.Names}} {{.Status}}'")

    def supervisor_processes(self):
        # Vagrant does not leave a supervisor running; report any stray launcher of this run.
        return f"set -o pipefail; ps -axo pid=,stat=,args= | awk -v t={q(self.tools() + '/vagrant/')} 'index($0, t) && !/awk/'"

    def host_resources(self):
        prefix = f"rwb-{self.run_id}-"
        return "\n".join([
            "set -eo pipefail",
            f"docker ps -a --filter label=rwb.run={self.run_id} --format 'container {{{{.Names}}}}'",
            f"docker volume ls -q --filter label=rwb.run={self.run_id} | awk '{{print \"volume \" $0}}'",
            f"docker network ls --filter label=rwb.run={self.run_id} --format 'network {{{{.Name}}}}'",
            f"docker images --format '{{{{.Repository}}}}:{{{{.Tag}}}} {{{{.ID}}}}' | awk 'index($0, \"{prefix}\") == 1 {{print \"image \" $0}}'",
        ])

    def cleanup_host(self):
        return remove_owned(self.host_resources())

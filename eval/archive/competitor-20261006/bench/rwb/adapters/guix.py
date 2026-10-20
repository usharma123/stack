"""GNU Guix: `guix time-machine` + `guix shell --pure` (bench/research/guix.md).

Guix pins the whole package graph through a channel commit (native lock: the
`guix describe -f channels` output generated in A). `guix shell` has no service lifecycle,
so PostgreSQL/Redis use the shared benchmark script, labelled scripted.

Versions at the pinned channel are Python 3.13.13, PostgreSQL 16.14, Redis 7.2.6, uv 0.10.12:
NOT the canonical PostgreSQL 17 / Redis 8 workload. Results carry that deviation.

Installation needs the official binary tarball, a store daemon and build users inside the
run's own container (adapters/guix/provision.sh). Missing prerequisites (no verified
tarball digest, unreachable download, denied daemon build sandbox) end provisioning with
exit 77 and a `RWB-BLOCKED:` line: an environment blocker, never a Guix failure.
"""
from .base import Adapter, q

GUIX_RELEASE = "1.5.0"
CHANNEL_COMMIT = "71d010188f039817c465985e46e185445fda6946"
DEFAULT_URL = f"https://ftpmirror.gnu.org/gnu/guix/guix-binary-{GUIX_RELEASE}.aarch64-linux.tar.xz"
EXPECTED = dict(python="3.13.13", postgresql="16.14", redis="7.2.6", uv="0.10.12")
PROVISION = "/rwb/src/adapters/guix/provision.sh"


class GuixAdapter(Adapter):
    name = "guix"
    title = f"GNU Guix {GUIX_RELEASE} (channel {CHANNEL_COMMIT[:7]})"
    image = "ev-base"
    features = dict(
        lockfile="native",            # channel commit -> generated channels.lock.scm
        frozen_setup="native",        # time-machine from the committed lock, nothing rewritten
        services="scripted", detached_services="scripted", readiness="scripted",
        per_checkout_ports="scripted", per_checkout_data="scripted",
        stop_confirmation="scripted", structured_status="scripted",
        wrong_instance_guard="unsupported")
    config_files = ("channels.scm", "manifest.scm")
    shared_files = ("rwb-env.sh", "rwb-services.sh")
    lock_files = ("channels.lock.scm",)
    timeouts = dict(setup=7200, start=300, step=600, ready=120)

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.url = self.options.get("guix_binary_url", DEFAULT_URL)
        self.sha256 = self.options.get("guix_binary_sha256", "")
        self.daemon_flags = self.options.get("guix_daemon_flags", "")
        self.title = f"GNU Guix {GUIX_RELEASE} (channel {CHANNEL_COMMIT[:7]})" + (
            f" daemon {self.daemon_flags}" if self.daemon_flags else "")
        self.pins = dict(guix_release=GUIX_RELEASE, channel=CHANNEL_COMMIT, binary_url=self.url,
                         binary_sha256=self.sha256 or "missing", daemon_flags=self.daemon_flags or "none",
                         **EXPECTED, deviation="PostgreSQL 16 and Redis 7 instead of 17 and 8")

    def _provision_env(self):
        return (f"export RWB_GUIX_URL={q(self.url)} RWB_GUIX_SHA256={q(self.sha256)} "
                f"RWB_GUIX_DAEMON_FLAGS={q(self.daemon_flags)}")

    def provision(self):
        return [(f"provision-guix-{step}", f"{self._provision_env()}; bash {PROVISION} {step}", user)
                for step, user in (("preflight", "root"), ("install", "root"), ("canary", None))]

    def env(self):
        return 'export PATH="/usr/local/bin:$PATH"'

    def versions(self):
        return (f"{self.env()}; set -e; guix --version | head -1; "
                f"echo channel {CHANNEL_COMMIT}; sha256sum {PROVISION}")

    def local_env(self, co):
        return self.bench_local_env(co)

    def tm(self, lock):
        return f"guix time-machine -q -C {lock} --"

    def shell(self, co, body, lock="channels.lock.scm"):
        inner = f"source ./rwb-env.sh && {body}"
        return (f"{self.env()}; cd {q(co.path)} && {self.tm(lock)} shell -q --pure -m manifest.scm -- "
                f"bash --noprofile --norc -c {q(inner)}")

    def enter(self, co, body):
        return self.shell(co, body)

    def _version_checks(self):
        return "set -e; " + "; ".join([
            f"python3 --version | grep -qx 'Python {EXPECTED['python']}'",
            f"postgres --version | grep -q ' {EXPECTED['postgresql']}$'",
            f"redis-server --version | grep -q 'v={EXPECTED['redis']} '",
            f"uv --version | grep -q '^uv {EXPECTED['uv']}'"])

    def setup(self, co):
        # Cold A resolves the channel and writes the lock; B/E arrive with A's lock.
        describe = (f"{self.env()}; cd {q(co.path)} && test -s channels.lock.scm || "
                    f"{{ {self.tm('channels.scm')} describe -f channels > channels.lock.scm.tmp && "
                    "mv channels.lock.scm.tmp channels.lock.scm; }")
        return f"( {describe} ) && {self.shell(co, self._version_checks())}"

    def frozen_setup(self, co):
        return f"test -s {q(co.path)}/channels.lock.scm && " + self.shell(co, self._version_checks())

    def break_config(self, co):
        return f"sed -i 's/postgresql@16.14/postgresql@99.99.99/' {q(co.path)}/manifest.scm"

    def start(self, co):
        return self.enter(co, "bash ./rwb-services.sh up")

    def status(self, co):
        return self.enter(co, "bash ./rwb-services.sh status")

    def stop(self, co):
        return self.enter(co, "bash ./rwb-services.sh down")

"""Stack (this repository). Linux ARM64 binary built from the checkout under test."""
import hashlib
from pathlib import Path

from .base import SHA256_FN, Adapter, q, sha256_check
from .common import MISE_VERSION, install_mise

DEFAULT_BINARY = "/tmp/stack-bench-build/target/release/stack"


class StackAdapter(Adapter):
    name = "stack"
    title = "Stack"
    image = "ev-base"
    features = dict(
        lockfile="native", frozen_setup="native", services="native", detached_services="native",
        readiness="native", per_checkout_ports="native", per_checkout_data="native",
        stop_confirmation="native", structured_status="native", wrong_instance_guard="native")
    config_files = ("stack.toml",)
    lock_files = ("stack.lock",)
    setup_scope = ("`stack compile` only: resolves versions, writes stack.lock and assigns ports. Tool and "
                   "service installation happens in `stack up` (start), so first_task is the comparable time")
    cache_note = ("fresh ev-base container: no mise/Stack tool cache; downloads in A's `up`, reused by B "
                  "(same container); pinned mise installed at provision (not timed)")

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        super().__init__(options, variant, run_id)
        self.binary = Path(self.options.get("stack_binary", DEFAULT_BINARY))
        self.expected_sha = self.options.get("stack_sha256")
        self.pins = dict(stack_binary=str(self.binary), mise=MISE_VERSION)
        if self.binary.exists():
            self.pins["stack_sha256"] = hashlib.sha256(self.binary.read_bytes()).hexdigest()
            if self.expected_sha and self.expected_sha != self.pins["stack_sha256"]:
                raise ValueError(f"stack binary hash {self.pins['stack_sha256']} != expected {self.expected_sha}")

    def mounts(self):
        return super().mounts() + [(self.binary.parent, "/rwb/stack-bin")]

    def env(self):
        return 'export PATH="$HOME/.local/bin:/rwb/stack-bin:$PATH" NO_COLOR=1 MISE_YES=1'

    def provision(self):
        check = sha256_check(self.pins.get("stack_sha256", "missing"), "/rwb/stack-bin/stack")
        return [("provision-mise", install_mise(), None), ("provision-stack-hash", check, None)]

    def versions(self):
        return f"{self.env()}; {SHA256_FN}; stack --version; mise --version; rwb_sha256 /rwb/stack-bin/stack \"$(command -v mise)\""

    def _in(self, co, body):
        return f"{self.env()}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        return f"sed -i 's/^version = \"17.11\"$/version = \"99.99.99\"/' {q(co.path)}/stack.toml"

    def setup(self, co):
        # compile resolves (cold A) or reuses committed pins (B) and assigns this checkout's
        # ports. Stack installs locked tools at `up` (the start step), not at compile/exec.
        return self._in(co, "stack --json compile")

    def frozen_setup(self, co):
        # Installing from the lock requires `up` in Stack; services are stopped again at once.
        return self._in(co, "stack --json compile --locked && stack --json up && stack --json down")

    def enter(self, co, body):
        return self._in(co, f"stack exec -- bash -c {q(body)}")

    def start(self, co):
        return self._in(co, "stack --json up")

    def ready(self, co):
        return self._in(co, "stack --json status")

    def status(self, co):
        return self._in(co, "stack --json status")

    def stop(self, co):
        return self._in(co, "stack --json down")

    def planned_pg_port(self, co):
        return self._in(co, "stack --json inspect | jq -er '.data.ports.postgres'")

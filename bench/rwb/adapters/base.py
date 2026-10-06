"""Adapter contract. One adapter per tool; it returns bash bodies, the scenario runs them.

A new competitor is one module here plus its checked-in configuration under
bench/adapters/<name>/, registered in registry.py. Everything an adapter does that is not
the tool's own documented feature must be declared `scripted` in FEATURES and live in that
config directory so reviewers can read it.
"""
from dataclasses import dataclass
from pathlib import Path
import shlex

BENCH = Path(__file__).resolve().parents[2]
q = shlex.quote

# Portable SHA-256 (GNU coreutils in Linux images, `shasum` on a macOS host transport).
SHA256_FN = ('rwb_sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; '
             'else shasum -a 256 "$@"; fi; }')


def sha256_check(expected, path):
    """Shell lines failing unless the file at path has the expected SHA-256 (any host)."""
    return (f'{SHA256_FN}; __rwb_h=$(rwb_sha256 {path} | cut -d" " -f1) && '
            f'if [ "$__rwb_h" != {q(expected)} ]; then echo "sha256 mismatch for {path}: $__rwb_h" >&2; exit 1; fi')

# Capabilities compared across tools. Values: native | scripted | unsupported.
FEATURES = {
    "lockfile": "Committed lock reproduces exact tool versions",
    "frozen_setup": "Setup that refuses to change the lock",
    "services": "Declares and supervises PostgreSQL/Redis",
    "detached_services": "Services keep running between independent commands",
    "readiness": "Waits until services accept application work",
    "per_checkout_ports": "Two checkouts get non-conflicting ports",
    "per_checkout_data": "Two checkouts get separate data directories",
    "stop_confirmation": "Stop returns only after services are gone",
    "structured_status": "Machine-readable service status",
    "wrong_instance_guard": "Refuses or poisons endpoints that are not this checkout's",
}


@dataclass
class Checkout:
    name: str           # a, b, c, d, e
    path: str           # absolute path inside the transport
    pg_port: int        # used only by adapters whose ports are configured, not assigned
    redis_port: int
    token: str = ""     # per-checkout source token; the app must report this exact value

    @property
    def instance(self):
        return f"rwb-{self.name}"


class Adapter:
    name = ""
    title = ""
    # "docker": every checkout of this tool shares ONE disposable container (one host and
    # network namespace, so port conflicts between checkouts are real), started from
    # `image` by the harness. "host": commands run on the host in a run-owned temp dir;
    # the adapter must name every resource it creates with `self.run_id` and remove them
    # in `cleanup_host()` (Compose projects, Dev Containers, DevPod workspaces, ...).
    transport = "docker"
    image = None
    # Variant selected with --tool name:variant (e.g. mise:worktree). First is default.
    variants = ("default",)
    # Scenario checks that do not apply to this tool's model -> not_applicable.
    not_applicable = {}
    # What separates two checkouts' data: service-instance | container | database
    # (see rwb.verify.BOUNDARIES). Declared, then verified with typed receipts.
    isolation_boundary = "service-instance"
    # Interpreter used for app commands inside enter(); Pixi overrides (no uv venv).
    app_python = '"${UV_PROJECT_ENVIRONMENT:-.venv}/bin/python"'
    user = "agent"
    home = "/home/agent"
    version_label = ""      # filled from the run (exact tool version string)
    features = {}
    # Files copied into every checkout from bench/adapters/<name>/ (relative paths).
    config_files = ()
    # Shared benchmark glue copied from bench/adapters/_shared/ (always `scripted` work).
    shared_files = ()
    # True when start() itself blocks until services pass the tool's readiness probes; the
    # first identity is then checked without retries, as for an explicit ready().
    start_waits_ready = False
    # Paths inside a checkout whose bytes form the reproducible lock.
    lock_files = ()
    # The canonical in-container location of the read-only bench tree.
    src = "/rwb/src"
    timeouts = dict(setup=1800, start=300, step=300)
    # What this adapter's setup() covers. Setup commands differ in scope between tools (Stack
    # `compile` only resolves; tools install at `up`), so the bare setup timing is never a
    # cross-tool comparison; the end-to-end first-task timing is. Recorded in meta/summary.
    setup_scope = ""
    # Cache/image state the run starts from (e.g. a prebuilt image with a warm Nix store).
    # "first checkout" means first in this run, not a universal cold install.
    cache_note = "image caches not cleared; first-checkout timings may include preseeded caches"
    # What prepare() does besides benchmark glue (fixture copy, config copy, source token).
    # first_task starts from a PREPARED checkout, so tool operations listed here (e.g. native
    # worktree creation) are excluded from it; their time is reported as prepare.<co>.
    prepare_scope = "benchmark glue only: fixture + tool config copy, source token"
    # What start() covers when it is more than launching processes (e.g. a PTY driver plus
    # readiness wait): reported with ready.<co> so it is not read as bare start latency.
    start_scope = ""
    # isola-style tools: stop ends the checkout's processes, but its data endpoints stay
    # reachable by design (shared servers), so the app is not required to fail after stop.
    stop_keeps_data_endpoints = False
    # DDEV-style entries that restart a stopped project: the after-stop app call is skipped
    # and stop is decided by the stop exit plus stopped_probe() alone.
    entry_auto_resumes = False
    # Output evidence that checkout D's setup/start refused the deliberately bad version
    # (an unrelated nonzero exit is never counted as the intended rejection).
    bad_config_pattern = r"99\.99\.99|postgresql_99"

    def __init__(self, options=None, variant=None, run_id="dryrun"):
        self.options = options or {}
        self.variant = variant or self.variants[0]
        if self.variant not in self.variants:
            raise ValueError(f"{self.name}: unknown variant {self.variant}; have {self.variants}")
        self.run_id = run_id
        if self.transport == "host":
            self.src = str(BENCH)
        missing = set(FEATURES) - set(self.features)
        if missing:
            raise TypeError(f"{self.name}: undeclared features {sorted(missing)}")
        for key, mode in self.features.items():
            if mode not in ("native", "scripted", "unsupported"):
                raise TypeError(f"{self.name}: bad mode {mode} for {key}")

    # ---- provisioning (recorded, never counted as tool setup) -------------------------
    def config_dir(self):
        return BENCH / "adapters" / self.name

    def mounts(self):
        """(host path, container path) pairs, mounted read-only."""
        return [(BENCH, self.src)]

    def provision(self):
        """[(label, body, user)] run once before versions; e.g. installing a pinned CLI."""
        return []

    def versions(self):
        raise NotImplementedError

    root = None  # host transport: the harness sets the run-owned temp directory here

    def workdir(self):
        return self.root or f"{self.home}/rwb"

    def checkout(self, name, index, token=""):
        return Checkout(name, f"{self.workdir()}/{name}", 25432 + index, 26379 + index, token)

    # ---- checkout preparation (meta phase) ------------------------------------------
    def prepare(self, co, lock_from=None):
        """Copy the fixture and this tool's config; optionally the committed lock from another
        checkout. Writes co's source token into the code (checked against the running app)."""
        lines = [f"set -eu", f"mkdir -p {q(co.path)}",
                 f"cp -R {self.src}/fixtures/app/. {q(co.path)}/",
                 f"rm -rf {q(co.path)}/.venv",
                 f"printf '%s\\n' {q(co.token)} > {q(co.path)}/rwbapp/SOURCE_TOKEN"]
        for rel in self.config_files:
            target = posixpath_dir(rel)
            if target:
                lines.append(f"mkdir -p {q(co.path)}/{q(target)}")
            lines.append(f"cp {self.src}/adapters/{self.name}/{q(rel)} {q(co.path)}/{q(rel)}")
        for rel in self.shared_files:
            lines.append(f"cp {self.src}/adapters/_shared/{q(rel)} {q(co.path)}/{q(rel)}")
        lines += self.local_env(co)
        if lock_from is not None:
            for rel in self.lock_files:
                lines.append(f"cp {q(lock_from.path)}/{q(rel)} {q(co.path)}/{q(rel)}")
        return "\n".join(lines)

    def local_env(self, co):
        """Machine-local, uncommitted per-checkout settings (ports), as shell lines."""
        return []

    def bench_local_env(self, co):
        """Common helper: write bench.local.env (PGPORT, REDIS_PORT, RWB_INSTANCE)."""
        return [f"printf 'PGPORT=%s\\nREDIS_PORT=%s\\nRWB_INSTANCE=%s\\n' {co.pg_port} {co.redis_port} "
                f"{co.instance} > {q(co.path)}/bench.local.env"]

    def break_config(self, co):
        """Shell lines that make the checkout request a version that does not exist."""
        raise NotImplementedError

    # ---- the tool's own operations ----------------------------------------------------
    def setup(self, co):
        """Resolve/lock and install tools. Cold for A, warm caches for B."""
        raise NotImplementedError

    def frozen_setup(self, co):
        """Install from the committed lock and refuse to change it; None if unsupported."""
        return None

    def enter(self, co, body):
        """Run body inside the tool's environment for checkout co."""
        raise NotImplementedError

    def start(self, co):
        return None

    def ready(self, co):
        """Native readiness command, or None (the scenario then runs the app's `wait`)."""
        return None

    def status(self, co):
        return None

    def stop(self, co):
        return None

    def cleanup(self, co):
        """Best-effort, scoped teardown at the end; defaults to stop."""
        return self.stop(co)

    def app_source_path(self, co):
        """Where co's code is seen by the app process; None skips the path check (the
        source token is always checked)."""
        return self.app_dir(co)

    def instance_identity(self, co):
        """Optional body printing one JSON object that identifies co's service instances
        beyond what the app reports (container IDs, volume names). Compared across A/B."""
        return None

    def host_env(self, workdir):
        """Host transport only: environment for every host command (None = inherit)."""
        return None

    def host_resources(self):
        """Host transport only: body listing resources still owned by this run (empty = clean)."""
        return None

    def cleanup_host(self):
        """Host transport only: body removing every resource named with self.run_id."""
        return None

    def service_processes(self):
        """Body listing PIDs+args of leftover PostgreSQL/Redis server processes (non-zombie)."""
        return ("ps -eo pid=,user=,stat=,args= | awk '$2==\"agent\" && $3 !~ /^Z/' | "
                "grep -E '(postgres|redis-server)( |$)' | grep -v -E 'grep|awk' || true")

    def supervisor_processes(self):
        """Body listing leftover supervisors/managers (reported as observed, not a leak)."""
        return ("ps -eo pid=,user=,stat=,args= | awk '$2==\"agent\" && $3 !~ /^Z/' | "
                "grep -E 'process-compose|pitchfork|devenv|flox-activations|nix-daemon' | grep -v -E 'grep|awk' || true")

    def planned_pg_port(self, co):
        """Body printing the PostgreSQL port co WILL use (last stdout line), for tools that
        assign ports themselves. None: the configured co.pg_port is used."""
        return None

    def artifacts(self, co):
        """Checkout-relative paths (files or directories, e.g. service logs) copied into the
        result directory at the end of the run, before teardown. Missing paths are skipped."""
        return ()

    def diagnostics(self, co):
        """[(name, body)] run for each started checkout before teardown (e.g. `compose logs`).
        Output is kept as raw step logs; exit codes never change an outcome."""
        return []

    def conflict_logs(self, co):
        """Optional body printing the tool's own service logs/status after a failed scripted
        readiness in the occupied-port case (e.g. `flox services logs`). A readiness failure
        counts as detection only if this output (or the wait's) names the conflict."""
        return None

    def port_map(self, co):
        """Optional body printing a JSON port-mapping receipt, for tools whose app reaches
        services through NAT (a host app on a Docker published port). Shape:
        {"pg": {"published": P, "target": T}, "redis": {...}, "evidence": "<container id>"}
        built from `docker inspect`/`docker port` of the checkout's own containers. Without
        it, the URL port must equal the port the server reports. Run after each identity."""
        return None

    def conflict_pattern(self, port):
        """Regex that a start/readiness failure's output must match to count as detecting
        the occupied port (default: the port number or an address-in-use message)."""
        from rwb.verify import conflict_pattern
        return conflict_pattern(port)

    def stopped_probe(self, co, identity):
        """Body that exits 0 once co's services are gone. Default: its URL ports refuse.
        Shared-server (database boundary) or container adapters override this."""
        ports = [identity["pg"]["port"], identity["redis"]["port"]]
        return " && ".join(f"({self.port_closed(p)})" for p in ports)

    def port_closed(self, port):
        return (f"for i in $(seq 1 150); do (exec 3<>/dev/tcp/127.0.0.1/{int(port)}) 2>/dev/null || exit 0; "
                f"sleep 0.2; done; echo 'port {int(port)} still accepting' >&2; exit 1")

    def occupy(self, port, pidfile, tag):
        """Start a benchmark-owned listener on port (bad-startup scenario). Its argv[0] is the
        run-unique tag so cleanup can confirm ownership before signalling the recorded pid."""
        listener = (f"if command -v nc >/dev/null 2>&1; then exec -a {q(tag)} nc -lk 127.0.0.1 {int(port)}; "
                    f"else exec -a {q(tag)} python3 -c 'import socket,sys,time; s=socket.socket(); "
                    f"s.bind((\"127.0.0.1\", int(sys.argv[1]))); s.listen(16); time.sleep(86400)' {int(port)}; fi")
        return (f"set -e; mkdir -p \"$(dirname {q(pidfile)})\"; test ! -e {q(pidfile)}; "
                f"launch=nohup; command -v setsid >/dev/null 2>&1 && launch=setsid; "
                f"$launch bash -c {q(listener)} >/dev/null 2>&1 < /dev/null & echo $! > {q(pidfile)}; set +e; "
                f"for i in $(seq 1 50); do (exec 3<>/dev/tcp/127.0.0.1/{int(port)}) 2>/dev/null && break; sleep 0.1; done; "
                # The port answering is not enough (someone else may hold it): our listener must
                # still be alive, i.e. it won the bind.
                f"sleep 0.3; kill -0 \"$(cat {q(pidfile)})\" 2>/dev/null && "
                f"(exec 3<>/dev/tcp/127.0.0.1/{int(port)}) 2>/dev/null || "
                f"{{ echo 'benchmark listener did not take port {int(port)}' >&2; exit 1; }}")

    def release(self, pidfile, tag, port=None):
        """Stop the listener recorded in pidfile only if that pid still runs our tagged argv.

        `port` (legacy adapters whose occupy() takes no tag): the pid's argv must instead name
        that port; the pid file itself is run-unique, so a reused pid is still refused."""
        owned = f"{q(tag)}|{q(tag)}\\ *" + (f"|*\\ {int(port)}|*\\ {int(port)}\\ *" if port else "")
        return (f"test -f {q(pidfile)} || exit 0; pid=$(cat {q(pidfile)}); "
                f"args=$(ps -o args= -p \"$pid\" 2>/dev/null) || {{ rm -f {q(pidfile)}; echo \"listener $pid already gone\"; exit 0; }}; "
                f"case \"$args\" in {owned}) ;; *) echo \"pid $pid is not our listener ($args); not signalled\"; rm -f {q(pidfile)}; exit 0;; esac; "
                f"kill \"$pid\"; for i in $(seq 1 50); do kill -0 \"$pid\" 2>/dev/null || break; sleep 0.1; done; "
                f"if kill -0 \"$pid\" 2>/dev/null; then echo \"listener $pid survived SIGTERM\" >&2; exit 1; fi; "
                f"rm -f {q(pidfile)}; echo \"released listener $pid\"")

    # ---- application commands, identical for every tool ------------------------------
    def app_dir(self, co):
        """Directory holding co's code as seen inside enter() (a container path for Compose)."""
        return co.path

    def app(self, co, args):
        return self.enter(co, f"cd {q(self.app_dir(co))} && RWB_CHECKOUT={co.name} {self.app_python} -m rwbapp {args}")

    def deps(self, co):
        return self.enter(co, f'cd {q(self.app_dir(co))} && uv sync --frozen --python "$(command -v python3)"')

    def pytest(self, co):
        return self.enter(co, f"cd {q(self.app_dir(co))} && RWB_CHECKOUT={co.name} {self.app_python} -m pytest -q -p no:cacheprovider")

    def tool_versions(self, co):
        return self.enter(co, "set -e; command -v python3 uv postgres redis-server; python3 --version; "
                              "uv --version; postgres --version; redis-server --version")


def posixpath_dir(rel):
    return rel.rsplit("/", 1)[0] if "/" in rel else ""

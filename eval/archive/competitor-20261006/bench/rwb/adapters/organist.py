"""Organist (Nickel over Nix flakes), pinned to current main. See bench/research/organist.md.

Organist declares the packages (Python 3.13, uv, PostgreSQL 17, Redis 8) and the services
natively. `config.services` compiles to a Procfile that `nix run .#start-services` runs under
Honcho. Honcho is a foreground supervisor with no detach, status or readiness command and
no port or data allocation. The holder, data layout, ports and readiness are therefore
benchmark scripts (organist-holder.sh, organist-services.sh, _shared/rwb-env.sh), declared
`scripted`.

Each checkout is a git repository, as in real use. Nix evaluates only tracked files, so
runtime state (.rwb-state, .venv, bench.local.env) stays out of the flake source. The
committed locks are flake.lock (native Nix lock) and nickel.lock.ncl (Organist's generated
Nickel import file). Both are produced in A and committed into B, C and E.
"""
from .base import Adapter, q

ORGANIST_REV = "a7e4e638cade5e7c4f36a129b80d91bf3538088e"   # main, 2025-08-12 (only tag v0.1 is obsolete)
NIXPKGS_REV = "151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4"
GITIGNORE = ".rwb-state/\n.venv/\nbench.local.env\n__pycache__/\n.pytest_cache/\n"
NIX_CONFIG = "lazy-trees = false"
GIT = "git -c user.name=rwb -c user.email=rwb@invalid -c init.defaultBranch=main -c commit.gpgsign=false"


class OrganistAdapter(Adapter):
    name = "organist"
    title = "Organist"
    image = "ev-nix"
    features = dict(
        lockfile="native",               # flake.lock (+ generated nickel.lock.ncl)
        frozen_setup="native",           # nix --no-update-lock-file refuses lock changes
        services="native",               # config.services -> Procfile -> Honcho
        detached_services="scripted",    # Honcho is foreground-only; organist-holder.sh keeps it
        readiness="scripted",            # no readiness probe; the app's retrying `wait`
        per_checkout_ports="scripted",   # static ports in the uncommitted bench.local.env
        per_checkout_data="scripted",    # checkout-local .rwb-state via rwb-env.sh
        stop_confirmation="scripted",    # holder signals Honcho, waits, then checks ports
        structured_status="scripted",    # holder status JSON
        wrong_instance_guard="unsupported")
    config_files = ("flake.nix", "project.ncl", "organist-services.sh", "organist-holder.sh")
    shared_files = ("rwb-env.sh",)
    lock_files = ("flake.lock", "nickel.lock.ncl")
    timeouts = dict(setup=3600, start=600, step=300, ready=90)
    setup_scope = ("flake lock + Organist's Nickel lock regeneration + realising the dev shell and the "
                   "Honcho services app (all tool installation); services start in start()")
    cache_note = "ev-nix image's Nix store as built; substitutes fetched in A are reused by B (same store)"
    pins = dict(organist=f"github:nickel-lang/organist/{ORGANIST_REV} (main; no current release)",
                nixpkgs=f"github:NixOS/nixpkgs/{NIXPKGS_REV}", runner="honcho (from organist's services module)",
                nix_config=NIX_CONFIG)

    def provision(self):
        # ev-nix runs the Nix daemon from its entrypoint; nothing is installed here.
        return [("provision-organist-preflight", "\n".join([
            'command -v nix >/dev/null || { echo "RWB-BLOCKED: nix not on PATH in image" >&2; exit 77; }',
            'nix store info >/dev/null || { echo "RWB-BLOCKED: Nix daemon store not reachable" >&2; exit 77; }',
            "nix --version",
        ]), None)]

    def versions(self):
        return (f"set -e; nix --version; git --version; echo organist={ORGANIST_REV} nixpkgs={NIXPKGS_REV}")

    # ---- checkout: a git repository with runtime state ignored --------------------------
    def local_env(self, co):
        return self.bench_local_env(co)

    def artifacts(self, co):
        return (".rwb-state/logs",)  # Honcho's multiplexed service log

    def prepare(self, co, lock_from=None):
        return "\n".join([
            super().prepare(co, lock_from),
            f"cd {q(co.path)}",
            f"printf '%s' {q(GITIGNORE)} > .gitignore",
            f"{GIT} init -q && {GIT} add -A && {GIT} commit -q -m 'checkout {co.name}'",
        ])

    def _in(self, co, body):
        # Compatibility setting, process-scoped: ev-nix's Determinate Nix enables lazy trees.
        # Organist's lock generator then embeds an unmaterialised source path and
        # `regenerate-lockfile` fails with "path '/nix/store/...-source' is not valid"
        # (observed in a disposable ev-nix container, 2026-10-06).
        return f"set -eo pipefail; export NIX_CONFIG={q(NIX_CONFIG)}; cd {q(co.path)} && {body}"

    def break_config(self, co):
        path = f"{q(co.path)}/project.ncl"
        return (f"sed -i 's/nixpkgs#postgresql_17/nixpkgs#postgresql_99/g' {path}\n"
                f"grep -q 'nixpkgs#postgresql_99' {path}")

    # ---- the tool's own operations -----------------------------------------------------
    def setup(self, co):
        # Resolve (A) or reuse the committed lock (B, E); generate Organist's Nickel import
        # file; realise the dev shell and the Honcho services app (validates the Procfile).
        return self._in(co, " && ".join([
            "nix flake lock",
            f"{GIT} add flake.lock",
            "nix run .#regenerate-lockfile",
            f"{GIT} add nickel.lock.ncl",
            "{ nix flake metadata --json --no-update-lock-file | tr -d '\\n'; echo; }",
            "nix develop --no-update-lock-file -c true",
            "nix run --no-update-lock-file .#start-services -- check",
        ]))

    def frozen_setup(self, co):
        return self._in(co, " && ".join([
            "nix run --no-update-lock-file .#regenerate-lockfile",
            "nix develop --no-update-lock-file -c true",
            "nix run --no-update-lock-file .#start-services -- check",
        ]))

    def enter(self, co, body):
        return self._in(co, f"nix develop --no-update-lock-file -c bash -c {q('source ./rwb-env.sh && ' + body)}")

    def start(self, co):
        return self._in(co, "bash ./organist-holder.sh start")

    def status(self, co):
        return self._in(co, "bash ./organist-holder.sh status")

    def stop(self, co):
        return self._in(co, "bash ./organist-holder.sh stop")

    def supervisor_processes(self):
        return ("ps -eo pid=,user=,stat=,args= | awk '$2==\"agent\" && $3 !~ /^Z/' | "
                "grep -E 'honcho|start-services' | grep -v -E 'grep|awk' || true")

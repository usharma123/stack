"""name -> adapter class. Adding a competitor: write rwb/adapters/<name>.py, put its config in
bench/adapters/<name>/, add one line here, and run `python3 -m unittest discover -s bench/tests`
(test_adapter_contract checks every registered adapter)."""
import importlib

ADAPTERS = {
    "stack": "rwb.adapters.stack:StackAdapter",
    "mise": "rwb.adapters.mise:MiseAdapter",
    "flox": "rwb.adapters.flox:FloxAdapter",
    "devbox": "rwb.adapters.devbox:DevboxAdapter",
    "devenv": "rwb.adapters.devenv:DevenvAdapter",
    "nix": "rwb.adapters.nix:NixAdapter",
    "pixi": "rwb.adapters.pixi:PixiAdapter",
    "compose": "rwb.adapters.compose:ComposeAdapter",
    # Owned by the container-adapter agent (see CONTAINER-ADAPTERS.md).
    "devcontainers": "rwb.adapters.devcontainers:DevcontainersAdapter",
    "devpod": "rwb.adapters.devpod:DevpodAdapter",
    "ddev": "rwb.adapters.ddev:DdevAdapter",
    "lando": "rwb.adapters.lando:LandoAdapter",
    # Owned by the native-extra adapter agent (see NATIVE-EXTRA-ADAPTERS.md).
    "process-compose": "rwb.adapters.process_compose:ProcessComposeAdapter",
    "services-flake": "rwb.adapters.services_flake:ServicesFlakeAdapter",
    "pkgx": "rwb.adapters.pkgx:PkgxAdapter",
    "dnvr": "rwb.adapters.dnvr:DnvrAdapter",
    "guix": "rwb.adapters.guix:GuixAdapter",
    # Owned by the worktree adapter agent (see WORKTREE-ADAPTERS.md).
    "workz": "rwb.adapters.workz:WorkzAdapter",
    "worktrunk": "rwb.adapters.worktrunk:WorktrunkAdapter",
    "git-grove": "rwb.adapters.git_grove:GitGroveAdapter",
    # Owned by the agent-environment adapter agent (see AGENT-ENV-ADAPTERS.md).
    "isola": "rwb.adapters.isola:IsolaAdapter",
    "berth": "rwb.adapters.berth:BerthAdapter",
    "branchbox": "rwb.adapters.branchbox:BranchboxAdapter",
    # Owned by the final extra adapter agent.
    "tilt": "rwb.adapters.tilt:TiltAdapter",
    "organist": "rwb.adapters.organist:OrganistAdapter",
    "vagrant": "rwb.adapters.vagrant:VagrantAdapter",
}


def load(name):
    """'tool' or 'tool:variant' -> (adapter class, variant or None)."""
    tool, _, variant = name.partition(":")
    if tool not in ADAPTERS:
        raise KeyError(f"unknown tool {tool!r}; registered: {', '.join(sorted(ADAPTERS))}")
    module, _, cls = ADAPTERS[tool].partition(":")
    return getattr(importlib.import_module(module), cls), variant or None


def available():
    """Registered adapters whose module exists (others are still being written)."""
    found = {}
    for name in ADAPTERS:
        try:
            found[name] = load(name)[0]
        except ModuleNotFoundError:
            continue
    return found

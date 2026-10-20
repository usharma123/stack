"""Write a run-private Docker client config: the user's daemon context, no credentials.

Usage: python3 -I docker_client_config.py SRC_DIR DST_DIR
       python3 -I docker_client_config.py --seal DST_DIR MARKER "<context name> <endpoint>"
       python3 -I docker_client_config.py --verify DST_DIR MARKER

The host's Docker Desktop credential helper (`credsStore: desktop`) fails non-interactively
("error getting credentials - err: exit status 1"), which blocked DDEV's and Lando's public
image pulls. This reuses ComposeAdapter's reviewed approach: a private client config holding
only the current context name, so pulls are anonymous and go to the same daemon.

Kept from SRC: `currentContext`, `cliPluginsExtraDirs`, plus SRC/cli-plugins as an extra
plugin directory (referenced in place, so `docker compose`/`buildx` resolve as for the user)
and the active context's metadata directory under contexts/meta only. Never copied: auths,
credsStore, credHelpers, any other key, other contexts, or any contexts/tls material. SRC is
only read. Only key names are printed, never values of dropped keys.

Fails closed (exit 77, `RWB-BLOCKED:`) before anything is written when the daemon selection
cannot be reproduced without credentials: an inherited DOCKER_HOST/DOCKER_CONTEXT/TLS
variable (Lando strips them, so its Compose would use another daemon), or an active context
with TLS material or SkipTLSVerify (without the omitted certificates docker/cli silently falls
back to plain HTTP or unauthenticated TLS: cli/context/docker/load.go, moby client.go).

--seal runs only after the preflight validated the selection (user and private configs resolve
the same context endpoint, plus Lando's engine-socket check). It records that endpoint and the
digest of DST's selection files (config.json, contexts/**) in MARKER, outside DST. --verify,
the first line of every later body, is silent when MARKER exists and DST still resolves the
sealed endpoint with identical files; otherwise exit 77 before any docker call. Without it an
empty or missing DST after a rejected preflight makes docker/cli fall back to the default
socket, so receipts and cleanup would query (and certify) a daemon that was never validated.
"""
import glob
import hashlib
import json
import os
import shutil
import sys

KEPT = ("currentContext", "cliPluginsExtraDirs")
# Daemon/TLS selection the private config cannot carry (docker/cli flags.go / cli.go).
ENV_OVERRIDES = ("DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_TLS", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH")


def blocked(reason):
    print(f"RWB-BLOCKED: {reason}", file=sys.stderr)
    raise SystemExit(77)


def active_context(src, name):
    """(meta dir id, metadata) of context `name`, rejecting TLS-bearing contexts."""
    meta_root = os.path.join(src, "contexts", "meta")
    found = None
    for ident in sorted(os.listdir(meta_root)) if os.path.isdir(meta_root) else ():
        try:
            with open(os.path.join(meta_root, ident, "meta.json")) as fh:
                data = json.load(fh)
        except (OSError, ValueError):
            continue
        if isinstance(data, dict) and data.get("Name") == name:
            found = ident, data
            break
    if found is None:
        raise SystemExit(f"Docker context {name!r} has no metadata under {meta_root}")
    ident, data = found
    tls = os.path.join(src, "contexts", "tls", ident)
    if os.path.isdir(tls) and any(files for _, _, files in os.walk(tls)):
        blocked(f"Docker context {name!r} uses TLS material, which the private config does not copy; "
                "unsupported (use a local socket context)")
    endpoints = data.get("Endpoints") if isinstance(data.get("Endpoints"), dict) else {}
    if any(isinstance(ep, dict) and ep.get("SkipTLSVerify") for ep in endpoints.values()):
        blocked(f"Docker context {name!r} sets SkipTLSVerify; unsupported (use a local socket context)")
    return ident


def build(src, dst):
    present = [v for v in ENV_OVERRIDES if v in os.environ]
    if present:
        blocked(f"{','.join(present)} set; only the persisted currentContext is supported (values not shown)")
    src, dst = os.path.realpath(src), os.path.realpath(dst)
    if src == dst or dst.startswith(src + os.sep):
        raise SystemExit(f"refusing to write the private Docker config inside the user's config {src}")
    try:
        with open(os.path.join(src, "config.json")) as fh:
            user = json.load(fh)
    except FileNotFoundError:
        user = {}
    if not isinstance(user, dict):
        raise SystemExit("user Docker config.json is not an object")
    out = {}
    if isinstance(user.get("currentContext"), str) and user["currentContext"]:
        out["currentContext"] = user["currentContext"]
    name = out.get("currentContext", "default")
    # The built-in default context has no stored metadata or TLS (env overrides rejected above).
    ident = None if name == "default" else active_context(src, name)
    dirs = [d for d in user.get("cliPluginsExtraDirs") or [] if isinstance(d, str)]
    plugins = os.path.join(src, "cli-plugins")
    if os.path.isdir(plugins) and plugins not in dirs:
        dirs.append(plugins)
    if dirs:
        out["cliPluginsExtraDirs"] = dirs
    os.makedirs(dst, exist_ok=True)
    if ident is not None:
        shutil.copytree(os.path.join(src, "contexts", "meta", ident),
                        os.path.join(dst, "contexts", "meta", ident), dirs_exist_ok=True)
    tmp = os.path.join(dst, "config.json.part")
    with open(tmp, "w") as fh:
        json.dump(out, fh, indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, os.path.join(dst, "config.json"))
    dropped = sorted(k for k in user if k not in KEPT)
    print(f"private docker config {dst}: currentContext={name} "
          f"cliPluginsExtraDirs={len(dirs)} context-metadata={0 if ident is None else 1} "
          f"dropped-keys={','.join(dropped) or '(none)'} tls-material=none-in-active-context,not-copied")
    return out


def selection_files(dst):
    """{relative path: sha256} of the files that decide daemon selection in DST."""
    out = {}
    for rel in ["config.json"] + sorted(
            os.path.relpath(os.path.join(d, f), dst)
            for d, _, files in os.walk(os.path.join(dst, "contexts")) for f in files):
        path = os.path.join(dst, rel)
        if os.path.islink(path):
            out[rel] = "link:" + os.readlink(path)
            continue
        try:
            with open(path, "rb") as fh:
                out[rel] = hashlib.sha256(fh.read()).hexdigest()
        except OSError:
            out[rel] = None
    return out


def resolved_context(dst):
    """(context name, endpoint or None for the built-in default) DST's config selects."""
    with open(os.path.join(dst, "config.json")) as fh:
        config = json.load(fh)
    if not isinstance(config, dict):
        raise ValueError("config.json is not an object")
    name = config.get("currentContext") or "default"
    if not isinstance(name, str):
        raise ValueError("currentContext is not a string")
    if name == "default":
        return name, None
    data = None
    for path in sorted(glob.glob(os.path.join(dst, "contexts", "meta", "*", "meta.json"))):
        with open(path) as fh:
            meta = json.load(fh)
        if isinstance(meta, dict) and meta.get("Name") == name:
            data = meta
    endpoint = ((data or {}).get("Endpoints") or {}).get("docker")
    host = endpoint.get("Host") if isinstance(endpoint, dict) else None
    if not isinstance(host, str) or not host:
        raise ValueError(f"context {name!r} has no stored docker endpoint")
    return name, host


def matches(context, resolved):
    parts = context.split()
    name, host = resolved
    return len(parts) == 2 and parts[0] == name and (host is None or parts[1] == host)


def seal(dst, marker, context):
    dst = os.path.realpath(dst)
    files = selection_files(dst)
    if files["config.json"] is None or any(v is None for v in files.values()):
        raise SystemExit(f"cannot seal: incomplete private Docker config {dst}")
    if any(rel.split(os.sep)[:2] == ["contexts", "tls"] for rel in files):
        blocked("private Docker config holds TLS material; refusing to seal it")
    if not matches(context, resolved_context(dst)):
        raise SystemExit(f"cannot seal: validated context [{context}] is not what {dst} selects")
    tmp = marker + ".part"
    with open(tmp, "w") as fh:
        json.dump({"docker_config": dst, "context": context, "files": files}, fh, indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, marker)
    print(f"docker daemon selection sealed: [{context}] {marker}")


def verify(dst, marker):
    """Silent on success: bodies' stdout is often a parsed receipt."""
    dst = os.path.realpath(dst)
    try:
        with open(marker) as fh:
            sealed = json.load(fh)
    except FileNotFoundError:
        blocked("no validated Docker daemon selection for this run (preflight did not complete it); "
                "no docker call made")
    except (OSError, ValueError):
        blocked(f"unreadable Docker daemon selection marker {marker}; no docker call made")
    try:
        ok = (isinstance(sealed, dict) and sealed.get("docker_config") == dst
              and isinstance(sealed.get("context"), str) and isinstance(sealed.get("files"), dict)
              and sealed["files"].get("config.json") and sealed["files"] == selection_files(dst)
              and matches(sealed["context"], resolved_context(dst)))
    except (OSError, ValueError, AttributeError):
        ok = False
    if not ok:
        blocked(f"private Docker config {dst} no longer matches the validated daemon selection; "
                "no docker call made")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--seal"] and len(sys.argv) == 5:
        seal(*sys.argv[2:])
    elif sys.argv[1:2] == ["--verify"] and len(sys.argv) == 4:
        verify(*sys.argv[2:])
    elif len(sys.argv) == 3 and not sys.argv[1].startswith("--"):
        build(sys.argv[1], sys.argv[2])
    else:
        raise SystemExit("usage: docker_client_config.py SRC_DIR DST_DIR | --seal DST MARKER CONTEXT | --verify DST MARKER")

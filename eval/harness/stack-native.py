#!/usr/bin/env python3
"""Native macOS smoke benchmark in isolated HOME and XDG directories.

Usage: python3 eval/harness/stack-native.py PACKAGE_DIR MISE_BINARY WORK_DIR RESULTS_DIR
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time

root = Path(__file__).resolve().parents[2]
package, mise, work, results = [Path(p).resolve() for p in sys.argv[1:]]
work.mkdir(parents=True, exist_ok=False)
results.mkdir(parents=True, exist_ok=True)
home = work / "home"
home.mkdir()
env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
           XDG_CACHE_HOME=str(home / ".cache"), XDG_DATA_HOME=str(home / ".local/share"),
           XDG_STATE_HOME=str(home / ".local/state"),
           PATH=f"{mise.parent}:{package / 'binaries/darwin-arm64'}:{os.environ['PATH']}")
binary = package / "binaries/darwin-arm64/stack"
info = json.loads((package / "build-info.json").read_text())
assert hashlib.sha256(binary.read_bytes()).hexdigest() == info["hashes"]["darwin-arm64"]
fixture = root / "eval/fixtures" / ("stack-" + info["version"])
shutil.copytree(fixture / "examples/bundles", work / "bundles")
for name in ("appA", "appB"):
    shutil.copytree(root / "eval/fixture", work / name)
    shutil.copy(fixture / "examples/app/stack.toml", work / name / "stack.toml")
records = []

def step(label, app, args, expected=0):
    start = time.monotonic()
    p = subprocess.run(args, cwd=work / app, env=env, stdin=subprocess.DEVNULL,
                       capture_output=True, text=True, timeout=900)
    records.append({"step": label, "exit_code": p.returncode,
                    "ms": round((time.monotonic() - start) * 1000, 2)})
    (results / (label + ".log")).write_text(p.stdout + p.stderr)
    (results / "steps.json").write_text(json.dumps(records, indent=2) + "\n")
    print(json.dumps(records[-1]), flush=True)
    assert p.returncode == expected, p.stdout + p.stderr
    return p.stdout

try:
    step("versions", "appA", ["sh", "-c", "stack --version; mise --version; sw_vers"])
    sessions = []
    for app in ("appA", "appB"):
        step(app + "-compile", app, ["stack", "compile", "--json"])
        up = json.loads(step(app + "-up", app, ["stack", "up", "--json"]))
        assert up["ok"] and all(c["ready"] and c["identity"] == "instance" for c in up["data"]["checks"])
        sessions.append(up["data"]["session"])
        step(app + "-tests", app, ["stack", "exec", "--require-all", "--", "bash", "-c", "set -e; uv sync -q; uv run pytest -q; mise run seed; acme"])
        actual = step(app + "-identity", app, ["stack", "exec", "--require", "postgres", "--", "bash", "-c", 'psql "$DATABASE_URL" -Atc "show data_directory"']).strip()
        assert actual == sessions[-1]["services"]["postgres"]["data_dir"]
    for service in ("postgres", "redis"):
        assert sessions[0]["services"][service]["port"] != sessions[1]["services"][service]["port"]
    for i in range(10):
        step(f"exec-{i}", "appA", ["stack", "exec", "--require-all", "--", "true"])
    step("idempotent-up", "appA", ["stack", "up", "--json"])
    down = json.loads(step("appA-down", "appA", ["stack", "down", "--json"]))
    assert down["ok"] and down["data"]["confirmed"]
    step("appB-still-works", "appB", ["stack", "exec", "--require-all", "--", "uv", "run", "pytest", "-q"])
    output = step("withheld-env", "appA", ["stack", "exec", "--", "bash", "-c", 'printf "%s" "$DATABASE_URL"'])
    assert "unverified.stack.invalid" in output
    refused = json.loads(step("required-refusal", "appA", ["stack", "exec", "--require-all", "--json", "--", "true"], expected=1))
    assert not refused["ok"] and refused["error"]["code"] == "service_unavailable"
    down = json.loads(step("appB-down", "appB", ["stack", "down", "--json"]))
    assert down["ok"] and down["data"]["confirmed"]
    for session in sessions:
        for service in session["services"].values():
            try:
                os.kill(service["pid"], 0)
                raise AssertionError(f"Service process still alive: {service['pid']}")
            except ProcessLookupError:
                pass
    (results / "summary.json").write_text(json.dumps({"ok": True, "platform": "darwin-arm64", "version": info["version"], "commit": info["commit"], "exec_median_ms": statistics.median(r["ms"] for r in records if r["step"].startswith("exec-")), "service_pids_gone": True}, indent=2) + "\n")
finally:
    for app in ("appA", "appB"):
        subprocess.run(["stack", "down", "--json"], cwd=work / app, env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
    subprocess.run(["mise", "exec", "--", "pitchfork", "supervisor", "stop"], cwd=work / "appA", env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)

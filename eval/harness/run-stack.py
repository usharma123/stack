#!/usr/bin/env python3
"""Benchmark an unpacked published npm package; never build the working tree.

Usage: python3 eval/harness/run-stack.py PACKAGE_DIR RESULTS_DIR
Requires the existing ev-mise image and registry:2 image.
"""
import hashlib
import json
import pathlib
import subprocess
import sys
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[2]
package, results = (pathlib.Path(p).resolve() for p in sys.argv[1:])
results.mkdir(parents=True, exist_ok=True)
(results / "logs").mkdir(exist_ok=True)
info = json.loads((package / "build-info.json").read_text())
fixture = ROOT / "eval/fixtures" / ("stack-" + info["version"])
if not fixture.is_dir():
    raise SystemExit("No preserved release fixture for " + info["version"])
arch = subprocess.check_output(["docker", "info", "--format", "{{.Architecture}}"], text=True).strip()
platform = "linux-arm64" if arch in ("aarch64", "arm64") else "linux-x64"
binary = package / "binaries" / platform / "stack"
digest = hashlib.sha256(binary.read_bytes()).hexdigest()
assert digest == info["hashes"][platform], "Published binary checksum mismatch"
name = "stack-bench-" + uuid.uuid4().hex[:8]
registry = name + "-reg"
metadata = {"package": info, "platform": platform, "binary_sha256": digest,
            "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "image": subprocess.check_output(["docker", "image", "inspect", "ev-mise", "--format", "{{.Id}}"], text=True).strip(),
            "harness_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()}
(results / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")

def run(*args):
    subprocess.run(args, check=True)

try:
    run("docker", "run", "-d", "--init", "--name", name,
        "-v", f"{fixture / 'examples'}:/examples:ro", "-v", f"{ROOT / 'eval'}:/eval:ro",
        "-v", f"{binary.parent}:/opt/stack:ro", "-v", f"{results}:/results", "ev-mise")
    run("docker", "run", "-d", "--name", registry, "--network", f"container:{name}", "registry:2")
    run("docker", "cp", str(fixture / "tests/e2e/assert.sh"), name + ":/tmp/stack-e2e-assert.sh")
    run("docker", "exec", name, "bash", "-c", "mkdir -p /srv && chown agent /srv")
    scenarios = [("benchmark", "/eval/harness/stack.sh")]
    for path in sorted((fixture / "tests/e2e").glob("[0-9]-*.sh")):
        dest = "/tmp/" + path.name
        run("docker", "cp", str(path), name + ":" + dest)
        scenarios.append((path.stem, dest))
    outcomes = []
    for label, script in scenarios:
        print(f"Running {label}", flush=True)
        started = time.monotonic()
        with (results / "logs" / f"{label}.log").open("w") as log:
            try:
                proc = subprocess.run(["docker", "exec", "-u", "agent", name, "bash", script],
                                      stdout=log, stderr=subprocess.STDOUT, timeout=1200)
                code = proc.returncode
            except subprocess.TimeoutExpired:
                code = 124
        outcome = {"scenario": label, "exit_code": code, "seconds": round(time.monotonic() - started, 3)}
        outcomes.append(outcome)
        print(json.dumps(outcome), flush=True)
        (results / "scenarios.json").write_text(json.dumps(outcomes, indent=2) + "\n")
        if code:
            print((results / "logs" / f"{label}.log").read_text()[-5000:], flush=True)
            break
    run("docker", "exec", name, "bash", "-c",
        "mkdir -p /results/evidence; cp /tmp/*.json /tmp/*.out /tmp/wrong-instance.log /tmp/libpq.log /results/evidence/ 2>/dev/null || true; mise --version > /results/mise-version.txt; ps -eo pid,ppid,stat,comm,args > /results/processes.txt")
    sys.exit(0 if len(outcomes) == len(scenarios) and all(o["exit_code"] == 0 for o in outcomes) else 1)
finally:
    subprocess.run(["docker", "rm", "-f", registry, name], stdout=subprocess.DEVNULL)

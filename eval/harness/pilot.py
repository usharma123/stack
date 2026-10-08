#!/usr/bin/env python3
"""Bounded, repeatable SCRIPTED concurrency pilot for stack on this machine.

This is not an agent-productivity study. Each "worker" is a deterministic script that does
what an agent task would do (start the project's services, run the bundled task and tests,
check which instances it reached, stop), several projects at once, with controlled failures.
Real-agent task completion is a separate protocol: docs/pilot-protocol.md.

Usage:
  python3 eval/harness/pilot.py --stack target/release/stack [--projects 3] [--rounds 2]
      [--phases fresh,cached] [--out eval/results/pilot-<run id>]

Needs mise and jq-free Python 3.9+. HOME/XDG are isolated in a short /tmp directory, so the
fresh phase downloads every tool; the cached phase reuses those installs in new projects.
Raw events and logs go to a NEW output directory; existing results are never touched.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import platform
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[2]
TASK = "set -e; uv sync -q; uv run pytest -q; mise run seed; acme"


def utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # Zombies are not running services.
    out = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return bool(out) and not out.startswith("Z")


def isolated_env(work, inherited=None):
    """Keep authentication/PATH, but discard all inherited provider selectors and state."""
    env = {k: v for k, v in (os.environ if inherited is None else inherited).items()
           if not k.startswith(("MISE_", "__MISE", "PITCHFORK_"))
           and k not in {"STACK_STATE_DIR", "STACK_CACHE_DIR"}}
    home = work / "h"
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_CACHE_HOME=str(home / ".cache"), XDG_DATA_HOME=str(home / ".local/share"),
               XDG_STATE_HOME=str(home / ".local/state"), STACK_STATE_DIR=str(home / ".local/state/stack"),
               STACK_CACHE_DIR=str(home / ".cache/stack"), PITCHFORK_STATE_DIR=str(work / "pf"),
               # Pitchfork registers projects in its config directory; name it rather than rely on HOME.
               PITCHFORK_CONFIG_DIR=str(home / ".config/pitchfork"),
               MISE_DATA_DIR=str(home / ".local/share/mise"), MISE_CACHE_DIR=str(home / ".cache/mise"),
               MISE_STATE_DIR=str(home / ".local/state/mise"), MISE_CONFIG_DIR=str(home / ".config/mise"),
               MISE_GLOBAL_CONFIG_FILE=str(home / ".config/mise/config.toml"),
               MISE_SYSTEM_CONFIG_FILE=str(home / ".config/mise/system.toml"),
               MISE_CEILING_PATHS=str(work), MISE_YES="1", NO_COLOR="1")
    return env


class Run:
    def __init__(self, args):
        self.args = args
        self.id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + "-" + uuid.uuid4().hex[:6]
        self.out = pathlib.Path(args.out or ROOT / "eval/results" / f"pilot-{self.id}").resolve()
        self.out.mkdir(parents=True, exist_ok=False)
        (self.out / "logs").mkdir()
        self.events = (self.out / "events.jsonl").open("w")
        self.lock = threading.Lock()
        self.stack = str(pathlib.Path(args.stack).resolve())
        self.work = pathlib.Path(tempfile.mkdtemp(prefix="spl.", dir="/tmp")).resolve()
        self.home = self.work / "h"
        self.env = isolated_env(self.work)
        self.seq = 0

    def event(self, **fields):
        with self.lock:
            self.seq += 1
            fields = {"seq": self.seq, "at": utc(), **fields}
            self.events.write(json.dumps(fields) + "\n")
            self.events.flush()
        return fields

    def sh(self, ctx, step, args, cwd, timeout=900):
        """Run one command, record a raw event and its full output."""
        start = time.monotonic()
        try:
            p = subprocess.run(args, cwd=cwd, env=self.env, stdin=subprocess.DEVNULL,
                               capture_output=True, text=True, timeout=timeout)
            code, out, err = p.returncode, p.stdout, p.stderr
        except subprocess.TimeoutExpired as e:
            code, out, err = 124, e.stdout or "", e.stderr or ""
            out = out.decode() if isinstance(out, bytes) else out
            err = err.decode() if isinstance(err, bytes) else err
        ms = round((time.monotonic() - start) * 1000, 1)
        log = self.out / "logs" / f"{ctx['phase']}-r{ctx['round']}-{ctx['project']}-{step}.log"
        log.write_text(f"$ {' '.join(args)}\n--- exit {code} in {ms} ms\n--- stdout\n{out}\n--- stderr\n{err}")
        self.event(**ctx, step=step, exit_code=code, ms=ms)
        return code, out, ms

    def stack_json(self, ctx, step, args, cwd, timeout=900):
        # Global flag first: anything after `--` belongs to the command being run.
        code, out, ms = self.sh(ctx, step, [self.stack, "--json", *args], cwd, timeout)
        try:
            return code, json.loads(out), ms
        except ValueError:
            return code, {"ok": False, "error": {"code": "unparseable_output"}}, ms


def metadata(run):
    def cmd(*a, cwd=None):
        try:
            return subprocess.run(a, capture_output=True, text=True, cwd=cwd, env=run.env, timeout=60).stdout.strip()
        except Exception as e:  # recorded, never fatal
            return f"unavailable: {e}"
    binary = pathlib.Path(run.stack).read_bytes()
    return {
        "run_id": run.id,
        "kind": "scripted_concurrency",
        "agent_productivity": "not measured; see docs/pilot-protocol.md",
        "started_utc": utc(),
        "args": vars(run.args),
        "stack_version": cmd(run.stack, "--version"),
        "stack_binary_sha256": hashlib.sha256(binary).hexdigest(),
        "source_commit": cmd("git", "rev-parse", "HEAD", cwd=ROOT),
        "source_dirty": bool(cmd("git", "status", "--porcelain", "--untracked-files=no", cwd=ROOT)),
        "mise_version": cmd("mise", "--version").splitlines()[0] if cmd("mise", "--version") else None,
        "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine(),
                     "python": platform.python_version(), "cpus": os.cpu_count()},
        "isolated_home": str(run.home),
    }


def failure_plan(args):
    """Deterministic: which project gets which controlled failure in which round."""
    kinds = [k for k in args.failures.split(",") if k]
    plan = {}
    for i, kind in enumerate(kinds):
        plan[(i % args.rounds, i % args.projects)] = kind
    return plan


def worker(run, phase, rnd, index, app, failure):
    ctx = {"phase": phase, "round": rnd, "project": f"p{index}"}
    result = {**ctx, "failure": failure, "task_ok": False, "wrong_instance": 0, "orphans": 0,
              "exec_ms": [], "notes": []}
    runner = subprocess.Popen(["sleep", "3600"])  # stands in for a long-lived agent runner
    try:
        code, up, result["up_ms"] = run.stack_json(ctx, "up", ["up", "--owner-pid", str(runner.pid)], app)
        if not up.get("ok"):
            result["notes"].append(f"up failed: {up.get('error', {}).get('code')}")
            return result
        session = up["data"]["session"]
        pids = [s["pid"] for s in session["services"].values() if s.get("pid")]
        code, _, result["task_ms"] = run.sh(ctx, "task", [run.stack, "exec", "--require-all", "--", "bash", "-c", TASK], app)
        result["task_ok"] = code == 0
        # Which instances did the app's own settings reach?
        expect = {name: s.get("data_dir") for name, s in session["services"].items()}
        probes = {
            "postgres": 'psql "$DATABASE_URL" -Atc "show data_directory"',
            "redis": 'redis-cli -u "$REDIS_URL" --no-auth-warning config get dir | tail -n 1',
        }
        for name, probe in probes.items():
            code, out, _ = run.sh(ctx, f"identity-{name}", [run.stack, "exec", "--require", name, "--", "bash", "-c", probe], app)
            reached = out.strip()
            same = code == 0 and expect.get(name) and os.path.realpath(reached) == os.path.realpath(expect[name])
            if code == 0 and not same:
                result["wrong_instance"] += 1
                result["notes"].append(f"{name} reached {reached!r}, expected {expect.get(name)!r}")
        for i in range(run.args.exec_samples):
            code, _, ms = run.sh(ctx, f"exec-{i}", [run.stack, "exec", "--require-all", "--", "true"], app)
            if code == 0:
                result["exec_ms"].append(ms)

        if failure == "service_kill":
            # A service dies under the project: commands must refuse, never fall through.
            victim = session["services"]["postgres"]["pid"]
            os.kill(victim, signal.SIGKILL)
            time.sleep(0.5)
            code, refused, _ = run.stack_json(ctx, "after-service-kill", ["exec", "--require-all", "--", "true"], app)
            ok = code != 0 and refused.get("error", {}).get("code") == "service_unavailable"
            result["notes"].append("service_kill: refused" if ok else f"service_kill: NOT refused ({refused})")
            result["failure_handled"] = ok
            code, up2, _ = run.stack_json(ctx, "recover-up", ["up", "--owner-pid", str(runner.pid)], app)
            result["recovered"] = bool(up2.get("ok"))
            if up2.get("ok"):
                pids += [s["pid"] for s in up2["data"]["session"]["services"].values() if s.get("pid")]
        if failure == "runner_death":
            # The agent runner dies without `down`: GC alone must reclaim exactly its services.
            runner.kill()
            runner.wait()
            code, gc, _ = run.stack_json(ctx, "gc-after-runner-death", ["gc"], app)
            mine = [e for e in gc.get("data", []) if pathlib.Path(e["project"]).resolve() == pathlib.Path(app).resolve()]
            ok = gc.get("ok") and len(mine) == 1 and mine[0]["stopped"]
            result["failure_handled"] = bool(ok)
            result["notes"].append("runner_death: reclaimed by gc" if ok else f"runner_death: gc result {gc}")
        else:
            code, down, _ = run.stack_json(ctx, "down", ["down"], app)
            if not (down.get("ok") and down["data"]["confirmed"]):
                result["notes"].append(f"down failed: {down.get('error', {}).get('code')}")
        time.sleep(0.5)
        survivors = [p for p in pids if alive(p)]
        result["orphans"] = len(survivors)
        if survivors:
            result["notes"].append(f"surviving service pids: {survivors}")
        return result
    finally:
        if runner.poll() is None:
            runner.kill()
            runner.wait()


def pct(values, q):
    if not values:
        return None
    values = sorted(values)
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))]


def dist(values):
    return {"n": len(values), "p50": pct(values, 0.5), "p90": pct(values, 0.9), "max": max(values) if values else None,
            "mean": round(statistics.mean(values), 1) if values else None}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stack", required=True)
    ap.add_argument("--projects", type=int, default=3)
    ap.add_argument("--rounds", type=int, default=2)
    ap.add_argument("--phases", default="fresh,cached")
    ap.add_argument("--failures", default="service_kill,runner_death")
    ap.add_argument("--exec-samples", type=int, default=5)
    ap.add_argument("--out")
    args = ap.parse_args()
    if not (1 <= args.projects <= 8 and 1 <= args.rounds <= 10):
        sys.exit("bounded: 1-8 projects, 1-10 rounds")
    with socket.socket() as s:
        if s.connect_ex(("127.0.0.1", 5432)) == 0:
            print("note: port 5432 is in use; withheld endpoints must still never reach it", file=sys.stderr)
    run = Run(args)
    meta = metadata(run)
    (run.out / "metadata.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"pilot {run.id} -> {run.out}", flush=True)
    plan = failure_plan(args)
    results = []
    try:
        for phase in [p for p in args.phases.split(",") if p]:
            if phase == "fresh" and run.home.exists():
                # Fresh means no installed tools or downloads: stop everything, start a new HOME.
                shutil.rmtree(run.home)
            run.home.mkdir(parents=True, exist_ok=True)
            bundles = run.work / phase / "bundles"
            shutil.copytree(ROOT / "examples/bundles", bundles)
            apps = []
            for i in range(args.projects):
                app = run.work / phase / f"p{i}"
                shutil.copytree(ROOT / "eval/fixture", app)
                shutil.copy(ROOT / "examples/app/stack.toml", app / "stack.toml")
                shutil.copy(ROOT / "examples/app/stack.lock", app / "stack.lock")
                ctx = {"phase": phase, "round": 0, "project": f"p{i}"}
                code, compiled, _ = run.stack_json(ctx, "compile", ["compile", "--locked"], app)
                if not compiled.get("ok"):
                    sys.exit(f"compile --locked failed in {app}: {compiled}")
                apps.append(app)
            for rnd in range(args.rounds):
                started = time.monotonic()
                with concurrent.futures.ThreadPoolExecutor(max_workers=args.projects) as pool:
                    futures = [pool.submit(worker, run, phase, rnd, i, apps[i], plan.get((rnd, i)))
                               for i in range(args.projects)]
                    round_results = [f.result() for f in futures]
                wall = round((time.monotonic() - started) * 1000, 1)
                for r in round_results:
                    r["round_wall_ms"] = wall
                    run.event(kind="result", **r)
                results.extend(round_results)
                print(f"{phase} round {rnd}: " + ", ".join(
                    f"{r['project']} task={'ok' if r['task_ok'] else 'FAIL'} wrong={r['wrong_instance']} orphans={r['orphans']}"
                    + (f" failure={r['failure']}:{'handled' if r.get('failure_handled') else 'NOT HANDLED'}" if r['failure'] else "")
                    for r in round_results), flush=True)
    finally:
        for app in (run.work.glob("*/p*")):
            subprocess.run([run.stack, "-C", str(app), "down", "--json"], env=run.env, capture_output=True, timeout=120)
        apps = sorted(run.work.glob("*/p*"))
        if apps:
            found = subprocess.run(["mise", "which", "pitchfork"], cwd=apps[0], env=run.env,
                                   capture_output=True, text=True, timeout=60)
            if found.returncode == 0 and found.stdout.strip():
                supervisor = pathlib.Path(found.stdout.strip()).resolve()
                try:
                    supervisor.relative_to(run.work)
                except ValueError:
                    run.event(step="cleanup", error="refused supervisor binary outside isolated work directory")
                else:
                    subprocess.run([str(supervisor), "supervisor", "stop"],
                                   env=dict(run.env, PITCHFORK_STATE_DIR=str(run.work / "pf")),
                                   capture_output=True, timeout=60)
        run.events.close()

    phases = {}
    for phase in {r["phase"] for r in results}:
        rs = [r for r in results if r["phase"] == phase]
        injected = [r for r in rs if r["failure"]]
        phases[phase] = {
            "tasks_attempted": len(rs),
            "tasks_succeeded": sum(r["task_ok"] for r in rs),
            "wrong_instance_incidents": sum(r["wrong_instance"] for r in rs),
            "orphaned_service_processes": sum(r["orphans"] for r in rs),
            "controlled_failures": {"injected": len(injected),
                                    "handled": sum(bool(r.get("failure_handled")) for r in injected),
                                    "by_kind": sorted({r["failure"] for r in injected})},
            "latency_ms": {
                "up": dist([r["up_ms"] for r in rs if "up_ms" in r]),
                "task": dist([r["task_ms"] for r in rs if "task_ms" in r]),
                "exec_verified": dist([ms for r in rs for ms in r["exec_ms"]]),
                "round_wall": dist(sorted({r["round_wall_ms"] for r in rs})),
            },
        }
    summary = {
        **{k: meta[k] for k in ("run_id", "kind", "agent_productivity", "stack_version", "source_commit", "source_dirty", "platform")},
        "finished_utc": utc(),
        "projects_concurrent": args.projects,
        "rounds_per_phase": args.rounds,
        "phases": phases,
        "results": results,
        "limits": [
            "Scripted workers, not agents: task success measures stack's lifecycle under concurrency, not agent productivity.",
            "One machine, one run; latencies are samples from this run, not a performance study.",
            "Fresh means an empty isolated HOME (no mise installs or downloads), not an empty network cache upstream.",
        ],
    }
    (run.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    shutil.rmtree(run.work, ignore_errors=True)
    bad = any(p["wrong_instance_incidents"] or p["orphaned_service_processes"] or
              p["tasks_succeeded"] != p["tasks_attempted"] or
              p["controlled_failures"]["handled"] != p["controlled_failures"]["injected"] for p in phases.values())
    print(json.dumps({k: v for k, v in summary.items() if k != "results"}, indent=2))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()

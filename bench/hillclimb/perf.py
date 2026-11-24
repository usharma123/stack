#!/usr/bin/env python3
"""Hill-climb harness: times the operations an agent fleet leans on.

usage: perf.py <stack-binary> <label> [--fleet N] [--reps N]
Appends one JSON line to results.jsonl next to this file. Builds a throwaway git repo
with postgres + redis and N worktrees under a temp dir, and tears everything down.
"""
import json, os, shutil, statistics, subprocess, sys, tempfile, time
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = '''[services.postgres]
preset = "postgres"
version = "17"

[services.redis]
preset = "redis"
version = "8"
'''

def sh(args, cwd, check=True, env=None):
    t = time.perf_counter()
    p = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env)
    dt = (time.perf_counter() - t) * 1000
    if check and p.returncode != 0:
        raise SystemExit(f"FAILED {args} in {cwd}\n{p.stdout}\n{p.stderr}")
    return dt, p

def med(xs): return round(statistics.median(xs), 1)

def main():
    binary, label = os.path.abspath(sys.argv[1]), sys.argv[2]
    fleet = int(sys.argv[sys.argv.index("--fleet") + 1]) if "--fleet" in sys.argv else 6
    reps = int(sys.argv[sys.argv.index("--reps") + 1]) if "--reps" in sys.argv else 15
    base = tempfile.mkdtemp(prefix="stackperf-", dir="/private/tmp")
    main_dir = os.path.join(base, "app")
    os.makedirs(main_dir)
    r = {"label": label, "binary": binary, "fleet": fleet, "when": time.strftime("%Y-%m-%dT%H:%M:%S")}
    trees = []
    try:
        open(os.path.join(main_dir, "stack.toml"), "w").write(MANIFEST)
        sh(["git", "init", "-q", "-b", "main"], main_dir)
        sh([binary, "compile"], main_dir)
        sh(["git", "add", "-A"], main_dir)
        sh(["git", "-c", "user.name=p", "-c", "user.email=p@p", "commit", "-qm", "init"], main_dir)
        sh([binary, "install"], main_dir)
        for i in range(fleet):
            d = os.path.join(base, f"wt{i}")
            sh(["git", "worktree", "add", "-q", d, "-b", f"a{i}"], main_dir)
            trees.append(d)

        # single checkout
        r["up_cold_ms"] = round(sh([binary, "up", "-q"], main_dir)[0], 1)
        r["exec_ms"] = med([sh([binary, "exec", "--", "true"], main_dir)[0] for _ in range(reps)])
        r["status_ms"] = med([sh([binary, "status", "--json"], main_dir)[0] for _ in range(reps)])
        r["env_ms"] = med([sh([binary, "env"], main_dir)[0] for _ in range(reps)])
        r["up_warm_ms"] = med([sh([binary, "up", "-q"], main_dir)[0] for _ in range(5)])
        r["restart_ms"] = round(sh([binary, "restart", "-q"], main_dir)[0], 1)
        r["down_ms"] = round(sh([binary, "down", "-q"], main_dir)[0], 1)
        r["up_again_ms"] = round(sh([binary, "up", "-q"], main_dir)[0], 1)

        # fleet: every agent brings its worktree up at once, then works, then leaves
        def timed(cmd):
            def f(d): return sh([binary] + cmd, d)[0]
            return f
        with ThreadPoolExecutor(fleet) as ex:
            t = time.perf_counter(); each = list(ex.map(timed(["up", "-q"]), trees))
            r["fleet_up_wall_ms"] = round((time.perf_counter() - t) * 1000, 1); r["fleet_up_each_ms"] = med(each)
            t = time.perf_counter(); each = list(ex.map(timed(["exec", "--", "true"]), trees * 4))
            r["fleet_exec_wall_ms"] = round((time.perf_counter() - t) * 1000, 1); r["fleet_exec_each_ms"] = med(each)
            # isolation: each worktree must reach its own database
            def ident(d):
                _, p = sh([binary, "exec", "--", "sh", "-c", "echo $DATABASE_URL $REDIS_URL"], d)
                return p.stdout.strip()
            ids = list(ex.map(ident, trees))
            r["fleet_distinct_endpoints"] = len(set(ids)) == fleet and all(ids)
            t = time.perf_counter(); each = list(ex.map(timed(["down", "-q"]), trees))
            r["fleet_down_wall_ms"] = round((time.perf_counter() - t) * 1000, 1)
        r["final_down_ms"] = round(sh([binary, "down", "-q"], main_dir)[0], 1)
        r["ok"] = True
    finally:
        for d in [main_dir] + trees:
            subprocess.run([binary, "down", "-q", "--purge"], cwd=d, capture_output=True)
            subprocess.run([binary, "down", "-q"], cwd=d, capture_output=True)
        subprocess.run([binary, "gc"], cwd=base, capture_output=True)
        shutil.rmtree(base, ignore_errors=True)
    with open(os.path.join(HERE, "results.jsonl"), "a") as f:
        f.write(json.dumps(r) + "\n")
    print(json.dumps(r, indent=1))

main()

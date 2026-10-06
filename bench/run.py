#!/usr/bin/env python3
"""Real-world competitor benchmark runner. See bench/README.md.

  python3 bench/run.py --tool stack --out bench/results/<new-dir> [--repeats 20 --warmups 3]
  python3 bench/run.py --tool flox --dry-run          # print the planned bodies, run nothing

One tool per invocation; the parent serializes runs. The output directory must not exist.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import tempfile
import time
import uuid

BENCH = Path(__file__).resolve().parent
sys.path.insert(0, str(BENCH))

from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES  # noqa: E402
from rwb.record import Recorder, utc  # noqa: E402
from rwb.scenario import ProvisionError, Scenario  # noqa: E402
from rwb.transport import DockerTransport, HostTransport  # noqa: E402
from rwb.verify import BOUNDARIES  # noqa: E402


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def tree_hashes(root):
    root = Path(root)
    return {str(p.relative_to(root)): sha256(p) for p in sorted(root.rglob("*"))
            if p.is_file() and "__pycache__" not in p.parts and ".venv" not in p.parts}


def git(*args):
    try:
        return subprocess.check_output(["git", *args], cwd=BENCH, text=True, stderr=subprocess.DEVNULL).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def parse(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--tool", required=True, help="adapter name, optionally name:variant")
    p.add_argument("--out", help="new results directory (default bench/results/<tool>-<run id>)")
    p.add_argument("--repeats", type=int, default=20)
    p.add_argument("--warmups", type=int, default=3)
    p.add_argument("--option", action="append", default=[], metavar="KEY=VALUE",
                   help="adapter option, e.g. stack_binary=/path/to/linux/stack")
    p.add_argument("--cpus", help="docker --cpus limit (recorded)")
    p.add_argument("--memory", help="docker --memory limit (recorded)")
    p.add_argument("--keep", action="store_true", help="keep the container after the run (diagnosis)")
    p.add_argument("--dry-run", action="store_true", help="print planned bodies; touch nothing")
    return p.parse_args(argv)


def main(argv=None):
    args = parse(argv)
    cls, variant = registry.load(args.tool)
    run_id = time.strftime("%Y%m%dt%H%M%S", time.gmtime()) + "-" + uuid.uuid4().hex[:6]
    options = dict(kv.split("=", 1) for kv in args.option)
    adapter = cls(options, variant, run_id)
    if adapter.isolation_boundary not in BOUNDARIES:
        raise SystemExit(f"{adapter.name}: unknown isolation boundary {adapter.isolation_boundary}")
    if args.dry_run:
        return dry_run(adapter)

    out = Path(args.out or BENCH / "results" / f"{adapter.name}-{adapter.variant}-{run_id}").resolve()
    rec = Recorder(out)
    workdir = Path(tempfile.mkdtemp(prefix=f"rwb-{run_id}-")) if adapter.transport == "host" else None
    if adapter.transport == "docker":
        resources = [x for flag, v in (("--cpus", args.cpus), ("--memory", args.memory)) if v for x in (flag, v)]
        tx = DockerTransport(rec, run_id, adapter.name, adapter.image, adapter.mounts(), adapter.user,
                             resources=resources)
    else:
        adapter.root = str(workdir / "w")
        tx = HostTransport(rec, workdir, env=adapter.host_env(workdir))

    meta = dict(run_id=run_id, tool=adapter.name, variant=adapter.variant, title=adapter.title,
                started_utc=utc(), valid=False, completed=False,
                host=dict(system=platform.system(), machine=platform.machine(), release=platform.release(),
                          python=platform.python_version()),
                harness=dict(commit=git("rev-parse", "HEAD"), dirty=bool(git("status", "--porcelain", "--", ".")),
                             files=tree_hashes(BENCH / "rwb") | {"run.py": sha256(BENCH / "run.py")}),
                fixture=tree_hashes(BENCH / "fixtures" / "app"),
                config=tree_hashes(adapter.config_dir()) if adapter.config_dir().exists() else {},
                features={k: adapter.features[k] for k in FEATURES},
                isolation_boundary=adapter.isolation_boundary, transport=adapter.transport,
                image=adapter.image, options=options, repeats=args.repeats, warmups=args.warmups,
                pins=getattr(adapter, "pins", {}), errors=[])
    rec.write_json("meta.json", meta)

    def on_signal(signum, _frame):
        raise KeyboardInterrupt(f"signal {signum}")
    signal.signal(signal.SIGTERM, on_signal)

    scenario = Scenario(adapter, tx, rec, args.repeats, args.warmups)
    try:
        if adapter.transport == "docker":
            tx.start()
            meta["image_id"] = tx.image_id
        scenario.execute()
        meta["completed"] = True
    except ProvisionError as error:
        meta["errors"].append(f"provision: {error}")
        scenario.add("provision", "fail", detail=str(error))
    except KeyboardInterrupt as error:
        meta["errors"].append(f"interrupted: {error}")
    except Exception as error:  # harness fault: invalid run, but still clean up
        meta["errors"].append(f"harness: {type(error).__name__}: {error}")
        scenario.add("harness", "error", detail=f"{type(error).__name__}: {error}")
    finally:
        cleanup_problems = []
        try:
            if tx.__class__ is HostTransport or getattr(tx, "created", False):
                cleanup_problems += scenario.cleanup()
            if adapter.transport == "host" and adapter.cleanup_host():
                r = tx.exec("host-cleanup", "cleanup", adapter.cleanup_host(), timeout=600)
                if not r.ok:
                    cleanup_problems.append(f"host cleanup exit {r.code}")
                left = adapter.host_resources()
                if left:
                    r = tx.exec("host-leftovers", "cleanup", left, timeout=120)
                    if r.stdout.strip() or not r.ok:
                        cleanup_problems.append("owned host resources remain")
        except Exception as error:
            cleanup_problems.append(f"cleanup raised {type(error).__name__}: {error}")
        if adapter.transport == "docker" and not args.keep:
            removed = tx.destroy()
            if removed is not None and not removed.ok or not tx.gone():
                cleanup_problems.append(f"container {tx.name} not removed")
        meta.update(cleanup_problems=cleanup_problems, finished_utc=utc(),
                    timings=scenario.timings, isolation=getattr(scenario, "isolation_receipt", None))
        meta["valid"] = meta["completed"] and scenario.out.valid() and not cleanup_problems and not meta["errors"]
        rec.write_json("outcomes.json", scenario.out.as_list())
        rec.write_json("meta.json", meta)
        (out / "summary.md").write_text(render_summary(meta, scenario.out.as_list()))
        rec.close()
        if workdir and not cleanup_problems:
            subprocess.run(["rm", "-rf", str(workdir)])
    print(out)
    return 0 if meta["valid"] else 1


def render_summary(meta, outcomes):
    lines = [f"# {meta['title']} ({meta['tool']}:{meta['variant']}) run {meta['run_id']}", "",
             f"valid: {meta['valid']} · completed: {meta['completed']} · transport: {meta['transport']}"
             f" · isolation boundary: {meta['isolation_boundary']}", ""]
    if meta["errors"] or meta.get("cleanup_problems"):
        lines += ["Errors: " + "; ".join(meta["errors"] + meta.get("cleanup_problems", [])), ""]
    lines += ["| check | status | mode | detail |", "|---|---|---|---|"]
    for o in outcomes:
        lines.append(f"| {o['check']} | {o['status']} | {o['mode']} | {o['detail'].replace('|', '/')[:200]} |")
    lines += ["", "| timing | outer p50 ms | outer p95 ms | inner p50 ms | inner p95 ms | ok/n |", "|---|---|---|---|---|---|"]
    for key, value in sorted(meta.get("timings", {}).items()):
        if isinstance(value, dict):
            o, i = value["outer"], value["inner"]
            lines.append(f"| {key} | {o['p50_ms']} | {o['p95_ms']} | {i['p50_ms']} | {i['p95_ms']} | {o['ok']}/{o['n']} |")
        elif value is not None:
            lines.append(f"| {key} (single, outer) | {round(value / 1e6, 1)} | | | | 1/1 |")
    return "\n".join(lines) + "\n"


def dry_run(adapter):
    """Print every body the scenario would send, using a fake transport."""
    from rwb.testing import FakeTransport, FakeRecorder
    rec = FakeRecorder()
    tx = FakeTransport(rec, adapter)
    scenario = Scenario(adapter, tx, rec, repeats=1, warmups=0)
    scenario.execute()
    scenario.cleanup()
    for label, phase, body in tx.calls:
        print(f"### {label} [{phase}]\n{body}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

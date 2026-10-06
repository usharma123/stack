#!/usr/bin/env python3
"""Real-world competitor benchmark runner. See bench/README.md.

  python3 bench/run.py --tool stack --out bench/results/<new-dir> [--repeats 20 --warmups 3]
  python3 bench/run.py --tool flox --dry-run          # print the planned bodies, run nothing

One tool per invocation; the parent serializes runs. The output directory must not exist.
Every run is diagnostic (meta.reportable stays false); final timings come only from a
parent-declared session manifest checked by bench/report.py.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import stat
import subprocess
import sys
import tempfile
import time
import traceback
import uuid

BENCH = Path(__file__).resolve().parent
sys.path.insert(0, str(BENCH))

from rwb.adapters import registry  # noqa: E402
from rwb.adapters.base import FEATURES  # noqa: E402
from rwb.outcomes import Outcomes  # noqa: E402
from rwb.record import Recorder, utc  # noqa: E402
from rwb.scenario import ProvisionBlocked, ProvisionError, Scenario  # noqa: E402
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
    # Everything between creating the results directory and the protected run below is
    # initialization: a failure here still leaves a truthful receipt and removes only the
    # exact tempdir this process created, then re-raises the original error.
    init = dict(run_id=run_id, tool=adapter.name, variant=adapter.variant, title=adapter.title,
                transport=adapter.transport, isolation_boundary=adapter.isolation_boundary,
                started_utc=utc(), started_unix_ns=time.time_ns())
    workdir = workdir_id = meta = None

    def on_signal(signum, _frame):
        raise KeyboardInterrupt(f"signal {signum}")
    try:
        signal.signal(signal.SIGTERM, on_signal)
        if adapter.transport == "host":
            workdir = Path(tempfile.mkdtemp(prefix=f"rwb-{run_id}-"))
            workdir_id = dir_identity(workdir)
        if adapter.transport == "docker":
            resources = [x for flag, v in (("--cpus", args.cpus), ("--memory", args.memory)) if v for x in (flag, v)]
            tx = DockerTransport(rec, run_id, adapter.name, adapter.image, adapter.mounts(), adapter.user,
                                 resources=resources)
        else:
            adapter.root = str(workdir / "w")
            tx = HostTransport(rec, workdir, env=adapter.host_env(workdir))
        meta = initial_meta(adapter, args, options, init)
        rec.write_json("meta.json", meta)

        scenario = Scenario(adapter, tx, rec, args.repeats, args.warmups)
    except BaseException as error:
        initialization_failed(rec, out, init, meta, workdir, workdir_id, error)
        raise
    try:
        if adapter.transport == "docker":
            tx.start()
            meta["image_id"] = tx.image_id
        scenario.execute()
        meta["completed"] = True
    except ProvisionBlocked as error:
        # Environment prerequisite missing: explicit blocked evidence; the run stays valid.
        scenario.block_all(str(error), [error.result] if error.result is not None else [])
        meta["blocked"] = str(error)
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
        cleanup_problems = teardown(scenario, adapter, tx, out, meta, keep=args.keep)
        meta.update(cleanup_problems=cleanup_problems, finished_utc=utc(), finished_unix_ns=time.time_ns(),
                    timings=scenario.timings, isolation=getattr(scenario, "isolation_receipt", None))
        meta["valid"] = meta["completed"] and scenario.out.valid() and not cleanup_problems and not meta["errors"]
        # valid: the evidence is trustworthy (no harness error, clean teardown). It says nothing
        # about the tool passing. measurement: whether the run can carry any timing at all.
        # Every run of this harness is diagnostic; only a manifest-selected report (report.py)
        # can carry final timings. Blocked/invalid runs publish none (raw receipts stay in steps.jsonl).
        if meta.get("blocked"):
            meta["measurement"] = "blocked-prerequisite: no checks executed, no timings"
            meta["timings"] = {}
        elif not meta["valid"]:
            meta["measurement"] = "invalid: harness error, interruption or unclean teardown; no timings usable"
            meta["timings"] = {}
        else:
            meta["measurement"] = "diagnostic: timings recorded per check; only passing checks' samples count"
        meta["reportable"] = False
        rec.write_json("outcomes.json", scenario.out.as_list())
        rec.write_json("meta.json", meta)
        (out / "summary.md").write_text(render_summary(meta, scenario.out.as_list()))
        rec.close()
        if workdir and not cleanup_problems:
            subprocess.run(["rm", "-rf", str(workdir)])
    print(out)
    return 0 if meta["valid"] else 1


def initial_meta(adapter, args, options, init):
    """The meta.json written before any command runs; it must be JSON-serializable."""
    return dict(init, valid=False, completed=False,
                host=dict(system=platform.system(), machine=platform.machine(), release=platform.release(),
                          python=platform.python_version()),
                harness=dict(commit=git("rev-parse", "HEAD"), dirty=bool(git("status", "--porcelain", "--", ".")),
                             files=tree_hashes(BENCH / "rwb") | {"run.py": sha256(BENCH / "run.py")}),
                fixture=tree_hashes(BENCH / "fixtures" / "app"),
                shared_glue=tree_hashes(BENCH / "adapters" / "_shared"),
                config=tree_hashes(adapter.config_dir()) if adapter.config_dir().exists() else {},
                features={k: adapter.features[k] for k in FEATURES},
                image=adapter.image, options=options, repeats=args.repeats, warmups=args.warmups,
                keep=args.keep, resources=dict(cpus=args.cpus, memory=args.memory),
                pins=getattr(adapter, "pins", {}), errors=[],
                setup_scope=adapter.setup_scope or "not declared",
                prepare_scope=adapter.prepare_scope, start_scope=adapter.start_scope,
                cache_note=adapter.cache_note)


def dir_identity(path):
    st = os.lstat(path)
    return dict(dev=st.st_dev, ino=st.st_ino, uid=st.st_uid)


def same_dir(st, identity):
    return (stat.S_ISDIR(st.st_mode) and dict(dev=st.st_dev, ino=st.st_ino, uid=st.st_uid) == identity
            and st.st_uid == os.getuid())


JSON_DEPTH, JSON_NODES, REPR_LIMIT = 32, 20000, 300


def safe_text(obj, render=repr):
    """repr()/str() that cannot raise (an Exception) and is bounded in length."""
    try:
        text = render(obj)
        if not isinstance(text, str):
            raise TypeError("not a str")
    except Exception:
        text = f"<{render.__name__} of {type(obj).__name__} failed>"
    return text if len(text) <= REPR_LIMIT else text[:REPR_LIMIT] + "...<truncated>"


def jsonable(value):
    """A JSON-safe copy that keeps what cannot be serialized visible as marker strings instead
    of failing on it: unsupported values, unsupported or colliding mapping keys, circular
    containers (tracked by the identity of each container on the active path), nesting deeper
    than JSON_DEPTH and anything past JSON_NODES values. Container subclasses are read through
    the base type's own iteration, so overridden methods are never called. Containers are
    iterated lazily and stop at the budget with one remainder marker, so the copy holds at most
    JSON_NODES values plus one marker per container open at the cutoff, and no key past the
    cutoff is rendered."""
    active, budget = set(), [JSON_NODES]
    omitted = f"<omitted: more than {JSON_NODES} values>"

    def key_text(key):
        if isinstance(key, str):
            return str.__str__(key)
        if key is None or key is True or key is False:
            return json.dumps(key)
        if isinstance(key, int):
            return int.__repr__(key)
        if isinstance(key, float):
            return float.__repr__(key)
        return f"<invalid key {type(key).__name__}: {safe_text(key)}>"

    def walk(obj, depth):
        budget[0] -= 1
        if budget[0] < 0:
            return omitted
        if obj is None or obj is True or obj is False:
            return obj
        if isinstance(obj, str):
            return str.__str__(obj)
        if isinstance(obj, int):
            return int.__index__(obj)
        if isinstance(obj, float):
            obj = float.__float__(obj)
            return obj if math.isfinite(obj) else f"<non-finite float: {obj!r}>"
        if not isinstance(obj, (dict, list, tuple)):
            return f"<unserializable {type(obj).__name__}: {safe_text(obj)}>"
        if id(obj) in active:
            return f"<circular reference: {type(obj).__name__}>"
        if depth >= JSON_DEPTH:
            return f"<omitted: {type(obj).__name__} nested deeper than {JSON_DEPTH}>"
        active.add(id(obj))
        try:
            if isinstance(obj, dict):
                copy = {}
                for key, item in dict.items(obj):
                    text = omitted if budget[0] <= 0 else key_text(key)
                    while text in copy:
                        text += " <duplicate key>"
                    if budget[0] <= 0:
                        copy[text] = omitted
                        break
                    copy[text] = walk(item, depth + 1)
                return copy
            copy = []
            for item in list.__iter__(obj) if isinstance(obj, list) else tuple.__iter__(obj):
                if budget[0] <= 0:
                    copy.append(omitted)
                    break
                copy.append(walk(item, depth + 1))
            return copy
        finally:
            active.discard(id(obj))
    return walk(value, 0)


TRUSTED_INIT_FIELDS = ("run_id", "tool", "variant", "title", "transport", "isolation_boundary",
                       "started_utc", "started_unix_ns")


def minimal_record(init, detail, tb, cleanup, fault):
    """The fallback receipt when the full one cannot be built: only exact str/int fields of the
    harness-built init dict plus harness-built text. No adapter-supplied object is touched."""
    def scalar(value):
        return value if type(value) in (str, int) else f"<omitted {type(value).__name__}>"
    problems = [p for p in cleanup.get("problems", []) if type(p) is str]
    record = {k: scalar(init.get(k)) for k in TRUSTED_INIT_FIELDS}
    record.update(
        valid=False, completed=False, reportable=False, timings={},
        errors=[f"initialization: {detail}"], cleanup_problems=problems,
        finished_utc=utc(), finished_unix_ns=time.time_ns(),
        measurement="invalid: initialization failed before the protected run; no checks executed, no timings",
        initialization_failure=dict(
            error=detail, traceback=tb, receipt=f"minimal: the full receipt could not be written ({fault})",
            workdir=dict(action=scalar(cleanup.get("action")), problems=problems)))
    return record


DIR_FLAGS = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
WORKDIR_DEPTH = 64


def fd_removal_supported():
    return (hasattr(os, "O_DIRECTORY") and hasattr(os, "O_NOFOLLOW") and os.scandir in os.supports_fd
            and {os.open, os.stat, os.unlink, os.rmdir} <= os.supports_dir_fd)


def remove_init_workdir(workdir, identity, steps_run):
    """Remove the tempdir created by this initialization, and nothing else.

    Only the exact path returned by mkdtemp is considered, and only while it is still a real
    (non-symlink) directory with the device, inode and owner recorded at creation, and no
    transport command has run in it (so everything inside was written by harness code).
    Otherwise it is left in place and the reason is returned as a problem. The deletion itself
    is bound to that identity through a directory descriptor (remove_verified_dir)."""
    receipt = dict(path=str(workdir) if workdir else None, identity=identity, action="none", problems=[])
    if workdir is None:
        receipt["action"] = "none created"
        return receipt
    problems = receipt["problems"]
    try:
        st = os.lstat(workdir)
    except FileNotFoundError:
        receipt["action"] = "already absent"
        return receipt
    if identity is None:
        problems.append(f"{workdir}: no creation identity recorded; left in place")
    elif stat.S_ISLNK(st.st_mode) or not stat.S_ISDIR(st.st_mode):
        problems.append(f"{workdir}: no longer a real directory; left in place")
    elif not same_dir(st, identity):
        problems.append(f"{workdir}: identity changed since creation; left in place")
    elif steps_run:
        problems.append(f"{workdir}: {steps_run} command(s) ran in it; left in place for inspection")
    elif not fd_removal_supported():
        problems.append(f"{workdir}: descriptor-relative removal is unavailable here; left in place")
    if problems:
        receipt["action"] = "left"
        return receipt
    return remove_verified_dir(Path(workdir), identity, receipt)


def remove_verified_dir(workdir, identity, receipt):
    """Open workdir without following a symlink, require the descriptor (and the name in its
    parent) to match the creation identity, delete the contents relative to that descriptor,
    and rmdir the name only if it still names the same directory. No fresh path lookup is ever
    traversed for deletion, so a directory swapped in at the path is left alone and reported.
    Residual limit: a replacement swapped in between the final identity check and rmdir is
    removed only if it is an empty directory (rmdir never deletes contents)."""
    problems, name = receipt["problems"], workdir.name
    parent = fd = None
    receipt["action"] = "left"
    try:
        parent = os.open(workdir.parent, os.O_RDONLY | os.O_DIRECTORY)
        fd = os.open(name, DIR_FLAGS, dir_fd=parent)
        if not (same_dir(os.fstat(fd), identity)
                and same_dir(os.stat(name, dir_fd=parent, follow_symlinks=False), identity)):
            problems.append(f"{workdir}: identity changed since creation; left in place")
            return receipt
        remove_dir_contents(fd, 0)
        if not same_dir(os.stat(name, dir_fd=parent, follow_symlinks=False), identity):
            problems.append(f"{workdir}: replaced during removal; owned contents were removed through the"
                            " verified descriptor, the entry now at the path was left in place")
            return receipt
        os.rmdir(name, dir_fd=parent)
        receipt["action"] = "removed"
        try:
            os.stat(name, dir_fd=parent, follow_symlinks=False)
            receipt["verified_absent"] = False
            problems.append(f"{workdir}: still present after removal")
        except FileNotFoundError:
            receipt["verified_absent"] = True
    except OSError as error:
        receipt["action"] = "failed"
        problems.append(f"{workdir}: removal raised {type(error).__name__}: {error}; rest left in place")
    finally:
        for descriptor in (fd, parent):
            if descriptor is not None:
                os.close(descriptor)
    return receipt


def remove_dir_contents(dir_fd, depth):
    """Delete everything below dir_fd without following symlinks. A subdirectory is entered only
    through a descriptor matching the entry lstat'ed here; nesting is bounded by WORKDIR_DEPTH."""
    if depth >= WORKDIR_DEPTH:
        raise OSError(f"nested deeper than {WORKDIR_DEPTH} directories")
    with os.scandir(dir_fd) as entries:
        names = [entry.name for entry in entries]
    for name in names:
        st = os.stat(name, dir_fd=dir_fd, follow_symlinks=False)
        if not stat.S_ISDIR(st.st_mode):
            os.unlink(name, dir_fd=dir_fd)
            continue
        child = os.open(name, DIR_FLAGS, dir_fd=dir_fd)
        try:
            cst = os.fstat(child)
            if (cst.st_dev, cst.st_ino) != (st.st_dev, st.st_ino):
                raise OSError(f"{name} changed while being removed")
            remove_dir_contents(child, depth + 1)
        finally:
            os.close(child)
        os.rmdir(name, dir_fd=dir_fd)


def defer_signals(arrived, saved):
    """Replace the SIGTERM/SIGINT handlers with ones that only record the signal (main thread
    only; elsewhere, or for a handler not installed from Python, nothing is changed)."""
    for signum in (signal.SIGTERM, signal.SIGINT):
        try:
            previous = signal.getsignal(signum)
            if previous is not None:
                signal.signal(signum, lambda s, _frame: arrived.append(s))
                saved[signum] = previous
        except (ValueError, OSError):
            pass


def restore_signals(saved):
    for signum, previous in saved.items():
        try:
            signal.signal(signum, previous)
        except (ValueError, OSError):
            pass


def initialization_failed(rec, out, init, meta, workdir, workdir_id, error):
    """Record an initialization failure and remove its exact tempdir. Never raises, not even for
    a second interrupt: SIGTERM/SIGINT are deferred while it runs, every recovery stage is
    guarded against BaseException, and Recorder.close runs in an outer finally. Each secondary
    fault is attached to the original error as a note; the caller re-raises that same object.
    Limit: a signal landing after the handlers are restored, before the caller's raise, is not
    deferred."""
    def note(text):
        try:
            error.add_note(text)
        except BaseException:
            print(text, file=sys.stderr)

    def fault_text(fault):
        return f"{type(fault).__name__}: {safe_text(fault, str)}"
    arrived, saved = [], {}
    cleanup = dict(path=str(workdir) if workdir else None, action="unknown",
                   problems=["tempdir cleanup was not attempted"])
    recorded = None
    try:
        defer_signals(arrived, saved)
        try:
            cleanup = remove_init_workdir(workdir, workdir_id, rec.seq)
            if not isinstance(cleanup, dict) or not isinstance(cleanup.get("problems"), list):
                raise TypeError(f"tempdir cleanup returned {type(cleanup).__name__}, not a receipt")
        except BaseException as fault:
            cleanup = dict(path=str(workdir) if workdir else None, action="unknown",
                           problems=[f"tempdir cleanup raised {fault_text(fault)}"])
        detail = f"{type(error).__name__}: {safe_text(error, str)}"
        try:
            tb = "".join(traceback.format_exception(error))
        except BaseException as fault:
            tb = f"<traceback unavailable: {fault_text(fault)}>"
        outcomes = Outcomes()
        outcomes.add("harness", "error", detail=f"initialization: {detail}")
        full_fault = None
        try:
            record = jsonable(dict(meta or init))
            errors = record.get("errors")
            record.update(
                valid=False, completed=False, reportable=False, timings={},
                errors=(errors if isinstance(errors, list) else []) + [f"initialization: {detail}"],
                cleanup_problems=cleanup["problems"], finished_utc=utc(), finished_unix_ns=time.time_ns(),
                measurement="invalid: initialization failed before the protected run; no checks executed, no timings",
                initialization_failure=dict(
                    error=detail, traceback=tb, initial_meta_written=(out / "meta.json").exists(),
                    steps_run=rec.seq, workdir=jsonable(cleanup)))
            rec.write_json("outcomes.json", outcomes.as_list())
            rec.write_json("meta.json", record)
            (out / "summary.md").write_text(render_summary(record, outcomes.as_list()))
            recorded = "receipt"
        except BaseException as fault:
            full_fault = fault_text(fault)
            note(f"rwb: recording the initialization failure in {out} raised {full_fault}")
        if recorded is None:
            try:
                record = minimal_record(init, detail, tb, cleanup, full_fault)
                rec.write_json("outcomes.json", outcomes.as_list())
                rec.write_json("meta.json", record)
                (out / "summary.md").write_text(render_summary(record, outcomes.as_list()))
                recorded = "minimal receipt"
            except BaseException as fault:
                note(f"rwb: recording the minimal initialization receipt in {out} raised {fault_text(fault)}")
        receipt = f"{recorded} in {out}/meta.json" if recorded else f"receipt NOT completed in {out}"
        note(f"rwb: initialization failed; {receipt}; tempdir {cleanup.get('action')}"
             + (f" ({'; '.join(map(str, cleanup['problems']))})" if cleanup.get("problems") else ""))
    except BaseException as fault:
        note(f"rwb: recording the initialization failure was interrupted by {fault_text(fault)}")
    finally:
        try:
            rec.close()
        except BaseException as fault:
            note(f"rwb: closing {out}/steps.jsonl raised {fault_text(fault)}")
        for signum in list(arrived):
            note(f"rwb: signal {signum} arrived during initialization-failure recovery; deferred, the"
                 " original error is re-raised")
        restore_signals(saved)


def teardown(scenario, adapter, tx, out, meta, keep=False):
    """Diagnostics and artifact copies first (before anything is torn down), then checkout
    cleanup, host cleanup and container removal. Each stage is guarded on its own: a failed
    diagnostic or copy is recorded and can never skip a later teardown stage."""
    problems = []
    meta.setdefault("artifact_errors", [])
    live = tx.__class__ is HostTransport or getattr(tx, "created", False)
    if live:
        try:
            scenario.diagnose()
        except Exception as error:
            meta["artifact_errors"].append(f"diagnostics raised {type(error).__name__}: {error}")
        try:
            meta["artifacts"] = scenario.collect_artifacts(out / "artifacts")
        except Exception as error:
            meta["artifact_errors"].append(f"artifact copy raised {type(error).__name__}: {error}")
        try:
            problems += scenario.cleanup()
        except Exception as error:
            problems.append(f"checkout cleanup raised {type(error).__name__}: {error}")
    try:
        if adapter.transport == "host" and adapter.cleanup_host():
            r = tx.exec("host-cleanup", "cleanup", adapter.cleanup_host(), timeout=600)
            if not r.ok:
                problems.append(f"host cleanup exit {r.code}")
            left = adapter.host_resources()
            if left:
                r = tx.exec("host-leftovers", "cleanup", left, timeout=120)
                if r.stdout.strip() or not r.ok:
                    problems.append("owned host resources remain")
    except Exception as error:
        problems.append(f"host cleanup raised {type(error).__name__}: {error}")
    if adapter.transport == "docker" and not keep:
        try:
            removed = tx.destroy()
            if removed is not None and not removed.ok or not tx.gone():
                problems.append(f"container {tx.name} not removed")
        except Exception as error:
            problems.append(f"container removal raised {type(error).__name__}: {error}")
    return problems


def render_summary(meta, outcomes):
    lines = [f"# {meta['title']} ({meta['tool']}:{meta['variant']}) run {meta['run_id']}", "",
             f"valid: {meta['valid']} · completed: {meta['completed']} · transport: {meta['transport']}"
             f" · isolation boundary: {meta['isolation_boundary']}",
             f"measurement: {meta.get('measurement', '')} · reportable: {meta.get('reportable', False)}", ""]
    if meta.get("blocked"):
        lines += [f"BLOCKED (environment prerequisite, not a tool result): {meta['blocked']}", ""]
    if meta["errors"] or meta.get("cleanup_problems"):
        lines += ["Errors: " + "; ".join(meta["errors"] + meta.get("cleanup_problems", [])), ""]
    lines += [f"Setup scope: {meta.get('setup_scope', 'not declared')}",
              f"Cache state: {meta.get('cache_note', '')}", ""]
    lines += ["| check | status | mode | detail |", "|---|---|---|---|"]
    for o in outcomes:
        lines.append(f"| {o['check']} | {o['status']} | {o['mode']} | {o['detail'].replace('|', '/')[:200]} |")
    if meta.get("blocked") or not meta.get("valid"):
        # Independent of meta["timings"]: an older or edited meta may still carry numbers.
        lines += ["", "No timings: the run is blocked or invalid. Raw command receipts remain in steps.jsonl."]
        return "\n".join(lines) + "\n"
    timings = meta.get("timings") or {}
    tasks = {k: v for k, v in timings.items() if k.startswith("first_task.")}
    if tasks:
        lines += ["", "End-to-end time to first verified work (setup, start, readiness, app deps, identity,",
                  "migrate, mark/CRUD/cache, pytest). `steps` sums those receipts; `wall` also includes",
                  "harness verification steps. This, not any single setup command, is the cross-tool task time.",
                  f"It starts from an already PREPARED checkout; prepare (reported separately as prepare.<co>) "
                  f"covers: {meta.get('prepare_scope', 'not declared')}.", "",
                  "| task | steps s | wall s | receipts | checkout |", "|---|---|---|---|---|"]
        for key, v in sorted(tasks.items()):
            if v.get("ok"):
                lines.append(f"| {key} | {round(v['steps_ns'] / 1e9, 2)} | {round(v['wall_ns'] / 1e9, 2)} | "
                             f"{len(v['steps'])} | {v['checkout']} |")
            else:
                lines.append(f"| {key} | | | | {v.get('reason', '')} |")
    lines += ["", "Phase timings (single commands; scopes differ by tool, see Setup scope; do not rank across tools):", "",
              "| timing | outer p50 ms | outer p95 ms | inner p50 ms | inner p95 ms | ok/n |", "|---|---|---|---|---|---|"]
    for key, value in sorted(timings.items()):
        if key.startswith("first_task."):
            continue
        if isinstance(value, dict) and "outer" in value:
            o, i = value["outer"], value["inner"]
            lines.append(f"| {key} | {o['p50_ms']} | {o['p95_ms']} | {i['p50_ms']} | {i['p95_ms']} | {o['ok']}/{o['n']} |")
        elif isinstance(value, int):
            lines.append(f"| {key} (single, outer) | {round(value / 1e6, 1)} | | | | 1/1 |")
    if meta.get("start_scope"):
        lines += ["", f"Start scope: {meta['start_scope']}."]
    lines += ["", "`ready.<co>` sums start, native readiness and the first verified identity (app deps excluded).",
              "First-checkout timings start from the image's cache state above; they are not universal cold installs."]
    return "\n".join(lines) + "\n"


def dry_run(adapter):
    """Print every body the scenario would send, using a fake transport."""
    from rwb.testing import FakeTransport, FakeRecorder
    rec = FakeRecorder()
    tx = FakeTransport(rec, adapter)
    if adapter.transport == "host":
        adapter.root = adapter.root or "/tmp/rwb-dry-run"  # checkout paths derive from it
    scenario = Scenario(adapter, tx, rec, repeats=1, warmups=0)
    tx.world.scenario = scenario  # the fake app reports each checkout's real source token
    scenario.execute()
    scenario.cleanup()
    for label, phase, body in tx.calls:
        print(f"### {label} [{phase}]\n{body}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

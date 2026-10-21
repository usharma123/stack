#!/usr/bin/env python3
"""Final measurement report: verify a parent-declared session manifest, derive the report.

  python3 bench/report.py template --session-id <id> --out <manifest.json>
  python3 bench/report.py attempt --run bench/results/<session>/<tool>
  python3 bench/report.py --manifest <manifest.json> --out <new-report-dir> [--require-complete]

Run directories are diagnostic evidence and are never modified (meta.reportable stays false).
The parent declares the session (frozen 26-entry roster, sample policy, serialization, review)
and the exact attempts; this command checks every receipt and hash and emits only the metrics
that pass every gate. Everything else keeps a row with its reason. See bench/README.md.

Relative paths in a manifest are resolved against the repository root.
"""
import argparse
import calendar
import hashlib
import json
from pathlib import Path
import sys
import time

BENCH = Path(__file__).resolve().parent
REPO = BENCH.parent
sys.path.insert(0, str(BENCH))

from rwb import verify  # noqa: E402
from rwb.outcomes import MODES, STATUSES  # noqa: E402
from rwb.record import StepResult  # noqa: E402
from rwb.scenario import MAIN_CHECKS  # noqa: E402
from rwb.stats import summarize_ns  # noqa: E402

SCHEMA = "rwb-measurement-manifest/1"
REPORT_SCHEMA = "rwb-measurement-report/1"
STATES = ("selected", "excluded", "blocked", "untested")
EVIDENCE_FILES = ("meta.json", "outcomes.json", "steps.jsonl")
# The only comparison metrics. Other phase numbers (setup.*, ready.*, deps.*) stay diagnostic.
METRICS = ("first_task.a", "first_task.b", "repeat.entry", "repeat.app_read")

# Frozen roster (SCOPE.md): display entry -> registered adapter.
ROSTER = (
    ("Stack", "stack"), ("mise / Pitchfork", "mise"), ("Flox", "flox"), ("Devbox", "devbox"),
    ("devenv", "devenv"), ("Nix", "nix"), ("Pixi", "pixi"), ("Docker Compose", "compose"),
    ("Dev Containers", "devcontainers"), ("DevPod", "devpod"), ("DDEV", "ddev"), ("Lando", "lando"),
    ("Process Compose", "process-compose"), ("services-flake", "services-flake"), ("pkgx / dev", "pkgx"),
    ("dnvr", "dnvr"), ("GNU Guix", "guix"), ("workz (rohansx)", "workz"), ("Worktrunk", "worktrunk"),
    ("GitGrove", "git-grove"), ("isola", "isola"), ("Berth", "berth"), ("BranchBox", "branchbox"),
    ("Tilt", "tilt"), ("Organist", "organist"), ("Vagrant", "vagrant"),
)

LIMITS = (
    "Describes this host, image/cache state, transport and recipe only; not native macOS results.",
    "first_task.<co> is one observation per run, not a startup distribution.",
    "repeat.* p50/p95 are descriptive nearest-rank values over one run's samples; warmups excluded.",
    "No overall rank, score, confidence claim or universal cold-install claim.",
    "Interval checks prove selected runs did not overlap each other; the parent declaration, not this "
    "report, attests that no other benchmark, build or install work ran concurrently.",
)


class ManifestError(ValueError):
    pass


def sha256_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def tree_digest(root):
    """One digest over every file (relative path + content hash) under a run directory."""
    root = Path(root)
    h = hashlib.sha256()
    for p in sorted(x for x in root.rglob("*") if x.is_file()):
        h.update(f"{p.relative_to(root).as_posix()}\0{sha256_file(p)}\n".encode())
    return h.hexdigest()


def resolve(path):
    p = Path(path)
    return p if p.is_absolute() else REPO / p


def nonneg_int(value):
    """A recorded duration: a non-negative int (bool, float, NaN and infinity are not durations)."""
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def contained(root, ref):
    """root/ref, or None unless ref is a relative path with no `..` or symlinked component that
    resolves to an existing entry inside root (so it is covered by tree_digest)."""
    if not isinstance(ref, str) or not ref or Path(ref).is_absolute() or ".." in Path(ref).parts:
        return None
    path = root / ref
    for part in [path] + list(path.parents):
        if part == root:
            break
        if part.is_symlink():
            return None
    try:
        path.resolve(strict=True).relative_to(root.resolve())
    except (OSError, ValueError):
        return None
    return path


def log_path(root, ref):
    """A raw log referenced by a step, or None unless it is a regular file inside the hashed run tree.
    Absolute paths, `..` traversal and symlinks (inside or escaping) are refused: anything outside
    tree_digest could change after the manifest was written without changing a hash."""
    path = contained(root, ref)
    return path if path is not None and path.is_file() else None


def artifact_problem(root, art, steps):
    """None if a meta.artifacts receipt is consistent, else the problem. A successful copy
    (ok=true) must name an existing file or directory at artifacts/<checkout>/<path> inside the
    hashed run tree (a copied directory may not hold symlinks leading out of it). A failed
    best-effort copy (ok=false) stays diagnostic: its target need not exist."""
    seq = art.get("seq") if isinstance(art, dict) else None
    if not isinstance(art, dict) or not isinstance(art.get("ok"), bool) \
            or not (seq is None or (isinstance(seq, int) and not isinstance(seq, bool))):
        return f"malformed artifact receipt {str(art)[:120]}"
    name, rel = art.get("checkout"), art.get("path")
    if seq is not None and seq not in steps:
        return f"artifact {rel} cites missing step {seq}"
    if not art["ok"]:
        return None
    if seq is None or steps[seq]["exit"] != 0 or steps[seq]["timed_out"]:
        return f"artifact {rel} is marked copied without a successful copy receipt"
    if not isinstance(name, str) or not name or "/" in name or not isinstance(rel, str):
        return f"malformed artifact receipt {str(art)[:120]}"
    path = contained(root, f"artifacts/{name}/{rel}")
    if path is None or not (path.is_file() or path.is_dir()):
        return f"copied artifact artifacts/{name}/{rel} missing inside the hashed run tree"
    if path.is_dir():
        top = root.resolve()
        for link in (x for x in path.rglob("*") if x.is_symlink()):
            try:
                link.resolve(strict=True).relative_to(top)
            except (OSError, ValueError):
                return f"copied artifact artifacts/{name}/{rel} holds a symlink leaving the run tree"
    return None


def utc_ns(text):
    """'YYYY-MM-DDTHH:MM:SSZ' -> unix ns."""
    return calendar.timegm(time.strptime(text, "%Y-%m-%dT%H:%M:%SZ")) * 10**9


def gates(co):
    return (f"setup.{co}", f"start.{co}", f"deps.{co}", f"migrate.{co}", f"crud_cache.{co}", f"tests.{co}")


def task_labels(co):
    """(required, optional) step labels of first_task.<co> (see Scenario.first_task)."""
    required = {f"{co}-{x}" for x in ("setup", "start", "deps", "start-identity", "migrate", "mark",
                                      "crud", "cache", "pytest")}
    return required, {f"{co}-start-ready"}


# ---- manifest -----------------------------------------------------------------------------
def check_manifest(m):
    p = []
    if not isinstance(m, dict) or m.get("schema") != SCHEMA:
        raise ManifestError(f"manifest schema must be {SCHEMA!r}")
    s, proto, attempts = m.get("session"), m.get("protocol"), m.get("attempts")
    if not isinstance(s, dict) or not isinstance(proto, dict) or not isinstance(attempts, list):
        raise ManifestError("manifest needs session, protocol and attempts")
    for key in ("id", "timing_policy", "serialization_declaration"):
        if not isinstance(s.get(key), str) or not s[key].strip():
            p.append(f"session.{key} must be a non-empty string")
    if s.get("purpose") != "final-measurement":
        p.append("session.purpose must be 'final-measurement'")
    if not (s.get("review") is None or isinstance(s.get("review"), str)):
        p.append("session.review must be a string or null")
    try:
        utc_ns(s.get("plan_created_utc"))
    except (TypeError, ValueError):
        p.append("session.plan_created_utc must be YYYY-MM-DDTHH:MM:SSZ")
    policy = s.get("sample_policy")
    if not (isinstance(policy, dict) and isinstance(policy.get("repeats"), int) and policy["repeats"] >= 1
            and isinstance(policy.get("warmups"), int) and policy["warmups"] >= 0):
        p.append("session.sample_policy needs integer repeats >= 1 and warmups >= 0")
    roster = s.get("roster")
    frozen = dict((tool, entry) for entry, tool in ROSTER)
    if not isinstance(roster, list) or not all(isinstance(r, dict) for r in roster):
        p.append("session.roster must be a list of {entry, tool, variant}")
    else:
        tools = [r.get("tool") for r in roster]
        if sorted(tools) != sorted(frozen) or len(set(tools)) != len(tools):
            p.append(f"session.roster must list each of the {len(ROSTER)} frozen tools exactly once")
        for r in roster:
            if r.get("tool") in frozen and r.get("entry") != frozen[r["tool"]]:
                p.append(f"roster entry for {r['tool']} must be named {frozen[r['tool']]!r}")
            if not isinstance(r.get("variant"), str):
                p.append(f"roster {r.get('tool')}: variant must be explicit")
    for key in ("harness", "fixture", "shared_glue", "lanes", "resources"):
        if not isinstance(proto.get(key), dict):
            p.append(f"protocol.{key} must be an object")
    harness = proto["harness"] if isinstance(proto.get("harness"), dict) else {}
    if not isinstance(harness.get("commit"), str) or not isinstance(harness.get("files"), dict):
        p.append("protocol.harness needs commit and files")
    for key in ("platform", "cache_policy"):
        if not proto.get(key):
            p.append(f"protocol.{key} is required")
    roster_variant = {r.get("tool"): r.get("variant") for r in roster or [] if isinstance(r, dict)}
    final = {}
    for i, a in enumerate(attempts):
        where = f"attempts[{i}]"
        if not isinstance(a, dict) or a.get("state") not in STATES:
            p.append(f"{where}: state must be one of {STATES}")
            continue
        if a.get("tool") not in roster_variant:
            p.append(f"{where}: tool {a.get('tool')!r} is not in the session roster")
        elif a.get("variant") != roster_variant[a["tool"]]:
            p.append(f"{where}: variant {a.get('variant')!r} differs from the roster")
        if a["state"] in ("selected", "excluded"):
            if not a.get("path") or not a.get("run_id") or not isinstance(a.get("hashes"), dict):
                p.append(f"{where}: {a['state']} attempts need path, run_id and hashes")
        if a["state"] != "selected" and not a.get("reason"):
            p.append(f"{where}: {a['state']} attempts need a reason")
        if a["state"] != "excluded":
            if a.get("tool") in final:
                p.append(f"{where}: {a.get('tool')} already has a final state at attempts[{final[a['tool']]}]")
            final[a.get("tool")] = i
    if p:
        raise ManifestError("; ".join(p))


# ---- one run directory --------------------------------------------------------------------
def load_run(path, hashes):
    """Parse and integrity-check a run directory. Returns (run or None, evidence problems)."""
    path = resolve(path)
    problems = []
    if not path.is_dir():
        return None, [f"run directory {path} does not exist"]
    for name in EVIDENCE_FILES:
        if not (path / name).is_file():
            problems.append(f"missing {name}")
        elif hashes.get(name) != sha256_file(path / name):
            problems.append(f"{name} hash differs from the manifest")
    if hashes.get("tree") != tree_digest(path):
        problems.append("run tree digest differs from the manifest (a file was added, removed or changed)")
    try:
        meta = json.loads((path / "meta.json").read_text())
        outcomes = json.loads((path / "outcomes.json").read_text())
        records = [json.loads(line) for line in (path / "steps.jsonl").read_text().splitlines() if line.strip()]
    except (OSError, ValueError) as error:
        return None, problems + [f"unparseable evidence: {error}"]
    if not isinstance(meta, dict) or not isinstance(outcomes, list):
        return None, problems + ["meta.json must be an object and outcomes.json a list"]
    steps = {}
    for r in records:
        seq = r.get("seq") if isinstance(r, dict) else None
        # outer_ns is a monotonic interval; inner_ns may be absent (null) but is otherwise an int.
        # A negative inner_ns is a well-formed receipt of a wall clock stepping back; repeat() refuses
        # to summarize it rather than treating the whole run as tampered.
        inner = r.get("inner_ns") if isinstance(r, dict) else None
        if not isinstance(seq, int) or isinstance(seq, bool) or not isinstance(r.get("label"), str) \
                or not isinstance(r.get("exit"), int) or isinstance(r.get("exit"), bool) \
                or not nonneg_int(r.get("outer_ns")) or not isinstance(r.get("timed_out"), bool) \
                or not (inner is None or (isinstance(inner, int) and not isinstance(inner, bool))):
            problems.append(f"malformed step record {str(r)[:120]}")
            continue
        if seq in steps:
            problems.append(f"duplicate step seq {seq}")
        steps[seq] = r
        for stream in ("stdout", "stderr"):
            if log_path(path, r.get(stream)) is None:
                problems.append(f"step {seq}: missing raw {stream} log inside the hashed run tree")
    by_check = {}
    for o in outcomes:
        if not isinstance(o, dict) or o.get("status") not in STATUSES or o.get("mode") not in MODES:
            problems.append(f"malformed outcome {str(o)[:120]}")
            continue
        if o.get("check") in by_check:
            problems.append(f"duplicate outcome {o['check']}")
        by_check[o.get("check")] = o
        missing = [e for e in o.get("evidence") or [] if e not in steps]
        if missing:
            problems.append(f"outcome {o['check']} cites missing steps {missing}")
    arts = meta.get("artifacts") or []
    if not isinstance(arts, list):
        problems.append("meta.artifacts must be a list")
        arts = []
    for art in arts:
        problem = artifact_problem(path, art, steps)
        if problem:
            problems.append(problem)
    return dict(path=path, meta=meta, outcomes=outcomes, by_check=by_check, steps=steps), problems


def log_text(run, record):
    out = []
    for stream in ("stdout", "stderr"):
        path = log_path(run["path"], record.get(stream))
        try:
            out.append(path.read_text(errors="replace") if path else "")
        except OSError:
            out.append("")
    return out


def run_gates(run, attempt, m):
    """Run-level reasons this run cannot carry any timing (empty list: all gates pass)."""
    meta, s, proto = run["meta"], m["session"], m["protocol"]
    lane = proto["lanes"].get(attempt["tool"])
    p = []
    if s.get("review") is None:
        p.append("session has no review reference")
    if not attempt.get("result_review"):
        p.append("attempt has no result-review reference")
    for key in ("tool", "variant"):
        if meta.get(key) != attempt.get(key):
            p.append(f"meta.{key} {meta.get(key)!r} != declared {attempt.get(key)!r}")
    if meta.get("run_id") != attempt.get("run_id"):
        p.append(f"meta.run_id {meta.get('run_id')!r} != declared {attempt.get('run_id')!r}")
    if meta.get("reportable") is not False:
        p.append("meta.reportable is not false (not an unmodified harness receipt)")
    start, end = meta.get("started_unix_ns"), meta.get("finished_unix_ns")
    if not (isinstance(start, int) and isinstance(end, int) and start <= end):
        p.append("no nanosecond run interval recorded")
    elif start < utc_ns(s["plan_created_utc"]):
        p.append("run started before the session plan was created")
    harness = meta.get("harness") or {}
    if harness.get("commit") != proto["harness"]["commit"]:
        p.append("harness commit differs from the reviewed commit")
    if harness.get("files") != proto["harness"]["files"]:
        p.append("harness file hashes differ from the reviewed bytes")
    if meta.get("fixture") != proto["fixture"]:
        p.append("fixture hashes differ")
    if meta.get("shared_glue") != proto["shared_glue"]:
        p.append("shared glue (adapters/_shared) hashes differ or were not recorded")
    if not isinstance(lane, dict):
        p.append("protocol.lanes has no entry for this tool")
        lane = {}
    elif meta.get("config") != lane.get("config"):
        p.append("lane config hashes differ")
    for key in ("transport", "image_id"):
        if key in lane and meta.get(key) != lane[key]:
            p.append(f"meta.{key} {meta.get(key)!r} != declared {lane[key]!r}")
    host = proto["platform"].get("host") if isinstance(proto["platform"], dict) else None
    if host is not None and meta.get("host") != host:
        p.append("host identity differs from the declared platform")
    policy = s["sample_policy"]
    if (meta.get("repeats"), meta.get("warmups")) != (policy["repeats"], policy["warmups"]):
        p.append(f"sample policy {meta.get('repeats')}/{meta.get('warmups')} != declared "
                 f"{policy['repeats']}/{policy['warmups']}")
    if meta.get("resources") != proto["resources"]:
        p.append(f"resources {meta.get('resources')!r} != declared {proto['resources']!r}")
    if meta.get("keep") is not False:
        p.append("container kept (--keep) or keep not recorded")
    if meta.get("blocked"):
        p.append(f"provisioning blocked: {meta['blocked']}")
    if meta.get("completed") is not True:
        p.append("run incomplete")
    if meta.get("valid") is not True:
        p.append("run invalid")
    if meta.get("errors"):
        p.append("harness errors: " + "; ".join(map(str, meta["errors"])))
    if meta.get("cleanup_problems"):
        p.append("unclean teardown: " + "; ".join(map(str, meta["cleanup_problems"])))
    missing = [c for c in MAIN_CHECKS if c not in run["by_check"]]
    if missing:
        p.append("incomplete outcomes: missing " + ", ".join(missing))
    if any(o["status"] == "error" for o in run["by_check"].values()):
        p.append("an outcome is a harness error")
    versions = [r for r in run["steps"].values() if r["label"] == "tool-version"]
    text = "\n".join(log_text(run, versions[0])) if len(versions) == 1 and versions[0]["exit"] == 0 else None
    if text is None:
        p.append("no single successful tool-version receipt")
    else:
        for want in lane.get("version_contains") or []:
            if want not in text:
                p.append(f"tool-version output lacks {want!r}")
    if attempt["tool"] == "stack":
        p += stack_gates(attempt.get("stack"), meta)
    return p


def stack_gates(stack, meta):
    """Harness commit alone does not identify the built product: require its own identity."""
    if not isinstance(stack, dict):
        return ["stack attempt needs source revision/fingerprint, binary SHA256 and build receipt"]
    p = []
    if not (stack.get("source_revision") or stack.get("source_fingerprint")):
        p.append("stack source revision/fingerprint missing")
    digest = stack.get("binary_sha256")
    if not digest or digest != (meta.get("options") or {}).get("stack_sha256") \
            or digest != (meta.get("pins") or {}).get("stack_sha256"):
        p.append("stack binary SHA256 does not match the run's recorded option/pin")
    receipt = stack.get("build_receipt") or {}
    path = resolve(receipt["path"]) if receipt.get("path") else None
    if path is None or not path.is_file() or sha256_file(path) != receipt.get("sha256"):
        p.append("stack build receipt missing or hash differs")
    return p


# ---- metrics ------------------------------------------------------------------------------
def first_task(run, co):
    key = f"first_task.{co}"
    stored = (run["meta"].get("timings") or {}).get(key)
    if not isinstance(stored, dict) or stored.get("ok") is not True:
        reason = stored.get("reason") if isinstance(stored, dict) else "no record"
        return None, f"first task not reached ({reason})"
    failed = [g for g in gates(co) if run["by_check"].get(g, {}).get("status") != "pass"]
    if failed:
        return None, "gating checks did not pass: " + ", ".join(failed)
    seqs = stored.get("steps")
    if not isinstance(seqs, list) or len(set(seqs)) != len(seqs) or any(s not in run["steps"] for s in seqs):
        return None, "task step receipts missing or duplicated"
    recs = [run["steps"][s] for s in seqs]
    bad = [r["seq"] for r in recs if r["exit"] != 0 or r["timed_out"]]
    if bad:
        return None, f"task steps {bad} did not succeed"
    required, optional = task_labels(co)
    labels = [r["label"] for r in recs]
    if len(set(labels)) != len(labels) or not required <= set(labels) <= required | optional:
        return None, f"task step labels {sorted(labels)} differ from the declared task"
    total = sum(r["outer_ns"] for r in recs)
    if total != stored.get("steps_ns"):
        return None, f"stored steps_ns {stored.get('steps_ns')} != receipt sum {total}"
    # wall_ns is the host span of the task; prepare_ns may be absent (null) but is otherwise a duration.
    if not nonneg_int(stored.get("wall_ns")):
        return None, f"stored wall_ns {stored.get('wall_ns')!r} is not a non-negative integer duration"
    if stored.get("prepare_ns") is not None and not nonneg_int(stored["prepare_ns"]):
        return None, f"stored prepare_ns {stored['prepare_ns']!r} is not a non-negative integer duration"
    return dict(kind="single observation", steps_ns=total, steps_s=round(total / 1e9, 3),
                wall_ns=stored.get("wall_ns"), starts_from=stored.get("starts_from"),
                excluded_prepare=stored.get("excluded_prepare"), prepare_ns=stored.get("prepare_ns"),
                checkout=stored.get("checkout"), transport=stored.get("transport"), evidence=seqs), None


def read_ok(run, record):
    """Reparse an app-read receipt: exit 0 alone can accompany a semantic failure."""
    stdout = log_text(run, record)[0]
    payload = StepResult(record["seq"], record["label"], "warm", record["exit"], record["outer_ns"],
                         stdout=stdout).json()
    try:
        item = verify.app_result(payload, "read", record["exit"], record["timed_out"])["item"]
    except ValueError:
        return False
    return item.get("sku") == "keeper-a"


def repeat(run, key, policy):
    outcome = run["by_check"].get(key, {})
    if outcome.get("status") != "pass":
        return None, f"outcome {outcome.get('status', 'missing')}: {outcome.get('detail', '')}".rstrip(": ")
    # The promised condition is two running checkouts; A-only samples do not establish it.
    failed = [g for g in ("isolation",) + gates("a") + gates("b")
              if run["by_check"].get(g, {}).get("status") != "pass"]
    if failed:
        return None, "two-checkout condition not established: " + ", ".join(failed)
    warmups, repeats = policy["warmups"], policy["repeats"]
    expected = [f"{key}-{'warmup' if i < warmups else 'sample'}-{i:03d}" for i in range(warmups + repeats)]
    found = {}
    for r in run["steps"].values():
        if r["label"].startswith(key + "-"):
            found.setdefault(r["label"], []).append(r)
    if sorted(found) != sorted(expected) or any(len(v) != 1 for v in found.values()):
        return None, (f"sample receipts differ from policy {warmups} warmups + {repeats} samples "
                      f"({len(found)} labels found)")
    recs = [found[label][0] for label in expected]
    bad = [r["seq"] for r in recs if r["exit"] != 0 or r["timed_out"]
           or (key == "repeat.app_read" and not read_ok(run, r))]
    if bad:
        kind = "warmup" if any(s in bad for s in (r["seq"] for r in recs[:warmups])) else "sample"
        return None, f"{kind} receipts {bad} failed"
    samples = recs[warmups:]
    seqs = [r["seq"] for r in samples]
    if outcome.get("evidence") != seqs:
        return None, "outcome evidence does not cite exactly the sample receipts"
    outer = summarize_ns([(True, r["outer_ns"]) for r in samples])
    stored = (run["meta"].get("timings") or {}).get(key) or {}
    if stored.get("outer") != outer or stored.get("inner") != summarize_ns([(True, r.get("inner_ns"))
                                                                            for r in samples]) \
            or stored.get("warmups") != warmups:
        return None, "stored summary disagrees with the raw receipts"
    inner = [r["inner_ns"] for r in samples if r.get("inner_ns") is not None]
    negative = [r["seq"] for r in samples if r.get("inner_ns") is not None and r["inner_ns"] < 0]
    if negative:  # in-container wall clock stepped back: no inner summary is trustworthy
        inner_out = dict(available=0, p50_ms=None, p95_ms=None,
                         reason=f"negative inner_ns in sample receipts {negative}; inner summary withheld")
    else:
        inner_stats = summarize_ns([(True, i) for i in inner])
        inner_out = dict(available=len(inner), p50_ms=inner_stats["p50_ms"], p95_ms=inner_stats["p95_ms"])
    return dict(kind="descriptive distribution, one run", n=len(samples), warmups=warmups,
                outer={k: outer[k] for k in ("p50_ms", "p95_ms", "min_ms", "max_ms")},
                inner=inner_out,
                transport=stored.get("transport"), evidence=seqs,
                warmup_evidence=[r["seq"] for r in recs[:warmups]]), None


# ---- report -------------------------------------------------------------------------------
def build(m, manifest_path):
    check_manifest(m)
    s = m["session"]
    attempts = []
    for a in m["attempts"]:
        row = dict(tool=a["tool"], variant=a["variant"], state=a["state"], path=a.get("path"),
                   run_id=a.get("run_id"), reason=a.get("reason"), result_review=a.get("result_review"),
                   hashes=a.get("hashes"), evidence_problems=[], timing_gate_problems=[], metrics={},
                   reportable=False, run=None)
        if a.get("path"):
            run, row["evidence_problems"] = load_run(a["path"], a.get("hashes") or {})
            row["run"] = run
        attempts.append(row)
    overlap(attempts)
    reported, omitted = [], []
    for a, declared in zip(attempts, m["attempts"]):
        run = a.pop("run")
        a["coverage"], a["covered"] = coverage(a, run)
        if run is None:
            continue
        meta = run["meta"]
        a.update(outcomes=run["outcomes"], interval=[meta.get("started_unix_ns"), meta.get("finished_unix_ns")],
                 versions=dict(tool_version=version_head(run), pins=meta.get("pins"),
                               deviations=(m["protocol"]["lanes"].get(a["tool"]) or {}).get("deviations", [])),
                 scopes=dict(setup=meta.get("setup_scope"), prepare=meta.get("prepare_scope"),
                             start=meta.get("start_scope"), cache=meta.get("cache_note"),
                             isolation_boundary=meta.get("isolation_boundary"), transport=meta.get("transport")))
        if a["state"] != "selected":
            continue
        a["timing_gate_problems"] += run_gates(run, declared, m)
        blockers = a["evidence_problems"] + a["timing_gate_problems"]
        for metric in METRICS:
            if blockers:
                value, reason = None, "run not eligible: " + "; ".join(blockers)
            elif metric.startswith("first_task."):
                value, reason = first_task(run, metric.split(".")[1])
            else:
                value, reason = repeat(run, metric, s["sample_policy"])
            a["metrics"][metric] = dict(eligible=value is not None, reason=reason)
            if value is None:
                omitted.append(dict(tool=a["tool"], variant=a["variant"], run_id=a["run_id"], metric=metric,
                                    reason=reason))
            else:
                reported.append(dict(tool=a["tool"], variant=a["variant"], run_id=a["run_id"], metric=metric,
                                     **value))
        a["reportable"] = any(v["eligible"] for v in a["metrics"].values())
    final = {a["tool"]: a for a in attempts if a["state"] != "excluded"}
    roster = []
    for r in s["roster"]:
        a = final.get(r["tool"])
        counts = {}
        for o in (a or {}).get("outcomes") or []:
            counts[o["status"]] = counts.get(o["status"], 0) + 1
        roster.append(dict(entry=r["entry"], tool=r["tool"], variant=r["variant"],
                           coverage=a["coverage"] if a else "missing: no attempt declared",
                           covered=bool(a and a["covered"]),
                           run_id=(a or {}).get("run_id"), reason=(a or {}).get("reason"), outcome_counts=counts,
                           reported_metrics=[k for k, v in ((a or {}).get("metrics") or {}).items()
                                             if v["eligible"]]))
    return dict(schema=REPORT_SCHEMA, generated_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                manifest=dict(path=str(manifest_path), sha256=sha256_file(manifest_path)),
                session={k: s[k] for k in ("id", "purpose", "plan_created_utc", "sample_policy", "timing_policy",
                                           "serialization_declaration", "review")},
                protocol=dict(harness_commit=m["protocol"]["harness"]["commit"], platform=m["protocol"]["platform"],
                              resources=m["protocol"]["resources"], cache_policy=m["protocol"]["cache_policy"]),
                coverage_complete=all(row["covered"] for row in roster),
                roster=roster, attempts=attempts, reported_timings=reported, omitted=omitted, limits=list(LIMITS))


def overlap(attempts):
    """Declared order must be execution order and no two declared runs may overlap."""
    timed = []
    for a in attempts:
        meta = (a["run"] or {}).get("meta") or {}
        start, end = meta.get("started_unix_ns"), meta.get("finished_unix_ns")
        if isinstance(start, int) and isinstance(end, int):
            timed.append((a, start, end))
    for i, (a, s1, e1) in enumerate(timed):
        for b, s2, e2 in timed[i + 1:]:
            if s1 < e2 and s2 < e1:
                a["timing_gate_problems"].append(f"overlaps run {b['run_id']}")
                b["timing_gate_problems"].append(f"overlaps run {a['run_id']}")
            elif s2 < s1:
                b["timing_gate_problems"].append(f"ran before {a['run_id']} but is declared after it")


def coverage(a, run):
    """(text, covered). A roster row is covered only by an explicit blocked/untested declaration or by
    intact, identity-matching evidence of a completed run or a provisioning block; missing, tampered,
    mislabeled, invalid or incomplete evidence leaves the row uncovered."""
    if a["state"] in ("blocked", "untested"):
        return f"{a['state']} (declared): {a['reason']}", True
    if run is None:
        return "evidence missing: " + "; ".join(a["evidence_problems"]), False
    if a["evidence_problems"]:
        return "evidence invalid: " + "; ".join(a["evidence_problems"]), False
    meta = run["meta"]
    wrong = [k for k in ("tool", "variant", "run_id") if meta.get(k) != a.get(k)]
    if wrong:
        return "evidence invalid: meta " + ", ".join(wrong) + " differ from the declaration", False
    if meta.get("blocked"):
        return f"blocked (provisioning): {meta['blocked']}", True
    if meta.get("valid") is not True:
        return "invalid run (harness error, interruption or unclean teardown)", False
    return ("completed", True) if meta.get("completed") is True else ("incomplete", False)


def version_head(run):
    for r in run["steps"].values():
        if r["label"] == "tool-version":
            return "\n".join(log_text(run, r)).strip()[:600]
    return None


def render(report):
    s = report["session"]
    lines = [f"# Measurement report {s['id']}", "",
             f"Manifest `{report['manifest']['path']}` sha256 `{report['manifest']['sha256']}`.",
             f"Plan created {s['plan_created_utc']} · samples {s['sample_policy']['repeats']} + "
             f"{s['sample_policy']['warmups']} warmups · harness `{report['protocol']['harness_commit']}`.",
             f"Session review: {s['review'] or 'NONE (no timings reported)'}.",
             f"Serialization: {s['serialization_declaration']}", "",
             f"Coverage complete: {report['coverage_complete']}", "",
             "| entry | tool:variant | coverage | outcomes | reported metrics |", "|---|---|---|---|---|"]
    for r in report["roster"]:
        counts = ", ".join(f"{k} {v}" for k, v in sorted(r["outcome_counts"].items()))
        lines.append(f"| {r['entry']} | {r['tool']}:{r['variant']} | {cell(r['coverage'])} | {counts} | "
                     f"{', '.join(r['reported_metrics']) or '-'} |")
    lines += ["", "## Reported timings", "",
              "`first_task.*`: one observation (sum of task step receipts, from a prepared checkout). "
              "`repeat.*`: nearest-rank p50/p95 over one run's samples; warmups excluded.", "",
              "| tool | run | metric | value | n | inner available | evidence |", "|---|---|---|---|---|---|---|"]
    for t in report["reported_timings"]:
        if t["metric"].startswith("first_task."):
            value, n, inner = f"{t['steps_s']} s (wall {round(t['wall_ns'] / 1e9, 3)} s)", 1, "-"
        else:
            value = f"outer p50 {t['outer']['p50_ms']} / p95 {t['outer']['p95_ms']} ms"
            n, inner = t["n"], (f"withheld: {t['inner']['reason']}" if t["inner"].get("reason") else
                                f"{t['inner']['available']}/{t['n']} (p50 {t['inner']['p50_ms']} ms)")
        lines.append(f"| {t['tool']} | {t['run_id']} | {t['metric']} | {value} | {n} | {inner} | "
                     f"seq {t['evidence'][0]}-{t['evidence'][-1]} |")
    lines += ["", "## Omitted metrics", "", "| tool | metric | reason |", "|---|---|---|"]
    for o in report["omitted"]:
        lines.append(f"| {o['tool']} | {o['metric']} | {cell(o['reason'])} |")
    lines += ["", "## Attempts", ""]
    for a in report["attempts"]:
        lines.append(f"- {a['tool']}:{a['variant']} {a['state']} `{a['path'] or '-'}` run {a['run_id'] or '-'}"
                     f" · review {a['result_review'] or '-'} · {a['coverage']}"
                     + (f" · {a['reason']}" if a["reason"] else ""))
    lines += ["", "## Limits", ""] + [f"- {x}" for x in report["limits"]]
    return "\n".join(lines) + "\n"


def cell(text):
    return str(text).replace("|", "/").replace("\n", " ")[:300]


# ---- helpers for the parent ---------------------------------------------------------------
def attempt_block(run_dir):
    """Hashes and identity of one run directory, for pasting into a manifest. Review is left empty."""
    path = resolve(run_dir)
    meta = json.loads((path / "meta.json").read_text())
    try:
        rel = str(path.resolve().relative_to(REPO))
    except ValueError:
        rel = str(path.resolve())
    return dict(tool=meta.get("tool"), variant=meta.get("variant"), state="selected", path=rel,
                run_id=meta.get("run_id"), reason=None, result_review=None,
                hashes={**{n: sha256_file(path / n) for n in EVIDENCE_FILES}, "tree": tree_digest(path)})


def template(session_id):
    """A manifest skeleton with the current bytes' hashes. Reviews stay null: fill them only
    after an actual review. Attempts start empty; add each with `attempt` after it runs."""
    import run as runner
    from rwb.adapters import registry
    lanes, roster = {}, []
    for entry, tool in ROSTER:
        cls, variant = registry.load(tool)
        adapter = cls({}, variant, "template")
        config = adapter.config_dir()
        lanes[tool] = dict(config=runner.tree_hashes(config) if config.exists() else {}, transport=adapter.transport,
                           version_contains=[], deviations=[])
        roster.append(dict(entry=entry, tool=tool, variant=adapter.variant))
    return dict(
        schema=SCHEMA,
        session=dict(id=session_id, purpose="final-measurement",
                     plan_created_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), roster=roster,
                     sample_policy=dict(repeats=20, warmups=3),
                     timing_policy="one tool per synchronous run.py invocation in roster order; no --keep",
                     serialization_declaration="", review=None),
        protocol=dict(harness=dict(commit=runner.git("rev-parse", "HEAD"),
                                   files=runner.tree_hashes(BENCH / "rwb") | {"run.py": runner.sha256(BENCH / "run.py")}),
                      fixture=runner.tree_hashes(BENCH / "fixtures" / "app"),
                      shared_glue=runner.tree_hashes(BENCH / "adapters" / "_shared"),
                      lanes=lanes, platform=dict(host=None, note=""), resources=dict(cpus=None, memory=None),
                      cache_policy=""),
        attempts=[])  # undeclared lanes stay `missing`; placeholders would fake coverage


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if not argv or argv[0] not in ("template", "attempt", "build", "-h", "--help"):
        argv.insert(0, "build")
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("template", help="print a manifest skeleton with current hashes")
    t.add_argument("--session-id", required=True)
    t.add_argument("--out", help="write here (must not exist) instead of stdout")
    at = sub.add_parser("attempt", help="print one attempt block (hashes) for a run directory")
    at.add_argument("--run", required=True)
    b = sub.add_parser("build", help="verify a manifest and write report.json/report.md")
    b.add_argument("--manifest", required=True)
    b.add_argument("--out", required=True, help="new report directory (must not exist)")
    b.add_argument("--require-complete", action="store_true", help="exit 1 unless every roster row is covered by intact "
                   "evidence or an explicit blocked/untested declaration")
    args = p.parse_args(argv)
    if args.cmd == "template":
        text = json.dumps(template(args.session_id), indent=2) + "\n"
        if args.out:
            with open(args.out, "x") as f:
                f.write(text)
        else:
            sys.stdout.write(text)
        return 0
    if args.cmd == "attempt":
        sys.stdout.write(json.dumps(attempt_block(args.run), indent=2) + "\n")
        return 0
    manifest = Path(args.manifest).resolve()
    try:
        report = build(json.loads(manifest.read_text()), manifest)
    except (OSError, ValueError) as error:  # ManifestError is a ValueError
        print(f"manifest rejected: {error}", file=sys.stderr)
        return 2
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=False)
    (out / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    (out / "report.md").write_text(render(report))
    print(out)
    return 1 if args.require_complete and not report["coverage_complete"] else 0


if __name__ == "__main__":
    sys.exit(main())

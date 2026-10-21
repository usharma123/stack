"""Append-only step evidence: one JSONL record per command plus raw stdout/stderr files."""
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import threading
import time

PHASES = ("meta", "setup", "lifecycle", "workload", "warm", "failure", "verify", "cleanup")
INNER = re.compile(rb"\n?@@RWB-INNER (\d+) (\d+) (-?\d+)\n?$")


def utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


@dataclass
class StepResult:
    seq: int
    label: str
    phase: str
    code: int
    outer_ns: int
    inner_ns: int = None
    stdout: str = ""
    stderr: str = ""
    timed_out: bool = False
    extra: dict = field(default_factory=dict)

    @property
    def ok(self):
        return self.code == 0 and not self.timed_out

    def json(self):
        """Parse the last JSON object line of stdout (the app prints exactly one)."""
        for line in reversed(self.stdout.strip().splitlines()):
            line = line.strip()
            if line.startswith("{"):
                try:
                    return json.loads(line)
                except ValueError:
                    return None
        return None


def split_inner(stderr):
    """Strip the in-container timing marker from stderr; return (stderr, inner_ns, inner_rc)."""
    match = INNER.search(stderr)
    if not match:
        return stderr, None, None
    start, end, code = (int(x) for x in match.groups())
    return stderr[:match.start()], end - start, code


class Recorder:
    def __init__(self, out):
        self.out = Path(out)
        self.out.mkdir(parents=True, exist_ok=False)
        (self.out / "logs").mkdir()
        self._steps = (self.out / "steps.jsonl").open("a")
        self._lock = threading.Lock()
        self.seq = 0
        self.results = []

    def next_seq(self):
        with self._lock:
            self.seq += 1
            return self.seq

    def write(self, seq, label, phase, argv, transport, code, outer_ns, stdout, stderr, timed_out,
              started, extra=None):
        if phase not in PHASES:
            raise ValueError(f"unknown phase {phase}")
        stderr, inner_ns, inner_code = split_inner(stderr)
        safe = re.sub(r"[^A-Za-z0-9_.-]", "_", label)[:80]
        base = self.out / "logs" / f"{seq:04d}-{safe}"
        base.with_suffix(".stdout").write_bytes(stdout)
        base.with_suffix(".stderr").write_bytes(stderr)
        record = dict(seq=seq, label=label, phase=phase, started_utc=started, transport=transport,
                      argv=argv, exit=code, timed_out=timed_out, outer_ns=outer_ns, inner_ns=inner_ns,
                      inner_exit=inner_code, stdout=str(base.with_suffix(".stdout").relative_to(self.out)),
                      stderr=str(base.with_suffix(".stderr").relative_to(self.out)), **(extra or {}))
        with self._lock:
            self._steps.write(json.dumps(record) + "\n")
            self._steps.flush()
        result = StepResult(seq, label, phase, code, outer_ns, inner_ns,
                            stdout.decode(errors="replace"), stderr.decode(errors="replace"),
                            timed_out, extra or {})
        self.results.append(result)
        return result

    def write_json(self, name, payload):
        path = self.out / name
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
        tmp.replace(path)

    def close(self):
        self._steps.close()

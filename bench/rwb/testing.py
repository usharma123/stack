"""Fakes for offline validity tests and --dry-run. They simulate a well-behaved tool; tests
inject faults (wrong instance, leaked markers, wrong code) to prove the checks catch them."""
import json
import re

from .record import StepResult


class FakeRecorder:
    def __init__(self):
        self.results, self.seq = [], 0

    def next_seq(self):
        self.seq += 1
        return self.seq


class FakeWorld:
    """Running services per checkout and app receipts. Faults are flags tests can set."""

    def __init__(self, scenario=None):
        self.scenario = scenario
        self.running = {}
        self.generation = {}
        self.data = {}           # checkout -> {"keeper": bool, "durable": bool}
        self.share_with = {}     # checkout -> checkout whose services it reaches (wrong instance)
        self.source_of = {}      # checkout -> checkout whose code runs (wrong code)
        self.fail = set()        # labels forced to fail
        self.lock_changes = False
        self.shared_server = False  # database-boundary tools: one cluster, per-checkout DBs
        self.bad_receipt = {}       # command -> result object replacing the correct one
        self.codes = {}             # label -> (exit code, timed_out) forced for that step
        self.no_hash = set()        # checkouts whose lock-hash step prints nothing
        self.mutators = []          # callables(StepResult) applied to every result (fault injection)

    def token(self, co):
        return self.scenario.co[co].token if self.scenario else f"tok-{co}"

    def path(self, co):
        return self.scenario.co[co].path if self.scenario else f"/w/{co}"

    def identity(self, co):
        target = self.share_with.get(co, co)
        if target not in self.running:
            return None
        gen = self.generation[target]
        index = "abcde".index(target)
        server = "shared" if self.shared_server else target
        src = self.source_of.get(co, co)
        return dict(
            pg=dict(data_directory=f"/w/{server}/pg", port=45000 + index, version="17.11", cluster_name="",
                    user="postgres", database=f"db_{target}" if self.shared_server else "postgres",
                    started=f"{server}-{gen if not self.shared_server else 0}",
                    system_identifier=f"sys-{server}"),
            redis=dict(run_id=f"run-{server}-{gen if not self.shared_server else 0}", port=46000 + index,
                       version="8.10.2", db=index if self.shared_server else 0, pid=1, dir=f"/w/{server}/redis",
                       appendonly="yes", appendfsync="always", save=""),
            migrations=["0001", "0002"], pg_markers=[target], redis_marker=target,
            python=dict(version="3.13.16", executable="/x/python"),
            urls=dict(database=f"postgresql://p@127.0.0.1:{45000 + index}/x", redis=f"redis://127.0.0.1:{46000 + index}/0"),
            declared={}, source=dict(module=f"{self.path(src)}/rwbapp", token=self.token(src)))

    def app(self, co, command, args):
        ident = self.identity(co)
        if ident is None:
            return 3, dict(command=command, ok=False, error=dict(code="unreachable", message="refused"))
        target = self.share_with.get(co, co)
        state = self.data.setdefault(target, dict(keeper=False, durable=False, migrated=False))
        result = {}
        if command in ("identity", "wait"):
            result = ident
        elif command == "migrate":
            result = dict(applied=[] if state["migrated"] else ["0001", "0002"], current=["0001", "0002"])
            state["migrated"] = True
        elif command == "mark":
            state["keeper"] = True
            result = dict(checkout=co)
        elif command == "crud":
            result = dict(steps=["create", "read", "update", "list", "delete"])
        elif command == "cache":
            result = dict(sequence=["miss", "hit", "miss", "hit"])
        elif command == "read":
            result = dict(source="hit", item=dict(sku=f"keeper-{co}"))
        elif command == "persist":
            state["durable"] = True
            result = dict(saved=True)
        elif command == "persisted":
            result = dict(pg_keeper=state["keeper"], redis_durable=state["durable"])
        elif command == "check":
            if target != co:
                return 1, dict(command=command, ok=False, error=dict(code="check_failed", message=f"saw {target}"))
            result = dict(ident, problems=[])
        if command in self.bad_receipt:
            result = self.bad_receipt[command]
        return 0, dict(command=command, ok=True, result=result)


class FakeTransport:
    kind = "fake"

    def __init__(self, recorder, adapter, world=None):
        self.recorder, self.adapter = recorder, adapter
        self.world = world or FakeWorld()
        self.calls = []
        self.created = True

    def exec(self, label, phase, body, timeout=600, user=None):
        self.calls.append((label, phase, body))
        seq = self.recorder.next_seq()
        code, out = 0, ""
        m = re.search(r"RWB_CHECKOUT=([a-e]) \S+ -m rwbapp (\w+)(.*?)(?:'|$)", body)
        co = re.match(r"([a-e])-", label)
        timed_out = False
        if label in self.world.codes:
            code, timed_out = self.world.codes[label]
            if label.endswith("-identity") or "rwbapp" in body:
                out = json.dumps(dict(command=m.group(2), ok=True, result={})) if m else ""
        elif label in self.world.fail:
            code = 1
        elif m and "pytest" not in body:
            code, payload = self.world.app(m.group(1), m.group(2), m.group(3))
            out = json.dumps(payload)
        elif "-m pytest" in body:
            code = 0 if self.world.identity(m.group(1) if m else co.group(1)) else 1
        elif co and (label.endswith("-start") or label.endswith("-restart") or label.endswith("-start-again")):
            name = co.group(1)
            if name not in self.world.running:
                self.world.running[name] = True
                self.world.generation[name] = self.world.generation.get(name, 0) + 1
        elif co and (label.endswith("-stop") or label.endswith("-cleanup")):
            self.world.running.pop(co.group(1), None)
        elif label.endswith("-lock-hash"):
            name = co.group(1)
            path = self.world.path(name)
            digest = "changed" if (name == "c" and self.world.lock_changes) else "same"
            if name not in self.world.no_hash:
                out = "".join(f"{digest}  {path}/{rel}\n" for rel in self.adapter.lock_files)
        elif label == "d-setup-invalid":
            code = 1
        elif label.endswith("-stopped-probe"):
            name = co.group(1)
            code = 1 if name in self.world.running else 0
        elif label == "e-planned-port":
            out = "45999\n"
        result = StepResult(seq, label, phase, code, 1_000_000, 900_000, out, "", timed_out)
        for mutate in self.world.mutators:
            mutate(result)
        self.recorder.results.append(result)
        return result

"""Outcome model. A tool's missing feature is `unsupported`, never `fail`.

pass            the check ran and its evidence satisfied the expectation
fail            the check ran and the tool (or its recipe) did not satisfy it
unsupported     the adapter declares the capability absent; nothing was run
not_applicable  the check does not apply to this tool's model (for example no owned ports)
blocked         a prerequisite check did not pass, so this one could not run meaningfully
observed        informational evidence with no expectation (for example a default-port collision)
error           the harness itself failed; the run is invalid
"""
from dataclasses import asdict, dataclass, field

STATUSES = ("pass", "fail", "unsupported", "not_applicable", "blocked", "observed", "error")
MODES = ("native", "scripted", "unsupported", "n/a")


@dataclass
class Outcome:
    check: str
    status: str
    mode: str = "n/a"
    detail: str = ""
    evidence: list = field(default_factory=list)  # step seq numbers

    def __post_init__(self):
        if self.status not in STATUSES:
            raise ValueError(f"bad status {self.status}")
        if self.mode not in MODES:
            raise ValueError(f"bad mode {self.mode}")


class Outcomes:
    def __init__(self):
        self.items = []

    def add(self, check, status, mode="n/a", detail="", evidence=()):
        if any(item.check == check for item in self.items):
            raise ValueError(f"duplicate outcome {check}")
        outcome = Outcome(check, status, mode, detail, [e.seq if hasattr(e, "seq") else e for e in evidence])
        self.items.append(outcome)
        return outcome

    def status(self, check):
        for item in self.items:
            if item.check == check:
                return item.status
        return None

    def passed(self, *checks):
        return all(self.status(c) == "pass" for c in checks)

    def valid(self):
        return not any(item.status == "error" for item in self.items)

    def as_list(self):
        return [asdict(item) for item in self.items]

"""The common real-world workload, identical for every adapter. See bench/README.md.

Every check produces an Outcome with the step evidence (seq numbers) that decided it.
Tool failures are results (`fail`); only harness faults are `error`. A capability the
adapter declares `unsupported` is recorded as such and its dependent checks are skipped.
"""
import hashlib
import secrets
import time

from . import verify
from .adapters.base import SHA256_FN
from .outcomes import Outcomes
from .stats import summarize_ns

CHECKOUTS = ("a", "b", "c", "d", "e")


class Scenario:
    def __init__(self, adapter, transport, recorder, repeats=20, warmups=3, only=None):
        self.ad, self.tx, self.rec = adapter, transport, recorder
        self.repeats, self.warmups = repeats, warmups
        self.only = set(only or ())
        self.out = Outcomes()
        self.co = {name: adapter.checkout(name, i, secrets.token_hex(8)) for i, name in enumerate(CHECKOUTS)}
        self.ids = {}           # checkout -> identity receipt
        self.instances = {}     # checkout -> adapter instance receipt
        self.started = set()    # checkouts whose services may be running (for cleanup)
        self.timings = {}
        self.locks = {}
        self.listeners = []     # (pid file, argv tag) of benchmark-owned listeners to release
        # Per checkout: the user-visible steps from setup to the first passing test suite
        # (no harness-only verification steps); summed into first_task.<co>.
        self.task_steps = {}
        self.task_t0 = {}
        self.port_maps = {}     # checkout -> validated port-mapping receipt (NAT tools)

    # ---- primitives ---------------------------------------------------------------
    def run(self, label, phase, body, timeout=None):
        return self.tx.exec(label, phase, body, timeout=timeout or self.ad.timeouts["step"])

    def feature(self, key):
        return self.ad.features[key]

    def mode(self, key):
        mode = self.feature(key)
        return "n/a" if mode == "unsupported" else mode

    def skipped(self, check):
        """True (and recorded) when the adapter declares check not applicable: nothing runs."""
        if check in self.ad.not_applicable:
            self.out.add(check, "not_applicable", "n/a", self.ad.not_applicable[check])
            return True
        return False

    def add(self, check, status, mode="n/a", detail="", evidence=()):
        if check in self.ad.not_applicable and status not in ("error",):
            return self.out.add(check, "not_applicable", mode, self.ad.not_applicable[check], evidence)
        return self.out.add(check, status, mode, detail, evidence)

    def app(self, co, label, phase, args, timeout=None):
        result = self.run(label, phase, self.ad.app(self.co[co], args), timeout)
        payload = result.json()
        command = args.split()[0]
        try:
            return result, verify.app_result(payload, command, result.code, result.timed_out)
        except ValueError as error:
            result.extra["app_error"] = str(error)
            return result, None

    def identify(self, co, label, phase="verify", wait=False):
        """App identity receipt for checkout co, with URL/path/source-token problems."""
        args = f"wait --timeout {self.ad.timeouts.get('ready', 90)}" if wait else "identity"
        result, ident = self.app(co, label, phase, args, timeout=self.ad.timeouts["start"])
        port_map = None
        map_body = self.ad.port_map(self.co[co]) if ident is not None else None
        if map_body:
            # Read the NAT mapping right after the identity, from the tool's own containers.
            mapped = self.run(f"{co}-port-map", "verify", map_body)
            port_map = mapped.json() if mapped.ok else f"port map command exit {mapped.code}"
            if port_map is None:  # a declared mapping must produce a receipt: fail closed
                port_map = "port map command printed no JSON receipt"
            self.port_maps[co] = port_map
        problems = ["no identity receipt: " + result.extra.get("app_error", "")] if ident is None else \
            verify.identity_problems(ident, self.co[co].token, self.ad.app_source_path(self.co[co]),
                                     port_map if map_body else None)
        if ident is not None:
            body = self.ad.instance_identity(self.co[co])
            if body:
                extra = self.run(f"{co}-instance-identity", "verify", body)
                self.instances[co] = extra.json() if extra.ok else None
        return result, ident, problems

    def hash_locks(self, co, label):
        files = " ".join(f"{self.co[co].path}/{rel}" for rel in self.ad.lock_files)
        # Missing files print nothing and are reported as missing by the caller.
        if not files:
            return None, {}
        result = self.run(label, "verify", f"{SHA256_FN}; rwb_sha256 {files}")
        hashes = {}
        for line in result.stdout.splitlines():
            parts = line.split()
            if len(parts) == 2:
                hashes[parts[1][len(self.co[co].path) + 1:]] = parts[0]
        return result, hashes

    # ---- the workload ---------------------------------------------------------------
    def prepare(self, co, lock_from=None, extra=""):
        """Prepare a checkout; a failure is recorded and blocks everything that depends on it."""
        body = self.ad.prepare(self.co[co], lock_from=self.co[lock_from] if lock_from else None)
        r = self.run(f"{co}-prepare", "meta", body + ("\n" + extra if extra else ""))
        self.timings[f"prepare.{co}"] = r.outer_ns if r.ok else None
        if not r.ok:
            self.add(f"prepare.{co}", "fail", "n/a", f"exit {r.code}" + (" (timed out)" if r.timed_out else ""), [r])
        return r.ok

    def execute(self):
        self.provision_and_versions()
        if not self.prepare("a"):
            for check in ("setup.a", "isolation", "stop.a", "lock.frozen_copy", "occupied_port"):
                self.add(check, "blocked", detail="checkout A preparation failed")
            return self.out
        if not self.bring_up("a", cold=True):
            return self.out
        self.workload("a")
        self.repeat_start("a")
        # B is a teammate's checkout of the same commit: A's lock is committed into it.
        if self.prepare("b", lock_from="a" if self.locks.get("a") else None):
            if self.bring_up("b", cold=False):
                self.workload("b")
        else:
            self.add("setup.b", "blocked", detail="checkout B preparation failed")
        self.isolation()
        self.repeats_phase()
        self.status_receipt()
        self.stop_restart()
        self.frozen_copy()
        self.bad_config()
        self.occupied_port()
        return self.out

    def provision_and_versions(self):
        for label, body, user in self.ad.provision():
            result = self.tx.exec(label, "meta", body, timeout=self.ad.timeouts["setup"], user=user)
            if result.code == BLOCKED_EXIT and not result.timed_out:
                # A missing environment prerequisite (adapter prints `RWB-BLOCKED: <reason>`):
                # an explicit blocked result with evidence, never a tool failure.
                reasons = [line.split("RWB-BLOCKED:", 1)[1].strip()
                           for line in (result.stdout + "\n" + result.stderr).splitlines() if "RWB-BLOCKED:" in line]
                raise ProvisionBlocked(f"{label}: " + ("; ".join(reasons) or "prerequisite missing (exit 77)"), result)
            if not result.ok:
                raise ProvisionError(f"provisioning step {label} failed (exit {result.code})")
        self.run("tool-version", "meta", self.ad.versions())

    def bring_up(self, co, cold):
        c = self.co[co]
        # A is the first checkout in this run (caches as the image left them, see
        # cache_note); B is a teammate's second checkout with A's lock and warm caches.
        phase_label = "first_checkout" if cold else "second_checkout"
        self.task_t0[co] = time.perf_counter_ns()
        setup = self.run(f"{co}-setup", "setup", self.ad.setup(c), self.ad.timeouts["setup"])
        self.task_steps[co] = [setup]
        self.timings[f"setup.{phase_label}"] = setup.outer_ns if setup.ok else None
        self.add(f"setup.{co}", "pass" if setup.ok else "fail", self.mode("lockfile"),
                 "" if setup.ok else f"exit {setup.code}", [setup])
        if not setup.ok:
            return False
        if cold:
            hashed, self.locks["a"] = self.hash_locks("a", "a-lock-hash")
            if self.ad.lock_files:
                ok = hashed is not None and hashed.ok and len(self.locks["a"]) == len(self.ad.lock_files)
                self.add("lock.created", "pass" if ok else "fail", self.mode("lockfile"),
                         "" if ok else "lock file(s) missing after setup", [hashed])
            else:
                self.add("lock.created", "unsupported", "n/a", "adapter declares no lock files")
        return self.start_and_identify(co, f"{co}-start", record_as=f"start.{co}", deps=phase_label)

    def install_deps(self, co, phase_label):
        c = self.co[co]
        deps = self.run(f"{co}-deps", "setup", self.ad.deps(c), self.ad.timeouts["setup"])
        self.task_steps.setdefault(co, []).append(deps)
        self.timings[f"deps.{phase_label}"] = deps.outer_ns if deps.ok else None
        self.add(f"deps.{co}", "pass" if deps.ok else "fail", "native", "" if deps.ok else f"exit {deps.code}", [deps])
        self.run(f"{co}-tool-versions", "meta", self.ad.tool_versions(c))
        return deps.ok

    def start_and_identify(self, co, label, record_as, deps=None):
        """start -> native readiness -> (first time: app deps) -> identity.

        Deps follow start because some tools (Stack) install their locked tools at `up`."""
        c = self.co[co]
        evidence = []
        if self.feature("services") == "unsupported":
            self.add(record_as, "unsupported", "n/a", "adapter declares no service lifecycle")
            return False
        start = self.run(label, "lifecycle", self.ad.start(c), self.ad.timeouts["start"])
        evidence.append(start)
        self.started.add(co)
        first = deps is not None  # first bring-up of this checkout: part of its first task
        if first:
            self.task_steps.setdefault(co, []).append(start)
        if not start.ok:
            self.add(record_as, "fail", self.mode("services"), f"start exit {start.code}", evidence)
            return False
        ready_body = self.ad.ready(c)
        if ready_body:
            ready = self.run(f"{label}-ready", "lifecycle", ready_body, self.ad.timeouts["start"])
            evidence.append(ready)
            if first:
                self.task_steps[co].append(ready)
            if not ready.ok:
                self.add(record_as, "fail", self.mode("readiness"), f"native readiness exit {ready.code}", evidence)
                return False
        if deps and not self.install_deps(co, deps):
            self.add(record_as, "blocked", self.mode("services"), "app dependencies failed", evidence)
            return False
        if ready_body or self.ad.start_waits_ready:
            # Native readiness claims usable services: the first identity must succeed without retries.
            result, ident, problems = self.identify(co, f"{label}-identity")
        else:
            result, ident, problems = self.identify(co, f"{label}-identity", wait=True)
        evidence.append(result)
        if first:
            self.task_steps[co].append(result)
        self.timings[f"ready.{co}"] = sum(r.outer_ns for r in evidence)
        if ident is None or problems:
            self.add(record_as, "fail", self.mode("readiness"), "; ".join(problems), evidence)
            return False
        self.ids[co] = ident
        self.add(record_as, "pass", self.mode("services"),
                 f"readiness={self.feature('readiness')} pg={ident['pg']['port']} redis={ident['redis']['port']}",
                 evidence)
        return True

    def workload(self, co):
        steps = []
        r, migrated = self.app(co, f"{co}-migrate", "workload", "migrate")
        steps.append(r)
        ok = migrated is not None and migrated["applied"] == ["0001", "0002"]
        r2, again = self.app(co, f"{co}-migrate-again", "workload", "migrate")
        steps.append(r2)
        self.add(f"migrate.{co}", "pass" if ok and again is not None and again["applied"] == [] else "fail",
                 "native", "" if ok else f"migrate receipt {migrated}", steps[-2:])
        results = []
        for args in (f"mark --checkout {co}", f"crud --checkout {co}", f"cache --checkout {co}"):
            r, res = self.app(co, f"{co}-{args.split()[0]}", "workload", args)
            results.append((r, res))
        crud_ok = all(res is not None for _, res in results)
        self.add(f"crud_cache.{co}", "pass" if crud_ok else "fail", "native",
                 "; ".join(r.extra.get("app_error", "") for r, res in results if res is None), [r for r, _ in results])
        tests = self.run(f"{co}-pytest", "workload", self.ad.pytest(self.co[co]))
        self.add(f"tests.{co}", "pass" if tests.ok else "fail", "native", "" if tests.ok else f"exit {tests.code}", [tests])
        self.first_task(co, [steps[0]] + [r for r, _ in results] + [tests])

    def first_task(self, co, workload_steps):
        """Time to first verified application work for checkout co: setup -> start ->
        readiness -> app deps -> first identity -> migrate -> mark/CRUD/cache -> tests.

        `steps_ns` sums those user-equivalent steps' outer times (harness-only receipts such as
        lock hashes and tool versions excluded); `wall_ns` is the host span including them.
        Recorded only when every gating check passed; otherwise the reason is kept."""
        steps = self.task_steps.get(co, []) + workload_steps
        gates = (f"setup.{co}", f"start.{co}", f"deps.{co}", f"migrate.{co}", f"crud_cache.{co}", f"tests.{co}")
        failed = [g for g in gates if self.out.status(g) != "pass"]
        key = f"first_task.{co}"
        if failed or not all(r.ok for r in steps):
            self.timings[key] = dict(ok=False, reason="not reached: " + ", ".join(failed or ["a step failed"]))
            return
        self.timings[key] = dict(ok=True, steps_ns=sum(r.outer_ns for r in steps),
                                 starts_from="prepared checkout", excluded_prepare=self.ad.prepare_scope,
                                 prepare_ns=self.timings.get(f"prepare.{co}"),
                                 wall_ns=time.perf_counter_ns() - self.task_t0[co],
                                 steps=[r.seq for r in steps], transport=self.tx.kind,
                                 checkout="first in run" if co == "a" else "second, A's lock, warm caches")

    def repeat_start(self, co):
        """Starting again while running must reuse, not duplicate or replace, the services."""
        if co not in self.ids:
            return
        again = self.run(f"{co}-start-again", "lifecycle", self.ad.start(self.co[co]), self.ad.timeouts["start"])
        if not again.ok:
            self.add("start.repeat", "fail", self.mode("services"),
                     f"repeated start exit {again.code}" + (" (timed out)" if again.timed_out else ""), [again])
            return
        result, ident, problems = self.identify(co, f"{co}-start-again-identity", wait=True)
        if ident is None:
            self.add("start.repeat", "fail", self.mode("services"), "; ".join(problems), [again, result])
            return
        problems += verify.unchanged_instance(self.ids[co], ident)
        detail = f"start exit {again.code}" + ("; " + "; ".join(problems) if problems else "")
        self.add("start.repeat", "fail" if problems else "pass", self.mode("services"), detail, [again, result])

    def isolation(self):
        if not ("a" in self.ids and "b" in self.ids):
            self.add("isolation", "blocked", detail="A or B did not reach a verified identity")
            return
        ra, ca = self.app("a", "a-check", "verify", "check --checkout a --forbid b")
        rb, cb = self.app("b", "b-check", "verify", "check --checkout b --forbid a")
        problems = []
        for r, c, name in ((ra, ca, "a"), (rb, cb, "b")):
            if c is None:
                problems.append(f"{name}: {r.extra.get('app_error')}")
        extra = (self.instances.get("a"), self.instances.get("b")) if self.ad.instance_identity(self.co["a"]) else None
        boundary = self.ad.isolation_boundary
        problems += verify.distinct_instances(self.ids["a"], self.ids["b"], boundary, extra)
        receipt = dict(boundary=boundary, a=verify.isolation_key(self.ids["a"], boundary),
                       b=verify.isolation_key(self.ids["b"], boundary))
        self.isolation_receipt = receipt
        detail = "; ".join(problems) or f"boundary={boundary}"
        self.add("isolation", "fail" if problems else "pass", self.mode("per_checkout_data"), detail, [ra, rb])

    def repeats_phase(self):
        if "a" not in self.ids:
            self.add("repeat.entry", "blocked", detail="A not running")
            self.add("repeat.app_read", "blocked", detail="A not running")
            return
        a = self.co["a"]
        for key, body in (("repeat.entry", self.ad.enter(a, "true")),
                          ("repeat.app_read", self.ad.app(a, "read --checkout a"))):
            samples, failed = [], []
            for i in range(self.warmups + self.repeats):
                warm = i < self.warmups
                r = self.run(f"{key}-{'warmup' if warm else 'sample'}-{i:03d}", "warm", body)
                if key == "repeat.app_read":
                    try:
                        item = verify.app_result(r.json(), "read", r.code, r.timed_out)["item"]
                        good = item.get("sku") == "keeper-a"
                    except ValueError:
                        good = False
                    if not good and r.code == 0:
                        r.code = 1  # semantic failure even though the process exited 0
                if not warm:
                    samples.append((r.ok, r.outer_ns, r.inner_ns, r.seq))
                    if not r.ok:
                        failed.append(r.seq)
            self.timings[key] = dict(outer=summarize_ns([(ok, o) for ok, o, _, _ in samples]),
                                     inner=summarize_ns([(ok, i) for ok, _, i, _ in samples]),
                                     warmups=self.warmups, transport=self.tx.kind)
            self.add(key, "fail" if failed else "pass", "native",
                     f"{len(failed)} of {len(samples)} samples failed" if failed else "", [s[3] for s in samples])

    def status_receipt(self):
        body = self.ad.status(self.co["a"])
        if body is None:
            self.add("status", "unsupported", "n/a", "no status command")
            return
        r = self.run("a-status", "lifecycle", body)
        self.add("status", "observed", self.mode("structured_status"), f"exit {r.code}", [r])

    def stop_restart(self):
        a, b = self.co["a"], self.co["b"]
        if "a" not in self.ids:
            for check in ("stop.a", "b.survives", "restart.a", "persist.pg", "persist.redis"):
                self.add(check, "blocked", detail="A not running")
            return
        self.app("a", "a-persist", "lifecycle", "persist --checkout a")
        stop = self.run("a-stop", "lifecycle", self.ad.stop(a), self.ad.timeouts["start"])
        gone = self.run("a-stopped-probe", "verify", self.ad.stopped_probe(a, self.ids["a"]), 60)
        # A stopped tool must not let the app silently reach anything: identity through the
        # tool's entry must fail. Two declared exceptions, both decided by stop + the
        # adapter's lifecycle-specific stopped probe instead:
        #  - entry_auto_resumes: the entry restarts a stopped project (DDEV), so no after-stop
        #    app call is made at all (it would undo the stop being verified);
        #  - stop_keeps_data_endpoints: data endpoints outlive a checkout stop by design
        #    (shared servers); the after-stop reachability is recorded as `observed`.
        resumes = bool(getattr(self.ad, "entry_auto_resumes", False))
        keeps = bool(getattr(self.ad, "stop_keeps_data_endpoints", False))
        refused = None if resumes else self.app("a", "a-after-stop", "verify", "identity", timeout=60)[0]
        steps = [r for r in (stop, gone, refused) if r is not None]
        if gone.timed_out or (refused is not None and refused.timed_out):
            self.add("stop.a", "blocked", self.mode("stop_confirmation"), "post-stop probe timed out", steps)
            self.ids.pop("a", None)
            for check in ("b.survives", "restart.a", "persist.pg", "persist.redis"):
                self.add(check, "blocked", detail="stop could not be confirmed")
            return
        app_refused = refused is not None and not refused.ok
        ok = stop.ok and gone.ok and (resumes or keeps or app_refused)
        if resumes:
            after = "after-stop entry not run (entry auto-resumes the project)"
        else:
            after = f"app after stop exit {refused.code}"
        self.add("stop.a", "pass" if ok else "fail", self.mode("stop_confirmation"),
                 f"stop exit {stop.code}, probe exit {gone.code}, {after}", steps)
        if keeps and refused is not None:
            self.add("stop.a.data_endpoints", "observed", "n/a",
                     "data endpoints still reachable after stop (declared shared-server design)" if refused.ok
                     else f"data endpoints refused after stop (exit {refused.code})", [refused])
        if "b" in self.ids:
            rb, cb = self.app("b", "b-after-a-stop", "verify", "check --checkout b --forbid a")
            self.add("b.survives", "pass" if cb is not None else "fail", "n/a",
                     rb.extra.get("app_error", ""), [rb])
        else:
            self.add("b.survives", "blocked", detail="B not running")
        before = self.ids.pop("a")
        if not self.start_and_identify("a", "a-restart", "restart.a"):
            for check in ("persist.pg", "persist.redis"):
                self.add(check, "blocked", detail="restart failed")
            return
        r, persisted = self.app("a", "a-persisted", "verify", "persisted --checkout a")
        problems = verify.restart_changes(before, self.ids["a"], self.ad.isolation_boundary)
        pg_ok = persisted is not None and persisted["pg_keeper"] and not problems
        self.add("persist.pg", "pass" if pg_ok else "fail", "native",
                 "; ".join(problems) or r.extra.get("app_error", ""), [r])
        redis_policy = {k: self.ids["a"]["redis"].get(k) for k in ("appendonly", "appendfsync", "save", "dir")}
        durable = persisted is not None and persisted["redis_durable"] is True
        self.add("persist.redis", "pass" if durable else "fail", "native", f"policy={redis_policy}", [r])
        rc, cache = self.app("a", "a-cache-after-restart", "verify", "cache --checkout a")
        self.add("cache.after_restart", "pass" if cache is not None else "fail", "native",
                 rc.extra.get("app_error", ""), [rc])

    def frozen_copy(self):
        c = self.co["c"]
        if self.skipped("lock.frozen_copy"):
            return
        if self.ad.frozen_setup(c) is None:
            self.add("lock.frozen_copy", "unsupported", "n/a", "no frozen/locked setup mode")
            return
        if "a" not in self.locks:
            self.add("lock.frozen_copy", "blocked", detail="A's lock was not produced")
            return
        if not self.prepare("c", lock_from="a"):
            self.add("lock.frozen_copy", "blocked", detail="checkout C preparation failed")
            return
        frozen_body = self.ad.frozen_setup(c)
        starts = getattr(self.ad, "frozen_setup_starts_services", None)
        if starts is None:  # infer: the frozen recipe embeds this adapter's start command
            start_body = self.ad.start(c) if self.feature("services") != "unsupported" else None
            starts = bool(start_body) and start_body in frozen_body
        if starts:
            self.started.add("c")  # its services must be cleaned up with per-checkout evidence
        r = self.run("c-frozen-setup", "setup", frozen_body, self.ad.timeouts["setup"])
        hashed, after = self.hash_locks("c", "c-lock-hash")
        expected = len(self.ad.lock_files)
        complete = (hashed is not None and hashed.ok and len(after) == expected
                    and len(self.locks["a"]) == expected)
        same = complete and after == self.locks["a"]
        versions = self.run("c-tool-versions", "verify", self.ad.tool_versions(c))
        a_versions = next((x for x in self.rec.results if x.label == "a-tool-versions"), None)
        # Version lines must match exactly; executable paths may legitimately differ per project
        # (e.g. a per-project Nix env wrapper) and are reported, not compared.
        va = _version_lines(_strip_paths(a_versions.stdout, self.co["a"].path)) if a_versions else None
        vc = _version_lines(_strip_paths(versions.stdout, c.path))
        same_versions = a_versions is not None and a_versions.ok and va == vc and bool(vc)
        paths_differ = a_versions is not None and _path_lines(_strip_paths(a_versions.stdout, self.co["a"].path)) != \
            _path_lines(_strip_paths(versions.stdout, c.path))
        ok = r.ok and same and versions.ok and same_versions
        if not complete:
            self.add("lock.frozen_copy", "blocked", self.mode("frozen_setup"),
                     f"lock hashes incomplete (A {len(self.locks['a'])}, C {len(after)} of {expected})",
                     [x for x in (r, hashed) if x is not None])
            return
        if r.timed_out or versions.timed_out:
            self.add("lock.frozen_copy", "blocked", self.mode("frozen_setup"), "timed out", [r, versions])
            return
        detail = (f"setup exit {r.code}; lock {'unchanged' if same else 'CHANGED'}; versions {'match' if same_versions else 'differ'}"
                  + ("; executable paths differ (observed)" if paths_differ else ""))
        self.add("lock.frozen_copy", "pass" if ok else "fail", self.mode("frozen_setup"), detail,
                 [r, hashed, versions])

    def bad_config(self):
        d = self.co["d"]
        if self.skipped("bad_config"):
            return
        try:
            breaker = self.ad.break_config(d)
        except NotImplementedError:
            self.add("bad_config", "unsupported", "n/a", "adapter has no invalid-version recipe")
            return
        if not self.prepare("d", extra=breaker):
            self.add("bad_config", "blocked", detail="checkout D preparation (or config breaking) failed")
            return
        before = self.run("d-procs-before", "verify", self.ad.service_processes())
        setup = self.run("d-setup-invalid", "failure", self.ad.setup(d), self.ad.timeouts["setup"])
        start_body = self.ad.start(d) if setup.ok and self.feature("services") != "unsupported" else None
        start = self.run("d-start-invalid", "failure", start_body, self.ad.timeouts["start"]) if start_body else None
        after = self.run("d-procs-after", "verify", self.ad.service_processes())
        steps = [r for r in (setup, start) if r is not None]
        kinds = [verify.refusal(r.code, r.timed_out) for r in steps]
        # The refusal must be the intended one: its output names the bad version/package.
        pattern = self.ad.bad_config_pattern
        intended = any(k == "refused" and verify.relevant(r.stdout + "\n" + r.stderr, pattern)
                       for k, r in zip(kinds, steps))
        leaked = len(after.stdout.splitlines()) > len(before.stdout.splitlines())
        if start is not None:
            self.started.add("d")
        evidence = [r for r in (before, setup, start, after) if r is not None]
        codes = f"setup exit {setup.code}" + (f", start exit {start.code}" if start else "")
        if "infra" in kinds or not (before.ok and after.ok):
            self.add("bad_config", "blocked", self.mode("lockfile"), f"infrastructure fault: {codes}", evidence)
            return
        if "refused" in kinds and not intended:
            # Something failed, but not because of the bad version: no evidence either way.
            self.add("bad_config", "blocked", self.mode("lockfile"),
                     f"{codes}: failure output lacks the intended diagnostic /{pattern}/", evidence)
            return
        ok = intended and not leaked
        self.add("bad_config", "pass" if ok else "fail", self.mode("lockfile"),
                 codes + (" (intended diagnostic)" if intended else " (bad version accepted)") +
                 (", new service processes left running" if leaked else ""), evidence)

    def occupied_port(self):
        """Bad startup: the port this checkout would use is held by a foreign listener."""
        e = self.co["e"]
        if self.skipped("occupied_port"):
            return
        if self.feature("services") == "unsupported":
            self.add("occupied_port", "unsupported", "n/a", "no service lifecycle")
            return
        if not self.prepare("e", lock_from="a" if self.locks.get("a") else None):
            self.add("occupied_port", "blocked", detail="checkout E preparation failed")
            return
        setup = self.run("e-setup", "setup", self.ad.setup(e), self.ad.timeouts["setup"])
        if not setup.ok:
            self.add("occupied_port", "blocked", detail=f"setup failed (exit {setup.code}); no listener started",
                     evidence=[setup])
            return
        port_body = self.ad.planned_pg_port(e)
        planned = e.pg_port
        if port_body:
            r = self.run("e-planned-port", "verify", port_body)
            try:
                planned = int(r.stdout.strip().splitlines()[-1])
            except (ValueError, IndexError):
                self.add("occupied_port", "blocked", detail="could not learn the planned port", evidence=[r])
                return
        # Run-unique pid file and argv tag: cleanup signals only a pid still running our tag.
        tag = f"rwb-squat-{self.tx_run_id()}-e"
        pidfile = f"{self.ad.workdir()}/.rwb-owned/{tag}.pid"
        # Registered before the listener starts, released in cleanup() whatever happens next.
        if _takes_tag(self.ad.occupy):
            self.listeners.append((pidfile, tag, None))
            body = self.ad.occupy(planned, pidfile, tag)
        else:  # adapter override from contract v1: ownership is checked by port instead
            self.listeners.append((pidfile, tag, planned))
            body = self.ad.occupy(planned, pidfile)
        squat = self.run("e-occupy-port", "failure", body)
        if not squat.ok:
            self.add("occupied_port", "blocked", detail="could not start the benchmark-owned listener",
                     evidence=[setup, squat])
            return
        start = self.run("e-start", "failure", self.ad.start(e), self.ad.timeouts["start"])
        self.started.add("e")
        ready = None
        evidence = [squat, start]
        pattern = self.ad.conflict_pattern(planned)
        start_relevant = verify.relevant(start.stdout + "\n" + start.stderr, pattern)
        ready_relevant = True
        if start.ok and self.ad.ready(e):
            r = self.run("e-ready", "failure", self.ad.ready(e), self.ad.timeouts["start"])
            ready = (r.code, r.timed_out)
            ready_relevant = verify.relevant(r.stdout + "\n" + r.stderr, pattern)
            evidence.append(r)
        ident = None
        if start.ok and (ready is None or ready[0] == 0):
            deps = self.run("e-deps", "setup", self.ad.deps(e), self.ad.timeouts["setup"])
            evidence.append(deps)
            if not deps.ok:
                self.add("occupied_port", "blocked", detail=f"app dependencies failed (exit {deps.code})",
                         evidence=evidence)
                return
            wait = ready is None
            result, ident, problems = self.identify("e", "e-identity", phase="failure", wait=wait)
            evidence.append(result)
            if ident is not None and problems:
                ident = None  # reached something that is not this checkout's verified instance
            if wait:
                # Scripted readiness: the app's own wait is the gate. Its failure alone proves
                # nothing about the conflict; it counts as detection only when the tool's own
                # service logs/status (conflict_logs) or the wait output name the conflict.
                ready = (0 if ident is not None else (124 if result.timed_out else 3), result.timed_out)
                if ident is None and not result.timed_out:
                    text = result.stdout + "\n" + result.stderr
                    logs_body = self.ad.conflict_logs(e)
                    if logs_body:
                        logs = self.run("e-conflict-logs", "failure", logs_body, 120)
                        evidence.append(logs)
                        text += "\n" + logs.stdout + "\n" + logs.stderr
                    ready_relevant = verify.relevant(text, pattern)
        label, good = verify.classify_conflict((start.code, start.timed_out), ready, ident, {planned},
                                               start_relevant, ready_relevant)
        self.add("occupied_port", "pass" if good else "fail", self.mode("readiness"), label, evidence)

    def block_all(self, reason, evidence=()):
        self.add("provision", "blocked", detail=reason, evidence=evidence)
        for check in MAIN_CHECKS:
            self.add(check, "blocked", detail="not executed: provisioning blocked")

    def collect_artifacts(self, dest):
        """Copy each started/prepared checkout's declared artifacts into dest (best effort,
        recorded). Needs a transport with copy_out; fakes skip it."""
        copied = []
        copy = getattr(self.tx, "copy_out", None)
        if copy is None:
            return copied
        for name in CHECKOUTS:
            for rel in self.ad.artifacts(self.co[name]):
                result = copy(f"{self.co[name].path}/{rel}", f"{dest}/{name}/{rel}")
                if result is not None:
                    copied.append(dict(checkout=name, path=rel, ok=result.ok, seq=result.seq))
        return copied

    def tx_run_id(self):
        return getattr(self.ad, "run_id", "dryrun")

    # ---- cleanup (always runs) ------------------------------------------------------
    def diagnose(self):
        """Pre-teardown diagnostics (outputs kept as step logs; never change an outcome)."""
        for name in CHECKOUTS:
            if name in self.started:
                for diag, body in self.ad.diagnostics(self.co[name]):
                    self.run(f"{name}-diag-{diag}", "cleanup", body, 120)

    def cleanup(self):
        problems = []
        for pidfile, tag, port in self.listeners:
            r = self.run("release-listener", "cleanup", self.ad.release(pidfile, tag, port))
            if not r.ok:
                problems.append(f"could not release listener {pidfile}")
        for name in reversed(CHECKOUTS):
            if name in self.started:
                body = self.ad.cleanup(self.co[name])
                if body:
                    r = self.run(f"{name}-cleanup", "cleanup", body, self.ad.timeouts["start"])
                    if not r.ok:
                        problems.append(f"{name} cleanup exit {r.code}")
        leftovers = self.run("leftover-processes", "cleanup", self.ad.service_processes())
        lines = [line for line in leftovers.stdout.splitlines() if line.strip()]
        if not leftovers.ok:
            # A failed or timed-out probe proves nothing about leftovers.
            self.add("cleanup.processes", "error", self.mode("stop_confirmation"),
                     f"process probe failed (exit {leftovers.code}, timed out {leftovers.timed_out})", [leftovers])
            problems.append("leftover process probe failed")
        else:
            self.add("cleanup.processes", "fail" if lines else "pass", self.mode("stop_confirmation"),
                     f"{len(lines)} service processes remain" if lines else "", [leftovers])
        supervisors = self.run("leftover-supervisors", "cleanup", self.ad.supervisor_processes())
        names = [line.split(None, 3)[-1][:80] for line in supervisors.stdout.splitlines() if line.strip()]
        self.add("cleanup.supervisors", "observed", "n/a", "; ".join(names) or "none", [supervisors])
        return problems


class ProvisionError(RuntimeError):
    pass


BLOCKED_EXIT = 77
# Checks that cannot run when provisioning is blocked (recorded as blocked, run stays valid).
MAIN_CHECKS = ("setup.a", "lock.created", "deps.a", "start.a", "migrate.a", "crud_cache.a", "tests.a",
               "start.repeat", "setup.b", "deps.b", "start.b", "migrate.b", "crud_cache.b", "tests.b",
               "isolation", "repeat.entry", "repeat.app_read", "status", "stop.a", "b.survives", "restart.a",
               "persist.pg", "persist.redis", "cache.after_restart", "lock.frozen_copy", "bad_config",
               "occupied_port")


class ProvisionBlocked(RuntimeError):
    def __init__(self, reason, result=None):
        super().__init__(reason)
        self.result = result


def _takes_tag(method):
    import inspect
    try:
        return "tag" in inspect.signature(method).parameters
    except (TypeError, ValueError):
        return False


def _strip_paths(text, path):
    return text.replace(path, "<checkout>")


def _is_path(line):
    return line.startswith(("/", "<checkout>"))


def _path_lines(text):
    return [line.strip() for line in text.splitlines() if _is_path(line.strip())]


def _version_lines(text):
    """Non-path lines carrying a version number (tool chatter such as "creating venv" is ignored)."""
    import re
    return [line.strip() for line in text.splitlines()
            if line.strip() and not _is_path(line.strip()) and re.search(r"\d+\.\d+", line)]


def sha256_text(text):
    return hashlib.sha256(text.encode()).hexdigest()

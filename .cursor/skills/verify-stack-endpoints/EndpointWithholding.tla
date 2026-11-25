---- MODULE EndpointWithholding ----
\* Which service endpoints Stack gives commands and reports (src/session.rs: verify_one,
\* verify_session, binding_vars, poison, endpoints, sanitize_endpoint, plan; src/project.rs:
\* check_single_instance_presets; src/shell.rs, src/main.rs and src/mcp.rs as surfaces).
\*
\* One checkout with a Postgres service `pg` and a Redis service `rd`, and in some worlds a
\* second Postgres service `pg2` that another layer declares. Stack's commands are serialized by
\* the project lock: `up` (start, then a verification loop that keeps each passing check), `down`
\* and one action per planned surface (status, env, exec without --require, exec --require pg,
\* exec --require-all, run), each of which compiles, checks every service live and builds the
\* command's environment in one step. Between any two steps the machine can kill a service, let
\* another program take its port, replace a process outside Stack's lifecycle commands, or the
\* project can edit its configuration. Each surface runs with a caller environment of its own.
\*
\* What is promised is what Stack checked when it planned: the identity it confirmed then, over
\* the variables the command gets, for the process the current generation's `up` launched.
\* Nothing is promised about a service that dies, or a port another program takes, after that
\* check; `PerpetualReach` states that stronger property only to show it fails (LIMIT config).
EXTENDS Naturals, FiniteSets

CONSTANTS Faults, Commands, Events

Fault(f) == f \in Faults

All == {"pg", "rd", "pg2"}
Postgres(s) == s # "rd"
Gens == {"a", "b", "c"}          \* a: as compiled; b: [env] PGPORT names another port; c: [env] DATABASE_URL names another server
Callers == {"clean", "hostaddr", "stale"}
Surfaces == {"status", "env", "exec", "execPg", "execAll", "run"}
NoPid == [s \in All |-> 0]
NoSession == [on |-> FALSE, gen |-> "a", launching |-> FALSE, pid |-> NoPid]

VARIABLES
    dup,          \* the composed stack declares a second postgres service (pg2)
    secret,       \* how the project's DATABASE_URL carries its password: "userinfo" or "query"
    cfg,          \* the configuration in the files now (its digest)
    session,      \* the session record: on, gen (digest up recorded), launching, pid (as recorded)
    pid,          \* the process the supervisor runs for each service (0: none, else a start count)
    listener,     \* what answers on each service's port: "mine" (its own server), "other", "none"
    phase,        \* "idle" or "verifying" (up's verification loop)
    upCaller,     \* the environment `up` runs with
    kept,         \* up's kept checks: a service that passed is not checked again
    keptPid,      \* the process each kept check saw (what the final record stores)
    keptTrue,     \* history: what each kept check's service really was when it passed
    holding,      \* history: a command was given the service's checked endpoint and may still run
    commands, events,
    uncheckedReach,      \* history: a command could connect for a service Stack did not confirm
    requiredUnverified,  \* history: a command that required a service ran without it confirmed
    listWrong,           \* history: STACK_UNVERIFIED named services other than those withheld
    reportedNotGiven,    \* history: a reported endpoint was not what a command got
    reportedForeign,     \* history: a reported endpoint named another port
    reportedCredential,  \* history: a report carried a password
    upUnchecked,         \* history: up recorded success for a service that never passed a real check
    siblingAnswered      \* history: a service's check was answered by another service of this checkout

vars == <<dup, secret, cfg, session, pid, listener, phase, upCaller, kept, keptPid, keptTrue, holding,
          commands, events, uncheckedReach, requiredUnverified, listWrong, reportedNotGiven,
          reportedForeign, reportedCredential, upUnchecked, siblingAnswered>>
flags == <<uncheckedReach, requiredUnverified, listWrong, reportedNotGiven, reportedForeign,
           reportedCredential, upUnchecked, siblingAnswered>>
upVars == <<phase, upCaller, kept, keptPid, keptTrue>>

Svcs == IF dup THEN All ELSE {"pg", "rd"}

Init ==
    /\ dup \in BOOLEAN
    /\ secret \in {"userinfo", "query"}
    /\ cfg = "a"
    /\ session = NoSession
    /\ pid = NoPid
    /\ listener = [s \in All |-> "none"]
    /\ phase = "idle" /\ upCaller = "clean"
    /\ kept = [s \in All |-> FALSE] /\ keptPid = NoPid /\ keptTrue = [s \in All |-> FALSE]
    /\ holding = [s \in All |-> FALSE]
    /\ commands = Commands /\ events = Events
    /\ uncheckedReach = FALSE /\ requiredUnverified = FALSE /\ listWrong = FALSE
    /\ reportedNotGiven = FALSE /\ reportedForeign = FALSE /\ reportedCredential = FALSE
    /\ upUnchecked = FALSE /\ siblingAnswered = FALSE

---------------------------------------------------------------------------------------------
\* Compile: project::check_single_instance_presets refuses a second postgres (or redis) service
\* before anything is reserved or started, in every compile (locked operations included).
Compiles == ~dup \/ Fault("DuplicateAllowed")

\* The provider's environment (mise::render): DATABASE_URL, PGHOST and PGPORT for the postgres
\* preset, REDIS_URL for redis. With two postgres services they share one DATABASE_URL, which
\* names pg's port. [env] in stack.toml can point DATABASE_URL elsewhere (cfg c) or PGPORT at
\* another port (cfg b); either is a configuration edit, so the digest changes.
UrlTarget == IF cfg = "c" THEN "away" ELSE "pg"
PortOwn == cfg # "b"

\* Where a libpq or redis client reaches for service s with the environment as Stack sees it
\* when probing (the provider's over Stack's own, which is the caller's): wire::client_vars sees
\* the caller's PGHOSTADDR and falls back to psql, which honours it over the URL's host.
ProbeTarget(s, c) ==
    IF ~Postgres(s) THEN "rd"
    ELSE IF c = "hostaddr" /\ ~Fault("ProbeIgnoresCaller") THEN "away"
    ELSE UrlTarget

\* The same, as the command's own client would reach it before anything is withheld.
RealTarget(s, c) ==
    IF ~Postgres(s) THEN "rd"
    ELSE IF c = "hostaddr" THEN "away"
    ELSE UrlTarget

\* The server a target leads to: a service's own server only while it listens on its port.
Server(t) == IF t \in All /\ listener[t] = "mine" THEN t ELSE "other"

\* verify_one (what verify_some and up's loop run): supervisor status running and the pid
\* alive, the port accepting connections, then the identity question over the app's URL
\* (current_setting('data_directory') / CONFIG GET dir compared with the supervisor's data dir).
VerifyOne(s, c) ==
    /\ pid[s] # 0
    /\ listener[s] # "none"
    /\ Server(ProbeTarget(s, c)) = s

\* verify_session: a check that passed still fails without a launch record (NotLaunched), while
\* the record says launching (LaunchIncomplete), when the digest differs (ConfigChanged), and
\* when the process is not the one the record names (ProcessChanged).
Overridden(s) ==
    \/ ~session.on
    \/ session.launching /\ ~Fault("LaunchingCountsVerified")
    \/ session.gen # cfg /\ ~Fault("StaleGenerationTrusted")
    \/ session.pid[s] # pid[s] /\ ~Fault("ProcessChangeIgnored")

Ready(s, c) == VerifyOne(s, c) /\ ~Overridden(s)

\* What is true, independently of how Stack decides it: the current generation's completed
\* launch named this very process, it serves its port, and the command's client reaches it.
Launched(s) ==
    session.on /\ ~session.launching /\ session.gen = cfg /\ pid[s] # 0 /\ session.pid[s] = pid[s]
Confirmed(s, c) == Launched(s) /\ Server(RealTarget(s, c)) = s

---------------------------------------------------------------------------------------------
\* Withholding (binding_vars, then poison in plan).

\* binding_vars: the preset's variables set by the provider or inherited from the caller (PGHOST
\* always, since libpq defaults it), plus variables naming the service's port. Before b623433
\* only the provider's were considered.
Withheld(s, c) ==
    IF ~Postgres(s) THEN {"REDIS_URL"}
    ELSE {"DATABASE_URL", "PGHOST", "PGPORT"}
         \cup (IF c = "hostaddr" /\ ~Fault("CallerVarsIgnored") THEN {"PGHOSTADDR"} ELSE {})

\* poison: anything that may select a host keeps a value naming unverified.stack.invalid
\* (URLs keep their credentials and port, host query keys and fragments dropped); variables that
\* name no host are removed. RemoveInsteadOfPoison removes everything (apps then fall back to a
\* default host).
Poisoned(v) ==
    IF v = "PGPORT" \/ Fault("RemoveInsteadOfPoison") THEN "absent" ELSE "invalid"

\* A variable's value in the command's environment: the provider's, else the caller's, else
\* unset; withheld ones poisoned or removed. `run` executes `mise run`, which evaluates the
\* provider configuration again and restores the provider's values.
Base(v, c) ==
    CASE v = "PGHOSTADDR" -> IF c = "hostaddr" THEN "caller" ELSE "absent"
      [] OTHER -> "prov"
Val(v, c, W, surface) ==
    IF v \notin W THEN Base(v, c)
    ELSE IF surface = "run" /\ Base(v, c) = "prov" THEN "prov"
    ELSE Poisoned(v)

\* Where the command's client connects for s, from its environment: libpq prefers PGHOSTADDR to
\* the URL's host; an unresolvable host connects nowhere; an unset host or port falls back to
\* libpq's or redis's default (never one of Stack's ports).
PgHost(c, W, surface, viaUrl) ==
    LET ha == Val("PGHOSTADDR", c, W, surface)
        url == Val("DATABASE_URL", c, W, surface)
        host == Val("PGHOST", c, W, surface)
        port == Val("PGPORT", c, W, surface)
    IN IF ha = "caller" THEN "away"
       ELSE IF ha = "invalid" THEN "none"
       ELSE IF viaUrl /\ url = "prov" THEN UrlTarget
       ELSE IF viaUrl /\ url = "invalid" THEN "none"
       ELSE IF host = "invalid" THEN "none"
       ELSE IF host = "absent" THEN "default"
       ELSE IF port = "prov" THEN (IF PortOwn THEN "pg" ELSE "otherport")
       ELSE "default"
UrlReach(s, c, W, surface) ==
    IF Postgres(s) THEN PgHost(c, W, surface, TRUE)
    ELSE CASE Val("REDIS_URL", c, W, surface) = "prov" -> "rd"
           [] Val("REDIS_URL", c, W, surface) = "invalid" -> "none"
           [] OTHER -> "default"
\* libpq from PG* variables alone, as `psql` with no URL connects.
EnvReach(s, c, W, surface) ==
    IF Postgres(s) THEN PgHost(c, W, surface, FALSE) ELSE UrlReach(s, c, W, surface)

\* A connection for s that is not to s's own confirmed server: anything but none for a service
\* Stack did not confirm (by URL or by PG* alone), and anything but its server by the app's URL
\* for one it did. PG* alone for a confirmed service are not promised (only DATABASE_URL is).
Unchecked(s, c, W, surface) ==
    IF Confirmed(s, c)
    THEN UrlReach(s, c, W, surface) \notin {s, "none"}
    ELSE UrlReach(s, c, W, surface) # "none" \/ EnvReach(s, c, W, surface) # "none"

\* Services whose app URL the command gets withheld (what STACK_UNVERIFIED must list).
UrlVar(s) == IF Postgres(s) THEN "DATABASE_URL" ELSE "REDIS_URL"
WithheldFrom(c, W, surface) == {s \in Svcs : Val(UrlVar(s), c, W, surface) # "prov"}

\* STACK_UNVERIFIED in the command's environment: the services that did not verify, else what
\* the caller had (a shell that loaded `stack env` earlier) unless Stack removes it.
Listed(c, U) ==
    IF U # {} THEN U
    ELSE IF c = "stale" /\ Fault("KeepInheritedList") THEN {"pg"}
    ELSE {}

\* plan: exec's --require names (CLI) or `require` (MCP); --require-all / `require_all`; a task
\* (`stack run`, MCP stack_run) always Require::All, because `mise run` restores what was withheld.
Required(surface) ==
    CASE surface = "execPg" -> {"pg"}
      [] surface = "execAll" -> Svcs
      [] surface = "run" -> IF Fault("RunRequiresNothing") THEN {} ELSE Svcs
      [] OTHER -> {}

\* endpoints: the rendered variables of a ready service whose values name its port (or a
\* companion when the URL does), sanitized. Before 023b6a7 every rendered variable was listed as
\* the environment held it, and passwords were masked only in a URL's user information.
Endpoints(s) ==
    IF ~Postgres(s) THEN {"REDIS_URL"}
    ELSE {"DATABASE_URL", "PGHOST"} \cup (IF PortOwn \/ Fault("EndpointsUnmatched") THEN {"PGPORT"} ELSE {})
Masked == secret = "userinfo" \/ ~Fault("NaiveMask")

---------------------------------------------------------------------------------------------
\* Stack's commands.

Begin == phase = "idle" /\ commands > 0 /\ commands' = commands - 1 /\ Compiles

\* up_within: compile; unless the session is this configuration's completed launch (`unverified`
\* false), record a new launch (launching: true) naming every service; start what is not running
\* (28ffb43: nothing again when all run unchanged), recording the processes seen. Over a completed
\* launch of this configuration no launch record is written: a service that died is started
\* again while the record keeps launching: false and the processes it named, so until up
\* finishes (or if it fails) the new process is not the recorded one (ProcessChanged). A service
\* whose port another program holds cannot bind and exits.
UpBegin(c) ==
    /\ Begin
    /\ LET current == session.on /\ session.gen = cfg /\ ~session.launching
           start(s) == s \in Svcs /\ (pid[s] = 0 \/ ~current)
           newPid == [s \in All |-> IF ~start(s) THEN pid[s]
                                    ELSE IF listener[s] = "other" THEN 0
                                    ELSE pid[s] + 1]
       IN /\ pid' = newPid
          /\ listener' = [s \in All |-> IF start(s) /\ listener[s] # "other" THEN "mine" ELSE listener[s]]
          /\ session' = IF current THEN session
                        ELSE [on |-> TRUE, gen |-> cfg, launching |-> TRUE, pid |-> newPid]
    /\ phase' = "verifying" /\ upCaller' = c
    /\ kept' = [s \in All |-> FALSE] /\ keptPid' = NoPid /\ keptTrue' = [s \in All |-> FALSE]
    /\ UNCHANGED <<dup, secret, cfg, holding, events, flags>>

\* Whose server answered s's identity question, when it is another service of this checkout.
Sibling(s, c) == Server(ProbeTarget(s, c)) \in Svcs \ {s}

\* verify_until_ready: one check of a service not yet kept (verify_some, which is verify_one);
\* one that fails is checked again on the next pass.
UpCheck(s) ==
    /\ phase = "verifying" /\ s \in Svcs /\ ~kept[s]
    /\ kept' = [kept EXCEPT ![s] = VerifyOne(s, upCaller)]
    /\ keptPid' = [keptPid EXCEPT ![s] = pid[s]]
    /\ keptTrue' = [keptTrue EXCEPT ![s] = pid[s] # 0 /\ Server(RealTarget(s, upCaller)) = s]
    /\ siblingAnswered' = (siblingAnswered \/ Sibling(s, upCaller))
    /\ UNCHANGED <<dup, secret, cfg, session, pid, listener, phase, upCaller, holding, commands,
                   events, uncheckedReach, requiredUnverified, listWrong, reportedNotGiven,
                   reportedForeign, reportedCredential, upUnchecked>>

\* exited_on_start (start_failed), or the deadline, or the command killed: the launching
\* record stays.
UpFails ==
    /\ phase = "verifying"
    /\ phase' = "idle"
    /\ UNCHANGED <<dup, secret, cfg, session, pid, listener, upCaller, kept, keptPid, keptTrue,
                   holding, commands, events, flags>>

\* Every service kept: the final record (launching: false) names, for each service, the process
\* its kept check saw (record_of(check)). exited_on_start looks at each pass's listing before
\* that pass's checks, so a kept service that dies after the last listing is still recorded; the
\* next plan's check finds it gone. (RecordListedPid, the description this model had before:
\* the processes of the last listing are recorded instead, which differs only when a process is
\* replaced inside the window, i.e. under SupervisorRetries.)
UpFinish ==
    /\ phase = "verifying"
    /\ \A s \in Svcs : kept[s]
    /\ LET recorded == IF Fault("RecordListedPid") THEN pid ELSE keptPid
       IN /\ session' = [session EXCEPT !.launching = FALSE, !.pid = recorded]
          /\ upUnchecked' = (upUnchecked \/ \E s \in Svcs : ~keptTrue[s] \/ recorded[s] # keptPid[s])
    /\ phase' = "idle"
    /\ UNCHANGED <<dup, secret, cfg, pid, listener, upCaller, kept, keptPid, keptTrue, holding, commands,
                   events, uncheckedReach, requiredUnverified, listWrong, reportedNotGiven,
                   reportedForeign, reportedCredential, siblingAnswered>>

\* down, confirmed: services stopped, records removed.
Down ==
    /\ Begin
    /\ session' = NoSession /\ pid' = NoPid
    /\ listener' = [s \in All |-> IF listener[s] = "mine" THEN "none" ELSE listener[s]]
    /\ UNCHANGED <<dup, secret, cfg, upVars, holding, events, flags>>

\* One planned surface (`plan`, or `status_with` for status), under the lock, with caller c:
\* every service checked live, then the command's environment and the report built from those
\* checks. A required service that did not verify refuses the command (service_unavailable).
Plan(surface, c) ==
    /\ Begin
    /\ LET U == {s \in Svcs : ~Ready(s, c)}
           W == UNION {Withheld(s, c) : s \in U}
           gives == surface # "status" /\ Required(surface) \cap U = {}
           R == Svcs \ U
       IN /\ uncheckedReach' = (uncheckedReach \/ (gives /\ \E s \in Svcs : Unchecked(s, c, W, surface)))
          /\ requiredUnverified' = (requiredUnverified \/ (gives /\ \E s \in Required(surface) : ~Confirmed(s, c)))
          /\ listWrong' = (listWrong \/ (gives /\ Listed(c, U) # WithheldFrom(c, W, surface)))
          /\ holding' = [s \in All |-> holding[s] \/ (gives /\ s \in Svcs /\ Confirmed(s, c) /\ UrlReach(s, c, W, surface) = s)]
          \* The report a ready service gets (status, up, exec --json), against what a command
          \* planned in this same state gets.
          /\ reportedNotGiven' = (reportedNotGiven \/ (surface = "status" /\
                 \E s \in R : \E v \in Endpoints(s) : Val(v, c, W, "exec") # "prov"))
          /\ reportedForeign' = (reportedForeign \/ (surface = "status" /\
                 \E s \in R : Postgres(s) /\ "PGPORT" \in Endpoints(s) /\ ~PortOwn))
          /\ reportedCredential' = (reportedCredential \/ (surface = "status" /\
                 \E s \in R : Postgres(s) /\ ~Masked))
          /\ siblingAnswered' = (siblingAnswered \/ \E s \in Svcs : Sibling(s, c))
    /\ UNCHANGED <<dup, secret, cfg, session, pid, listener, upVars, events, upUnchecked>>

---------------------------------------------------------------------------------------------
\* The machine, other programs and the project.

Event == events > 0 /\ events' = events - 1

ServiceDies(s) ==
    /\ Event /\ s \in Svcs /\ pid[s] # 0
    /\ pid' = [pid EXCEPT ![s] = 0]
    /\ listener' = [listener EXCEPT ![s] = IF @ = "mine" THEN "none" ELSE @]
    /\ UNCHANGED <<dup, secret, cfg, session, upVars, holding, commands, flags>>

PortTaken(s) ==
    /\ Event /\ listener[s] = "none"
    /\ listener' = [listener EXCEPT ![s] = "other"]
    /\ UNCHANGED <<dup, secret, cfg, session, pid, upVars, holding, commands, flags>>

\* A process replaced outside this checkout's lifecycle commands (an older Stack, someone using
\* the supervisor directly): the same server and data, a process the record does not name.
\* Never inside up's window: the supervisor starts a service only on a request (session.rs gives
\* every ServiceSpec retries: 0), and the project lock keeps other Stack commands out while up
\* runs. SupervisorRetries drops that assumption, as a supervisor restarting on its own would.
Respawn(s) ==
    /\ Event /\ (phase = "idle" \/ Fault("SupervisorRetries")) /\ s \in Svcs /\ pid[s] # 0
    /\ listener[s] = "mine"
    /\ pid' = [pid EXCEPT ![s] = @ + 1]
    /\ UNCHANGED <<dup, secret, cfg, session, listener, upVars, holding, commands, flags>>

EditConfig ==
    /\ Event
    /\ cfg' \in Gens \ {cfg}
    /\ UNCHANGED <<dup, secret, session, pid, listener, upVars, holding, commands, flags>>

\* Any command of a stack that does not compile is refused there, before anything else.
Refused ==
    /\ phase = "idle" /\ commands > 0 /\ ~Compiles
    /\ commands' = commands - 1
    /\ UNCHANGED <<dup, secret, cfg, session, pid, listener, upVars, holding, events, flags>>

Done ==
    /\ phase = "idle" /\ commands = 0
    /\ UNCHANGED vars

Next ==
    \/ \E c \in Callers : UpBegin(c)
    \/ \E s \in All : UpCheck(s)
    \/ UpFails \/ UpFinish \/ Down \/ Refused
    \/ \E surface \in Surfaces, c \in Callers : Plan(surface, c)
    \/ \E s \in All : ServiceDies(s) \/ PortTaken(s) \/ Respawn(s)
    \/ EditConfig
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------------------------
\* Safety properties.

\* A command can connect, for any service, only to that service's own server, which the current
\* generation's completed launch started and which Stack confirmed over the command's own
\* variables when it planned; otherwise every variable that could reach a server fails.
NoUncheckedReach == ~uncheckedReach

\* A command that required a service (exec --require, --require-all, run) ran only with it
\* confirmed.
RequireRefusesUnverified == ~requiredUnverified

\* STACK_UNVERIFIED names exactly the services whose endpoints the command was denied.
UnverifiedListIsCurrent == ~listWrong

\* An endpoint reported for a ready service is what a command planned then gets.
ReportedEndpointsAreGiven == ~reportedNotGiven

\* A reported endpoint names the service's own port.
ReportedEndpointsAreOwn == ~reportedForeign

\* No report carries a password.
ReportsCarryNoCredentials == ~reportedCredential

\* `up` records a launch as verified only when every service passed a real check during it, of
\* the very process it records.
UpVerifiedMeansChecked == ~upUnchecked

\* No service's identity question is answered by another service of the same checkout: each has
\* connection variables of its own, so each can verify (two postgres or two redis services share
\* them, and one of them can never verify).
NoCheckAnsweredBySibling == ~siblingAnswered

\* NOT promised (LIMIT config): a command given a checked endpoint keeps reaching that server
\* while it runs. A service that dies, or a port another program takes, after the check breaks it.
PerpetualReach == \A s \in All : holding[s] => listener[s] = "mine"
====

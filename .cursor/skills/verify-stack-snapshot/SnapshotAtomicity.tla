---- MODULE SnapshotAtomicity ----
\* A snapshot store and a two-service restore (src/snapshot.rs, with session::with_stopped,
\* init_tasks::set_markers and datadirs::{Held::swap_in, rebind, remove_tree}).
\*
\* Two actors: this checkout's commands, serialized by its project lock (`snapshot restore`,
\* `snapshot save --service a`, `restart b`, `up`), and another checkout of the same project
\* running one `snapshot save --force` that publishes a second generation. Both take the store
\* lock for store writes; a restore reads the generation it will restore before it holds the
\* store lock. Each rename, exchange, file write and directory removal is one action, and each
\* actor can be killed between any two. The fault names in `Faults` put back historical bugs.
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS Faults, Commands, Events

S == {"a", "b"}
Order == <<"a", "b">>
Valid == {"g1", "g2"}
Fault(f) == f \in Faults

VARIABLES
    exch,         \* the filesystem has an exchange (renameatx_np RENAME_SWAP, renameat2 EXCHANGE)
    published,    \* <store>/<name>: "g1", "g2", "torn" (metadata that is no generation), "none"
    aside,        \* valid generations a save set aside in `.old-*`
    tmp,          \* the saver's staging `.tmp-*`: "none", a generation
    prev,         \* what the saver's publication replaced
    saver,        \* the other checkout's `save --force`: its step
    saves,        \* forced saves left
    storeLock,    \* "free", "saver", "restorer"
    rcmd,         \* this checkout's command step, or "idle"
    cont,         \* the command a recovery runs for: "restore", "save", "up"
    rec,          \* the commit or undo running is recover_restore's
    whole,        \* every undo so far put its service back
    idx,          \* the service a loop over Order is at
    chosen,       \* the generation a restore chose before stopping anything
    copiedFrom,   \* history: the generation the restore copied
    confirm,      \* services this command confirmed stopped
    stopped,      \* services this command stopped, to start again at its end
    target,       \* each data directory's place: "old", "new" (the restored copy), "none", "other"
    fresh,        \* each copy's place `.<dir>.stack-restore-*`: "none", "new", "old"
    asideD,       \* each `.<dir>.stack-old-*`: "none", "old", "new"
    journal,      \* <state>/init/<checkout>.restore.json: "none", "swapping", "committed"
    markers,      \* each service's init marker describes: "old", "new"
    ledger,       \* each data record (data.json) names: "old", "new", "other"
    tokenSame,    \* the checkout is still the one whose token the journal names
    running,      \* each service runs
    commands, events,
    touchedRunning,   \* history: a restore step moved the data of a running service
    oldDroppedEarly,  \* history: old data was removed before the restore was recorded
    startedOnPending  \* history: a service started while a restore was still to finish or undo

vars == <<exch, published, aside, tmp, prev, saver, saves, storeLock, rcmd, cont, rec, whole,
          idx, chosen, copiedFrom, confirm, stopped, target, fresh, asideD, journal, markers,
          ledger, tokenSame, running, commands, events, touchedRunning, oldDroppedEarly,
          startedOnPending>>
storeVars == <<published, aside, tmp, prev, saver, saves>>
dataVars == <<target, fresh, asideD>>
recordVars == <<journal, markers, ledger, tokenSame>>
cmdVars == <<rcmd, cont, rec, whole, idx, chosen, copiedFrom, confirm, stopped>>

Init ==
    /\ exch \in BOOLEAN
    /\ published = "g1" /\ aside = {} /\ tmp = "none" /\ prev = "none"
    /\ saver = "idle" /\ saves = 1 /\ storeLock = "free"
    /\ rcmd = "idle" /\ cont = "restore" /\ rec = FALSE /\ whole = TRUE /\ idx = 1
    /\ chosen = "none" /\ copiedFrom = "none" /\ confirm = {} /\ stopped = {}
    /\ target = [s \in S |-> "old"]
    /\ fresh = [s \in S |-> "none"]
    /\ asideD = [s \in S |-> "none"]
    /\ journal = "none"
    /\ markers = [s \in S |-> "old"]
    /\ ledger = [s \in S |-> "old"]
    /\ tokenSame = TRUE
    /\ running = [s \in S |-> TRUE]
    /\ commands = Commands /\ events = Events
    /\ touchedRunning = FALSE /\ oldDroppedEarly = FALSE /\ startedOnPending = FALSE

---------------------------------------------------------------------------------------------
\* The store (generation, set_aside, recover_store, publish)

Newest(gens) == IF "g2" \in gens THEN "g2" ELSE "g1"

\* `generation`, read without the lock: the published one when its meta.json parses and names
\* it, else the newest valid one set aside.
ReadGen ==
    IF published \in Valid THEN published
    ELSE IF aside = {} \/ Fault("NoAsideRecovery") THEN "none"
    ELSE Newest(aside)

\* recover_store, under the store lock (its renames are taken as one step: between them the
\* name holds nothing and readers read the generation set aside): a name holding no valid
\* generation gets back the newest valid one set aside (what is at the name moved away first);
\* stale staging goes; one set aside stays while the name holds no valid generation.
RecoverStore ==
    LET seenValid == published \in Valid \/ (Fault("InvalidPublishedHidesAside") /\ published = "torn")
        putBack == ~seenValid /\ aside # {} /\ ~Fault("NoAsideRecovery")
        pub == IF putBack THEN Newest(aside) ELSE published
        left == IF putBack THEN aside \ {pub} ELSE aside
    IN /\ published' = pub
       /\ aside' = IF pub \in Valid \/ seenValid THEN {} ELSE left
       /\ tmp' = "none"

SaverLock ==
    /\ saver = "idle" /\ saves > 0 /\ storeLock = "free"
    /\ storeLock' = "saver" /\ saver' = "build"
    /\ RecoverStore
    /\ UNCHANGED <<exch, prev, saves, cmdVars, dataVars, recordVars, running, commands, events,
                   touchedRunning, oldDroppedEarly, startedOnPending>>

\* The new generation staged whole, meta.json last.
SaverBuild ==
    /\ saver = "build" /\ tmp' = "g2" /\ saver' = "publish"
    /\ UNCHANGED <<exch, published, aside, prev, saves, storeLock, cmdVars, dataVars, recordVars,
                   running, commands, events, touchedRunning, oldDroppedEarly, startedOnPending>>

\* publish: a rename onto nothing; else one exchange; else (no exchange, and always before
\* fbd8580) the published one renamed aside first.
SaverPublish ==
    /\ saver = "publish"
    /\ prev' = published
    /\ IF published = "none"
         THEN /\ published' = tmp /\ tmp' = "none" /\ saver' = "done" /\ UNCHANGED aside
       ELSE IF exch /\ ~Fault("NoAsideRecovery")
         THEN /\ published' = tmp /\ tmp' = published /\ saver' = "drop_tmp" /\ UNCHANGED aside
       ELSE /\ aside' = aside \cup ({published} \cap Valid)
            /\ published' = "none" /\ saver' = "publish2" /\ UNCHANGED tmp
    /\ UNCHANGED <<exch, saves, storeLock, cmdVars, dataVars, recordVars, running, commands,
                   events, touchedRunning, oldDroppedEarly, startedOnPending>>

SaverPublish2 ==
    /\ saver = "publish2" /\ published' = tmp /\ tmp' = "none" /\ saver' = "drop_old"
    /\ UNCHANGED <<exch, aside, prev, saves, storeLock, cmdVars, dataVars, recordVars, running,
                   commands, events, touchedRunning, oldDroppedEarly, startedOnPending>>

\* The exchanged-out old generation (now in the staging directory) is removed.
SaverDropTmp ==
    /\ saver = "drop_tmp" /\ tmp' = "none" /\ saver' = "done"
    /\ UNCHANGED <<exch, published, aside, prev, saves, storeLock, cmdVars, dataVars, recordVars,
                   running, commands, events, touchedRunning, oldDroppedEarly, startedOnPending>>

SaverDropOld ==
    /\ saver = "drop_old" /\ aside' = aside \ {prev} /\ saver' = "done"
    /\ UNCHANGED <<exch, published, tmp, prev, saves, storeLock, cmdVars, dataVars, recordVars,
                   running, commands, events, touchedRunning, oldDroppedEarly, startedOnPending>>

SaverDone ==
    /\ saver = "done" /\ saver' = "idle" /\ saves' = saves - 1 /\ storeLock' = "free"
    /\ UNCHANGED <<exch, published, aside, tmp, prev, cmdVars, dataVars, recordVars, running,
                   commands, events, touchedRunning, oldDroppedEarly, startedOnPending>>

---------------------------------------------------------------------------------------------
\* This checkout's data.

Journaled == IF journal = "none" THEN {} ELSE S
OldExists(s) == "old" \in {target[s], fresh[s], asideD[s]}
Svc == Order[idx]

\* The history checks every data-moving step of this checkout makes: whether it moved a running
\* service's data, and whether it removed old data before every service held its copy with
\* its marker and record describing it.
Touch ==
    /\ touchedRunning' = (touchedRunning \/ \E s \in S :
          running[s] /\ (target'[s] # target[s] \/ fresh'[s] # fresh[s] \/ asideD'[s] # asideD[s]))
    /\ oldDroppedEarly' = (oldDroppedEarly \/ \E s \in S :
          /\ OldExists(s)
          /\ "old" \notin {target'[s], fresh'[s], asideD'[s]}
          /\ ~(/\ \A t \in S : target[t] = "new"
               /\ markers[s] = "new"
               /\ ledger[s] = "new"))

Begin == rcmd = "idle" /\ commands > 0 /\ commands' = commands - 1

\* session::with_stopped: every service named and every service a pending journal names is
\* confirmed stopped before any data is touched (before 91a579b, only those named).
StopFor(named) ==
    LET c == named \cup (IF Fault("ReplayUnconfirmed") THEN {} ELSE Journaled)
        s == {x \in c : running[x]}
    IN /\ confirm' = c
       /\ stopped' = s
       /\ running' = [x \in S |-> running[x] /\ x \notin c]

\* `snapshot restore`: the generation is read before anything is stopped. One restore per
\* behavior (data is labelled old and new relative to it); the other commands replay it.
RestoreBegin ==
    /\ Begin /\ chosen = "none" /\ ReadGen \in Valid
    /\ chosen' = ReadGen /\ copiedFrom' = "none"
    /\ StopFor(S)
    /\ rcmd' = "recover" /\ cont' = "restore"
    /\ UNCHANGED <<exch, storeVars, storeLock, rec, whole, idx, dataVars, recordVars, events,
                   touchedRunning, oldDroppedEarly, startedOnPending>>

\* `snapshot save --service a`: what a save does to this checkout's data is recover a pending
\* restore first (its publication is the store's, modeled by the saver).
SaveBegin ==
    /\ Begin
    /\ StopFor({"a"})
    /\ rcmd' = "recover" /\ cont' = "save"
    /\ UNCHANGED <<exch, storeVars, storeLock, rec, whole, idx, chosen, copiedFrom, dataVars,
                   recordVars, events, touchedRunning, oldDroppedEarly, startedOnPending>>

\* `restart b` (snapshot::refuse_pending): refused while a restore is to be finished or undone.
RestartB ==
    /\ Begin
    /\ IF journal # "none" /\ ~Fault("RestartIgnoresJournal")
         THEN UNCHANGED <<running, startedOnPending>>
         ELSE /\ running' = [running EXCEPT !["b"] = TRUE]
              /\ startedOnPending' = (startedOnPending \/ journal # "none")
    /\ UNCHANGED <<exch, storeVars, storeLock, cmdVars, dataVars, recordVars, events,
                   touchedRunning, oldDroppedEarly>>

\* `up` (snapshot::recover_before_start): a pending restore is finished or undone first, only
\* while none of its services may run; then everything starts.
UpBegin ==
    /\ Begin
    /\ IF journal # "none" /\ \E s \in Journaled : running[s]
         THEN UNCHANGED <<rcmd, cont, confirm, stopped>>
         ELSE /\ rcmd' = IF journal = "none" THEN "up_start" ELSE "recover"
              /\ cont' = "up" /\ confirm' = Journaled /\ stopped' = {}
    /\ UNCHANGED <<exch, storeVars, storeLock, rec, whole, idx, chosen, copiedFrom, dataVars,
                   recordVars, running, events, touchedRunning, oldDroppedEarly, startedOnPending>>

UpStart ==
    /\ rcmd = "up_start"
    /\ running' = [s \in S |-> TRUE]
    /\ startedOnPending' = (startedOnPending \/ journal # "none")
    /\ rcmd' = "idle"
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, idx, chosen, copiedFrom,
                   confirm, stopped, dataVars, recordVars, commands, events, touchedRunning, oldDroppedEarly>>

AfterRecovery == CASE cont = "restore" -> "r_lock" [] cont = "save" -> "finish_ok" [] OTHER -> "up_start"

\* recover_restore(confirm): nothing touched unless every journaled service is confirmed
\* stopped; a committed restore is recorded, a swapping one undone.
Recover ==
    /\ rcmd = "recover"
    /\ IF journal = "none"
         THEN rcmd' = AfterRecovery /\ UNCHANGED <<rec, whole, idx>>
       ELSE IF ~(Journaled \subseteq confirm) /\ ~Fault("ReplayUnconfirmed")
         THEN rcmd' = "finish_ok" /\ UNCHANGED <<rec, whole, idx>>    \* stop_unconfirmed
       ELSE IF journal = "committed"
         THEN rcmd' = "c_check" /\ rec' = TRUE /\ UNCHANGED <<whole, idx>>
       ELSE rcmd' = "u_undo" /\ rec' = TRUE /\ whole' = TRUE /\ idx' = 1
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, chosen, copiedFrom, confirm, stopped,
                   dataVars, recordVars, running, commands, events, touchedRunning, oldDroppedEarly,
                   startedOnPending>>

\* Under the store lock: recover_store, then the generation read again must be the one chosen
\* (check_same), else nothing is replaced.
RestoreLock ==
    /\ rcmd = "r_lock" /\ storeLock = "free"
    /\ storeLock' = "restorer"
    /\ RecoverStore
    /\ LET now == IF published' \in Valid THEN published' ELSE "none"
       IN rcmd' = IF now = chosen \/ (Fault("NoGenerationRecheck") /\ now \in Valid)
                    THEN "r_copy" ELSE "finish_ok"
    /\ UNCHANGED <<exch, prev, saver, saves, cont, rec, whole, idx, chosen, copiedFrom, confirm,
                   stopped, dataVars, recordVars, running, commands, events, touchedRunning, oldDroppedEarly,
                   startedOnPending>>

\* 1. Every copy beside its data directory before anything is replaced (a failure removes them).
RestoreCopy ==
    /\ rcmd = "r_copy"
    /\ \/ /\ fresh' = [s \in S |-> "new"]
          /\ copiedFrom' = published
          /\ rcmd' = IF Fault("OneAtATime") THEN "r_swap" ELSE "r_journal"
          /\ idx' = 1
       \/ /\ rcmd' = "finish_ok" /\ UNCHANGED <<fresh, copiedFrom, idx>>
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, chosen, confirm, stopped,
                   target, asideD, recordVars, running, commands, events, startedOnPending>>
    /\ Touch

\* 2. The journal, before any swap (5af8555 introduced it).
RestoreJournal ==
    /\ rcmd = "r_journal"
    /\ \/ journal' = "swapping" /\ rcmd' = "r_swap" /\ UNCHANGED fresh
       \/ rcmd' = "finish_ok" /\ fresh' = [s \in S |-> "none"] /\ UNCHANGED journal  \* write failed
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, idx, chosen, copiedFrom,
                   confirm, stopped, target, asideD, markers, ledger, tokenSame, running,
                   commands, events, startedOnPending>>
    /\ Touch

\* A swap that fails puts its own directory back (Held::swap_in), then every swap is undone.
\* Before 5af8555 there was no journal or rollback, and old data was removed at once.
SwapFails ==
    IF Fault("OneAtATime") THEN rcmd' = "finish_stopped" /\ UNCHANGED <<rec, whole, idx>>
    ELSE rcmd' = "u_undo" /\ rec' = FALSE /\ whole' = TRUE /\ idx' = 1

\* 3. Each data directory swapped with its copy: one exchange, or two renames through aside.
RestoreSwap ==
    /\ rcmd = "r_swap"
    /\ IF idx > Len(Order)
         THEN /\ rcmd' = IF Fault("OneAtATime") THEN "c_markers" ELSE "r_commit"
              /\ UNCHANGED <<dataVars, rec, whole, idx>>
         ELSE LET s == Svc IN
              \/ /\ exch
                 /\ target' = [target EXCEPT ![s] = "new"]
                 /\ fresh' = [fresh EXCEPT ![s] = IF Fault("OneAtATime") THEN "none" ELSE "old"]
                 /\ idx' = idx + 1
                 /\ UNCHANGED <<asideD, rcmd, rec, whole>>
              \/ /\ ~exch
                 /\ target' = [target EXCEPT ![s] = "none"]
                 /\ asideD' = [asideD EXCEPT ![s] = "old"]
                 /\ rcmd' = "r_swap2"
                 /\ UNCHANGED <<fresh, rec, whole, idx>>
              \/ /\ SwapFails /\ UNCHANGED dataVars
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, chosen, copiedFrom, confirm, stopped,
                   recordVars, running, commands, events, startedOnPending>>
    /\ Touch

RestoreSwap2 ==
    /\ rcmd = "r_swap2"
    /\ LET s == Svc IN
       \/ /\ target' = [target EXCEPT ![s] = "new"]
          /\ fresh' = [fresh EXCEPT ![s] = "none"]
          /\ asideD' = [asideD EXCEPT ![s] = IF Fault("OneAtATime") THEN "none" ELSE "old"]
          /\ idx' = idx + 1 /\ rcmd' = "r_swap"
          /\ UNCHANGED <<rec, whole>>
       \/ /\ target' = [target EXCEPT ![s] = "old"]     \* the copy's rename failed: put back
          /\ asideD' = [asideD EXCEPT ![s] = "none"]
          /\ UNCHANGED fresh
          /\ SwapFails
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, chosen, copiedFrom, confirm, stopped,
                   recordVars, running, commands, events, startedOnPending>>
    /\ Touch

\* 4. The journal marked committed (a failed write rolls back).
RestoreCommit ==
    /\ rcmd = "r_commit"
    /\ \/ journal' = "committed" /\ rcmd' = "c_check" /\ rec' = FALSE /\ UNCHANGED <<whole, idx>>
       \/ SwapFails /\ UNCHANGED journal
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, chosen, copiedFrom, confirm, stopped,
                   dataVars, markers, ledger, tokenSame, running, commands, events,
                   touchedRunning, oldDroppedEarly, startedOnPending>>

\* check_committed (474434a): the checkout's token, each restored copy still the directory the
\* restore made at its place, each record still naming this checkout's directory there.
CommitCheck ==
    /\ rcmd = "c_check"
    /\ LET ok == tokenSame /\ (\A s \in S : target[s] = "new" /\ ledger[s] \in {"old", "new"})
       IN rcmd' = IF ok \/ Fault("CommitWithoutChecks") THEN "c_markers" ELSE "finish_stopped"
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, idx, chosen, copiedFrom,
                   confirm, stopped, dataVars, recordVars, running, commands, events,
                   touchedRunning, oldDroppedEarly, startedOnPending>>

\* init_tasks::set_markers: every marker in one write.
CommitMarkers ==
    /\ rcmd = "c_markers"
    /\ \/ markers' = [s \in S |-> "new"] /\ rcmd' = "c_ledger"
       \/ rcmd' = "finish_stopped" /\ UNCHANGED markers
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, idx, chosen, copiedFrom,
                   confirm, stopped, dataVars, journal, ledger, tokenSame, running, commands,
                   events, touchedRunning, oldDroppedEarly, startedOnPending>>

\* datadirs::rebind: every record bound to the journaled copy in one write, or none.
CommitLedger ==
    /\ rcmd = "c_ledger"
    /\ \/ /\ (\A s \in S : ledger[s] \in {"old", "new"}) \/ Fault("CommitWithoutChecks")
          /\ ledger' = [s \in S |-> "new"] /\ rcmd' = "c_clean" /\ idx' = 1
       \/ rcmd' = "finish_stopped" /\ UNCHANGED <<ledger, idx>>
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, chosen, copiedFrom, confirm,
                   stopped, dataVars, journal, markers, tokenSame, running, commands, events,
                   touchedRunning, oldDroppedEarly, startedOnPending>>

\* The old data removed, one service at a time (kept where a mount is inside it).
CommitClean ==
    /\ rcmd = "c_clean"
    /\ IF idx > Len(Order)
         THEN /\ rcmd' = (IF Fault("OneAtATime") THEN "finish_ok" ELSE "c_unjournal")
              /\ UNCHANGED <<dataVars, idx>>
         ELSE LET s == Svc IN
              /\ fresh' = [fresh EXCEPT ![s] = IF fresh[s] = "old" THEN "none" ELSE fresh[s]]
              /\ asideD' = [asideD EXCEPT ![s] = IF asideD[s] = "old" THEN "none" ELSE asideD[s]]
              /\ idx' = idx + 1
              /\ UNCHANGED <<target, rcmd>>
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, chosen, copiedFrom, confirm,
                   stopped, recordVars, running, commands, events, startedOnPending>>
    /\ Touch

CommitUnjournal ==
    /\ rcmd = "c_unjournal"
    /\ journal' = "none"
    /\ rcmd' = IF rec THEN AfterRecovery ELSE "finish_ok"
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, whole, idx, chosen, copiedFrom,
                   confirm, stopped, dataVars, markers, ledger, tokenSame, running, commands,
                   events, touchedRunning, oldDroppedEarly, startedOnPending>>

\* undo: each service put back by which directory is where, never by what its steps meant to
\* do; one that cannot be shown is left where it is. An I/O error can stop any undo.
Undo(s) ==
    CASE target[s] = "old" ->
           [t |-> "old", f |-> IF fresh[s] = "new" THEN "none" ELSE fresh[s], a |-> asideD[s], ok |-> TRUE]
      [] target[s] = "none" /\ asideD[s] = "old" ->
           [t |-> "old", f |-> IF fresh[s] = "new" THEN "none" ELSE fresh[s], a |-> "none", ok |-> TRUE]
      [] fresh[s] = "new" /\ asideD[s] # "old" ->
           [t |-> target[s], f |-> "none", a |-> asideD[s], ok |-> TRUE]
      [] target[s] = "new" /\ fresh[s] = "old" ->
           [t |-> "old", f |-> "none", a |-> asideD[s], ok |-> TRUE]
      [] target[s] = "new" /\ asideD[s] = "old" /\ fresh[s] = "none" ->
           [t |-> "old", f |-> "none", a |-> "none", ok |-> TRUE]
      [] OTHER -> [t |-> target[s], f |-> fresh[s], a |-> asideD[s], ok |-> FALSE]

UndoStep ==
    /\ rcmd = "u_undo"
    /\ IF idx > Len(Order)
         THEN /\ IF whole
                   THEN /\ journal' = "none"
                        /\ rcmd' = IF rec THEN AfterRecovery ELSE "finish_ok"
                   ELSE /\ rcmd' = "finish_stopped" /\ UNCHANGED journal
              /\ UNCHANGED <<dataVars, idx, whole>>
         ELSE LET s == Svc
                  u == Undo(s)
              IN \/ /\ target' = [target EXCEPT ![s] = u.t]
                    /\ fresh' = [fresh EXCEPT ![s] = u.f]
                    /\ asideD' = [asideD EXCEPT ![s] = u.a]
                    /\ whole' = (whole /\ u.ok)
                    /\ idx' = idx + 1
                 \/ /\ whole' = FALSE /\ idx' = idx + 1 /\ UNCHANGED dataVars
              /\ UNCHANGED <<rcmd, journal>>
    /\ UNCHANGED <<exch, storeVars, storeLock, cont, rec, chosen, copiedFrom, confirm, stopped,
                   markers, ledger, tokenSame, running, commands, events, startedOnPending>>
    /\ Touch

\* The end of with_stopped: the services stopped are started again, unless a restore left data
\* it could not finish or put back (restore_incomplete), which stays stopped.
Finish ==
    /\ rcmd \in {"finish_ok", "finish_stopped"}
    /\ running' = IF rcmd = "finish_ok" THEN [s \in S |-> running[s] \/ s \in stopped] ELSE running
    /\ startedOnPending' = (startedOnPending \/ (rcmd = "finish_ok" /\ stopped # {} /\ journal # "none"))
    /\ rcmd' = "idle"
    /\ storeLock' = IF storeLock = "restorer" THEN "free" ELSE storeLock
    /\ UNCHANGED <<exch, storeVars, cont, rec, whole, idx, chosen, copiedFrom, confirm, stopped,
                   dataVars, recordVars, commands, events, touchedRunning, oldDroppedEarly>>

---------------------------------------------------------------------------------------------
\* The environment.

Event == events > 0 /\ events' = events - 1

\* This checkout's command is killed between two steps (or the machine restarts under it).
CommandKilled ==
    /\ Event /\ rcmd # "idle"
    /\ rcmd' = "idle"
    /\ storeLock' = IF storeLock = "restorer" THEN "free" ELSE storeLock
    /\ UNCHANGED <<exch, storeVars, cont, rec, whole, idx, chosen, copiedFrom, confirm, stopped,
                   dataVars, recordVars, running, commands, touchedRunning, oldDroppedEarly, startedOnPending>>

SaverKilled ==
    /\ Event /\ saver # "idle"
    /\ saver' = "idle" /\ saves' = 0
    /\ storeLock' = IF storeLock = "saver" THEN "free" ELSE storeLock
    /\ UNCHANGED <<exch, published, aside, tmp, prev, cmdVars, dataVars, recordVars, running,
                   commands, touchedRunning, oldDroppedEarly, startedOnPending>>

\* The generation a killed save published is found damaged while the one it replaced is still
\* set aside (meta.json is synced before its rename, so this is damage, not the crash itself).
DamageMeta ==
    /\ Event /\ saver = "idle" /\ published \in Valid /\ aside # {}
    /\ published' = "torn"
    /\ UNCHANGED <<exch, aside, tmp, prev, saver, saves, storeLock, cmdVars, dataVars, recordVars,
                   running, commands, touchedRunning, oldDroppedEarly, startedOnPending>>

\* While a committed restore is unrecorded: another directory put at a restored copy's place,
\* the data record rebound by someone else, or the checkout replaced (another token).
EvidenceChanges ==
    /\ Event /\ journal = "committed" /\ rcmd = "idle"
    /\ \/ \E s \in S : target[s] = "new" /\ target' = [target EXCEPT ![s] = "other"]
                       /\ UNCHANGED <<ledger, tokenSame>>
       \/ \E s \in S : ledger[s] = "old" /\ ledger' = [ledger EXCEPT ![s] = "other"]
                       /\ UNCHANGED <<target, tokenSame>>
       \/ tokenSame /\ tokenSame' = FALSE /\ UNCHANGED <<target, ledger>>
    /\ UNCHANGED <<exch, storeVars, storeLock, cmdVars, fresh, asideD, journal, markers, running,
                   commands, touchedRunning, oldDroppedEarly, startedOnPending>>

Done ==
    /\ rcmd = "idle" /\ saver = "idle" /\ commands = 0
    /\ UNCHANGED vars

Next ==
    \/ SaverLock \/ SaverBuild \/ SaverPublish \/ SaverPublish2 \/ SaverDropTmp \/ SaverDropOld
    \/ SaverDone
    \/ RestoreBegin \/ SaveBegin \/ RestartB \/ UpBegin \/ UpStart \/ Recover \/ RestoreLock
    \/ RestoreCopy \/ RestoreJournal \/ RestoreSwap \/ RestoreSwap2 \/ RestoreCommit
    \/ CommitCheck \/ CommitMarkers \/ CommitLedger \/ CommitClean \/ CommitUnjournal
    \/ UndoStep \/ Finish
    \/ CommandKilled \/ SaverKilled \/ DamageMeta \/ EvidenceChanges
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------------------------
\* Safety properties.

\* Readers without the lock always find a generation of the snapshot.
ReaderFindsGeneration == ReadGen \in Valid

\* A valid generation is never lost: one is published or set aside.
ValidGenerationNeverLost == published \in Valid \/ aside # {}

\* A restore copies only the generation it chose before stopping the services.
RestoresChosenGeneration == copiedFrom \in {"none", chosen}

\* With no journal pending, every service holds its old data or every one its restored copy.
AllOrNothing ==
    journal = "none" => ((\A s \in S : target[s] = "old") \/ (\A s \in S : target[s] = "new"))

\* A service's old data goes only once every service holds its copy and its marker and record
\* describe the copy.
OldDataKeptUntilRecorded == ~oldDroppedEarly

\* With no journal pending, each init marker and data record describes the data in place.
MarkersDescribeData ==
    journal = "none" => \A s \in S : markers[s] = target[s] /\ ledger[s] = target[s]

\* No restore step moves the data of a running service.
NeverTouchesRunningService == ~touchedRunning

\* No service starts on data a restore has still to finish or undo.
NoStartOnPendingRestore == ~startedOnPending
====

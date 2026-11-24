---- MODULE SessionLifecycle ----
\* One checkout with one service, as `stack up`, `restart`, `down`, `down --purge`, `gc` and
\* `gc --reclaim-data` act on it (src/session.rs, src/session/gc_pass.rs, src/lease.rs,
\* src/checkout.rs, src/supervisor.rs, src/datadirs.rs), against a machine that crashes,
\* unmounts, deletes, and rewrites records under it. Every Stack step is one action; the
\* project lock serializes the commands of the checkout (`cmd`), and everything else
\* interleaves with them. The fault names in `Faults` put back historical bugs, each in the
\* one decision that bug got wrong.
EXTENDS Naturals, FiniteSets

CONSTANTS Faults, Commands, Events

Inst == {1, 2}                    \* service processes (generations) of the checkout
Unreadable == {"newer", "damaged"}
Fault(f) == f \in Faults

VARIABLES
    linux,        \* mounts can share a device (bind mounts) and PIDs are per namespace
    truth,        \* the checkout at its path: "present", "offline", "deleted", "replaced"
    offlineKind,  \* how an offline checkout is missing: "volume" (another device) or "bind"
    evidence,     \* the mount evidence records keep: "location", "device", "mountpoint"
    owner,        \* the lease's owner process: "none" (no owner), "alive", "exited"
    ownerNs,      \* the PID namespace the lease named it from: "same" or "other"
    ownerStart,   \* its start time could be read when the lease named it
    pidReused,    \* another process has the owner's PID now
    rebooted,     \* the machine restarted since the lease named the owner
    session,      \* the session record: "absent", "launching", "verified"
    running,      \* the checkout's live service processes
    recorded,     \* live processes the supervisor's state.json names (kept even when unreadable)
    hidden,       \* live processes whose environment, and so the checkout token, cannot be read
    supAlive,     \* the checkout's supervisor process runs
    supRec,       \* state.json: "none", "ok", "newer" (schema_version above 1), "damaged"
    ports,        \* the checkout's port reservation: "reserved", "released"
    data,         \* the service's recorded data directory: "absent", "intact", "partial", "removed"
    inner,        \* a mount in the data: "none", "dir" (inside), "file" (one file), "self"
    confirmed,    \* the last down or gc said every service stopped, and nothing started since
    cmd,          \* the step of the command holding the checkout's lock, or "idle"
    purge,        \* the running `down` was given --purge
    rm,           \* the step of a data removal: "idle", "check", "preflight", "remove"
    rmAllowed,    \* the removal may remove (down --purge, gc --reclaim-data), not only report
    sawMount,     \* a mount was in the data when this removal's preflight ran
    commands,     \* commands left to run
    events,       \* machine events left to happen
    reclaimedWhile,      \* history: the checkout's truth when gc dropped records or data as gone
    expiredWhileOwner,   \* history: the owner's truth when gc stopped the session as expired
    reportedWrong,       \* history: a status said the owner runs when it had exited
    removedWhileRunning, \* history: data was removed while a service process ran
    foreignLost,         \* history: what another mount held was removed
    lostAfterMountSeen   \* history: data was removed by a removal whose preflight ran beside a mount

vars == <<linux, truth, offlineKind, evidence, owner, ownerNs, ownerStart, pidReused, rebooted,
          session, running, recorded, hidden, supAlive, supRec, ports, data, inner, confirmed,
          cmd, purge, rm, rmAllowed, sawMount, commands, events,
          reclaimedWhile, expiredWhileOwner, reportedWrong, removedWhileRunning, foreignLost,
          lostAfterMountSeen>>
checkoutVars == <<linux, truth, offlineKind, evidence>>
leaseVars == <<owner, ownerNs, ownerStart, pidReused, rebooted>>
procVars == <<running, recorded, hidden, supAlive, supRec>>
historyVars == <<reclaimedWhile, expiredWhileOwner, reportedWrong, removedWhileRunning,
                 foreignLost, lostAfterMountSeen>>
removalVars == <<rm, rmAllowed, sawMount>>

Init ==
    /\ linux \in BOOLEAN
    /\ truth = "present"
    /\ offlineKind = "volume"
    /\ evidence \in {"location", "device"}   \* "device": a record Stack 0.1.24 or earlier kept
    /\ owner \in {"none", "alive"}
    /\ ownerNs \in {"same", "other"}
    /\ ownerStart \in BOOLEAN
    /\ pidReused = FALSE
    /\ rebooted = FALSE
    /\ session = "absent"
    /\ running = {} /\ recorded = {} /\ hidden = {}
    /\ supAlive = FALSE
    /\ supRec = "none"
    /\ ports = "released"
    /\ data = "absent"
    /\ inner = "none"
    /\ confirmed = FALSE
    /\ cmd = "idle" /\ purge = FALSE
    /\ rm = "idle" /\ rmAllowed = FALSE /\ sawMount = FALSE
    /\ commands = Commands
    /\ events = Events
    /\ reclaimedWhile = "never"
    /\ expiredWhileOwner = "never"
    /\ reportedWrong = FALSE
    /\ removedWhileRunning = FALSE
    /\ foreignLost = FALSE
    /\ lostAfterMountSeen = FALSE

---------------------------------------------------------------------------------------------
\* Observations, each as the code makes it.

\* lease::owner_state_with: what Stack can say of the lease's owner now. The start time now and
\* the kernel probe may each fail to answer (hidepid, another user's process), so a state is a
\* set of possible answers.
OwnerStates ==
    IF owner = "none" THEN {"none"}
    \* Before 3e18893 and e80afa3: alive while any process has the PID.
    ELSE IF Fault("PidOnlyOwner") THEN {IF owner = "alive" \/ pidReused THEN "alive" ELSE "exited"}
    ELSE IF rebooted THEN {"exited"}                      \* recorded in another boot
    ELSE IF linux /\ ownerNs = "other" /\ ~Fault("ForeignNamespaceTrusted") THEN {"unknown"}
    \* Judged by what the PID names here, in another namespace than the one it means.
    ELSE IF linux /\ ownerNs = "other" THEN {"exited", "unknown"}
    ELSE IF owner = "alive"
        THEN {IF ownerStart THEN "alive" ELSE "unknown", "unknown"}
             \* Before e80afa3: a probe that says nothing (EPERM, hidepid) was taken for an exit.
             \cup (IF Fault("UnknownOwnerExited") THEN {"exited"} ELSE {})
    ELSE IF pidReused THEN {IF ownerStart THEN "exited" ELSE "unknown", "unknown"}
    ELSE {"exited", "unknown"}

\* session::checkout_state over checkout::presence_with: whether the checkout is "present",
\* "gone" (deleted or replaced) or "unknown" (Unavailable). A lookup can always be refused or
\* fail (EACCES, EIO), which is unknown whatever is there.
Judge ==
    CASE truth = "present" -> "present"
      [] truth = "offline" ->
           \* A record naming the mount point's mount (only the 494f225 Refresh writes one).
           IF evidence = "mountpoint" THEN "gone"
           \* Before 93b56b0: a path that is not found is a deleted checkout.
           ELSE IF Fault("MissingPathIsDeleted") THEN "gone"
           \* Before 8717db4: the directory above on the recorded device shows a deletion.
           ELSE IF Fault("DeviceJudgesBindMount") /\ offlineKind = "bind" THEN "gone"
           \* Otherwise the mount above is not the one recorded (location), the device differs
           \* (an unmounted volume), or a device alone decides nothing where mounts share one.
           ELSE "unknown"
      [] OTHER -> IF evidence = "device" /\ linux THEN "unknown" ELSE "gone"
Judgements == {Judge, "unknown"}

\* What stopping finds of the running processes: those the record names (by pid, start time and
\* boot), and those carrying the checkout's token that can be read. A record that cannot be
\* read names nothing to the code that took it for none.
Found(base) == running \cap (base \cup (Inst \ hidden))
RecordBase == IF supRec \in Unreadable THEN {} ELSE recorded
\* supervisor::readable, and every strict read (apply, shutdown, wait_gone, forget, unresolved).
Refuses == supRec \in Unreadable /\ ~Fault("UnknownSchemaAsEmpty")

Begin == cmd = "idle" /\ rm = "idle" /\ commands > 0 /\ commands' = commands - 1

---------------------------------------------------------------------------------------------
\* stack up (session::up_within)

\* Under the project lock: both supervisor records must be readable before anything is stopped
\* or started; the compile reserves the ports; the checkout's mount is adopted (Recorded::adopted:
\* learned now, else the previous one kept).
UpBegin ==
    /\ Begin /\ truth = "present"
    /\ IF Refuses
         THEN cmd' = "idle" /\ UNCHANGED <<evidence, ports>>
         ELSE /\ cmd' = "up_record"
              /\ ports' = "reserved"
              /\ evidence' \in {"location", evidence}
    /\ UNCHANGED <<linux, truth, offlineKind, leaseVars, session, procVars, data, inner,
                   confirmed, purge, removalVars, events, historyVars>>

\* The launch is recorded before anything starts (`launching`, record_launch).
UpRecord ==
    /\ cmd = "up_record"
    /\ session' = "launching"
    /\ cmd' = "start"
    /\ UNCHANGED <<checkoutVars, leaseVars, procVars, ports, data, inner, confirmed, purge,
                   removalVars, commands, events, historyVars>>

\* start_in_order through supervisor::start. A supervisor that runs the service already has
\* nothing to start. Otherwise one is spawned: it refuses to start over a state it cannot read,
\* first stops what a dead supervisor left (supervisor::recover: by recorded pid, start time and
\* boot, and by token), refuses to start anything while one of those survives, and records each
\* process as it spawns it. A start can fail for reasons of its own (the process exits).
Start ==
    /\ cmd = "start"
    /\ \/ /\ supAlive /\ running # {}
          /\ cmd' = "verify"
          /\ UNCHANGED <<procVars, data, confirmed>>
       \/ /\ ~(supAlive /\ running # {})
          /\ ~Refuses
          /\ LET orphans == IF supAlive THEN {} ELSE running
                 stopped == orphans \cap Found(RecordBase)
                 left == running \ stopped
             IN \/ /\ \E i \in Inst \ left, h \in BOOLEAN :
                         /\ running' = left \cup {i}
                         \* A record taken for none is written over: only what this spawn made.
                         /\ recorded' = {i} \cup (IF supRec \in Unreadable THEN {} ELSE recorded \cap left)
                         /\ hidden' = (IF h THEN hidden \cup {i} ELSE hidden \ {i}) \cap running'
                   /\ supAlive' = TRUE
                   /\ supRec' = "ok"
                   /\ data' = IF data \in {"absent", "removed"} THEN "intact" ELSE data
                   /\ confirmed' = FALSE
                   /\ cmd' = "verify"
                \/ /\ stopped # {}          \* one did not stop: nothing new is started
                   /\ cmd' = "idle"
                   /\ UNCHANGED <<procVars, data, confirmed>>
       \/ /\ ~(supAlive /\ running # {})
          /\ cmd' = "idle"                  \* refused, or the start failed
          /\ UNCHANGED <<procVars, data, confirmed>>
    /\ UNCHANGED <<checkoutVars, leaseVars, session, ports, inner, purge, removalVars, commands,
                   events, historyVars>>

\* Every service verified: the record of the finished launch.
Verify ==
    /\ cmd = "verify"
    /\ session' = "verified"
    /\ cmd' = "idle"
    /\ UNCHANGED <<checkoutVars, leaseVars, procVars, ports, data, inner, confirmed, purge,
                   removalVars, commands, events, historyVars>>

---------------------------------------------------------------------------------------------
\* stack restart (session::restart_within)

RestartBegin ==
    /\ Begin /\ truth = "present" /\ session = "verified"
    /\ cmd' = IF Refuses THEN "idle" ELSE "restart_stop"
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, inner, confirmed,
                   purge, removalVars, events, historyVars>>

\* stop_and_wait: supervisor::stop, then wait_gone of the service (recovering by record and
\* token if the supervisor died). Not confirmed in time: the restart fails, nothing started.
RestartStop ==
    /\ cmd = "restart_stop"
    /\ LET found == Found(RecordBase) IN
         \/ /\ Refuses                 \* every look at the state is strict
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden>>
         \/ /\ ~Refuses
            /\ running' = running \ found
            /\ recorded' = recorded \ found
            /\ hidden' = hidden \ found
            /\ cmd' = "restart_record"
         \/ /\ ~Refuses /\ found # {}
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden>>
    /\ UNCHANGED <<checkoutVars, leaseVars, session, supAlive, supRec, ports, data, inner,
                   confirmed, purge, removalVars, commands, events, historyVars>>

RestartRecord ==
    /\ cmd = "restart_record"
    /\ session' = "launching"
    /\ cmd' = "start"
    /\ UNCHANGED <<checkoutVars, leaseVars, procVars, ports, data, inner, confirmed, purge,
                   removalVars, commands, events, historyVars>>

---------------------------------------------------------------------------------------------
\* stack down [--purge] (session::down_inner, down_unforgetting, down_stopping, purge_locked)

\* supervisor::shutdown reads both records strictly: one it cannot read fails the down, which
\* leaves its unconfirmed marker and stops nothing.
DownBegin ==
    /\ Begin /\ truth = "present"
    /\ purge' \in BOOLEAN
    /\ cmd' = IF Refuses THEN "idle" ELSE "down_wait"
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, inner, confirmed,
                   removalVars, events, historyVars>>

\* The shutdown is applied and waited for, then strays on the reserved ports and carriers of the
\* token are stopped. What is not found is not waited for. Not stopped by the deadline:
\* stop_unconfirmed, records kept.
DownWait ==
    /\ cmd = "down_wait"
    /\ LET found == Found(RecordBase) IN
         \/ /\ Refuses                 \* apply and every wait_gone look are strict (62e4fab)
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden, supAlive>>
         \/ /\ ~Refuses
            /\ running' = running \ found
            /\ recorded' = recorded \ found
            /\ hidden' = hidden \ found
            /\ supAlive' = FALSE
            /\ cmd' = "down_confirm"
         \/ /\ ~Refuses /\ found # {}
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden, supAlive>>
    /\ UNCHANGED <<checkoutVars, leaseVars, session, supRec, ports, data, inner, confirmed,
                   purge, removalVars, commands, events, historyVars>>

\* Confirmed: both session records removed. With --purge, purge_locked removes the recorded data
\* as reclaim_data does, then releases the ports.
DownConfirm ==
    /\ cmd = "down_confirm"
    /\ session' = "absent"
    /\ confirmed' = TRUE
    /\ IF purge
         THEN /\ cmd' = "purge" /\ rm' = "check" /\ rmAllowed' = TRUE
         ELSE /\ cmd' = "idle" /\ UNCHANGED <<rm, rmAllowed>>
    /\ UNCHANGED <<checkoutVars, leaseVars, procVars, ports, data, inner, purge, sawMount,
                   commands, events, historyVars>>

---------------------------------------------------------------------------------------------
\* stack gc [--reclaim-data] (gc_pass::gc_with, session::gc_locked, reclaim_gone,
\* reconcile_gone, reclaim_deleted)

\* One pass. Data and port records are brought up to date by observation (checkout::Refresh),
\* which changes one only from the very mount it names. An indexed session is judged under the
\* lock: unknown is left alone, gone is reclaimed, present is stopped once its lease ran out.
\* Without a session, what a gone checkout left is reclaimed.
GcBegin ==
    /\ Begin
    \* 494f225: Refresh took the mount of whatever was at the path (an empty mount point).
    /\ evidence' = IF Fault("RefreshAdoptsMountPoint") /\ truth = "offline" THEN "mountpoint" ELSE evidence
    /\ rmAllowed' \in BOOLEAN
    /\ IF session = "absent"
         THEN /\ cmd' = "gc_deleted"
              /\ UNCHANGED <<reclaimedWhile, expiredWhileOwner>>
         ELSE \E j \in Judgements :
                CASE j = "gone" ->
                       /\ cmd' = "gc_gone"
                       /\ UNCHANGED <<reclaimedWhile, expiredWhileOwner>>
                  [] j = "present" ->
                       \E st \in OwnerStates :
                         IF st = "exited"
                           THEN /\ cmd' = "gc_expire"
                                /\ expiredWhileOwner' = owner
                                /\ UNCHANGED reclaimedWhile
                           ELSE /\ cmd' = "idle"
                                /\ UNCHANGED <<reclaimedWhile, expiredWhileOwner>>
                  [] OTHER ->
                       /\ cmd' = "idle"
                       /\ UNCHANGED <<reclaimedWhile, expiredWhileOwner>>
    /\ UNCHANGED <<linux, truth, offlineKind, leaseVars, session, procVars, ports, data, inner,
                   confirmed, purge, rm, sawMount, events, reportedWrong, removedWhileRunning,
                   foreignLost, lostAfterMountSeen>>

\* reclaim_gone: shutdown through the recorded supervisor, then reconcile_gone (the supervisor's
\* state, then supervisor::unresolved, which reports an unreadable record). Anything not
\* confirmed keeps the session record. The faulty reading takes an unreadable record for none:
\* nothing is stopped, nothing is unresolved.
GcGone ==
    /\ cmd = "gc_gone"
    /\ LET found == IF supRec \in Unreadable THEN {} ELSE Found(recorded) IN
         \/ /\ Refuses
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden, supAlive, session, confirmed, reclaimedWhile>>
         \/ /\ ~Refuses
            /\ running' = running \ found
            /\ recorded' = recorded \ found
            /\ hidden' = hidden \ found
            /\ supAlive' = (supAlive /\ supRec \in Unreadable)
            /\ session' = "absent"
            /\ confirmed' = TRUE
            /\ reclaimedWhile' = truth
            /\ cmd' = "gc_deleted"
         \/ /\ ~Refuses /\ found # {}
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden, supAlive, session, confirmed, reclaimedWhile>>
    /\ UNCHANGED <<checkoutVars, leaseVars, supRec, ports, data, inner, purge, removalVars,
                   commands, events, expiredWhileOwner, reportedWrong, removedWhileRunning,
                   foreignLost, lostAfterMountSeen>>

\* gc_locked for a present checkout whose lease ran out: down_locked (no purge).
GcExpire ==
    /\ cmd = "gc_expire"
    /\ LET found == Found(RecordBase) IN
         \/ /\ Refuses
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden, supAlive, session, confirmed>>
         \/ /\ ~Refuses
            /\ running' = running \ found
            /\ recorded' = recorded \ found
            /\ hidden' = hidden \ found
            /\ supAlive' = FALSE
            /\ session' = "absent"
            /\ confirmed' = TRUE
            /\ cmd' = "idle"
         \/ /\ ~Refuses /\ found # {}
            /\ cmd' = "idle"
            /\ UNCHANGED <<running, recorded, hidden, supAlive, session, confirmed>>
    /\ UNCHANGED <<checkoutVars, leaseVars, supRec, ports, data, inner, purge, removalVars,
                   commands, events, historyVars>>

\* reclaim_deleted, with no session record left: ports::release_gone and each data record are
\* judged again (under the checkout's lock for the data); the data goes through reclaim_data,
\* which reports it without --reclaim-data.
GcDeleted ==
    /\ cmd = "gc_deleted"
    /\ \E j \in Judgements :
         IF j = "gone"
           THEN /\ ports' = "released"
                /\ reclaimedWhile' = IF ports = "reserved" \/ data \in {"intact", "partial"}
                                       THEN truth ELSE reclaimedWhile
                /\ rm' = "check"
                /\ cmd' = "gc_rm"
           ELSE /\ cmd' = "idle"
                /\ UNCHANGED <<ports, reclaimedWhile, rm>>
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, data, inner, confirmed, purge,
                   rmAllowed, sawMount, commands, events, expiredWhileOwner, reportedWrong,
                   removedWhileRunning, foreignLost, lostAfterMountSeen>>

\* The end of a purge or a gc data pass. A purge releases the ports. gc then takes a gone
\* checkout's supervisor directory (supervisor::orphaned_dirs): supervisor::shutdown, then
\* forget, which keeps a record it cannot read or one naming a live process.
ForgetSupervisor ==
    LET found == IF supRec \in Unreadable THEN {} ELSE Found(recorded)
        left == running \ found
    IN /\ running' = left
       /\ hidden' = hidden \cap left
       /\ supAlive' = (supAlive /\ supRec \in Unreadable)
       /\ IF supRec \in Unreadable \/ recorded \cap left = {}
            THEN /\ supRec' = "none"
                 /\ recorded' = {}
                 /\ reclaimedWhile' = IF supRec # "none" THEN truth ELSE reclaimedWhile
            ELSE /\ recorded' = recorded \cap left
                 /\ UNCHANGED <<supRec, reclaimedWhile>>

RmDone ==
    /\ cmd \in {"purge", "gc_rm"} /\ rm = "idle"
    /\ cmd' = "idle"
    /\ \/ /\ cmd = "purge"
          /\ ports' = "released"
          /\ UNCHANGED <<procVars, reclaimedWhile>>
       \/ /\ cmd = "gc_rm"
          /\ \E j \in Judgements : IF j = "gone" /\ ~Refuses THEN ForgetSupervisor
                                                       ELSE UNCHANGED <<procVars, reclaimedWhile>>
          /\ UNCHANGED ports
    /\ UNCHANGED <<checkoutVars, leaseVars, session, data, inner, confirmed, purge, removalVars,
                   commands, events, expiredWhileOwner, reportedWrong, removedWhileRunning,
                   foreignLost, lostAfterMountSeen>>

---------------------------------------------------------------------------------------------
\* Removing a recorded data directory (session::reclaim_data_with, datadirs::hold, Held::remove)

\* The recorded directory is held (none there: settled); the recorded supervisor must confirm the
\* service stopped (service_stopped: its state, then unresolved) and no process carrying the
\* token may be seen; only then, and only when allowed, does the removal start.
RmCheck ==
    /\ rm = "check"
    /\ LET confirmedStopped ==
             /\ ~Refuses
             /\ RecordBase \cap running = {}
             /\ running \ hidden = {}
       IN rm' = IF data \in {"intact", "partial"} /\ confirmedStopped /\ rmAllowed
                  THEN "preflight" ELSE "idle"
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, inner, confirmed,
                   cmd, purge, rmAllowed, sawMount, commands, events, historyVars>>

\* What the check before removal (Walk::Check, after the data directory's own mount check) finds.
Detected ==
    CASE Fault("NoRemovalPreflight") -> FALSE                     \* before 8549878: no check
      [] Fault("FileMountUnchecked") -> inner \in {"dir", "self"}   \* before 41eb061: files unchecked
      [] OTHER -> inner # "none"

\* The data directory must be on its parent's mount, and the whole walk finds nothing on another
\* mount, with every listing complete, before anything is removed: a mount found, or a listing
\* or open that fails, refuses the removal whole.
RmPreflight ==
    /\ rm = "preflight"
    /\ sawMount' = (inner # "none")
    /\ \/ Detected /\ rm' = "idle"
       \/ ~Detected /\ ~Fault("NoRemovalPreflight") /\ rm' = "idle"   \* a listing failed
       \/ ~Detected /\ rm' = "remove"
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, inner, confirmed,
                   cmd, purge, rmAllowed, commands, events, historyVars>>

\* Walk::Remove checks every directory as opened and every other entry as named before removing
\* it, so a mount that appeared since the check stops the removal there; a file mounted over
\* cannot be unlinked (EBUSY) either way. The walk before 8549878 descended into any directory.
RmRemove ==
    /\ rm = "remove"
    /\ LET outcome ==
             CASE inner = "none" -> "removed"
               [] Fault("NoRemovalPreflight") /\ inner \in {"dir", "self"} -> "across"
               [] inner = "self" -> "kept"
               [] OTHER -> "stopped"
       IN /\ data' = CASE outcome \in {"removed", "across"} -> "removed"
                       [] outcome = "kept" -> data
                       [] OTHER -> "partial"
          /\ foreignLost' = (foreignLost \/ outcome = "across")
    /\ removedWhileRunning' = (removedWhileRunning \/ (running # {} /\ data' # data))
    /\ lostAfterMountSeen' = (lostAfterMountSeen \/ (sawMount /\ data' # data))
    /\ rm' = "idle"
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, inner, confirmed, cmd,
                   purge, rmAllowed, sawMount, commands, events, reclaimedWhile,
                   expiredWhileOwner, reportedWrong>>

---------------------------------------------------------------------------------------------
\* Reading without the lock: `status` and `ps` report the lease (lease::LeaseSummary).

Status ==
    /\ \E st \in OwnerStates : reportedWrong' = (reportedWrong \/ (st = "alive" /\ owner # "alive"))
    /\ reportedWrong' # reportedWrong
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, inner, confirmed,
                   cmd, purge, removalVars, commands, events, reclaimedWhile, expiredWhileOwner,
                   removedWhileRunning, foreignLost, lostAfterMountSeen>>

---------------------------------------------------------------------------------------------
\* The machine.

Event == events > 0 /\ events' = events - 1

OwnerExits ==
    /\ Event /\ owner = "alive" /\ owner' = "exited"
    /\ UNCHANGED <<checkoutVars, ownerNs, ownerStart, pidReused, rebooted, session, procVars,
                   ports, data, inner, confirmed, cmd, purge, removalVars, commands, historyVars>>

ReusePid ==
    /\ Event /\ owner = "exited" /\ ~pidReused /\ pidReused' = TRUE
    /\ UNCHANGED <<checkoutVars, owner, ownerNs, ownerStart, rebooted, session, procVars, ports,
                   data, inner, confirmed, cmd, purge, removalVars, commands, historyVars>>

\* Every process ends; records stay as they were written; the running command is cut short.
Reboot ==
    /\ Event
    /\ owner' = IF owner = "alive" THEN "exited" ELSE owner
    /\ rebooted' = (owner # "none")
    /\ running' = {} /\ recorded' = {} /\ hidden' = {} /\ supAlive' = FALSE
    /\ cmd' = "idle" /\ rm' = "idle"
    /\ UNCHANGED <<checkoutVars, ownerNs, ownerStart, pidReused, session, supRec, ports, data,
                   inner, confirmed, purge, rmAllowed, sawMount, commands, historyVars>>

\* The Stack command is killed between two of its steps.
CommandKilled ==
    /\ Event /\ (cmd # "idle" \/ rm # "idle")
    /\ cmd' = "idle" /\ rm' = "idle"
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, inner, confirmed,
                   purge, rmAllowed, sawMount, commands, historyVars>>

ServiceDies ==
    /\ Event
    /\ \E i \in running :
         /\ running' = running \ {i}
         /\ recorded' = recorded \ {i}
         /\ hidden' = hidden \ {i}
    /\ UNCHANGED <<checkoutVars, leaseVars, session, supAlive, supRec, ports, data, inner,
                   confirmed, cmd, purge, removalVars, commands, historyVars>>

SupervisorDies ==
    /\ Event /\ supAlive /\ supAlive' = FALSE
    /\ UNCHANGED <<checkoutVars, leaseVars, session, running, recorded, hidden, supRec, ports,
                   data, inner, confirmed, cmd, purge, removalVars, commands, historyVars>>

\* A newer Stack rewrites state.json (still naming what runs), or it is damaged in place.
RecordUnreadable ==
    /\ Event /\ supRec = "ok"
    /\ supRec' \in Unreadable
    /\ UNCHANGED <<checkoutVars, leaseVars, session, running, recorded, hidden, supAlive, ports,
                   data, inner, confirmed, cmd, purge, removalVars, commands, historyVars>>

Unmount ==
    /\ Event /\ truth = "present"
    /\ truth' = "offline"
    /\ offlineKind' \in IF linux THEN {"volume", "bind"} ELSE {"volume"}
    /\ UNCHANGED <<linux, evidence, leaseVars, session, procVars, ports, data, inner, confirmed,
                   cmd, purge, removalVars, commands, historyVars>>

Remount ==
    /\ Event /\ truth = "offline" /\ truth' = "present"
    /\ UNCHANGED <<linux, offlineKind, evidence, leaseVars, session, procVars, ports, data, inner,
                   confirmed, cmd, purge, removalVars, commands, historyVars>>

Delete ==
    /\ Event /\ truth = "present" /\ truth' = "deleted"
    /\ UNCHANGED <<linux, offlineKind, evidence, leaseVars, session, procVars, ports, data, inner,
                   confirmed, cmd, purge, removalVars, commands, historyVars>>

\* Another directory made at the path, on the same mount.
Replace ==
    /\ Event /\ truth = "deleted" /\ truth' = "replaced"
    /\ UNCHANGED <<linux, offlineKind, evidence, leaseVars, session, procVars, ports, data, inner,
                   confirmed, cmd, purge, removalVars, commands, historyVars>>

\* A mount appears in the data: a volume or bind mount inside it, one file bind-mounted over
\* (Linux), or at the data directory itself.
MountInside ==
    /\ Event /\ data \in {"intact", "partial"} /\ inner = "none"
    /\ inner' \in IF linux THEN {"dir", "file", "self"} ELSE {"dir", "self"}
    /\ UNCHANGED <<checkoutVars, leaseVars, session, procVars, ports, data, confirmed, cmd, purge,
                   removalVars, commands, historyVars>>

Done ==
    /\ cmd = "idle" /\ rm = "idle" /\ commands = 0
    /\ UNCHANGED vars

Next ==
    \/ UpBegin \/ UpRecord \/ Start \/ Verify
    \/ RestartBegin \/ RestartStop \/ RestartRecord
    \/ DownBegin \/ DownWait \/ DownConfirm
    \/ GcBegin \/ GcGone \/ GcExpire \/ GcDeleted \/ RmDone
    \/ RmCheck \/ RmPreflight \/ RmRemove
    \/ Status
    \/ OwnerExits \/ ReusePid \/ Reboot \/ CommandKilled \/ ServiceDies \/ SupervisorDies
    \/ RecordUnreadable \/ Unmount \/ Remount \/ Delete \/ Replace \/ MountInside
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------------------------
\* Safety properties.

\* Records, ports and data are dropped as a gone checkout's only when it is deleted or replaced,
\* never while it is there or only out of reach (its volume or bind mount not mounted).
NoReclaimOfExistingCheckout == reclaimedWhile \in {"never", "deleted", "replaced"}

\* A session is stopped for its owner's exit only when the owner has exited.
ExpiryOnlyForExitedOwner == expiredWhileOwner # "alive"

\* A status never says the owner runs once it has exited.
OwnerReportedAliveOnlyIfAlive == ~reportedWrong

\* Every live service process is named by the supervisor record on disk, which Stack never
\* writes over or forgets while it cannot read it.
EveryProcessRecorded == running \subseteq recorded

\* A down or gc that says every service stopped is right until something starts again.
ConfirmedMeansStopped == confirmed => running = {}

\* Data is never removed while a service process runs.
DataRemovedOnlyWhenStopped == ~removedWhileRunning

\* Nothing another mount holds is ever removed.
NoRemovalAcrossMount == ~foreignLost

\* A removal whose preflight ran with a mount in the data removes nothing.
MountSeenAtPreflightKeepsData == ~lostAfterMountSeen
====

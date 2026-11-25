---- MODULE PortAndMcpContract ----
\* Two machines Stack runs concurrently with other processes, checked together as a disjoint
\* union: an initial choice of `part` picks one, whose actions alone then run (the other's
\* variables stay as initialized). They share no variable, so checking their product would only
\* multiply states.
\*
\* part = "ports": the machine-wide port registry (src/ports.rs: assign, reassign, release,
\* release_gone; src/project.rs compile; src/session up's port check and up --reassign-ports).
\* Two checkouts c1 (db, web) and c2 (db, web, pinned to port 1 from the start or once the
\* project configures it) share ports 1..5, of which 5 lies where outgoing connections may take a port (the
\* ephemeral range). A compile reads the registry under its lock and writes it whole: two steps,
\* so without the lock two compiles interleave; its services come in either order. Between steps
\* a checkout can go out of reach (its volume unmounted: presence unknown), come back, or be
\* deleted (presence absent; its services run until its supervisor reaps them), and foreign
\* programs can listen on up to two free ports and exit.
\*
\* part = "mcp": the stdio MCP server's admission (src/mcp/server.rs: read_frames, Credit,
\* Server::line/batch/request/cancel/accept/admit/start/done/finish/drop_waiting). A client sends
\* four tools/call requests, each on checkout A, B or none, directly or (once) two of them in a
\* batch with a ping, at most one notifications/cancelled, and closes its input. The reader moves
\* lines into a bounded queue; the loop handles them, calls ending (in any order relative to
\* lines) and input ending; each started call has a worker whose admission point settles whether
\* its tool runs; a signal that ends the server can come at any time. Bounds are scaled down
\* (MaxRunning 2, MaxWaiting 2, MaxPending 1 for 16, 256, 16): every rule compares a count with its
\* bound, so the smaller bounds reach the same cases (at, below and past each bound).
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS Faults, Steps

Fault(f) == f \in Faults

\* ------------------------------------------------------------------ ports: constants
Checkouts == {"c1", "c2"}
Services == {"db", "web"}
Orders == {<<"db", "web">>, <<"web", "db">>}  \* the orders compile's requests can come in
Order == <<"db", "web">>                     \* the order up --reassign-ports moves services in
Ports == 1..5
Ephemeral == {5}
NoPorts == [s \in Services |-> 0]

\* ------------------------------------------------------------------ mcp: constants
Calls == 1..4
Projects == {"A", "B", "none"}
MaxRunning == 2
MaxWaiting == 2
MaxPending == 1

VARIABLES
    part,
    \* ports
    res,          \* the registry: <<checkout, service, port>> reservations
    given,        \* the ports each checkout's rendered configuration uses (its last compile)
    run,          \* the port each service of each checkout listens on (0: not running)
    foreign,      \* ports a foreign program listens on
    presence,     \* "present", "unknown" (out of reach) or "absent" (deleted)
    lock,         \* the registry lock's holder, or "none"
    pc,           \* per checkout: "idle", or "assigning" between a compile's read and write
    snap,         \* the registry a compile read
    mode,         \* whether that compile reassigns
    steps,
    pinOn,        \* c2's project pins web to port 1 (its configuration, read by its next compile)
    ord,          \* per checkout: the order of the requests of the compile under way
    gaveBack,       \* history: a reassignment gave a service a port its checkout gave up
    ownPinConflict, \* history: a compile failed on a pin only its own other service held
    pinTookOwn,     \* witness: a compile pinned a port its own unpinned service held
    surrenderedFree, \* witness: up --reassign-ports moved two services while a port one gave up was free
    \* mcp
    pending,      \* lines written and read, waiting for the loop (the reader's bounded queue)
    closed,       \* the client closed its input (the reader reports the end after every line)
    loop,         \* "serving", "finishing" (input ended or a signal), "exited"
    proj,         \* each call's checkout
    status,       \* "unsent", "sent", "waiting", "worker", "running", "answered", "skipped", "refused"
    inBatch,      \* the call came in the batch
    waiting,      \* accepted calls not started, in arrival order
    busy,         \* checkouts with a call started (until its end is handled)
    running,      \* calls started and whose end the loop has not handled
    doneQ,        \* calls ended, their end not yet handled by the loop
    cancelled,    \* the call's Cancel fired (it was in flight)
    signal, stopped,
    owed, collected, batchSent, batchUsed, batchRead, cancelSent,
    accepted,     \* calls accepted, in arrival order (items of a batch in their order)
    startOrder,   \* calls given a worker, in that order
    cancelHit,          \* history: a cancel found the call waiting or in flight
    cancelBeforeTool,   \* history: ... before its tool began
    workerAfterSignal,  \* history: a call got a worker after the signal was raised
    toolAfterSignal     \* history: a call's tool began after the signal was raised

portVars == <<res, given, run, foreign, presence, lock, pc, snap, mode, steps, pinOn, ord,
              gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>
mcpVars == <<pending, closed, loop, proj, status, inBatch, waiting, busy, running,
             doneQ, cancelled, signal, stopped, owed, collected, batchSent, batchUsed, batchRead, cancelSent,
             accepted, startOrder, cancelHit, cancelBeforeTool, workerAfterSignal,
             toolAfterSignal>>
vars == <<part, portVars, mcpVars>>

\* ================================================================== ports
Pin(c, s) == IF c = "c2" /\ s = "web" /\ pinOn THEN 1 ELSE 0
Listening == (foreign \cup {run[c][s] : c \in Checkouts, s \in Services}) \ {0}
Own(r, c) == {x \in r : x[1] = c}
\* What a registry operation keeps of other checkouts: a checkout shown to be gone is dropped;
\* one merely out of reach is kept (93b56b0).
Gone(c) == IF Fault("PruneUnknown") THEN presence[c] # "present" ELSE presence[c] = "absent"
Pruned(r) == {x \in r : ~Gone(x[1])}

\* One request of `assign` against registry `r`: the outcomes ([ok, r, p, own]).
StepOutcomes(r, c, s, excluded) ==
    LET pin == Pin(c, s)
        mine == {x \in r : x[1] = c /\ x[2] = s}
        holders == {x \in r : x[3] = pin /\ ~(x[1] = c /\ x[2] = s)}
        reusable == {x \in mine : x[3] \notin Ephemeral \/ Fault("KeepEphemeral")}
        free == (Ports \ Ephemeral) \ ({x[3] : x \in r} \cup excluded \cup Listening)
        put(p) == (r \ mine) \cup {<<c, s, p>>}
    IN IF pin # 0 THEN
           IF holders # {} /\ ~Fault("PinUnchecked")
           THEN {[ok |-> FALSE, r |-> r, p |-> 0, own |-> \A x \in holders : x[1] = c]}
           ELSE {[ok |-> TRUE, r |-> put(pin), p |-> pin, own |-> FALSE]}
       ELSE IF reusable # {} THEN
           {[ok |-> TRUE, r |-> put(x[3]), p |-> x[3], own |-> FALSE] : x \in reusable}
       ELSE IF free = {} THEN {[ok |-> FALSE, r |-> r, p |-> 0, own |-> FALSE]}
       ELSE {[ok |-> TRUE, r |-> put(p), p |-> p, own |-> FALSE] : p \in free}

\* The checkout's pinned ports.
Pins(c) == {Pin(c, s) : s \in Services} \ {0}
\* Its reservations a compile gives up before assigning: all when reassigning; else those on a
\* port it pins held by a service not pinned there (a pin configured after Stack gave that port
\* to another of its services takes it).
GivenUpFirst(c, reassign, base) ==
    IF reassign THEN Own(base, c)
    ELSE IF Fault("PinKeepsOwnReservation") THEN {}
    ELSE {x \in Own(base, c) : x[3] \in Pins(c) /\ Pin(c, x[2]) # x[3]}

\* `assign` for checkout c over the registry it read, its requests in order o: every outcome
\* ([ok, r, out, own, took]); the registry is written only when both requests succeed. `took`:
\* a pinned port was held by one of its own unpinned services (the witness).
Assign(c, reassign, read, o) ==
    LET base == Pruned(read)
        start == base \ GivenUpFirst(c, reassign, base)
        pins == IF Fault("AllocateOverOwnPin") THEN {} ELSE Pins(c)
        givenUp == IF reassign /\ ~Fault("ReassignForgetsOld") THEN {x[3] : x \in Own(base, c)} ELSE {}
        excluded == pins \cup givenUp
        took == \E x \in Own(base, c) : x[3] \in Pins(c) /\ Pin(c, x[2]) # x[3]
    IN UNION {
         IF ~a.ok THEN {[ok |-> FALSE, r |-> read, out |-> NoPorts, own |-> a.own, took |-> FALSE]}
         ELSE {[ok |-> b.ok, r |-> IF b.ok THEN b.r ELSE read,
                out |-> [s \in Services |-> IF s = o[1] THEN a.p ELSE b.p],
                own |-> ~b.ok /\ b.own, took |-> b.ok /\ took] : b \in StepOutcomes(a.r, c, o[2], excluded)}
         : a \in StepOutcomes(start, c, o[1], excluded)}

\* The orders a compile of c is checked in. Without a pin, every outcome of one order is one of
\* the other (each request takes any eligible port), so only a checkout with a pin needs both.
OrdersOf(c) == IF Pins(c) # {} THEN Orders ELSE {Order}

Usable(c) == presence[c] = "present" /\ pc[c] = "idle"
LockFree == lock = "none"
Take(c) == IF Fault("NoRegistryLock") THEN "none" ELSE c

PortsInit ==
    /\ res \in {{}, {<<"c1", "db", 5>>}}   \* or a reservation made before the range moved (2d97f39)
    /\ given = [c \in Checkouts |-> NoPorts]
    /\ run = [c \in Checkouts |-> NoPorts]
    /\ foreign = {}
    /\ presence = [c \in Checkouts |-> "present"]
    /\ lock = "none"
    /\ pc = [c \in Checkouts |-> "idle"]
    /\ snap = [c \in Checkouts |-> {}]
    /\ mode = [c \in Checkouts |-> FALSE]
    /\ steps = 0
    /\ pinOn \in IF part = "ports" THEN BOOLEAN ELSE {FALSE}   \* (not doubling the other machine)
    /\ ord = [c \in Checkouts |-> Order]
    /\ gaveBack = FALSE
    /\ ownPinConflict = FALSE
    /\ pinTookOwn = FALSE
    /\ surrenderedFree = FALSE

Step == steps < Steps /\ steps' = steps + 1

\* project::compile -> ports::assign: read the registry under its lock ...
CompileRead(c, reassign, o) ==
    /\ Usable(c) /\ LockFree /\ Step
    /\ lock' = Take(c)
    /\ pc' = [pc EXCEPT ![c] = "assigning"]
    /\ snap' = [snap EXCEPT ![c] = res]
    /\ mode' = [mode EXCEPT ![c] = reassign]
    /\ ord' = [ord EXCEPT ![c] = o]
    /\ UNCHANGED <<res, given, run, foreign, presence, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

\* ... then choose (find_free binds each candidate: a listened port is skipped), write it whole
\* (only when the assignment succeeded) and release the lock; the checkout renders what it got.
CompileWrite(c) ==
    /\ pc[c] = "assigning"
    /\ \E o \in Assign(c, mode[c], snap[c], ord[c]) :
          /\ res' = IF o.ok THEN o.r ELSE res
          /\ given' = IF o.ok THEN [given EXCEPT ![c] = o.out] ELSE given
          /\ gaveBack' = (gaveBack \/ (o.ok /\ mode[c] /\
                \E s \in Services : Pin(c, s) = 0 /\ given[c][s] # 0 /\ o.out[s] \in {given[c][t] : t \in Services}))
          /\ ownPinConflict' = (ownPinConflict \/ (~o.ok /\ o.own))
          /\ pinTookOwn' = (pinTookOwn \/ o.took)
    /\ lock' = "none"
    /\ pc' = [pc EXCEPT ![c] = "idle"]
    /\ ord' = [ord EXCEPT ![c] = Order]
    /\ UNCHANGED <<run, foreign, presence, snap, mode, steps, pinOn, surrenderedFree>>

Conflicts(c) == {s \in Services : given[c][s] \in Listening \ {run[c][t] : t \in Services}}

\* up: every assigned port is checked first; any held by another program is a port_conflict
\* and nothing starts. Otherwise every service starts on the port its configuration names.
Up(c) ==
    /\ Usable(c) /\ Step
    /\ \A s \in Services : given[c][s] # 0
    /\ Conflicts(c) = {}
    /\ run' = [run EXCEPT ![c] = given[c]]
    /\ UNCHANGED <<res, given, foreign, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

\* ports::reassign's moves of the services of `seq` (in turn) of checkout c over registry r: each
\* service's old reservation replaced by a port in range, not reserved in r as it stands then, and
\* not in `avoid`. Every outcome ([r, new]); none when a service finds no port (ports_exhausted:
\* nothing is written, as if the command had not run).
RECURSIVE Moves(_, _, _, _, _)
Moves(c, seq, r, avoid, new) ==
    IF seq = <<>> THEN {[r |-> r, new |-> new]}
    ELSE LET s == Head(seq)
             free == (Ports \ Ephemeral) \ ({x[3] : x \in r} \cup avoid)
         IN UNION {Moves(c, Tail(seq), {x \in r : ~(x[1] = c /\ x[2] = s)} \cup {<<c, s, p>>}, avoid,
                         [new EXCEPT ![s] = p]) : p \in free}

\* up --reassign-ports after such a conflict naming no pinned port: ports::reassign moves the
\* conflicting services one after another. Programs holding their ports may exit meanwhile
\* (`freed`, taken as gone before the first choice: that only adds choices), so a port given up
\* binds again once its service moved; every port the services give up stays excluded until
\* each has its new one (a fault: only while its own service is moved).
UpReassign(c) ==
    /\ Usable(c) /\ LockFree /\ Step
    /\ \A s \in Services : given[c][s] # 0
    /\ Conflicts(c) # {}
    /\ \A s \in Conflicts(c) : Pin(c, s) = 0
    /\ \E freed \in SUBSET foreign :
          LET conf == Conflicts(c)
              surrendered == {x[3] : x \in {y \in Own(res, c) : y[2] \in conf}}
              gaveUp == {given[c][s] : s \in conf}
              avoid == (Listening \ freed) \cup (IF Fault("ReassignReusesSurrendered") THEN {} ELSE surrendered)
          IN \E o \in Moves(c, SelectSeq(Order, LAMBDA s : s \in conf), res, avoid, NoPorts) :
                LET moved == [s \in Services |-> IF s \in conf THEN o.new[s] ELSE given[c][s]]
                IN /\ res' = o.r
                   /\ given' = [given EXCEPT ![c] = moved]
                   /\ run' = [run EXCEPT ![c] = moved]
                   /\ foreign' = foreign \ freed
                   /\ gaveBack' = (gaveBack \/ \E s \in conf : o.new[s] \in gaveUp)
                   /\ surrenderedFree' = (surrenderedFree \/ (Cardinality(conf) > 1 /\ gaveUp \cap freed # {}))
    /\ UNCHANGED <<presence, lock, pc, snap, mode, ord, pinOn, ownPinConflict, pinTookOwn>>

\* The project configures c2's pin of port 1 (when it starts without one): its next compile reads
\* it (one under way read the configuration before the registry). Removing a pin is a service
\* no longer pinned, as c1's are, and is left out.
ConfigurePin ==
    /\ ~pinOn /\ pc["c2"] = "idle" /\ Step
    /\ pinOn' = TRUE
    /\ UNCHANGED <<res, given, run, foreign, presence, lock, pc, snap, mode, ord, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

Down(c) ==
    /\ Usable(c) /\ Step
    /\ run' = [run EXCEPT ![c] = NoPorts]
    /\ UNCHANGED <<res, given, foreign, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

\* down --purge: services stopped, every reservation of the checkout released.
Purge(c) ==
    /\ Usable(c) /\ LockFree /\ Step
    /\ run' = [run EXCEPT ![c] = NoPorts]
    /\ res' = res \ Own(res, c)
    /\ given' = [given EXCEPT ![c] = NoPorts]
    /\ UNCHANGED <<foreign, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

\* gc: release_gone, keeping a checkout whose session is still kept (its services run).
Gc ==
    /\ LockFree /\ Step
    /\ res' = {x \in res : ~(Gone(x[1]) /\ run[x[1]] = NoPorts)}
    /\ UNCHANGED <<given, run, foreign, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

Unmount(c) == /\ presence[c] = "present" /\ pc[c] = "idle" /\ Step
              /\ presence' = [presence EXCEPT ![c] = "unknown"]
              /\ UNCHANGED <<res, given, run, foreign, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>
Remount(c) == /\ presence[c] = "unknown" /\ Step
              /\ presence' = [presence EXCEPT ![c] = "present"]
              /\ UNCHANGED <<res, given, run, foreign, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>
Delete(c) ==  /\ presence[c] = "present" /\ pc[c] = "idle" /\ Step
              /\ presence' = [presence EXCEPT ![c] = "absent"]
              /\ UNCHANGED <<res, given, run, foreign, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>
\* The deleted checkout's supervisor confirms its services gone.
Reap(c) ==    /\ presence[c] = "absent" /\ run[c] # NoPorts /\ Step
              /\ run' = [run EXCEPT ![c] = NoPorts]
              /\ UNCHANGED <<res, given, foreign, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>
ForeignListen(p) == /\ Cardinality(foreign) < 2 /\ p \notin Listening /\ Step
                    /\ foreign' = foreign \cup {p}
                    /\ UNCHANGED <<res, given, run, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>
ForeignExit(p) == /\ p \in foreign /\ Step
                  /\ foreign' = foreign \ {p}
               /\ UNCHANGED <<res, given, run, presence, lock, pc, snap, mode, ord, pinOn, gaveBack, ownPinConflict, pinTookOwn, surrenderedFree>>

PortsNext ==
    \/ \E c \in Checkouts, r \in BOOLEAN : \E o \in OrdersOf(c) : CompileRead(c, r, o)
    \/ \E c \in Checkouts : CompileWrite(c) \/ Up(c) \/ UpReassign(c) \/ Down(c) \/ Purge(c)
                            \/ Unmount(c) \/ Remount(c) \/ Delete(c) \/ Reap(c)
    \/ Gc \/ ConfigurePin
    \/ \E p \in Ports : ForeignListen(p) \/ ForeignExit(p)

\* ------------------------------------------------------------------ ports: promises
\* The registry never reserves one port twice, nor two ports for one service of a checkout.
NoSharedReservation ==
    \A x, y \in res : (x[3] = y[3] \/ (x[1] = y[1] /\ x[2] = y[2])) => x = y

\* A checkout not shown to be gone (present or out of reach) keeps every port it was given until
\* its own compile, reassignment or purge changes it: no other checkout's operation takes it.
GivenIsReserved ==
    \A c \in Checkouts : presence[c] # "absent" =>
        \A s \in Services : given[c][s] # 0 => <<c, s, given[c][s]>> \in res

\* An assigned port is never one outgoing connections may take (pins are the project's own).
AssignedOutsideEphemeral ==
    \A c \in Checkouts, s \in Services : Pin(c, s) = 0 /\ given[c][s] # 0 => given[c][s] \notin Ephemeral

\* Reassigning (compile or up --reassign-ports) never gives a service back a port its checkout
\* gave up: any of the ports the operation took from its services, not only the service's own.
ReassignMoves == ~gaveBack

\* A pin fails only for another checkout's (or another pinned service's) reservation, never for a
\* port the same compile chose for one of the checkout's own services, nor one Stack gave one of
\* them before the pin was configured.
OwnPinNeverConflicts == ~ownPinConflict

\* Witnesses (WITNESS configs, expected violated): the fixed model reaches a compile pinning a
\* port its own unpinned service held, and up --reassign-ports moving two services while a port
\* one of them gave up was free.
NoPinOverOwnReservation == ~pinTookOwn
NoSurrenderedPortFree == ~surrenderedFree

\* Not promised (LIMIT config): a reservation keeps other checkouts off a port, not other
\* programs; `up` then refuses with port_conflict instead of starting.
ReservedPortsStayFree == \A x \in res : x[3] \notin foreign

\* ================================================================== mcp
NoCalls == [i \in Calls |-> FALSE]
In(x, s) == \E k \in 1..Len(s) : s[k] = x
Pos(x, s) == CHOOSE k \in 1..Len(s) : s[k] = x

McpInit ==
    /\ pending = <<>> /\ closed = FALSE
    /\ loop = "serving"
    /\ proj = [i \in Calls |-> "none"]
    /\ status = [i \in Calls |-> "unsent"]
    /\ inBatch = NoCalls
    /\ waiting = <<>> /\ busy = {} /\ running = 0 /\ doneQ = {}
    /\ cancelled = NoCalls
    /\ signal = FALSE /\ stopped = FALSE
    /\ owed = 0 /\ collected = 0 /\ batchSent = 0 /\ batchUsed = FALSE /\ batchRead = FALSE /\ cancelSent = FALSE
    /\ accepted = <<>> /\ startOrder = <<>>
    /\ cancelHit = NoCalls /\ cancelBeforeTool = NoCalls
    /\ workerAfterSignal = FALSE /\ toolAfterSignal = FALSE

\* A response (or none: "skipped") for call i, straight out or into its batch, which goes out
\* once nothing more is owed and it holds a response. Returns [owed, collected, sent].
Settle(o, c, s) == IF o = 0 /\ c > 0 THEN [owed |-> 0, collected |-> 0, sent |-> s + 1]
                   ELSE [owed |-> o, collected |-> c, sent |-> s]

\* Every waiting call dropped unanswered, its batch owed nothing more for it (4fdfc4c; a fault
\* leaves the batch owed).
DropAll(w, st, o, c, s) ==
    LET dropped == {w[k] : k \in 1..Len(w)}
        o2 == IF Fault("DropLeavesBatchOwed") THEN o ELSE o - Cardinality({i \in dropped : inBatch[i]})
        set == Settle(o2, c, s)
    IN [w |-> <<>>, st |-> [i \in Calls |-> IF i \in dropped THEN "skipped" ELSE st[i]],
        busy |-> busy, running |-> running, started |-> <<>>, stop |-> TRUE,
        owed |-> set.owed, collected |-> set.collected, sent |-> set.sent]

\* Admission: oldest first while a worker is free, passing over a call whose checkout has one
\* started (it keeps its place); once a signal came, nothing starts and every waiting call is
\* dropped (4fdfc4c). Returns the calls started and the new waiting queue and busy set.
RECURSIVE Scan(_, _, _, _, _)
Scan(w, kept, b, n, started) ==
    IF w = <<>> THEN [kept |-> kept, busy |-> b, n |-> n, started |-> started]
    ELSE LET i == Head(w)
             full == n >= MaxRunning /\ ~Fault("ThreadPerCall")
         IN IF full THEN [kept |-> kept \o w, busy |-> b, n |-> n, started |-> started]
            ELSE IF proj[i] \in b THEN
                 IF Fault("HeadOfLineBlocking") THEN [kept |-> kept \o w, busy |-> b, n |-> n, started |-> started]
                 ELSE Scan(Tail(w), Append(kept, i), b, n, started)
            ELSE Scan(Tail(w), kept, IF proj[i] = "none" THEN b ELSE b \cup {proj[i]}, n + 1, Append(started, i))

RECURSIVE Reverse(_)
Reverse(s) == IF s = <<>> THEN <<>> ELSE Append(Reverse(Tail(s)), Head(s))

\* Apply admission to (waiting w, statuses st, owed o, collected c, sent s): the new values.
Admit(w, st, o, c, s) ==
    IF signal /\ ~Fault("AdmitIgnoresSignal") THEN DropAll(w, st, o, c, s)
    ELSE
        LET r == Scan(IF Fault("NewestFirst") THEN Reverse(w) ELSE w, <<>>, busy, running, <<>>)
            kept == IF Fault("NewestFirst") THEN Reverse(r.kept) ELSE r.kept
        IN [w |-> kept, st |-> [i \in Calls |-> IF In(i, r.started) THEN "worker" ELSE st[i]],
            busy |-> r.busy, running |-> r.n, started |-> r.started, stop |-> stopped,
            owed |-> o, collected |-> c, sent |-> s]

\* Write the outcome of an admission over the given pre-state values.
Apply(a) ==
    /\ waiting' = a.w
    /\ status' = a.st
    /\ busy' = a.busy
    /\ running' = a.running
    /\ stopped' = a.stop
    /\ owed' = a.owed /\ collected' = a.collected /\ batchSent' = a.sent
    /\ startOrder' = startOrder \o a.started
    /\ workerAfterSignal' = (workerAfterSignal \/ (signal /\ a.started # <<>>))

\* --- the client, writing through the reader: a line goes in only while the reader's queue has
\* room (Credit; past it the client's writes block), and the end is seen after every line.
Room == Len(pending) < MaxPending \/ Fault("UnboundedFrames")

\* A and B are interchangeable in every rule: B is used only once A is (symmetry reduction).
Fresh(p) == p = "B" => \E j \in Calls : status[j] # "unsent" /\ proj[j] = "A"

SendCall(i, p) ==
    /\ status[i] = "unsent" /\ ~closed
    /\ \A j \in Calls : j < i => status[j] # "unsent"
    /\ Fresh(p)
    /\ proj' = [proj EXCEPT ![i] = p]
    /\ status' = [status EXCEPT ![i] = "sent"]
    /\ Room
    /\ pending' = Append(pending, [k |-> "call", ids |-> <<i>>])
    /\ UNCHANGED <<closed, loop, inBatch, waiting, busy, running, doneQ, cancelled,
                   signal, stopped, owed, collected, batchSent, batchUsed, batchRead, cancelSent, accepted,
                   startOrder, cancelHit, cancelBeforeTool, workerAfterSignal, toolAfterSignal>>

\* The last two calls in one batch, with a ping between them.
SendBatch(i, p, q) ==
    /\ ~batchUsed /\ ~closed /\ i = 3
    /\ status[i] = "unsent" /\ status[i + 1] = "unsent"
    /\ \A j \in Calls : j < i => status[j] # "unsent"
    /\ Fresh(p) /\ (q = "B" => p = "A" \/ Fresh(q))
    /\ proj' = [proj EXCEPT ![i] = p, ![i + 1] = q]
    /\ status' = [status EXCEPT ![i] = "sent", ![i + 1] = "sent"]
    /\ inBatch' = [inBatch EXCEPT ![i] = TRUE, ![i + 1] = TRUE]
    /\ batchUsed' = TRUE
    /\ Room
    /\ pending' = Append(pending, [k |-> "batch", ids |-> <<i, i + 1>>])
    /\ UNCHANGED <<closed, loop, waiting, busy, running, doneQ, cancelled,
                   signal, stopped, owed, collected, batchSent, batchRead, cancelSent, accepted,
                   startOrder, cancelHit, cancelBeforeTool, workerAfterSignal, toolAfterSignal>>

SendCancel(i) ==
    /\ ~cancelSent /\ ~closed /\ status[i] # "unsent"
    /\ cancelSent' = TRUE
    /\ Room
    /\ pending' = Append(pending, [k |-> "cancel", ids |-> <<i>>])
    /\ UNCHANGED <<closed, loop, proj, status, inBatch, waiting, busy, running, doneQ,
                   cancelled, signal, stopped, owed, collected, batchSent, batchUsed, batchRead, accepted,
                   startOrder, cancelHit, cancelBeforeTool, workerAfterSignal, toolAfterSignal>>

Close ==
    /\ ~closed /\ closed' = TRUE
    /\ UNCHANGED <<pending, loop, proj, status, inBatch, waiting, busy, running, doneQ,
                   cancelled, signal, stopped, owed, collected, batchSent, batchUsed, batchRead, cancelSent,
                   accepted, startOrder, cancelHit, cancelBeforeTool, workerAfterSignal,
                   toolAfterSignal>>

\* --- the loop: a line
\* tools/call: refused at once past MaxWaiting waiting (fac91c6), else accepted and admitted.
\* Its response, for a batch, goes there. Items are dispatched in order, then the batch's hold
\* (one owed response) is released.
RECURSIVE Dispatch(_, _, _, _, _, _)
Dispatch(ids, w, st, acc, o, c) ==
    IF ids = <<>> THEN [w |-> w, st |-> st, acc |-> acc, o |-> o, c |-> c]
    ELSE LET i == Head(ids)
             refuse == Len(w) >= MaxWaiting /\ ~Fault("UnboundedWaiting")
         IN IF refuse
            THEN Dispatch(Tail(ids), w, [st EXCEPT ![i] = "refused"], acc,
                          IF inBatch[i] THEN o - 1 ELSE o, IF inBatch[i] THEN c + 1 ELSE c)
            ELSE Dispatch(Tail(ids), Append(w, i), [st EXCEPT ![i] = "waiting"], Append(acc, i), o, c)

HandleCalls(f) ==
    LET isBatch == f.k = "batch"
        \* a batch owes its hold, its ping (answered at once) and each call
        o0 == IF isBatch THEN owed + 2 + Len(f.ids) ELSE owed
        c0 == IF isBatch THEN collected + 1 ELSE collected
        o1 == IF isBatch THEN o0 - 1 ELSE o0
        d == Dispatch(f.ids, waiting, status, <<>>, o1, c0)
        o2 == IF isBatch THEN d.o - 1 ELSE d.o       \* the hold released
        set == Settle(o2, d.c, batchSent)
        a == Admit(d.w, d.st, set.owed, set.collected, set.sent)
    IN /\ Apply(a)
       /\ accepted' = accepted \o d.acc

\* notifications/cancelled: one in flight has its Cancel fire; one waiting is dropped, owed
\* nothing more (a fault: waiting calls cannot be cancelled).
HandleCancel(i) ==
    LET inflight == status[i] \in {"worker", "running"}
        isWaiting == \E k \in 1..Len(waiting) : waiting[k] = i
        drop == isWaiting /\ ~Fault("CancelMissesWaiting")
        o2 == IF drop /\ inBatch[i] THEN owed - 1 ELSE owed
        set == Settle(o2, collected, batchSent)
    IN /\ cancelled' = [cancelled EXCEPT ![i] = cancelled[i] \/ inflight]
       /\ waiting' = IF drop THEN SelectSeq(waiting, LAMBDA j : j # i) ELSE waiting
       /\ status' = IF drop THEN [status EXCEPT ![i] = "skipped"] ELSE status
       /\ owed' = set.owed /\ collected' = set.collected /\ batchSent' = set.sent
       /\ cancelHit' = [cancelHit EXCEPT ![i] = cancelHit[i] \/ inflight \/ isWaiting]
       /\ cancelBeforeTool' = [cancelBeforeTool EXCEPT ![i] = cancelBeforeTool[i] \/ isWaiting \/ status[i] = "worker"]
       /\ UNCHANGED <<busy, running, stopped, accepted, startOrder, workerAfterSignal>>

HandleLine ==
    /\ loop = "serving" /\ pending # <<>>
    /\ pending' = Tail(pending)
    /\ LET f == Head(pending) IN
       IF f.k = "cancel" THEN HandleCancel(f.ids[1])
       ELSE /\ HandleCalls(f) /\ UNCHANGED <<cancelled, cancelHit, cancelBeforeTool>>
    /\ batchRead' = (batchRead \/ Head(pending).k = "batch")
    /\ UNCHANGED <<closed, loop, proj, inBatch, doneQ, signal, batchUsed, cancelSent,
                   toolAfterSignal>>

\* --- the loop: a call ended (its worker sent Done): its worker and checkout are free, then
\* admission runs with them freed.
AdmitWith(b, n) ==
    IF signal /\ ~Fault("AdmitIgnoresSignal") THEN Admit(waiting, status, owed, collected, batchSent)
    ELSE LET r == Scan(IF Fault("NewestFirst") THEN Reverse(waiting) ELSE waiting, <<>>, b, n, <<>>)
             kept == IF Fault("NewestFirst") THEN Reverse(r.kept) ELSE r.kept
         IN [w |-> kept, st |-> [j \in Calls |-> IF In(j, r.started) THEN "worker" ELSE status[j]],
             busy |-> r.busy, running |-> r.n, started |-> r.started, stop |-> stopped,
             owed |-> owed, collected |-> collected, sent |-> batchSent]

Done(i) ==
    /\ loop # "exited" /\ i \in doneQ
    /\ doneQ' = doneQ \ {i}
    /\ LET b == IF proj[i] = "none" THEN busy ELSE busy \ {proj[i]}
           a == AdmitWith(b, running - 1)
           freed == IF signal /\ ~Fault("AdmitIgnoresSignal") THEN [a EXCEPT !.busy = b, !.running = running - 1] ELSE a
       IN Apply(freed)
    /\ UNCHANGED <<pending, closed, loop, proj, inBatch, cancelled, signal, batchUsed, batchRead,
                   cancelSent, accepted, cancelHit, cancelBeforeTool, toolAfterSignal>>

\* --- the loop: input ended (after every line). Calls accepted still run and are answered.
HandleEnd ==
    /\ loop = "serving" /\ closed /\ pending = <<>>
    /\ loop' = "finishing"
    /\ IF Fault("EofDropsWaiting")
       THEN LET dropped == {waiting[k] : k \in 1..Len(waiting)}
                o2 == owed - Cardinality({i \in dropped : inBatch[i]})
                set == Settle(o2, collected, batchSent)
            IN /\ waiting' = <<>>
               /\ status' = [i \in Calls |-> IF i \in dropped THEN "skipped" ELSE status[i]]
               /\ owed' = set.owed /\ collected' = set.collected /\ batchSent' = set.sent
       ELSE UNCHANGED <<waiting, status, owed, collected, batchSent>>
    /\ UNCHANGED <<pending, closed, proj, inBatch, busy, running, doneQ, cancelled,
                   signal, stopped, batchUsed, batchRead, cancelSent, accepted, startOrder, cancelHit,
                   cancelBeforeTool, workerAfterSignal, toolAfterSignal>>

\* --- the loop sees the signal (at its top, and in finish): nothing more starts, waiting calls
\* are dropped unanswered, owed nothing more; lines not handled are never read.
SeeSignal ==
    /\ signal /\ loop # "exited" /\ (~stopped \/ waiting # <<>>)
    /\ LET a == DropAll(waiting, status, owed, collected, batchSent)
       IN /\ waiting' = a.w /\ status' = a.st /\ stopped' = TRUE
          /\ owed' = a.owed /\ collected' = a.collected /\ batchSent' = a.sent
    /\ loop' = "finishing"
    /\ UNCHANGED <<pending, closed, proj, inBatch, busy, running, doneQ, cancelled,
                   signal, batchUsed, batchRead, cancelSent, accepted, startOrder, cancelHit,
                   cancelBeforeTool, workerAfterSignal, toolAfterSignal>>

Exit ==
    /\ loop = "finishing" /\ running = 0
    /\ loop' = "exited"
    /\ UNCHANGED <<pending, closed, proj, status, inBatch, waiting, busy, running, doneQ,
                   cancelled, signal, stopped, owed, collected, batchSent, batchUsed, batchRead, cancelSent,
                   accepted, startOrder, cancelHit, cancelBeforeTool, workerAfterSignal,
                   toolAfterSignal>>

\* --- a worker: its admission point, just before the tool (ad3b172): the tool runs only if the
\* call was not cancelled and no signal has been seen; else it ends unanswered.
Admission(i) ==
    /\ status[i] = "worker"
    /\ LET dropped == cancelled[i] \/ (signal /\ ~Fault("WorkerSkipsCheck"))
           o2 == IF dropped /\ inBatch[i] THEN owed - 1 ELSE owed
           set == Settle(o2, collected, batchSent)
       IN IF dropped
          THEN /\ status' = [status EXCEPT ![i] = "skipped"]
               /\ doneQ' = doneQ \cup {i}
               /\ owed' = set.owed /\ collected' = set.collected /\ batchSent' = set.sent
               /\ UNCHANGED toolAfterSignal
          ELSE /\ status' = [status EXCEPT ![i] = "running"]
               /\ toolAfterSignal' = (toolAfterSignal \/ signal)
               /\ UNCHANGED <<doneQ, owed, collected, batchSent>>
    /\ UNCHANGED <<pending, closed, loop, proj, inBatch, waiting, busy, running,
                   cancelled, signal, stopped, batchUsed, batchRead, cancelSent, accepted, startOrder,
                   cancelHit, cancelBeforeTool, workerAfterSignal>>

\* The tool returns: answered, unless its Cancel fired (no response).
ToolEnd(i) ==
    /\ status[i] = "running"
    /\ LET answered == ~cancelled[i]
           o2 == IF inBatch[i] THEN owed - 1 ELSE owed
           c2 == IF inBatch[i] /\ answered THEN collected + 1 ELSE collected
           set == Settle(o2, c2, batchSent)
       IN /\ status' = [status EXCEPT ![i] = IF answered THEN "answered" ELSE "skipped"]
          /\ owed' = set.owed /\ collected' = set.collected /\ batchSent' = set.sent
    /\ doneQ' = doneQ \cup {i}
    /\ UNCHANGED <<pending, closed, loop, proj, inBatch, waiting, busy, running,
                   cancelled, signal, stopped, batchUsed, batchRead, cancelSent, accepted, startOrder,
                   cancelHit, cancelBeforeTool, workerAfterSignal, toolAfterSignal>>

Signal ==
    /\ ~signal /\ loop # "exited"
    /\ signal' = TRUE
    /\ UNCHANGED <<pending, closed, loop, proj, status, inBatch, waiting, busy, running,
                   doneQ, cancelled, stopped, owed, collected, batchSent, batchUsed, batchRead, cancelSent,
                   accepted, startOrder, cancelHit, cancelBeforeTool, workerAfterSignal,
                   toolAfterSignal>>

McpNext ==
    \/ \E i \in Calls, p \in Projects : SendCall(i, p)
    \/ \E i \in Calls, p, q \in Projects : SendBatch(i, p, q)
    \/ \E i \in Calls : SendCancel(i) \/ Done(i) \/ Admission(i) \/ ToolEnd(i)
    \/ Close \/ HandleLine \/ HandleEnd \/ SeeSignal \/ Exit \/ Signal

\* ------------------------------------------------------------------ mcp: promises
Started(i) == status[i] \in {"worker", "running"}
Accepted(i) == In(i, accepted)

\* At most MaxRunning calls hold a worker; at most MaxWaiting wait; at most MaxPending lines wait
\* for the loop (the reader blocks past it).
WorkerBound == running <= MaxRunning
WaitingBound == Len(waiting) <= MaxWaiting
PendingBound == Len(pending) <= MaxPending

\* One call at a time per checkout.
OneCallPerCheckout == \A p \in Projects \ {"none"} : Cardinality({i \in Calls : Started(i) /\ proj[i] = p}) <= 1

\* Calls on one checkout start in the order they were accepted: an earlier one has started
\* before, or will never run (cancelled or dropped).
PerCheckoutOrder ==
    \A i, j \in Calls :
        (proj[i] = proj[j] /\ proj[i] # "none" /\ Accepted(i) /\ Accepted(j)
            /\ Pos(i, accepted) < Pos(j, accepted) /\ In(j, startOrder))
            => ((In(i, startOrder) /\ Pos(i, startOrder) < Pos(j, startOrder)) \/ status[i] = "skipped")

\* Independent work: while a worker is free and no signal came, every waiting call waits for its
\* own busy checkout, never behind another checkout's.
WorkConserving ==
    (loop # "exited" /\ ~signal /\ running < MaxRunning) =>
        \A k \in 1..Len(waiting) : proj[waiting[k]] \in busy

\* A cancel that found its call waiting or before its tool keeps the tool from running; a
\* cancelled call is never answered.
CancelledNeverRuns == \A i \in Calls : cancelBeforeTool[i] => status[i] \notin {"running", "answered"}
CancelledNeverAnswered == \A i \in Calls : cancelHit[i] => status[i] # "answered"

\* After the signal, no call gets a worker, and no tool begins.
NoWorkerAfterSignal == ~workerAfterSignal
NoToolAfterSignal == ~toolAfterSignal

\* Not promised (LIMIT config): the admission check and the tool's first step are not one atomic
\* step; a signal can come while a tool runs (it is passed on to the commands it runs).
NoSignalWhileToolRuns == ~(signal /\ \E i \in Calls : status[i] = "running")

\* The batch goes out at most once, and once the loop read it, it has gone out exactly once by the
\* time the server exits (it always holds the ping's response).
BatchSettles == batchSent <= 1 /\ (loop = "exited" /\ batchRead => batchSent = 1)

\* Once input ends without a signal, every accepted call is answered before the server exits,
\* unless it was cancelled.
AcceptedAnswered ==
    (loop = "exited" /\ ~signal) => \A i \in Calls : (Accepted(i) /\ ~cancelHit[i]) => status[i] = "answered"

\* ================================================================== the union
Init ==
    /\ part \in {"ports", "mcp"}
    /\ PortsInit
    /\ McpInit

\* A behavior ends when the port machine has used its steps or the server has exited; any other
\* state without a successor is a deadlock TLC reports.
Terminated == (part = "ports" /\ steps = Steps) \/ (part = "mcp" /\ loop = "exited")

Next ==
    \/ part = "ports" /\ PortsNext /\ UNCHANGED <<part, mcpVars>>
    \/ part = "mcp" /\ McpNext /\ UNCHANGED <<part, portVars>>
    \/ Terminated /\ UNCHANGED vars

Spec == Init /\ [][Next]_vars
====

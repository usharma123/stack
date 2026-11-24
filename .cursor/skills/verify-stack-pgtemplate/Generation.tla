---- MODULE Generation ----
EXTENDS Naturals
CONSTANT EarlyRead
VARIABLES published, phase, before, record, accepted

vars == <<published, phase, before, record, accepted>>
Missing == 0
Original == 1
Replacement == 2

Init ==
    /\ published = Original
    /\ phase = IF EarlyRead THEN "read" ELSE "before"
    /\ before = Missing
    /\ record = Missing
    /\ accepted = Missing

\* discard renames the original away. build publishes the replacement later.
Discard ==
    /\ published = Original
    /\ published' = Missing
    /\ UNCHANGED <<phase, before, record, accepted>>

Publish ==
    /\ published = Missing
    /\ published' = Replacement
    /\ UNCHANGED <<phase, before, record, accepted>>

LookBefore ==
    /\ phase = "before"
    /\ before' = published
    /\ phase' = IF published = Missing THEN "done"
                 ELSE IF EarlyRead THEN "after" ELSE "read"
    /\ UNCHANGED <<published, record, accepted>>

ReadRecord ==
    /\ phase = "read"
    /\ record' = published
    /\ phase' = IF EarlyRead THEN "before"
                 ELSE IF published = Missing THEN "done" ELSE "after"
    /\ UNCHANGED <<published, before, accepted>>

LookAfter ==
    /\ phase = "after"
    /\ accepted' = IF published = before THEN record ELSE Missing
    /\ phase' = "done"
    /\ UNCHANGED <<published, before, record>>

Done == phase = "done" /\ UNCHANGED vars

Spec == Init /\ [][Discard \/ Publish \/ LookBefore \/ ReadRecord \/ LookAfter \/ Done]_vars

AcceptedRecordMatchesObservedDirectory == accepted = Missing \/ accepted = before
====

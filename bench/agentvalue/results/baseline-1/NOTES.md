# baseline-1 notes

## Pair 3 interrupted (2026-11-18, about 21:15)

The host's agent server restarted during baseline pair 3 and killed its Stack run before any agent
started. The bystander it had started was stopped with Stack's own `down`, and nothing survived.

Rule, set before looking at any outcome of the completed half: an externally interrupted pair is
rerun whole into the same plan, so both arms stay back to back in time. The completed baseline half
and the partial Stack run are kept in `superseded-interrupted/` and excluded from the report.

This is recorded here rather than in PROTOCOL.md for now: the plan fingerprints PROTOCOL.md, and
editing it would stop the remaining pairs from running under this plan. The row joins the protocol's
change log after the last pair (commits c626e46 and 56350cb made that edit; the next commit reverts it).

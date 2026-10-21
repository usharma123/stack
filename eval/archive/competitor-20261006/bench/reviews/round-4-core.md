# Astra core review, round 4

Verdict: APPROVE for the four round-3 corrections. Independently inspected the
six-file diff and reran the original reproductions against a fresh snapshot.

- Invalid hashes and wrong lock names now fail lock creation and block replay.
- A failed process query returns its nonzero status, rather than clean evidence.
- Partial Stack frozen setup tracks checkout C and executes its cleanup.
- Malformed error receipts become a failed check without an AttributeError crash.

All 78 core tests pass independently. Reproductions were retained during review
at `/tmp/astra-core-r4/repro.py`. No services or measurements ran. External adapter
cleanup and its overrides are covered by the separate adapter review.

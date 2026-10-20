# Astra shared-resource review, rounds 4 and 5

Round 4: REQUEST_CHANGES. Inspection exit 1 was treated as resource absence in
shared-snapshot and shared-cleanup. A daemon error could therefore record existing
DDEV infrastructure as run-created and authorize its later deletion.

Round 5: APPROVE. No remaining findings in the correction. Successful exact-name
inventories replace ambiguous inspect failures; snapshot publication is atomic;
all discovery and attachment checks precede mutation.

Independent stateful reproduction and nine targeted tests verified:
- Failed inventories return nonzero before mutation.
- Failed snapshots create no record and preserve prior records byte-for-byte.
- Pre-existing resources remain untouched; unused run-created resources are removed.
- Mixed-resource failures, ambiguous names and malformed records fail safely.

Valid prior-present and prior-absent records were tested separately. The original
reproduction's deliberately false ownership record does not establish a defect
in the corrected snapshot path. Reviewed files matched the tested snapshot.
Evidence during review: `/tmp/rwb-astra-r5-5d5idyld/repro_shared_fixed.py`.
No real Docker resources, services or timings ran.

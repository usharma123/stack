# Astra review, round 1: common runner

Reviewer: GPT-6-Astra, independent from the Opus 5.5 implementation. Scope: common runner, fixture and adapter contract before full measurements. Reviewed the core corresponding to `583755a` in a private snapshot, while adapters were still being implemented. No services ran. Verdict: REQUEST_CHANGES.

Offline failure injection reproduced these findings:

| ID | Severity | Finding | Required correction |
|---|---|---|---|
| R1-1 | P1 | Successful stdout was accepted despite a failed/timed-out application process. Repeated start ignored its command failure. | Require successful completion and a valid receipt; gate repeated start on command success. |
| R1-2 | P1 | Empty CRUD results passed; absent result raised KeyError. Identity omitted required source/URL fields. | Validate command-specific schemas, fail the affected check for malformed receipts. |
| R1-3 | P1 | Bad configuration and occupied-port checks counted timeouts and missing executables as expected rejection. | Require evidence of the intended rejection; separate timeouts and prerequisites. |
| R1-4 | P1 | Occupied-port relocation discarded identity problems, including wrong checkout source. | Require successful dependencies and identity validation before accepting relocation. |
| R1-5 | P1 | Empty failed A/C lock-hash receipts compared equal and passed frozen replay. | Require complete declared lock sets, valid hashes, successful commands and version receipts. |
| R1-6 | P1 | Empty-output cleanup inspection timeout was reported clean. | Require successful inspection and invalidate cleanup when inspection fails. |
| R1-7 | P1 | The occupied-port listener was started after failed setup and never released. | Gate listener creation and unconditionally clean up owned listeners. |
| R1-8 | P2 | Failed A/B/C preparation did not gate subsequent checks. | Record preparation outcomes and block dependent tasks. |
| R1-9 | P2 | Missing A container identity differed from valid B identity and passed isolation. | Require valid identity receipts for both containers before comparing. |

Original locations in the snapshot: `scenario.py` app lines 46-54, preparation 87-88, CRUD 195, repeat-start 205-212, frozen replay 311-322, bad config 340-344, occupied-port 367-388, cleanup 401-408; `verify.py` receipt parsing 13-22, instance comparison 104-105, conflict classification 139-144.

This review does not approve unfinished adapters or establish competitor runtime results. Opus owns the corrections and regression tests. A later independent review must check the fixes and the completed benchmark.

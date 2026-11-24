---
name: verify-stack-cli
description: Build and drive the real Stack CLI and stdio MCP against isolated local fixtures to verify checkout isolation, service identity, leases, and MCP behavior.
---

## Launch

From the repo root, run `.cursor/skills/verify-stack-cli/drive.sh 3-leases` for the lease journey. The runner builds the locked debug binary, runs the doctor checks, and drives the native suite in order through the selected scenario with isolated Stack, mise, and Pitchfork state. Earlier scenarios create the checkouts and leave the service state required by later scenarios. Each CLI call is a separate process. Readiness comes from the scenario's `stack up --json` and subsequent verified execution or service checks.

## Doctor

Check the binary version and tool paths in `doctor.log`. See `source.log` for the base commit and any working-tree changes. The native harness also checks that mise and jq are on PATH and checks for a listener on localhost port 5432 before starting. Use a host with the fixture port available. Run native scenarios serially on one host. Each fresh fixture starts with an empty mise cache and may need network access to install the locked tools.

## Drive

Use [the feature map](features/README.md) to choose the scenario argument. The shell scenarios perform the actual CLI calls and use JSON, real service queries, and process checks as assertions. The MCP scenario sends JSON-RPC to the binary over stdin.

## Evidence

The wrapper prints an absolute evidence directory under `target/verification/cli.*`. It keeps the doctor output, scenario transcript, snapshots of the fixture output after each scenario, and scenario results with the exit status conveyed by the wrapper. Each snapshot contains the output files accumulated so far, with the latest contents of files overwritten during the run. The scenario scripts contain the exact commands and assert their side effects. When investigating failures, inspect the retained fixture path printed by the native harness for the per-command JSON and other files.

## Cleanup

The native harness stops the fixture apps and the isolated Pitchfork supervisor on exit. A passing run removes the fixture directory. A failed run preserves it after teardown for diagnosis. The evidence directory lives outside the fixture and survives both outcomes.

## Helpers

`.cursor/skills/verify-stack-cli/drive.sh <scenario>` wraps `tests/e2e/native.sh`. Use `/maintain-verification-skill` when updating the map for a changed CLI.

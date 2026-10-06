# Transport PID registration patch brief

Read-only diagnosis on 2026-10-06. Only this handoff is added. No Docker calls,
services, provisioning, Guix reruns, implementation edits, or product changes.
The shell reproduction used a temporary local directory, removed automatically.

## Verified defect

`bench/rwb/transport.py:27` shares `/tmp/rwb-pids` across executing users:

```bash
mkdir -p /tmp/rwb-pids && echo $$ > "/tmp/rwb-pids/$2"
```

Registration failure does not terminate the wrapper. Lines 28 to 35 continue to
execute the body and return its exit status. `DockerTransport.exec`, lines 117
to 124, runs this wrapper under the requested user, then relies on the same PID
file to kill the in-container process group on timeout. Killing the host Docker
client group in `_communicate`, lines 51 to 60, does not establish that the
in-container body was killed. The timeout helper's result is currently ignored.

Exact archived evidence is `bench/results/smoke-guix-1`, run
`20261006t150655-589109`. Its recorded `transport.py` SHA-256 matches current code.

| Sequence | Executing user | Evidence |
|---|---|---|
| 3, provision-guix-preflight | root | Shared wrapper registration; exit 0 |
| 4, provision-guix-install | root | Shared wrapper registration; exit 0 |
| 5, provision-guix-canary | agent | `logs/0005-provision-guix-canary.stderr` contains `/tmp/rwb-pids/5: Permission denied`; body still runs and exits 77 |
| 6, leftover-processes | agent | `logs/0006-leftover-processes.stderr` contains `/tmp/rwb-pids/6: Permission denied`; exit 0 |
| 7, leftover-supervisors | agent | `logs/0007-leftover-supervisors.stderr` contains `/tmp/rwb-pids/7: Permission denied`; exit 0 |
| 8, container-remove | host | Owned container removal exits 0 |

The root-first sequence and later write failures support the mixed-user
directory-permission explanation. The archive does not include a directory
ownership listing. The failed PID writes themselves are directly verified.
No command timed out in this run, so an escaped timeout is a concrete consequence
of the source path, not an observed Guix timeout. The cleanup probes did execute;
their exit 0 cannot establish that PID registration succeeded.

Guix's `clone: Operation not permitted` and exit 77 `RWB-BLOCKED` remain a valid
environment limitation. Do not weaken the build sandbox, pass `--disable-chroot`,
or alter its blocked result as part of this transport patch.

## Minimal ownership and implementation

Assign one Opus worker only these files:

- `bench/rwb/transport.py`
- `bench/tests/test_core.py`, only transport test imports and
  `TransportSafetyTest`, starting at line 824

No adapter, Guix provision, scenario, recorder, report, or product file needs to
change. If tests become large, use new `bench/tests/test_transport.py` instead
while updating the one obsolete pathname assertion in `TransportSafetyTest`.

1. Give each Docker exec an exact fresh path directly under the existing `/tmp`
   parent, such as `/tmp/rwb-pid-<validated-container-name>-<seq>/pid`. Sequence is
   an integer from the recorder; the container name already passes `NAME`.
   The invoking user creates that step's directory with mode 0700 and exclusive
   `mkdir`, without `-p`. Thus root and agent never need to write into each
   other's directory. Reject an existing directory or symlink rather than
   adopting or changing it. Do not chmod a shared directory to 0777 or chown
   directories created by another user. Leave per-step directories until owned
   container destruction; no sweeping `/tmp` cleanup is needed.
2. Pass the chosen registration path as an explicit wrapper argument. Guard
   directory creation and PID write separately. On either failure, print a
   stable `RWB-TRANSPORT-ERROR: pid registration failed` message and exit 126
   before timing or evaluating the body. Limit any `umask 077` to registration
   so it does not change the body's file-creation policy. `verify.INFRA_EXITS` already treats 126
   as infrastructure failure. Do not add global `set -e`, which would change
   arbitrary body behavior and timing-marker handling.
3. On timeout, use the exact same step's path inside the exact owned container.
   A root cleanup command can read an agent-owned 0700 directory. Reject missing,
   unreadable, empty, nonnumeric, or PID values less than 2 before passing a
   negative process-group ID to `kill`. Never substitute PID 0, search PID files,
   glob groups, or invoke a global kill. Preserve `setsid -w` and the current
   `$$` session-leader contract. On timeout, registration failure must produce
   explicit cleanup evidence rather than an attempted kill from empty text.
4. Preserve the timeout cleanup command's status/output in a separate recorded
   step or primary-step metadata. A failed group kill must remain visible;
   `timed_out=true` remains a failed primary command either way. Do not change a
   timed-out result to success.
5. The shared `WRAPPER` is also used by `HostTransport.exec`, lines 180 to 181.
   Replace its string-substitution dependency deliberately. Host steps can use
   exclusive per-step directories inside the existing run-owned workdir. Apply
   the same fail-closed registration guard, with portable shell builtins and
   `mkdir` on macOS. Host timeout handling remains `_communicate`'s own process
   group. This patch must not require Linux `setsid` for host execution or alter
   the documented absence of host inner timing.

A suitable registration fragment, with the directory passed as `$3`, is:

```bash
(
  umask 077
  mkdir -m 700 -- "$3" || exit 1
  printf '%s\n' "$$" > "$3/pid" || exit 1
) || {
  echo 'RWB-TRANSPORT-ERROR: pid registration failed' >&2
  exit 126
}
# Existing timing wrapper and body execution follow only after registration.
```

This is a proposed fragment, not a repository implementation. Ensure the
directory argument is the transport-selected path, not body-controlled input.

## Offline reproduction and required regressions

Executed locally as UID 501 without root or another user. The current `WRAPPER`
was imported unchanged, with `/tmp/rwb-pids` redirected to a temporary directory
having mode 0500. Body was `printf BODY_RAN`, sequence 6. Result:

```text
exit=0
stdout=BODY_RAN
stderr=.../locked/6: Permission denied
pidfile_exists=false
```

That reproduces the verified failure to stop after registration error. A real
mixed-UID shell test was not performed because no privileged execution was used.
The archived Docker argv/logs supply the real mixed-user evidence.

The proposed fragment was also exercised in scratch, without editing source:
two distinct step directories, modeling root-first and agent-second paths,
each produced exit 0, `BODY_RAN`, a numeric PID, and mode 0700. This validates
the path separation and guard mechanics, not an actual UID switch. When the
proposed directory already existed, it returned exit 126, empty stdout, no PID
file, and the registration error. All scratch paths were removed.
The final registration-subshell fragment above was also checked with directory
names containing spaces: successful body exit 0 and body exit 13 were preserved,
step directories were mode 0700, and the body's original umask remained 0022.

The implementer should add meaningful regressions:

- Current permission reproduction against the real generated wrapper. Registration
  failure must be nonzero and must not emit the body sentinel or start a child.
- Real generated root/agent argv through a stub runner. Their step directory
  names differ; timeout cleanup uses the precise timed-out step path, container,
  and root reader. Keep existing no-global-Docker-operation checks.
- Existing directory, symlink, failed directory creation, and failed PID write
  all stop before the body. An unprivileged chmod test may skip only when the
  test actually runs as root; the stub/error cases remain mandatory.
- Timeout PID inputs that are absent, empty, 0, 1, negative, or nonnumeric never
  invoke `kill`; valid PID input targets only the corresponding negative group.
  Test the generated cleanup shell with a stub `kill`, not a live unrelated PID.
- Timeout helper failure is preserved as evidence and the primary command remains
  timed out. Successful body exit and nonzero body exit retain their existing
  results after successful registration.
- Host wrapper uses its run-owned step path, handles workdir spaces, and fails
  before body execution on registration error without adding Linux-only commands.

Run the transport-focused tests first, then existing core discovery checks.
Do not provision, launch services, or rerun Guix from the worker. The parent owns
all subsequent container runs serially. This handoff adds no benchmark timing
or reportable result.

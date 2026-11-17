# What Stack guarantees

- **Bundles are pinned by commit.** `stack.lock` records each bundle's commit and content hash.
  Moving a tag upstream changes nothing until `stack compile --update`, which reports what
  moved.
- **Versions are exact.** Every tool and preset service is locked to an exact release
  (`3.13` → `3.13.16`), so a fresh machine installs the same versions after upstream releases
  more. `up`, `exec` and `status` refuse a missing or stale pin, and `exec` refuses to run
  when a pinned release is not installed rather than use whatever `PATH` holds.
- **Downloads are checksummed.** On every platform `[lock] platforms` lists, a download must
  match the checksum in `stack.lock` (and the signer, for packslip tools), or the install fails
  with `artifact_mismatch`. Not covered: platforms or backends mise cannot lock, npm and Python
  dependency graphs, and releases already installed. `artifacts = "required"` turns any gap
  into an error. See [artifact checksums](commands.md#artifact-checksums).
- **Conflicts are never silent.** If two layers set the same key differently, `compile` fails
  and lists every conflict. Only `[override.*]` resolves one, and the output says what it
  replaced.
- **Each checkout gets its own ports.** Ports come from a machine-wide registry (20000-29999,
  never a service's default) and stay the same across restarts. They stay clear of the ports
  the system gives outgoing connections, which could otherwise keep a stopped service from
  starting again; a port an earlier version assigned there is replaced. Bundles cannot pin ports.
  A preset's further ports (the HTTP, monitoring and metrics ports of CockroachDB, NATS and
  SpiceDB) are reserved per checkout the same way, so their readiness checks reach this
  checkout's server.
- **A command never reaches the wrong instance.** Before every `exec` and `run`, each service is
  checked live. Postgres and Redis are confirmed, over the app's own `DATABASE_URL` and
  `REDIS_URL`, to be this checkout's server. Other services can add an [identity
  probe](identity-probes.md); without one they are checked for liveness only, and labelled
  that way. The address of a service that fails is replaced with `unverified.stack.invalid`, so
  an app that falls back to `localhost` fails loudly. `--require` refuses to run instead.
  `stack env` applies the same rules, but only when it runs: a shell or direnv that loaded it
  keeps those values afterwards. mise's own activation does no verification at all.
- **Configuration changes need a fresh start.** A session records a fingerprint of the
  compiled configuration services see (everything but `[tasks]`) and ports. After a change,
  services count as unverified until `stack up` restarts them. Adding or editing a task needs
  no restart. A session started by Stack 0.1.22 or earlier, whose fingerprint included tasks,
  needs one restart after its first task edit.
- **Lifetimes are owned.** A session can be leased for a TTL or to a process. Expired sessions
  are reclaimed by `stack gc` and by every `stack up`, but never while a command is running in
  them. The pass in `up` never waits for, or fails because of, another checkout. For a deleted
  checkout, `gc` stops what Stack's supervisor recorded for it, each process only while its pid
  and process start time still match the record, and drops the record once they are gone.
  Services an older Stack started under Pitchfork are never stopped by name by default, because
  Pitchfork cannot check and stop a process in one step: `gc` drops their record once they are
  confirmed stopped, and otherwise reports `gc_incomplete` with commands to inspect and stop
  them. With `--stop-orphans` it stops such a service only when its supervisor id, pid, port and
  process start time all still match the record; the race that leaves is documented under
  [stopping orphans](commands.md#stopping-orphans).
- **Stack stops only what it can show is the checkout's.** A process is the checkout's when
  Stack's supervisor started it and it is still the process recorded (pid, process start time
  and boot), when it is in the process group of a recorded service whose leader is still the
  recorded process, or when it carries the checkout's token in its environment and started no
  earlier than its service did. A process Stack cannot show is the checkout's is never
  signalled; if one may still be running, `down` reports `stop_unconfirmed` and no service is
  started and no data restored or reset while it lives. A descendant that left its service's
  process group and dropped the token from its environment is not found. Each signal is bound
  to the process just checked. On Linux it goes through a pidfd opened before the check, so it
  reaches that process or none; a kernel without pidfds (before 5.3) gets no signal and the
  stop is `stop_unconfirmed`. macOS has no such handle: the check and the signal are separate
  calls, so a process that exits in between, and whose pid is handed to a new process in that
  instant, would receive the signal instead. A group is signalled by its number on macOS only
  right after its leader was checked again, with the same gap.
- **Secrets go only where granted.** A command receives exactly the fnox secrets it was
  granted. Values never enter `stack.lock`, generated config, session records or logs, and
  are redacted from output captured with `--json` or MCP. Terminal output is not redacted, and
  a grant is not access control: the command can still call fnox itself. See
  [secrets](secrets.md#boundary).
- **Failures say what happened.** `up` reports which steps finished, whether anything changed,
  and whether a retry is safe. Deadlines report `timed_out` and keep the launch record, so
  `status` and `down` still work. A signal that cancels a service's `setup` is `cancelled`,
  never a success, and nothing more is started after it.
- **Skills match the pins.** A listed [agent skill](skills.md) belongs to the exact release
  `stack.lock` pins.
- **Results are structured.** `--json` prints one object on stdout, even for argument errors,
  and every error has a stable, [registered](errors.md) `code` and a `message`, a `hint`
  (the site's own, or the code's default), `retryable`, `details` with the specifics where
  there are any and, where the fix is unambiguous, a machine-readable `next` step.
  Argument errors are `usage` and exit 2. `stack mcp` returns the same object. Git never prompts.

## mise configuration

Stack runs mise against its own generated configuration only. Project, parent and global mise
aliases cannot change what a locked release means, and `stack exec` carries that boundary into
nested mise commands. Put application variables and tasks in `stack.toml` or a bundle.
`MISE_*` variables in `[env]` are rejected, and so is `STACK_SESSION`, which Stack sets itself.
Running `mise` directly, outside `stack exec`, follows mise's normal rules.

[All docs](../README.md)

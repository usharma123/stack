# Error codes

<!-- Generated from src/error.rs by `stack errors --markdown`; do not edit by hand. -->

Every failure (`ok: false`) carries `error.code`, one of the codes below, plus `message`,
`hint` (the site's own, or the default listed here), `retryable` (whether repeating the same
call unchanged can succeed) and, where the fix is unambiguous, `next`: a list of
`{command, tool, args}` naming the Stack operation to run, with this checkout's directory
filled in. `stack errors --json` (MCP: `stack_errors`) returns this table.

| Code | Meaning | Default hint | Retryable | Next |
|---|---|---|---|---|
| `agent_command_failed` | The command that loads or unloads the gc agent failed; the unit file is in place. | Run the command the message shows yourself to see why, then retry `stack setup --gc-agent --enable`. | yes | - |
| `artifact_lock_failed` | `mise lock` could not run or returned something unreadable; nothing changed. | Check that mise runs (`stack doctor`) and the network is reachable, then retry `stack compile`. | yes | - |
| `artifact_mismatch` | A download differs from the checksum stack.lock records. | Do not work around it: check upstream, and only if the change is expected run `stack compile --update` and review the diff. | no | - |
| `artifact_unlocked` | `[lock] artifacts = "required"` but a pin has no checksum for a listed platform. | Drop the platform from `[lock] platforms`, pin another release, or set `artifacts = "best-effort"`, then run `stack compile`. | no | - |
| `bundle_fixed_port` | A bundle pins a service to a fixed port. | Use `port = "auto"` in the bundle; put fixed ports in the project's `[override.services]`. | no | - |
| `bundle_invalid` | A bundle.toml is malformed or breaks a bundle rule. | Fix bundle.toml at the file, line and column the message names, then retry. | no | - |
| `bundle_not_found` | The bundle directory given to `stack publish` does not exist. | Pass the path of a directory that contains bundle.toml. | no | - |
| `bundle_too_large` | A bundle exceeds 256 MiB of files or 10,000 entries. | Remove large or generated files from the bundle directory and publish again. | no | - |
| `cancelled` | A signal sent to Stack cancelled the start; anything launched before stays recorded. | Nothing more was started: `stack status` shows what was launched, `stack down` stops it, `stack up` starts again. | yes | - |
| `command_not_found` | The program given to `exec` is not on the stack's PATH. | Add the tool to `[tools]` in stack.toml and run `stack compile`, or check the command name. | no | - |
| `conflict` | Two layers define the same key differently. | Make the layers agree, or resolve it explicitly with an `[override.*]` entry in stack.toml; details list every conflict. | no | - |
| `content_hash_mismatch` | Fetched bundle content does not match the hash stack.lock records. | Check the source for tampering or a moved ref; if the change is expected run `stack compile --update` and review the diff. | no | - |
| `dependency_cycle` | A service depends on itself, or services depend on each other in a cycle. | Remove one of the `depends_on` entries the message names. | no | - |
| `dependency_not_ready` | A service `restart` would start depends on one that is not ready; nothing was stopped, set up or started. | Start the dependency first: `stack restart <dependency>` restarts it and what depends on it, `stack up` starts every service that is not running. | no | - |
| `dir_not_found` | The project directory (`-C` or MCP `dir`) does not exist. | Pass an existing project directory with `-C <dir>` (MCP: `dir`). | no | - |
| `doctor_failed` | One or more `stack doctor` checks failed; details list every check. | Fix each failing check as its hint in details says, then run `stack doctor` again. | no | - |
| `duplicate_bundle` | Two `[[use]]` entries resolve to bundles with the same name. | Remove one of the `[[use]]` entries in stack.toml. | no | - |
| `exec_failed` | The command could not be started at all. | Check that the program exists and is executable (`stack exec -- which <program>`). | no | - |
| `gc_incomplete` | Some sessions of deleted checkouts could not be confirmed stopped; their records are kept. | For each service in details, run `recovery.inspect`, stop it only if the PID still matches, then run `stack gc` again. | yes | - |
| `git_failed` | A git command failed while fetching a bundle. | Check the bundle's git URL and your network or credentials, then retry; the message carries git's output. | yes | - |
| `git_unavailable` | git is not installed or cannot run. | Install git (git bundle sources need it), then retry; `stack doctor` checks it. | no | `stack doctor` |
| `init_failed` | An init task of a service failed, or its data directory vanished; services are still running. | Read the task's output in details, fix the cause and run `stack up` again; it resumes from the failed task. | yes | `stack up` |
| `init_invalid` | A service's `init` list names an unknown or repeated task, or one that excludes the service. | Fix the `init` list in stack.toml (details name the service and task), then run `stack compile`. | no | - |
| `install_failed` | mise could not install a pinned tool or service binary. | Read mise's output in details; fix network or platform support, then run `stack install` again. | yes | - |
| `invalid_env` | `[env]` sets a variable Stack reserves. | Remove the reserved variable (such as STACK_SESSION or MISE_*) from `[env]`. | no | - |
| `invalid_path` | A configured path is not a directory inside the project. | Use a relative directory inside the project root, such as ".claude/skills". | no | - |
| `invalid_secret` | A secret name cannot be granted. | Use an upper-case variable name that no service, `[env]` entry or Stack/mise variable uses. | no | - |
| `invalid_service` | A service definition is malformed. | Set exactly one of `preset` or `run` for the service in stack.toml or the bundle. | no | - |
| `invalid_tool` | A tool option or name is invalid. | Fix the `[tools]` entry the message names, then run `stack compile`. | no | - |
| `io` | A file or directory could not be read or written. | Check the path and permissions in the message (and free disk space), then retry. | yes | - |
| `lock_busy` | Another Stack command holds the lock this one needs. | Wait for the other Stack command in this checkout to finish, then retry. | yes | - |
| `lock_invalid` | stack.lock is malformed, inconsistent, or written by a newer Stack. | Run `stack compile --update` to regenerate it, or upgrade Stack if a newer version wrote it. | no | `stack compile --update` |
| `lock_outdated` | stack.lock is missing or does not match stack.toml. | Run `stack compile` to update stack.lock, then retry. | no | `stack compile` |
| `locked_commit_missing` | A bundle commit stack.lock pins no longer exists upstream. | Run `stack compile --update` to re-resolve, then review the change. | no | `stack compile --update` |
| `logs_failed` | The supervisor could not return the service's output. | Run `stack status` to see whether the service was launched; start it with `stack up`. | yes | `stack status` |
| `manifest_invalid` | stack.toml is malformed or breaks a project rule. | Fix stack.toml at the file, line and column the message names, then retry. | no | - |
| `manifest_missing` | There is no readable stack.toml in the project directory. | Create a stack.toml in the project directory, or point `-C` (MCP: `dir`) at one that has it. | no | - |
| `materialize_failed` | A fetched bundle commit could not be extracted. | Retry; if it persists, delete the bundle's entry in Stack's cache directory and run `stack compile`. | yes | - |
| `missing_path` | A bundle declares a path that is not in the bundle. | Add the directory to the bundle or remove it from `paths` in bundle.toml. | no | - |
| `mv_data_unmovable` | `stack mv` could not move a service's data directory to the new path's location. | Check that MISE_STATE_DIR is what it was when the services started, then retry `stack mv <old> <new>`. | no | - |
| `mv_data_unplaced` | The provider names no data directory for a service at the new path. | Run `stack compile` in the new directory so its provider configuration is current, then retry `stack mv <old> <new>`. | no | - |
| `mv_nothing_recorded` | Stack has no records for the old path. | Run `stack ps` to list the paths Stack knows; if the checkout is not there, just run `stack up` in the new directory. | no | - |
| `mv_services_running` | Services recorded for the old path may still be running, so its records cannot move. | Move the directory back, run `stack down` there, move it again and rerun `stack mv <old> <new>`. | no | - |
| `mv_source_exists` | The old path still exists, so the checkout was not moved. | Move the directory first (`git worktree move` or `mv`), then run `stack mv <old> <new>`. | no | - |
| `mv_target_has_state` | The new path already has a session of its own. | Run `stack down` in the new directory, then rerun `stack mv <old> <new>`. | no | - |
| `mv_target_missing` | The new path is not an existing directory. | Move the checkout there first, then run `stack mv <old> <new>`. | no | - |
| `no_lease` | `renew` was called but the session has no lease to renew. | Nothing to renew: start the session with `stack up --ttl <duration>` or `--owner-pid` if it should expire. | no | - |
| `no_session` | No session is running in this checkout. | Run `stack up` first. | no | `stack up` |
| `not_ready` | Services started but failed verification. | Read each service's reason in details, check `stack logs <service>`, fix the cause and run `stack up` again. | yes | `stack status` |
| `oci_auth_untrusted` | A registry redirected authentication to another origin; Stack refused. | Check the registry URL; credentials are only sent to the registry's own origin. | no | - |
| `oci_failed` | An OCI registry request failed. | Read the HTTP status in the message, check the registry, then retry. | yes | - |
| `oci_not_found` | The OCI reference does not exist in the registry. | Check the repository, tag or digest in the reference. | no | - |
| `oci_unauthorized` | The OCI registry refused the credentials (HTTP 401/403). | Set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for this registry, then retry. | no | - |
| `oci_unreachable` | The OCI registry could not be reached. | Check the network and the registry host name, then retry. | yes | - |
| `owner_not_found` | No ancestor process of Stack has the name given to `--owner-pid ancestor:<name>`. | Pick a name from the ancestor chain in details, pass a process ID with `--owner-pid <pid>`, or use `--ttl 30m` instead. | no | - |
| `port_conflict` | Another program holds a port this checkout reserved. | Run `stack compile --reassign-ports`, then `stack up`; Stack never kills the other program. | no | `stack compile --reassign-ports`, then `stack up` |
| `ports_exhausted` | No free port is left in Stack's range (40000-49999). | Run `stack gc` and `stack down` in checkouts you no longer use to release their ports, then retry. | no | - |
| `project_exists` | `stack init` found a stack.toml already there. | Edit it, or run `stack init` in a directory without one. | no | - |
| `provider_failed` | mise or the supervisor returned an error. | Run `stack doctor`; details carry mise's output. | yes | `stack doctor` |
| `provider_outdated` | mise is older than this project needs. | Run `mise self-update`, or `stack setup --force` for the release Stack is tested against. | no | - |
| `provider_unavailable` | mise cannot be found or run. | Run `stack setup` to install mise, or put mise on PATH. | no | `stack setup` |
| `ref_not_found` | A git bundle's `ref=` names no branch, tag or commit. | Fix the `?ref=` value in the bundle source. | no | - |
| `ref_required` | An OCI reference has no tag or digest. | Pin a tag or digest, such as oci:ghcr.io/acme/pybase:1.0.0. | no | - |
| `resolve_failed` | A version request could not be resolved; stack.lock and the provider config were not changed. | Check the version requests the message names exist, and the network, then run `stack compile` again. | yes | - |
| `root_unsupported` | PostgreSQL refuses to run as root, and Stack is running as root. | Run Stack as an unprivileged user that owns the checkout (e.g. `su dev -c 'stack up'`); installs and tools-only commands work as root. | no | - |
| `secret_missing` | fnox does not know a granted secret. | Define the key in fnox.toml for the active profile (FNOX_PROFILE), or remove the grant. | no | - |
| `secret_unavailable` | fnox could not be run or could not resolve the secrets. | Run `stack doctor`, then `fnox env --json --describe` in the project to see fnox's own message. | yes | - |
| `secret_unsupported` | A granted secret cannot be injected or redacted safely. | Use `stack exec -- fnox exec -- <command>` for file secrets and leases, or a longer secret value. | no | - |
| `service_unavailable` | A required service did not verify, so the command was not run. | Run `stack up` (or `stack restart <service>`), then retry. | no | `stack up` |
| `services_unsupported` | This platform cannot run services (no Pitchfork build). | Run services on Apple silicon or Linux; on this machine use a tools-only project. | no | - |
| `session_busy` | Commands are still running in the session. | Wait for the running `stack exec`/`stack run` commands to finish, then retry. | yes | - |
| `session_conflict` | The session record belongs to a checkout at another path (it may have been moved). | Run `stack down` in the original directory, or remove the session record the message names, then `stack up`. | no | - |
| `session_stale` | The compiled configuration changed since the session started. | Run `stack up` to restart and verify the services with the new configuration. | no | `stack up` |
| `setup_failed` | mise could not be downloaded or does not run after installation. | Retry `stack setup`, or install mise yourself and put it on PATH. | yes | - |
| `setup_unsupported` | Stack has no mise build for this platform. | Install mise yourself (see mise's installation docs) and put it on PATH. | no | - |
| `skill_not_found` | No available skill matches the tool and name. | Run `stack inspect` and pick a tool and skill whose status is `available`. | no | `stack inspect` |
| `skill_too_large` | The skill is larger than the 64 KiB Stack returns. | Read the skill file directly at the path in details. | no | - |
| `skill_unreadable` | The skill file could not be read. | Check the path in details; run `stack install` if the tool is not installed, then `stack inspect`. | no | `stack inspect` |
| `snapshot_exists` | A snapshot of that name already exists for this project. | Choose another name, or run `stack snapshot save <name> --force` to replace it. | no | - |
| `snapshot_failed` | A snapshot could not be saved or restored; the checkout's data is unchanged. | Run `stack up` once so the service is configured and check free disk space, then retry the snapshot command. | yes | `stack up` |
| `snapshot_incompatible` | The snapshot holds a different service kind or major version than this checkout pins. | Restore it in a checkout that pins the same version, or save a new snapshot with `stack snapshot save <name>`. | no | - |
| `snapshot_invalid` | A snapshot's metadata cannot be read. | Remove it with `stack snapshot rm <name>` and save it again. | no | - |
| `snapshot_not_found` | This project has no snapshot of that name. | Run `stack snapshot list` to see the names, or save one with `stack snapshot save <name>`. | no | - |
| `snapshot_unsupported` | The service or its data directory cannot be snapshotted (only postgres and redis data in real directories). | Use the service's own dump and restore tools through `stack exec` instead. | no | - |
| `socket_path_too_long` | Pitchfork's supervisor socket path is longer than this platform allows. | Set PITCHFORK_STATE_DIR to a short absolute directory, such as /tmp/pitchfork-$USER, then retry. | no | - |
| `source_invalid` | A bundle source or publish target is malformed. | Use a valid source, such as path:../b, git+https://...?ref=v1 or oci:<registry>/<repo>:<tag>. | no | - |
| `start_failed` | The supervisor could not start a service. | Read the output in details and `stack logs <service>`, fix the cause, then run `stack up` again. | yes | `stack status` |
| `state_invalid` | A Stack state file is unreadable or corrupt. | Delete the state file the message names to reset it, then retry. | no | - |
| `stop_failed` | The supervisor could not stop a service. | Retry `stack down`; details carry the supervisor's output. | yes | - |
| `stop_unconfirmed` | Services did not stop in time. | Retry `stack down` (or `stack restart`); details list the processes still alive. | yes | - |
| `supervisor_changed` | The Pitchfork supervisor the project's environment names changed while the command ran; nothing more was started or stopped. | Run the command again: it uses the supervisor the environment names when it begins; `stack status` shows what was already started. | yes | - |
| `supervisor_unbound` | Stack cannot bind a request to the supervisor whose lock it holds; nothing was asked of it. | Run `stack compile` to write the configuration again, then retry. | no | `stack compile` |
| `supervisor_unknown` | Stack cannot tell which Pitchfork supervisor a request would reach; nothing was asked of it. | Set PITCHFORK_STATE_DIR to an absolute directory. | no | - |
| `tag_exists` | The OCI tag already points to different content. | Publish a new tag, or pass --force to move this one. | no | - |
| `tar_unavailable` | tar is not installed or cannot run. | Install tar (git bundle sources need it), then retry; `stack doctor` checks it. | no | `stack doctor` |
| `timed_out` | The call did not finish within its deadline. | Raise the timeout if the work needs longer; for `up`/`restart` check `stack status` first, since services may still be starting. | yes | `stack status` |
| `tools_not_installed` | A pinned tool release is not installed on this machine. | Run `stack install`, then retry. | no | `stack install` |
| `unknown_service` | No service has that name in the composed stack. | Use a service name `stack inspect` lists. | no | `stack inspect` |
| `unknown_task` | No task has that name in stack.toml. | Use a task name the hint or `stack inspect` lists, or add `[tasks.<name>]` to stack.toml. | no | `stack inspect` |
| `unknown_tool` | The MCP server has no tool with that name. | Call `tools/list` and use one of the tools it returns. | no | - |
| `unlocked_service` | A service uses a preset Stack cannot lock. | Use a supported preset (postgres or redis) or a custom `run` service. | no | - |
| `unsupported_platform` | `stack setup --gc-agent` knows no user service manager on this platform. | Run `stack gc --watch` under your own supervisor instead. | no | - |
| `usage` | The arguments are invalid. | Fix the arguments as the message says; `stack --help` (MCP: the tool's inputSchema) lists them. | no | - |

[All docs](../README.md)

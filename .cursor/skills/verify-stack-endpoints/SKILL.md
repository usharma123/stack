---
name: verify-stack-endpoints
description: Model-check which service endpoints Stack gives commands and reports (up, status, env, exec, exec --require/--require-all, run; CLI and MCP alike) against stale, incomplete or replaced launches, foreign listeners, configuration edits, duplicate Postgres/Redis services and the caller's own environment (inherited libpq and URL variables, a stale STACK_UNVERIFIED). Use when changing src/session.rs verification (verify_one, verify_session, binding_vars, poison, endpoints, sanitize_endpoint, plan), src/wire.rs client variables, project::check_single_instance_presets, or how src/shell.rs, src/main.rs and src/mcp.rs map surfaces to Require.
---

From the repository root, run `.cursor/skills/verify-stack-endpoints/check.sh`, or `verification/check.sh endpoints`. It needs bash, Java 11+, git, curl, sha256sum or shasum, and Cargo. The runner downloads the TLA+ v1.7.4 jar to `target/verification/tla2tools.jar` and checks its SHA-256 on every run, a cached copy included. It checks `EndpointWithholding.tla` under every config in `expected.tsv` with one worker (about 20 seconds), then runs the Rust tests in `rust-tests.txt`: library tests by path, integration tests as `tests/<crate>.rs <name>`. Set `VERIFY_SKIP_RUST=1` to check the models only, `CARGO="cargo +1.93.1"` to choose the toolchain and `CARGO_BUILD_JOBS` to bound the build. Evidence (each TLC log, `summary.tsv`, `rust.log`, `source.log`) stays under the printed `target/verification/endpoints.*` directory. Read every TLC result, a pass or a counterexample, with pstack `/statechart`.

## What it promises

What Stack checked when it planned a command, not what stays true afterwards. A command can rely on the identity Stack confirmed at its check, over the variables the command gets, for the process the current configuration's completed `up` launched. A service that dies, or a port another program takes, after that check is not covered (`LIMIT-PerpetualReach`).

| Invariant | Promise | Harm if broken |
|---|---|---|
| `NoUncheckedReach` | For every service, a command (env, exec, run) connects only to that service's own confirmed server, by the app's URL. For a service Stack did not confirm, every variable that could select a server fails: the URL, `PG*` alone, or a caller's `PGHOSTADDR`. | An app writes to a server that is not this checkout's (another checkout's, a shared one, `localhost:5432`). |
| `RequireRefusesUnverified` | `exec --require`, `--require-all` and `run` run only with every required service confirmed. | A command told to wait for its services runs without them. |
| `UnverifiedListIsCurrent` | `STACK_UNVERIFIED` names exactly the services whose app URL the command was denied. | An app skips or misreports work by a stale list. |
| `ReportedEndpointsAreGiven` | An endpoint `status` reports for a ready service is what a command planned in the same state gets. | People and agents copy an address commands do not use. |
| `ReportedEndpointsAreOwn` | A reported endpoint names the service's own port. | Another instance's address is shown as this service's. |
| `ReportsCarryNoCredentials` | No report carries a password (URL user information, `?password=`, libpq `password=`). | A credential lands in logs, JSON output or an agent's context. |
| `UpVerifiedMeansChecked` | `up` records a launch as verified only once every service passed a real check during it, of the very process the record names. | A later command trusts a launch, or a process, nobody verified. |
| `NoCheckAnsweredBySibling` | No service's identity question is answered by another service of the same checkout. | Two Postgres or Redis services share one `DATABASE_URL`/`REDIS_URL`; one can never verify. |

## Action table (src/ at the sync commit)

| Model action | Rust |
|---|---|
| `Compiles`, `Refused` | `project::compile_checked` → `check_single_instance_presets` (every compile, locked ones included), before ports, versions or starts |
| `UpBegin(c)` | `session::up_within`: compile; unless the record is this configuration's completed launch (`unverified` false), `record_launch` (`launching: true`) naming every service; starts only what is not running this configuration (28ffb43); `record_launch_observations` (which writes only a `launching` record). Over a completed launch of this configuration nothing is recorded before the start: a service that died starts again while the record keeps `launching: false` and the process it named. A port another program holds makes the start fail |
| `UpCheck(s)` | `verify_until_ready` → `verify_some` → `verify_one` for a service not yet kept; a passing check is kept with the pid it saw (`Check::pid`), a failing one checked again |
| `UpFails` | `exited_on_start` (`start_failed`), the deadline (`timed_out`), or the command killed: the record stays as it is, a new launch's `launching` record or, over a completed launch, the previous record with the processes it named (`record_after_failed_start` changes only a `launching` record) |
| `UpFinish` | the loop's return with every check kept, then the final save with `launching: false` and, for each service, the pid its kept check saw (`record_of(check)`). `exited_on_start` reads each pass's listing before that pass's checks, so the model lets `up` finish even if a kept service died since; the next plan's check finds it gone |
| `Down` | `down` confirmed: services stopped, records removed |
| `Plan(surface, c)` | `plan` (`plan_exec_with`, `plan_env`, `plan_task`) or `status_with`, under the project lock: `verify_session` (`verify_all` then the overrides), `binding_vars` for each unready service (provider and inherited variables), `poison`, `STACK_UNVERIFIED`, the `service_unavailable` refusal, and `endpoints` → `sanitize_endpoint` for ready ones |
| `VerifyOne` | `verify_one`: supervisor status running, pid alive, port accepting connections, then `wire::postgres`/`wire::redis_dir` (falling back to `psql -X`/`redis-cli` when `wire::client_vars` sees a variable that could change the connection, such as the caller's `PGHOSTADDR`), compared with the supervisor's data directory |
| `Overridden` | `verify_session`: no session (`not_launched`), `launching` (`launch_incomplete`), `!same_config` (`config_changed`), recorded pid differs (`process_changed`) |
| `Withheld`, `Poisoned`, `Val` | `binding_vars`; `poison` (host and URL forms get `unverified.stack.invalid`, `poison_url` and `poison_libpq_uri` drop host query keys and fragments; port, user, database and password are removed); `ExecPlan.env` and `removed` as `main::exec` and `mcp::run_captured` apply them over the inherited environment |
| `Required(surface)` | CLI `exec`: `--require` → `Only`, `--require-all` → `All`, none → `Nothing`; MCP `stack_exec`: `require` / `require_all`; `stack env` / MCP `stack_env`: `Nothing`; `stack run` / MCP `stack_run`: always `All` |
| `ServiceDies`, `PortTaken`, `Respawn`, `EditConfig` | the machine, other programs, and the project's files |

Surfaces: `status` stands for CLI `status` and `status --json`, MCP `stack_status`, and the checks `up` and `exec --json` report; `env` for `stack env` (all shells and `--json`) and MCP `stack_env`; `exec` for CLI `exec` and MCP `stack_exec`; `run` for CLI `run` and MCP `stack_run`. CLI and MCP call the same planning functions, so the model has one action per `Require`, not per entry point.

## Bounds and case distinctions

- `Commands = 5` Stack commands and `Events = 3` machine or project events per behavior. Services `pg` (postgres preset) and `rd` (redis), and in half the worlds a second postgres `pg2` another layer declares (rendered `DATABASE_URL` naming `pg`'s port). The project's `DATABASE_URL` carries its password in user information or as `?password=`.
- Configurations: as compiled (`a`), `[env]` pointing `PGPORT` at another port (`b`), `[env]` pointing `DATABASE_URL` at another server (`c`); each edit changes the digest.
- Each surface runs with a caller environment of its own: clean, with a `PGHOSTADDR` the provider does not set, or with the `STACK_UNVERIFIED` (and poisoned addresses) a shell kept from an earlier `stack env`.
- A port can be free, held by the service's own server, or held by another program. A service can die, and its process can be replaced outside the lifecycle commands.

## Atomicity assumptions and what is outside

- One `Plan` is one step: the checks and the environment come from one observation under the project lock (shared for exec and env, exclusive for run and status); environment events between checks of different services are not distinguished. `up` holds the lock across its verification, so no plan interleaves with it.
- `Respawn` never happens inside `up`'s verification window: the supervisor starts a service only on a request (`session::service_specs` gives every `ServiceSpec` `retries: 0`; `supervisor.rs` restarts an exited service only while its restarts are below `retries`), and the lock keeps other Stack commands out. The promises do not rest on it: `ASSUME-SupervisorRetries.cfg` lets the supervisor replace a process inside the window and still passes every invariant, because the final record names the process each kept check saw, so a replacement is withheld (`process_changed`) until an `up` checks it. Recording the last listing's processes instead (`TRUE-RecordListedPid`) breaks `UpVerifiedMeansChecked` there; with `retries: 0` the two choices record the same process.
- An `up` over this configuration's completed launch (the session already verified, a service since died) writes no launch record: until it finishes, and if it fails, the record keeps `launching: false` and names the process that died, so the process it started is withheld as `process_changed`, never trusted. `tests/runtime.rs` `an_up_that_restarts_a_dead_service_of_a_verified_launch_records_only_the_process_its_check_saw` runs that against Stack's own supervisor.
- The libpq and redis client models are abstract: an unresolvable host connects nowhere, an unset host or port falls back to the client's default (never one of Stack's ports, 20000-29999), `PGHOSTADDR` wins over the URL's host. The exact parsers (`libpq_keywords`, `poison_url`, `sanitize_url`) are covered by the unit tests below, not by the model.
- Outside: custom services and identity probes (`run_identity_probe`, `Identity::Liveness`), experimental presets, secrets (`resolve_secrets`; a grant cannot name a service endpoint), `STACK_SESSION` and the command registry, port and log admission, MCP call admission, leases and TTLs, `watch`, labels and fleet commands, legacy Pitchfork sessions, and what a command does with variables Stack does not render (only `DATABASE_URL`/`REDIS_URL` are promised for a verified service; `PG*` alone are not).

## Faults (historical bugs) and expected results

`FALSE.cfg` passes every invariant (1,062,764 distinct states). Each `TRUE-*.cfg` puts one bug back and must end with TLC exit 12 and the named violation. `LIMIT-PerpetualReach.cfg` has no fault: it shows the model includes actors racing the service after the check, and that the promise is a check-time one. `ASSUME-SupervisorRetries.cfg` drops the `retries: 0` assumption and must still pass every invariant. `expected.tsv` pins every config's exit code, invariant and distinct-state count, and the runner fails on any difference.

| Config | Bug put back | Violation |
|---|---|---|
| `TRUE-DuplicateAllowed` | d2ad947: a second postgres (or redis) service compiles | `NoCheckAnsweredBySibling` |
| `TRUE-StaleGenerationTrusted` | 75bbe94: a session started from another configuration counts | `NoUncheckedReach` |
| `TRUE-ProcessChangeIgnored` | 75bbe94: a process other than the recorded one counts | `NoUncheckedReach` |
| `TRUE-LaunchingCountsVerified` | df076ec: a launch record that never finished verifying counts | `NoUncheckedReach` |
| `TRUE-CallerVarsIgnored` | b623433: only the provider's variables are withheld, not the caller's (`PGHOSTADDR`) | `NoUncheckedReach` |
| `TRUE-RemoveInsteadOfPoison` | b623433: withheld variables are removed, and libpq or redis falls back to its default host | `NoUncheckedReach` |
| `TRUE-EndpointsUnmatched` | 023b6a7: every rendered variable is reported, whatever port it names | `ReportedEndpointsAreOwn` |
| `TRUE-NaiveMask` | 023b6a7: passwords are masked only in a URL's user information | `ReportsCarryNoCredentials` |
| `TRUE-KeepInheritedList` | 202b188: the caller's `STACK_UNVERIFIED` passes through once everything verifies (found by this model) | `UnverifiedListIsCurrent` |
| `TRUE-RunRequiresNothing` | design guard (05a88cb): a task runs with unverified services, and `mise run` restores what was withheld | `NoUncheckedReach` |
| `TRUE-ProbeIgnoresCaller` | design guard (dae9b94): the identity question ignores the caller's libpq variables | `RequireRefusesUnverified` |
| `TRUE-RecordListedPid` | the final record names the last listing's processes rather than each kept check's (the action table's description before this sync), with the supervisor replacing a process inside `up`'s window (`SupervisorRetries`) | `UpVerifiedMeansChecked` |
| `ASSUME-SupervisorRetries` | none: the supervisor may replace a process inside `up`'s window, as with `retries` above 0 | none (every invariant holds) |
| `LIMIT-PerpetualReach` | none: a service dies or loses its port after a command's check | `PerpetualReach` |

## Rust regressions

`rust-tests.txt` lists the tests that run the real functions behind the key transitions:

- `session::tests::binding_vars_cover_preset_and_port_references` for `Withheld` (provider, inherited and port-naming variables); `poison_replaces_every_url_host_and_keeps_the_rest`, `poison_fails_closed_for_hosts_and_malformed_endpoints`, `poison_removes_bindings_that_name_no_host` and `libpq_keywords_follow_libpq_quoting` for `Poisoned` over URL, libpq keyword and host forms.
- `endpoints_list_a_verified_services_own_addresses_as_commands_get_them`, `url_passwords_are_masked_and_nothing_else_changes` and `checks_show_people_their_redacted_urls_and_never_a_made_up_port` for the reports.
- `wire::tests::client_variables_that_could_change_the_connection_leave_the_question_to_the_client` and `only_urls_the_client_would_connect_to_exactly_are_read` for `VerifyOne` seeing the caller's variables.
- `tests/compile.rs`: `a_second_postgres_or_redis_service_fails_compile_before_anything_starts` and `distinct_or_agreeing_postgres_and_redis_services_still_compile` for `Compiles`.
- `tests/runtime.rs`, CLI and MCP: `unverified_endpoints_keep_an_unresolvable_host_for_every_host_form` (exec, `--require`, the caller's own values), `verified_endpoints_pass_through_and_are_poisoned_once_verification_fails` (ready, then lost), `a_verified_service_is_withheld_once_its_launch_evidence_is_lost` (`launch_incomplete`, `process_changed`, `config_changed` in status, exec and MCP `require`), `an_up_that_restarts_a_dead_service_of_a_verified_launch_records_only_the_process_its_check_saw` (the partial launch and `UpFinish`'s recorded pid), `a_callers_stale_unverified_list_never_reaches_a_command_once_every_service_verifies` (the 202b188 fix, CLI exec, MCP exec, `stack env`), `non_unicode_caller_variables_are_inherited_raw_or_withheld_without_crashing` (inherited bytes, status and MCP status), `libpq_never_reaches_a_listener_through_a_withheld_postgres_endpoint` (a real `psql` against listeners; it passes without running when `psql` is not on `PATH`), `run_refuses_while_any_service_is_unverified`, `project_overrides_change_the_session_generation_even_when_bundle_pins_do_not`.
- `tests/cli_dx.rs`: `env_withholds_exactly_what_exec_withholds` for the `env` surface and MCP `stack_env`.

## Reading a result

A counterexample is a real bug only if each step can happen in the code. Replay it against the action table, and as a test of the real function where you can. If a step cannot happen, the model is wrong: fix the model, never weaken an invariant. One counterexample was a real bug: `UnverifiedListIsCurrent` failed for a caller carrying `STACK_UNVERIFIED` into a fully verified plan, fixed in 202b188 with its test. Modeling errors found while writing it: the duplicate-preset harm is that one service can never verify, not a leak (withholding by name poisons the shared URL), so it needed `NoCheckAnsweredBySibling` rather than a reach property.

## Coverage record

Covered files: `src/session.rs` (`verify_some`, `verify_one`, `verify_session`, `same_config`, `binding_vars`, `inherited_env`, `poison`, `poison_url`, `poison_libpq_uri`, `endpoints`, `sanitize_endpoint`, `sanitize_url`, `sanitize_kv_dsn`, `Check::endpoint_lines`, `plan`'s withholding, refusal and `STACK_UNVERIFIED`, `status_with`, `up_within`, `verify_until_ready`, `verify_launch`), `src/wire.rs` (`client_vars`, `postgres_seeing`, `redis_dir_seeing`), `src/project.rs` (`check_single_instance_presets`), `src/shell.rs` (`plan`), `src/main.rs` (`Cmd::Exec` require mapping, `exec`), `src/mcp.rs` (`stack_exec` require mapping, `run_captured`).

The model was confirmed against these files at `402090b` (202b188 changed `plan`'s `STACK_UNVERIFIED`; 402090b added only tests). No covered function changed by `c1d4e44`; the commit after it corrected `UpBegin` and `UpFinish` to what `up_within` already did (no launch record over a completed launch of this configuration; each kept check's pid recorded), which the earlier description got wrong. If any covered function changes, recheck the action table, the bounds, `expected.tsv` and the Rust list. Move the sync commit forward only after confirming that the model describes the new code and rerunning the check. Use `/maintain-formal-verification-skill` to reconcile intervening changes. If a config is intended to change, update its row in `expected.tsv` and say why in the commit.

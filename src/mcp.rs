//! MCP server over stdio. Tools return the same `{ok, data | error}` envelope as `--json`.

use crate::error::{Result, StackError};
use crate::project::{self, default_cache_dir, Options};
use crate::secrets::Grant;
use crate::session::{self, Ctx, LeaseOptions, Require};
use crate::source::Mode;
use crate::state::default_state_dir;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod server;

/// Newest first; the first is offered when the client asks for one not listed.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const OUTPUT_LIMIT: usize = 64 * 1024;
const DEFAULT_EXEC_TIMEOUT: u64 = 600;

/// What the server tells a client about using its tools, once, at initialization.
const INSTRUCTIONS: &str = "Pass `dir`, the absolute path of the checkout you work in, to every tool (it may be omitted only when this client exposes exactly one root). Use stack_up before work that needs services, stack_run for the project's tasks (such as tests) and stack_exec for other commands (unverified service endpoints are withheld), stack_restart after editing code a running service loaded, stack_status to diagnose, and stack_down when finished. Nothing reloads services for you: a service's `watch` list only makes stack_status, stack_exec and stack_run report changed files, and stack_up leaves a running service with unchanged configuration as it is, so call stack_restart after such edits (changed: true restarts just the services whose watched files changed). Tools in this stack may ship agent skills; `stack_inspect` lists them under `skills` and `stack_skill` returns one. The resource stack://guide is the full agent guide.";

fn startup_timeout_schema() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Seconds the whole startup may take (default 600); or give `timeout`" })
}

/// `timeout`: the same bound as `timeout_secs`, as a duration string like the CLI's `--timeout`.
fn duration_schema(what: &str) -> Value {
    json!({ "type": "string", "description": format!("{what}, as a duration such as \"90s\", \"5m\" or \"1h\" (default 10m). Same as timeout_secs; give one or neither") })
}

/// Serve the tools over stdio until the client closes stdin. Requests run concurrently (see
/// [`server`]); every response and notification is written as one line.
pub fn serve() -> std::io::Result<()> {
    server::serve(std::io::BufReader::new(std::io::stdin()), std::io::stdout())
}

fn send(out: &mut impl Write, v: Value) -> std::io::Result<()> {
    writeln!(out, "{v}")?;
    out.flush()
}

fn negotiate(requested: Option<&str>) -> &'static str {
    PROTOCOL_VERSIONS
        .iter()
        .find(|v| Some(**v) == requested)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0])
}

/// `dir` of a tool that acts on one checkout.
const DIR_DESCRIPTION: &str = "Absolute path of the checkout (the directory with stack.toml). Pass it on every call: it may be omitted only when the client exposes exactly one MCP root, which is then used. The server's own working directory is never used";

fn schema(props: Value, required: &[&str]) -> Value {
    let mut p = props;
    p["dir"] = json!({ "type": "string", "description": DIR_DESCRIPTION });
    json!({ "type": "object", "properties": p, "required": required, "additionalProperties": false })
}

/// The schema of a machine-wide tool, whose `dir` is optional and says only what `dir` does.
fn machine_wide_schema(props: Value, required: &[&str], dir: &str) -> Value {
    let mut schema = schema(props, required);
    schema["properties"]["dir"]["description"] = json!(dir);
    schema
}

/// Tools that act machine-wide: they need no checkout.
const MACHINE_WIDE: &[&str] = &["stack_gc", "stack_ps", "stack_mv", "stack_errors"];

/// `dir` of a machine-wide tool that only reports it.
const REPORTED_DIR: &str = "Optional absolute path of a checkout, reported back as `project`; this tool acts machine-wide either way";

/// The one `pattern` the tool schemas use: a fnox secret name.
const SECRET_NAME_PATTERN: &str = "^[A-Z_][A-Z0-9_]*$";

fn secret_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase() || c == '_') && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Arguments of one tool that cannot be given together, as on the command line.
const CONFLICTS: &[(&str, &str, &str)] = &[
    ("stack_compile", "update", "locked"),
    ("stack_exec", "require", "require_all"),
    ("stack_restart", "services", "changed"),
    ("stack_up", "timeout", "timeout_secs"),
    ("stack_up", "owner_pid", "owner_ancestor"),
    ("stack_restart", "timeout", "timeout_secs"),
    ("stack_exec", "timeout", "timeout_secs"),
    ("stack_run", "timeout", "timeout_secs"),
    ("stack_gc", "dry_run", "stop_orphans"),
    ("stack_up", "no_init", "reinit"),
];

/// Arguments of one tool that mean something only with another, as on the command line.
const REQUIRES: &[(&str, &str, &str)] = &[("stack_exec", "ttl", "up"), ("stack_run", "ttl", "up")];

/// Check a call's arguments against the tool's advertised `inputSchema` before anything else
/// happens: no project is opened, no provider asked, nothing written. Unknown names, wrong
/// types and out-of-range numbers are `usage` errors; an argument given as `null` counts as
/// omitted. Returns the arguments as an object (`{}` when the call gave none).
fn validate(name: &str, args: &Value) -> Result<serde_json::Map<String, Value>> {
    let tools = tools();
    let Some(tool) = tools.as_array().into_iter().flatten().find(|t| t["name"] == name) else {
        return Err(StackError::new("unknown_tool", format!("no tool named '{name}'")));
    };
    let schema = &tool["inputSchema"];
    let usage = |message: String| StackError::new("usage", format!("{name}: {message}"));
    let args = match args {
        Value::Null => serde_json::Map::new(),
        Value::Object(map) => map.clone(),
        other => return Err(usage(format!("arguments must be an object, not {other}"))),
    };
    let properties = schema["properties"].as_object().expect("tool schemas list their properties");
    for (key, value) in &args {
        let Some(property) = properties.get(key) else {
            let known: Vec<&str> = properties.keys().map(String::as_str).collect();
            let hint = match closest(key, &known) {
                _ if name == "stack_run" && key.starts_with("secret") => {
                    "stack_run grants exactly the secrets the task declares: declare them under [tasks.<name>] secrets = [...] in stack.toml, or use stack_exec with secrets".to_string()
                }
                Some(near) => format!("did you mean `{near}`? {name} accepts: {}", known.join(", ")),
                None => format!("{name} accepts: {}", known.join(", ")),
            };
            return Err(usage(format!("unknown argument `{key}`")).hint(hint));
        };
        // A malformed secret name is the same `invalid_secret` the CLI's `--secret` reports.
        if key == "secrets" {
            if let Some(names) = value.as_array().and_then(|items| items.iter().map(|v| v.as_str().map(String::from)).collect::<Option<Vec<_>>>()) {
                crate::secrets::malformed(&names)?;
            }
        }
        if !value.is_null() {
            check_value(value, property).map_err(|why| {
                let e = usage(format!("`{key}` {why}"));
                match (key.as_str(), value) {
                    ("command", Value::String(s)) => e.hint(format!(
                        "command is an argv list run without a shell, such as [\"echo\", \"hi\"]; for shell syntax use [\"sh\", \"-c\", {}]",
                        Value::String(s.clone())
                    )),
                    _ => e,
                }
            })?;
        }
    }
    for key in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if args.get(key).map_or(true, Value::is_null) {
            return Err(usage(format!("`{key}` is required")));
        }
    }
    let given = |key: &str| match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Null) | None => false,
        Some(_) => true,
    };
    for (_, a, b) in CONFLICTS.iter().filter(|(tool, _, _)| *tool == name) {
        if given(a) && given(b) {
            return Err(usage(format!("`{a}` and `{b}` cannot be given together")));
        }
    }
    for (_, a, b) in REQUIRES.iter().filter(|(tool, _, _)| *tool == name) {
        if given(a) && !given(b) {
            return Err(usage(format!("`{a}` needs `{b}: true`")));
        }
    }
    Ok(args)
}

/// Why `value` does not fit `property` (one schema of a tool's `properties`), if it does not.
fn check_value(value: &Value, property: &Value) -> std::result::Result<(), String> {
    let kind = property["type"].as_str().unwrap_or_default();
    let shown = |v: &Value| match v {
        Value::String(_) => "a string".to_string(),
        Value::Array(_) => "an array".to_string(),
        Value::Object(_) => "an object".to_string(),
        other => other.to_string(),
    };
    match kind {
        "string" => {
            let s = value.as_str().ok_or_else(|| format!("must be a string, not {}", shown(value)))?;
            if property["pattern"].as_str() == Some(SECRET_NAME_PATTERN) && !secret_name(s) {
                return Err(format!("{s:?} is not a secret name (uppercase letters, digits and _, not starting with a digit)"));
            }
            Ok(())
        }
        "boolean" => value.as_bool().map(drop).ok_or_else(|| format!("must be true or false, not {}", shown(value))),
        "integer" => {
            // Beyond i64 (only u64 is) counts as past any maximum.
            let Some(n) = value.as_i64().or_else(|| value.as_u64().map(|_| i64::MAX)) else {
                return Err(format!("must be a whole number, not {}", shown(value)));
            };
            let (min, max) = (property["minimum"].as_i64(), property["maximum"].as_i64());
            if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                let range = match (min, max) {
                    (Some(a), Some(b)) => format!("from {a} to {b}"),
                    (Some(a), None) => format!("at least {a}"),
                    (None, Some(b)) => format!("at most {b}"),
                    (None, None) => unreachable!("only a bound can be out of range"),
                };
                return Err(format!("must be {range}, not {value}"));
            }
            Ok(())
        }
        "array" => {
            let items = value.as_array().ok_or_else(|| format!("must be an array, not {}", shown(value)))?;
            if property["minItems"].as_u64().is_some_and(|m| (items.len() as u64) < m) {
                return Err("must not be empty".into());
            }
            for item in items {
                check_value(item, &property["items"]).map_err(|why| format!("item {item}: {why}"))?;
            }
            Ok(())
        }
        other => unreachable!("tool schemas use no {other:?} properties"),
    }
}

/// The accepted name `given` most likely meant: one it starts or ends, or one within two edits.
fn closest<'a>(given: &str, known: &[&'a str]) -> Option<&'a str> {
    let given = given.to_ascii_lowercase();
    let distance = |a: &str, b: &str| {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut prev = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let next = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
                prev = row[j + 1];
                row[j + 1] = next;
            }
        }
        row[b.len()]
    };
    known
        .iter()
        .map(|k| (if k.starts_with(given.as_str()) || given.starts_with(k) { 0 } else { distance(&given, k) }, *k))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

/// What every tool's `structuredContent` is: the `--json` envelope. `data` is the operation's
/// result (see each command's documentation); `error` is set instead when `ok` is false.
fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean", "description": "Whether Stack did what was asked (for stack_exec and stack_run, check data.exit_code too; for stack_status, data.healthy)" },
            "passed": { "type": "boolean", "description": "Whether what was asked for is in the wanted state: false when ok is false, when stack_exec or stack_run exited non-zero or by a signal, and when stack_status finds a service unhealthy" },
            "command": { "type": "string", "description": "The operation that answered: the tool's name without stack_" },
            "stack_version": { "type": "string", "description": "The Stack release that answered" },
            "schema": { "type": "integer", "description": "Version of the envelope and result shapes; raised only on an incompatible change" },
            "duration_ms": { "type": "integer", "description": "How long the call took, in milliseconds" },
            "data": { "description": "The operation's result, when ok is true" },
            "error": {
                "type": "object",
                "description": "Why Stack could not do it, when ok is false",
                "properties": {
                    "code": { "type": "string", "description": "Stable error code, such as usage, service_unavailable or timed_out" },
                    "message": { "type": "string" },
                    "hint": { "type": "string", "description": "What to do next: the site's own hint, or the code's default from stack_errors" },
                    "retryable": { "type": "boolean", "description": "Whether repeating the same call unchanged can succeed" },
                    "next": { "type": "array", "description": "Where the fix is unambiguous, the operations to run, in order: {command (CLI argv), tool (MCP tool name, or null), args (its arguments)}" },
                    "details": { "type": "array", "description": "Specifics: affected services, captured output, progress records" },
                },
                "required": ["code", "message"],
            },
        },
        "required": ["ok", "passed", "command", "stack_version", "schema", "duration_ms"],
    })
}

/// The tools, each with its `inputSchema` and the [`output_schema`] its results follow.
fn tools() -> Value {
    let mut tools = tool_definitions();
    for tool in tools.as_array_mut().into_iter().flatten() {
        tool["outputSchema"] = output_schema();
    }
    tools
}

/// A tool's `title` and behaviour hints, as MCP clients show and use them. `destructive` only
/// means something for a tool that is not read-only.
fn annotations(title: &str, read_only: bool, destructive: Option<bool>, idempotent: bool, open_world: bool) -> Value {
    let mut a = json!({ "title": title, "readOnlyHint": read_only, "idempotentHint": idempotent, "openWorldHint": open_world });
    if let Some(destructive) = destructive {
        a["destructiveHint"] = json!(destructive);
    }
    a
}

/// One tool: its name, title (also in its annotations), description and input schema.
fn tool(name: &str, title: &str, annotations: Value, description: &str, input: Value) -> Value {
    json!({ "name": name, "title": title, "description": description, "annotations": annotations, "inputSchema": input })
}

fn tool_definitions() -> Value {
    json!([
        tool("stack_inspect", "Inspect the stack", annotations("Inspect the stack", true, None, true, false),
            "Show the composed stack without changing anything: bundles, tools, env, services, tasks, ports and where each value came from. Use it to learn what a checkout provides. Returns the report, including `skills`: the agent skills of the releases stack.lock pins, each available, no_skill, not_installed or unavailable (read one with stack_skill).",
            schema(json!({ "all_skills": { "type": "boolean", "description": "Also list the skills of tools Stack adds for its own supervisor, under provider_skills. For people; an agent should not drive the supervisor" } }), &[])),
        tool("stack_skill", "Read a tool's skill", annotations("Read a tool's skill", true, None, true, false),
            "Return the SKILL.md text of a pinned tool's agent skill (CLI: stack skill), one that stack_inspect lists under `skills` as available. Use it to learn how to use that tool at the pinned release; read it, do not run it. Returns {tool, version, name, entrypoint, bytes, text}, text at most 64 KiB. Fails with skill_not_found, or usage listing the names when the tool ships several and name is omitted.",
            schema(json!({ "tool": { "type": "string", "description": "Tool name as listed under `skills`, e.g. \"fnox\"" }, "name": { "type": "string", "description": "Skill name, needed only when the tool ships several" } }), &["tool"])),
        tool("stack_init", "Write a starting stack.toml", annotations("Write a starting stack.toml", false, Some(false), false, false),
            "Write a starting stack.toml in dir with the tools and preset services named (CLI: stack init). Use it when a checkout has none (manifest_missing), then call stack_compile. It never replaces an existing file (project_exists). Returns {path, tools, services, next}.",
            schema(json!({
                "tools": { "type": "array", "items": { "type": "string" }, "description": "Tools as NAME or NAME=VERSION, e.g. \"python=3.13\" (no version: latest, pinned at the first compile)" },
                "services": { "type": "array", "items": { "type": "string" }, "description": "Preset services as NAME, NAME=PRESET, either with @VERSION, e.g. \"postgres@17\" or \"cache=redis\"" }
            }), &[])),
        tool("stack_compile", "Compile stack.lock", annotations("Compile stack.lock", false, Some(false), true, true),
            "Resolve bundles and exact tool and service versions, then write stack.lock and the generated provider config. Use it after editing stack.toml, or when another tool fails with lock_outdated. Pins are kept unless their request changed; update re-resolves everything; locked only checks; reassign_ports gives fresh ports after port_conflict. Returns versions, ports and warnings.",
            schema(json!({
                "update": { "type": "boolean", "description": "Re-resolve every request, moving pins to the newest matching releases" },
                "locked": { "type": "boolean", "description": "Change nothing; fail with lock_outdated if stack.lock is missing or stale" },
                "reassign_ports": { "type": "boolean", "description": "Give this checkout new ports (after port_conflict)" }
            }), &[])),
        tool("stack_up", "Start services", annotations("Start services", false, Some(false), true, true),
            "Start every service and verify each one, recording a session. Use it before work that needs services. Safe to repeat: a service already running with unchanged configuration is kept, not restarted (after editing code it loaded, use stack_restart). It uses stack.lock as it is; a stale lock fails with lock_outdated. On timed_out, what was launched stays recorded. Returns {session, checks, steps}.",
            schema(json!({
                "ttl": { "type": "string", "description": "Lease: reclaim the session after this long without use, e.g. \"30m\" (stack_status, stack_exec, stack_run and stack_renew extend it)" },
                "owner_pid": { "type": "integer", "minimum": 1, "maximum": session::MAX_OWNER_PID, "description": "Lease: reclaim the session when this long-lived process exits" },
                "owner_ancestor": { "type": "string", "description": "Lease: tie the session to the nearest ancestor of this stack server with this command name, e.g. \"claude\" (CLI: --owner-pid ancestor:<name>); owner_not_found lists the ancestors when none matches" },
                "reassign_ports": { "type": "boolean", "description": "On port_conflict with another program, give this checkout's conflicting ports new numbers and continue; data.reassigned_ports lists old and new. A pinned port is never moved" },
                "timeout_secs": startup_timeout_schema(),
                "timeout": duration_schema("Time the whole startup may take"),
                "no_init": { "type": "boolean", "description": "Run no init task (a service's init = [...], run once per data directory after services verify), even pending ones" },
                "reinit": { "type": "array", "items": { "type": "string" }, "description": "Services whose init tasks run again from the first, whatever was recorded" },
                "from_snapshot": { "type": "string", "description": "Before the first start, fill each data directory that does not exist yet from this snapshot (stack_snapshot_save); its init counts as done" }
            }), &[])),
        tool("stack_snapshot_save", "Save a data snapshot", annotations("Save a data snapshot", false, Some(false), false, false),
            "Save this checkout's postgres and redis data as a named snapshot that every checkout (worktree) of the project can restore. Use it after migrating and seeding, so new worktrees can start from it. Running services are stopped for the copy, then started and verified again. Returns {name, services, restarted, steps}.",
            schema(json!({
                "name": { "type": "string", "description": "Snapshot name: letters, digits, '.', '_' or '-'" },
                "services": { "type": "array", "items": { "type": "string" }, "description": "Only these services (default: every postgres and redis service)" },
                "force": { "type": "boolean", "description": "Replace a snapshot of the same name" }
            }), &["name"])),
        tool("stack_snapshot_restore", "Restore a data snapshot", annotations("Restore a data snapshot", false, Some(true), true, false),
            "Replace this checkout's service data with a snapshot of the same service kind and major version. Use it to reset a database to a saved state; it discards the current data. Running services are stopped, the data swapped in one step, then started and verified again. Init counts as done for the init tasks the snapshot satisfied.",
            schema(json!({ "name": { "type": "string", "description": "Snapshot name, as stack_snapshot_list shows it" } }), &["name"])),
        tool("stack_snapshot_list", "List data snapshots", annotations("List data snapshots", true, None, true, false),
            "List this project's snapshots. Use it to find a name for stack_snapshot_restore or stack_up's from_snapshot. Returns each snapshot's services with their kind, version and init state when saved, and the checkout it came from.",
            schema(json!({}), &[])),
        tool("stack_snapshot_rm", "Remove a data snapshot", annotations("Remove a data snapshot", false, Some(true), true, false),
            "Remove one of this project's snapshots. Use it to free disk once a snapshot is no longer needed; every checkout of the project loses it. Fails with snapshot_not_found for an unknown name.",
            schema(json!({ "name": { "type": "string", "description": "Snapshot name" } }), &["name"])),
        tool("stack_install", "Install pinned tools", annotations("Install pinned tools", false, Some(false), true, true),
            "Install the locked tools and service binaries without starting anything or recording a session. Use it to warm a checkout: stack_exec refuses to run while a pin is missing (tools_not_installed) unless given install. A missing or stale stack.lock fails with lock_outdated. Returns {versions, ports, steps}.",
            schema(json!({}), &[])),
        tool("stack_restart", "Restart services", annotations("Restart services", false, Some(false), false, false),
            "Restart services of the running session (all when services is omitted) and verify the whole stack again. Use it after editing code a running service loaded, files under its `watch` included: watching only reports changes, it never reloads. changed: true restarts just the services whose watched files changed. Fails with session_stale after a configuration change (use stack_up).",
            schema(json!({
                "services": { "type": "array", "items": { "type": "string" }, "description": "Service names to restart; omitted or empty restarts every service. The others keep running and the session keeps its id" },
                "changed": { "type": "boolean", "description": "Restart only the services whose watched files changed since they started (changed_since_start), and nothing when none did" },
                "timeout_secs": startup_timeout_schema(),
                "timeout": duration_schema("Time the whole restart may take")
            }), &[])),
        tool("stack_logs", "Show service logs", annotations("Show service logs", true, None, true, false),
            "Return the last lines services wrote, as the supervisor kept them; it never follows. Use it when a check fails or a service misbehaves. since_start, since and grep narrow the lines. With service, returns {service, lines, truncated, started_at, since_start}; without, every service's: {services: {<name>: {lines, ...}}, errors}, errors naming services whose output could not be read.",
            schema(json!({
                "service": { "type": "string", "description": "Service name from the stack; omitted means every service" },
                "tail": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Lines from the end (default 100); with grep, matching lines" },
                "since_start": { "type": "boolean", "description": "Only the current process's output, not earlier runs'" },
                "since": { "type": "string", "description": "Only output written in this last span, e.g. \"10m\"" },
                "grep": { "type": "string", "description": "Only lines containing this text (a plain substring, not a regex)" },
                "ignore_case": { "type": "boolean", "description": "Match grep ignoring case" }
            }), &[])),
        tool("stack_env", "Show the command environment", annotations("Show the command environment", true, None, true, false),
            "Return the environment stack_exec would give a command now. Use it to see which variables a command gets, or to hand them to a tool Stack cannot wrap; prefer stack_exec, which verifies on every call. Returns {env, unset, withheld, skipped}: withheld lists each unverified service and the variables removed for it; MISE_* variables are skipped.",
            schema(json!({}), &[])),
        tool("stack_status", "Check services", annotations("Check services", true, None, true, false),
            "Verify every service now, renew an unexpired lease, and report session and lease state. Start here to diagnose. A service that is down still gives ok: true, with data.healthy false and data.checks saying why. changed_since_start lists files under a service's `watch` paths edited after it started; nothing reloads them, so call stack_restart for that service.",
            schema(json!({}), &[])),
        tool("stack_exec", "Run a command", annotations("Run a command", false, None, false, true),
            "Run one command, an argv list without a shell, with the stack's tools and env, capturing its output. Use it for anything that is not a stack.toml task; for shell syntax pass [\"sh\", \"-c\", \"...\"]. Endpoints of unverified services are withheld; require or require_all refuse to run instead. Returns {exit_code, stdout, stderr, checks}: it worked when ok and exit_code is 0.",
            schema(json!({
                "command": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "argv, e.g. [\"pytest\", \"-q\"]; nothing is expanded. Use [\"sh\", \"-c\", \"...\"] for $VAR, pipes and globs" },
                "require": { "type": "array", "items": { "type": "string" }, "description": "Services that must verify, or the command does not run (service_unavailable)" },
                "require_all": { "type": "boolean", "description": "Every service must verify, or the command does not run" },
                "secrets": { "type": "array", "items": { "type": "string", "pattern": SECRET_NAME_PATTERN }, "description": "fnox secret names to grant this command only, resolved through the stack's pinned fnox. No granted value appears literally in the parsed stdout or stderr strings (timed_out details included): it is replaced by [redacted:KEY], or [redacted] where naming the key could spell out a value. Key names are not secret and are listed under secrets. Values shorter than 8 bytes, or ones a marker could spell out, are refused (secret_unsupported). Values the command transforms (encoded, split, or matching only after JSON escaping) are not caught. A grant is injection and redaction, not access control: the command can run fnox itself to read ungranted secrets, which are not redacted" },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Seconds the whole call may take, counted from the call with planning included, before the command is killed and the call fails with timed_out, its output so far in the error details (default 600). A call whose planning is cut short fails with timed_out before anything runs; or give `timeout`" },
                "timeout": duration_schema("Time the whole call may take, planning included, before the command is killed"),
                "up": { "type": "boolean", "description": "Run stack_up first when there is no session, or it is stale, expired or a service does not verify; data.prepared says what ran. up gets what is left of the timeout, at most 10 minutes" },
                "ttl": { "type": "string", "description": "With up: the lease of a session up records, e.g. \"30m\" (default: keep the current lease)" },
                "install": { "type": "boolean", "description": "Install pinned tools that are missing (as stack_install would) instead of failing with tools_not_installed; data.prepared lists them" }
            }), &["command"])),
        tool("stack_run", "Run a task", annotations("Run a task", false, None, false, true),
            "Run a task declared in stack.toml ([tasks.<name>]) through mise once every service verifies, capturing output like stack_exec. Use it for the project's own tasks, such as tests; use stack_exec for commands that should run with services down. The task gets exactly the secrets it declares, redacted from its output. Returns {exit_code, stdout, stderr, checks}: it worked when ok and exit_code is 0.",
            schema(json!({
                "task": { "type": "string", "description": "Task name from [tasks.<name>]; unknown_task lists them" },
                "args": { "type": "array", "items": { "type": "string" }, "description": "Appended to the task's command" },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Seconds the whole call may take, planning and installing missing pins included, before the task is killed and the call fails with timed_out (default 600); or give `timeout`" },
                "timeout": duration_schema("Time the whole call may take, planning included, before the task is killed"),
                "up": { "type": "boolean", "description": "Run stack_up first when there is no session, or it is stale, expired or a service does not verify; data.prepared says what ran. up gets what is left of the timeout, at most 10 minutes" },
                "ttl": { "type": "string", "description": "With up: the lease of a session up records, e.g. \"30m\" (default: keep the current lease)" }
            }), &["task"])),
        tool("stack_renew", "Renew the lease", annotations("Renew the lease", false, Some(false), true, false),
            "Renew this checkout's session lease so stack_gc does not reclaim it. Use it during long work under a ttl lease with no other stack calls (stack_exec and stack_run also renew). Returns the session.",
            schema(json!({
                "ttl": { "type": "string", "description": "Set the lease's TTL, e.g. \"30m\"; gives a session without a lease one" }
            }), &[])),
        tool("stack_down", "Stop services", annotations("Stop services", false, Some(true), true, false),
            "Stop every service of this checkout; succeeds only once their processes are confirmed gone. Use it before you finish or remove the checkout. Then stops the supervisor that ran them, if Stack started it and nothing else uses it (data.supervisor says what happened). Service data is kept unless purge is set. Returns {stopped, confirmed, supervisor}.",
            schema(json!({
                "stop_supervisor": { "type": "boolean", "description": "Stop the supervisor when Stack started it and nothing else uses it (default true); false keeps it running" },
                "purge": { "type": "boolean", "description": "Once every service is confirmed stopped, also remove the service data stack recorded for this checkout (only where gc --reclaim-data would: the recorded directory, no link followed, its marker this checkout's, its service confirmed stopped) and release its ports and identity tokens. data.purge says what was removed and what was kept, and why; complete is false when anything was kept" }
            }), &[])),
        tool("stack_gc", "Reclaim stale sessions", annotations("Reclaim stale sessions", false, Some(true), true, false),
            "Reclaim, machine-wide, sessions whose lease expired and services of deleted checkouts. Use it for cleanup, not routine work: it stops other checkouts' expired sessions too. A deleted checkout with no record left gets its ports released and its data reported; reclaim_data removes that data. dry_run only reports. Fails with gc_incomplete if a stop could not be confirmed (records are then kept).",
            machine_wide_schema(json!({
                "reclaim_data": { "type": "boolean", "description": "Also remove the service data of deleted checkouts (default false: only reported)" },
                "stop_orphans": { "type": "boolean", "description": "Also stop a deleted checkout's service where gc would offer recovery.stop, only when its supervisor id, pid, port and process start time all still match the record (Pitchfork cannot compare and stop in one step: a daemon restarted under the same id at that moment would be stopped)" },
                "dry_run": { "type": "boolean", "description": "Change nothing and take no lock: report what would be reclaimed (would_reclaim: session, project, reason, action, services) and what deleted checkouts would release (deleted_checkouts: ports, and data with action remove_if_safe under reclaim_data, else report)" }
            }), &[], REPORTED_DIR)),
        tool("stack_ps", "List checkouts", annotations("List checkouts", true, None, true, false),
            "List every checkout Stack has records for on this machine, with its session, lease and services (port, pid, liveness, data size) and totals. Use it to see what runs where, or to find leftovers of deleted checkouts (reclaimable marks what stack_gc would reclaim). Liveness is not identity verification; use stack_status for that. Read-only; takes no lock and needs no dir.",
            machine_wide_schema(json!({ "no_size": { "type": "boolean", "description": "Skip measuring data directories, which can be slow" } }), &[], REPORTED_DIR)),
        tool("stack_mv", "Move a checkout's records", annotations("Move a checkout's records", false, Some(false), false, false),
            "Carry a checkout's session record, port reservations, identity tokens and recorded service data from its old path to its new one. Use it after a checkout was moved or renamed on disk, so it keeps its ports and data. Refuses, changing nothing, while anything recorded for the old path may run (mv_services_running), when the new path has a session, or when data cannot be moved.",
            machine_wide_schema(json!({
                "from": { "type": "string", "description": "Where the checkout was: an absolute path, or relative to `dir`" },
                "to": { "type": "string", "description": "Where it is now: an absolute path, or relative to `dir`" }
            }), &["from", "to"], "Optional absolute path that relative `from` and `to` resolve against")),
        tool("stack_doctor", "Check prerequisites", annotations("Check prerequisites", true, None, true, false),
            "Check that the providers Stack needs (mise, git, tar) are installed, that the supervisor's socket path fits this platform, and that the project compiles. Use it when setup-level errors appear, such as a missing provider or socket_path_too_long. Returns a list of {name, ok, detail} checks; doctor_failed lists them all when one fails.",
            schema(json!({}), &[])),
        tool("stack_errors", "List error codes", annotations("List error codes", true, None, true, false),
            "List every error code Stack can return. Use it to learn what a code means and what fixes it without the docs. Returns each code's meaning, default hint, whether retrying the same call unchanged can succeed (retryable), and the operations that fix it (next).",
            machine_wide_schema(json!({}), &[], REPORTED_DIR)),
    ])
}

/// Run one `tools/call`. `roots` are the client's MCP roots: `None` when it offers none.
fn call(params: &Value, roots: Option<&[PathBuf]>) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let mut call = crate::contract::Call::new(name.strip_prefix("stack_").unwrap_or(name));
    let mut resolved = None;
    let outcome = validate(name, &params["arguments"])
        .and_then(|args| {
            let args = Value::Object(args);
            resolved = project(name, &args, roots)?;
            dispatch(name, &args, &ctx(resolved.as_deref()))
        })
        .map_err(|e| with_usage_hint(e, "see this tool's inputSchema in tools/list"));
    call.dir = resolved.clone();
    let mut envelope = call.outcome(&outcome);
    // Which checkout the call acted on, so a wrong one shows in every result.
    if let Some(project) = &resolved {
        envelope["project"] = json!(project);
    }
    let is_error = envelope["ok"] == false;
    json!({
        "content": [{ "type": "text", "text": envelope.to_string() }],
        "structuredContent": envelope,
        "isError": is_error,
    })
}

/// The checkout a call acts on: its `dir`, which must be absolute, else the client's only root.
/// Never the server's working directory: one server is shared by agents in many checkouts, so
/// that directory says nothing about which one a call means. `None` for a machine-wide tool
/// given neither.
fn project(name: &str, args: &Value, roots: Option<&[PathBuf]>) -> Result<Option<PathBuf>> {
    let dir = match args["dir"].as_str() {
        Some(dir) => {
            let dir = Path::new(dir);
            if !dir.is_absolute() {
                return Err(StackError::new("usage", format!("{name}: `dir` must be an absolute path, not {:?}", dir.display().to_string()))
                    .hint("pass `dir` as the absolute path of your checkout, e.g. the output of `pwd` there"));
            }
            dir.to_path_buf()
        }
        None => match roots {
            Some([only]) => only.clone(),
            _ if MACHINE_WIDE.contains(&name) => return Ok(None),
            Some(several) if several.len() > 1 => {
                let shown: Vec<String> = several.iter().map(|r| r.display().to_string()).collect();
                return Err(StackError::new("usage", format!("{name}: `dir` is required: the client exposes {} roots, so none can be chosen", several.len()))
                    .hint("pass `dir` as the absolute path of the checkout to act on")
                    .with_detail(json!({ "roots": shown })));
            }
            _ => {
                return Err(StackError::new("usage", format!("{name}: `dir` is required"))
                    .hint("pass `dir` as the absolute path of your checkout; this server is shared and never acts on its own working directory"))
            }
        },
    };
    let root = dir
        .canonicalize()
        .map_err(|e| StackError::new("dir_not_found", format!("{}: {e}", dir.display())).hint("pass `dir` as the absolute path of an existing checkout"))?;
    Ok(Some(root))
}

/// Whether a call needs the client's roots to find its checkout: it names no `dir` and acts
/// on one checkout. Arguments that fail validation need nothing; the call reports them.
fn needs_roots(params: &Value) -> bool {
    let name = params["name"].as_str().unwrap_or_default();
    validate(name, &params["arguments"]).is_ok_and(|args| args.get("dir").map_or(true, Value::is_null) && !MACHINE_WIDE.contains(&name))
}

/// The checkout a call will act on, for running calls on one checkout one at a time; `None`
/// when it acts on none or will fail before acting.
fn project_key(params: &Value, roots: Option<&[PathBuf]>) -> Option<PathBuf> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = Value::Object(validate(name, &params["arguments"]).ok()?);
    project(name, &args, roots).ok().flatten()
}

fn ctx(project: Option<&Path>) -> Ctx {
    // A machine-wide tool given no checkout reads only the state directory.
    Ctx {
        root: project.map(Path::to_path_buf).unwrap_or_default(),
        cache: default_cache_dir(),
        state: default_state_dir(),
    }
}

fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).expect("report serializes")
}

fn dispatch(name: &str, args: &Value, ctx: &Ctx) -> Result<Value> {
    let compile = |mode, write, reassign_ports| {
        project::compile(&Options {
            root: ctx.root.clone(),
            mode,
            write,
            cache: ctx.cache.clone(),
            state: ctx.state.clone(),
            reassign_ports,
            resolver: None,
            locker: None,
        })
    };
    match name {
        "stack_inspect" => compile(project::inspect_mode(&ctx.root), false, false).map(|mut r| {
            r.attach_skills(&ctx.cache, args["all_skills"] == true);
            to_value(r)
        }),
        "stack_skill" => skill(args, ctx).map(to_value),
        "stack_init" => {
            let strings = |key: &str| -> Vec<String> { args[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect() };
            crate::init::init(&ctx.root, &strings("tools"), &strings("services")).map(to_value)
        }
        "stack_compile" => {
            let mode = if args["update"] == true {
                Mode::Update
            } else if args["locked"] == true {
                Mode::Frozen
            } else {
                Mode::UseLock
            };
            compile(mode, true, args["reassign_ports"] == true).map(|mut r| {
                r.attach_skills(&ctx.cache, false);
                to_value(r)
            })
        }
        "stack_up" => {
            let ttl_secs = args["ttl"].as_str().map(parse_duration).transpose()?;
            // Validated before any lifecycle work; null means omitted.
            let owner_pid = match &args["owner_pid"] {
                Value::Null => None,
                v => Some(
                    v.as_u64()
                        .ok_or_else(|| {
                            StackError::new("usage", format!("owner_pid {v} is not a process ID"))
                        })
                        .and_then(session::owner_pid)?,
                ),
            };
            let owner_pid = match args["owner_ancestor"].as_str() {
                Some(name) => Some(crate::lease::Owner::ancestor(name)?.resolve()?),
                None => owner_pid,
            };
            let timeout = startup_timeout(args)?;
            session::up_with(
                ctx,
                LeaseOptions {
                    ttl_secs,
                    owner_pid,
                },
                timeout,
                session::UpOptions {
                    reassign_ports: args["reassign_ports"] == true,
                    no_init: args["no_init"] == true,
                    reinit: strings(args, "reinit")?,
                    from_snapshot: args["from_snapshot"].as_str().map(String::from),
                },
            )
            .map(to_value)
        }
        "stack_install" => session::install(ctx).map(to_value),
        "stack_snapshot_save" => {
            let name = args["name"].as_str().unwrap_or_default();
            crate::snapshot::save(ctx, name, &strings(args, "services")?, args["force"] == true).map(to_value)
        }
        "stack_snapshot_restore" => crate::snapshot::restore(ctx, args["name"].as_str().unwrap_or_default()).map(to_value),
        "stack_snapshot_list" => crate::snapshot::list(ctx).map(to_value),
        "stack_snapshot_rm" => crate::snapshot::rm(ctx, args["name"].as_str().unwrap_or_default()),
        "stack_logs" => {
            let tail = match &args["tail"] {
                Value::Null => 100,
                v => v.as_u64().filter(|n| (1..=10_000).contains(n)).ok_or_else(|| StackError::new("usage", format!("tail {v} must be 1 to 10000")))? as usize,
            };
            let q = crate::logs::Query {
                services: args["service"].as_str().map(String::from).into_iter().collect(),
                tail,
                since_start: args["since_start"] == true,
                since_secs: args["since"].as_str().map(parse_duration).transpose()?,
                grep: args["grep"].as_str().map(String::from),
                ignore_case: args["ignore_case"] == true,
            };
            crate::logs::fetch(ctx, &q).map(to_value)
        }
        "stack_restart" => {
            // Omitted or empty means every service; anything malformed must not widen to that.
            let services: Vec<String> = match &args["services"] {
                Value::Null => Vec::new(),
                Value::Array(items) => items
                    .iter()
                    .map(|v| v.as_str().map(String::from).ok_or_else(|| StackError::new("usage", format!("service name {v} is not a string"))))
                    .collect::<Result<_>>()?,
                v => return Err(StackError::new("usage", format!("services must be an array of service names, not {v}"))),
            };
            let timeout = startup_timeout(args)?;
            if args["changed"] == true {
                session::restart_changed(ctx, timeout).map(to_value)
            } else {
                session::restart(ctx, &services, timeout).map(to_value)
            }
        }
        "stack_status" => session::status(ctx).map(to_value),
        "stack_env" => crate::shell::plan(ctx).map(to_value),
        "stack_renew" => {
            let ttl_secs = args["ttl"].as_str().map(parse_duration).transpose()?;
            session::renew_with(ctx, ttl_secs).map(to_value)
        }
        "stack_down" => session::down_purging(ctx, args["stop_supervisor"] != false, args["purge"] == true).map(to_value),
        "stack_gc" if args["dry_run"] == true => session::gc_plan(&ctx.state, args["reclaim_data"] == true).map(to_value),
        "stack_gc" => session::gc_checked(&ctx.state, args["reclaim_data"] == true, args["stop_orphans"] == true).map(to_value),
        "stack_ps" => crate::fleet::ps(&ctx.state, args["no_size"] != true).map(to_value),
        "stack_mv" => {
            // Never resolved against the server's working directory, like `dir`.
            let path = |key: &str| {
                args[key]
                    .as_str()
                    .map(Path::new)
                    .filter(|p| p.is_absolute() || !ctx.root.as_os_str().is_empty())
                    .ok_or_else(|| StackError::new("usage", format!("stack_mv: `{key}` must be an absolute path unless `dir` is given")))
            };
            crate::fleet::mv(&ctx.state, &ctx.root, path("from")?, path("to")?).map(to_value)
        }
        "stack_doctor" => crate::doctor::run(&ctx.root, &ctx.cache, &ctx.state).map(to_value),
        "stack_errors" => Ok(crate::error::registry_json()),
        "stack_exec" => exec(args, ctx),
        "stack_run" => run(args, ctx),
        _ => Err(StackError::new(
            "unknown_tool",
            format!("no tool named '{name}'"),
        )),
    }
}

/// One skill's text, from the same discovery `stack_inspect` runs, never a provider's.
fn skill(args: &Value, ctx: &Ctx) -> Result<crate::skills::SkillText> {
    let tool = args["tool"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| StackError::new("usage", "tool is required"))?;
    let name = match &args["name"] {
        Value::Null => None,
        v => Some(v.as_str().filter(|s| !s.is_empty()).ok_or_else(|| StackError::new("usage", "name must not be empty"))?),
    };
    crate::skills::pinned(&ctx.root, &ctx.cache, &ctx.state, tool, name)
}

/// `timeout_secs` (whole seconds, at least 1) or `timeout` (a duration string, at least 1s) of
/// `stack_up` and `stack_restart`. Omitted (or null) means the default. Checked before any
/// lifecycle work.
fn startup_timeout(args: &Value) -> Result<Duration> {
    timeout_arg(args, session::DEFAULT_STARTUP_TIMEOUT)
}

/// The call's bound from `timeout_secs` or `timeout`, which [`validate`] refuses together;
/// `default` when neither is given.
fn timeout_arg(args: &Value, default: Duration) -> Result<Duration> {
    match (&args["timeout_secs"], &args["timeout"]) {
        (Value::Null, Value::Null) => Ok(default),
        (Value::Null, Value::String(text)) => match parse_duration(text)? {
            0 => Err(StackError::new("usage", format!("timeout {text:?} must be at least 1s"))),
            secs => Ok(Duration::from_secs(secs)),
        },
        (Value::Null, v) => Err(StackError::new("usage", format!("timeout {v} must be a duration string such as \"90s\" or \"5m\""))),
        (v, Value::Null) => v
            .as_u64()
            .filter(|secs| *secs >= 1)
            .map(Duration::from_secs)
            .ok_or_else(|| StackError::new("usage", format!("timeout_secs {v} must be a whole number of seconds, at least 1"))),
        _ => Err(StackError::new("usage", "`timeout` and `timeout_secs` cannot be given together")),
    }
}

fn exec(args: &Value, ctx: &Ctx) -> Result<Value> {
    let command = strings(args, "command")?;
    let secrets = strings(args, "secrets")?;
    let require = if args["require_all"] == true { Require::All } else { Require::Only(strings(args, "require")?) };
    let prepare = prepare(args, args["install"] == true)?;
    captured(args, ctx, || session::plan_prepared(ctx, prepare, || session::plan_exec_with(ctx, &command, &require, &Grant { keys: secrets, captured: true })))
}

/// `up`, `ttl` and (`stack_exec` only) `install`, checked before anything runs.
fn prepare(args: &Value, install: bool) -> Result<session::Prepare> {
    let ttl_secs = args["ttl"].as_str().map(parse_duration).transpose()?;
    Ok(session::Prepare { up: args["up"] == true, ttl_secs, install })
}

/// An array-of-strings argument: omitted (or `null`) is empty, and anything else is refused
/// rather than read as less than it says. [`validate`] has already checked it.
fn strings(args: &Value, key: &str) -> Result<Vec<String>> {
    match &args[key] {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(String::from).ok_or_else(|| StackError::new("usage", format!("`{key}` item {v} is not a string"))))
            .collect(),
        v => Err(StackError::new("usage", format!("`{key}` must be an array of strings, not {v}"))),
    }
}

fn run(args: &Value, ctx: &Ctx) -> Result<Value> {
    let task = args["task"].as_str().ok_or_else(|| StackError::new("usage", "task is required"))?;
    let extra = strings(args, "args")?;
    let prepare = prepare(args, false)?;
    captured(args, ctx, || session::plan_prepared(ctx, prepare, || session::plan_task(ctx, task, &extra, true)))
}

/// Plan and run within `timeout_secs` (or `timeout`), which counts from the call: planning (including a
/// task's install of missing pins) and the command share it.
fn captured(args: &Value, ctx: &Ctx, plan: impl FnOnce() -> Result<session::ExecPlan>) -> Result<Value> {
    let timeout = timeout_arg(args, Duration::from_secs(DEFAULT_EXEC_TIMEOUT))?;
    let (plan, _deadline) = session::plan_within(Some(timeout), "raise timeout_secs", plan)?;
    let mut plan = plan;
    let result = run_and_finish(ctx, &mut plan, timeout)?;
    if result["timed_out"] == true {
        return Err(timed_out(timeout, result, "raise timeout_secs"));
    }
    Ok(result)
}

/// [`run_captured`], then release `plan`'s execution, before the result is returned and the
/// next call can start the stack again: also when the command could not be run at all.
pub fn run_and_finish(ctx: &Ctx, plan: &mut session::ExecPlan, timeout: Duration) -> Result<Value> {
    let outcome = run_captured(ctx, plan, timeout);
    reported(outcome, plan.finish())
}

/// A captured run's outcome as it is reported, with a release that could not be recorded
/// added to it: a warning beside the command's result and any warnings it has, or a detail
/// of the error that kept it from running, whose code and message stay as they are.
fn reported(outcome: Result<Value>, released: Result<()>) -> Result<Value> {
    let Err(e) = released else { return outcome };
    let warning = json!(session::unreleased(&e));
    match outcome {
        Ok(mut result) => {
            match result["warnings"].as_array_mut() {
                Some(warnings) => warnings.push(warning),
                None => result["warnings"] = json!([warning]),
            }
            Ok(result)
        }
        Err(error) => Err(error.with_detail(json!({ "warnings": [warning] }))),
    }
}

/// A command stack killed at its deadline did not do its job: `timed_out`, with the captured
/// result (output so far, checks) as the error's only detail.
pub fn timed_out(timeout: Duration, result: Value, raise: &str) -> StackError {
    StackError::new("timed_out", format!("command did not finish within {}s and was killed", timeout.as_secs()))
        .hint(format!("{raise} if it needs longer; its output so far is in details"))
        .details(vec![result])
}

/// Run a planned command with bounded output capture; MCP and `stack exec --json` call it
/// through [`run_and_finish`], which also releases the execution.
pub fn run_captured(ctx: &Ctx, plan: &session::ExecPlan, timeout: Duration) -> Result<Value> {
    // Inherit the server's environment as raw bytes, like `stack exec`; `std::env::vars`
    // panics on values that are not Unicode.
    let mut command = Command::new(&plan.program);
    for var in &plan.removed {
        command.env_remove(var);
    }
    // Granted values are replaced as the output streams in, before it is bounded.
    let output = crate::process::capture_redacted(
        command
            .args(&plan.args)
            .envs(&plan.env)
            .current_dir(plan.dir.as_deref().unwrap_or(&ctx.root)),
        timeout,
        OUTPUT_LIMIT,
        Some(&plan.redactor),
    )
    .map_err(|e| {
        StackError::new(
            "exec_failed",
            format!("cannot execute {}: {e}", plan.program.display()),
        )
    })?;
    let mut result = json!({
        "exit_code": output.exit_code,
        "timed_out": output.timed_out,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
        "stdout_bytes": output.stdout_bytes,
        "stderr_bytes": output.stderr_bytes,
        "unverified": plan.checks.iter().filter(|c| !c.ready).map(|c| &c.service).collect::<Vec<_>>(),
        "checks": plan.checks,
    });
    // Only when `up` (or `install`) ran first, so other results keep their shape.
    if !plan.prepared.is_empty() {
        result["prepared"] = json!(plan.prepared);
    }
    // Only for a command a signal ended, so other results keep their shape.
    if let Some(signal) = output.signal {
        result["signal"] = json!(signal);
    }
    // Names only, and only for commands granted secrets, so other results keep their shape.
    if !plan.secrets.is_empty() {
        result["secrets"] = json!(plan.secrets);
        result["warnings"] = json!(plan.secret_warnings);
    }
    Ok(result)
}

/// `90`, `90s`, `30m`, `2h`, `1d`.
pub fn parse_duration(s: &str) -> Result<u64> {
    const HINT: &str = "give whole seconds, minutes, hours or days: 90, 90s, 30m, 2h or 1d";
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| StackError::new("usage", format!("invalid duration '{s}'")).hint(HINT))?;
    let mult = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => {
            return Err(StackError::new(
                "usage",
                format!("invalid duration unit in '{s}'; use s, m, h or d"),
            )
            .hint(HINT))
        }
    };
    n.checked_mul(mult)
        .ok_or_else(|| StackError::new("usage", "duration is too large").hint(HINT))
}

/// Every `usage` error says where to look, even one raised without a hint of its own.
pub fn with_usage_hint(e: StackError, help: &str) -> StackError {
    if e.code == "usage" && e.hint.is_none() {
        return e.hint(help.to_string());
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unrecorded_release_is_added_to_what_the_run_reports() {
        let unrecorded = || Err(StackError::new("io", "no record"));
        assert_eq!(reported(Ok(json!({ "exit_code": 0 })), Ok(())).unwrap(), json!({ "exit_code": 0 }));
        let result = reported(Ok(json!({ "exit_code": 3, "warnings": ["a secret was not granted"] })), unrecorded()).unwrap();
        let warnings = result["warnings"].as_array().unwrap();
        assert_eq!(warnings[0], "a secret was not granted", "the command's own warnings are kept");
        assert!(warnings[1].as_str().unwrap().contains("still recorded as running"), "{result}");
        assert_eq!(result["exit_code"], 3);
        let error = reported(Err(StackError::new("exec_failed", "cannot execute x").with_detail(json!({ "kept": true }))), unrecorded()).unwrap_err();
        assert_eq!((error.code, error.message.as_str(), error.hint.as_deref()), ("exec_failed", "cannot execute x", None));
        assert_eq!(error.details[0], json!({ "kept": true }));
        assert!(error.details[1]["warnings"][0].as_str().unwrap().contains("stack down"), "{:?}", error.details);
    }

    #[test]
    fn startup_timeouts_are_whole_seconds_of_at_least_one() {
        assert_eq!(startup_timeout(&json!({})).unwrap(), session::DEFAULT_STARTUP_TIMEOUT);
        assert_eq!(startup_timeout(&json!({ "timeout_secs": null })).unwrap(), session::DEFAULT_STARTUP_TIMEOUT);
        assert_eq!(startup_timeout(&json!({ "timeout_secs": 1 })).unwrap(), Duration::from_secs(1));
        assert_eq!(startup_timeout(&json!({ "timeout_secs": 86400 })).unwrap(), Duration::from_secs(86400));
        for bad in [json!(0), json!(-1), json!(1.5), json!("30"), json!(true), json!([5])] {
            let e = startup_timeout(&json!({ "timeout_secs": bad })).unwrap_err();
            assert_eq!(e.code, "usage", "{bad}");
        }
    }

    /// Whether `value` fits `schema`, for the keywords the output schema uses.
    fn fits(value: &Value, schema: &Value) -> bool {
        let typed = match schema["type"].as_str() {
            Some("object") => value.is_object(),
            Some("boolean") => value.is_boolean(),
            Some("string") => value.is_string(),
            Some("array") => value.is_array(),
            Some("integer") => value.is_u64() || value.is_i64(),
            Some(_) => false,
            None => true,
        };
        let required = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).all(|k| value.get(k).is_some());
        let properties = schema["properties"].as_object().into_iter().flatten().all(|(k, s)| value.get(k).map_or(true, |v| fits(v, s)));
        typed && required && properties
    }

    #[test]
    fn every_tool_advertises_the_envelope_its_results_follow() {
        for tool in tools().as_array().unwrap() {
            assert_eq!(tool["outputSchema"], output_schema(), "{}", tool["name"]);
        }
        // An error result and a success result both fit it.
        let failed = call(&json!({ "name": "stack_nope", "arguments": {} }), None);
        assert_eq!(failed["isError"], true);
        assert!(fits(&failed["structuredContent"], &output_schema()), "{failed}");
        let usage = call(&json!({ "name": "stack_exec", "arguments": { "command": "echo hi" } }), None);
        assert!(fits(&usage["structuredContent"], &output_schema()) && usage["structuredContent"]["error"]["hint"].is_string(), "{usage}");
        // Read-only: inspect writes nothing and asks no provider for an `[env]`-only project.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stack.toml"), "[env]\nA = 'b'\n").unwrap();
        let fine = call(&json!({ "name": "stack_inspect", "arguments": { "dir": dir.path() } }), None);
        assert_eq!(fine["structuredContent"]["ok"], true, "{fine}");
        assert!(fits(&fine["structuredContent"], &output_schema()), "{fine}");
        assert!(!dir.path().join(".stack").exists() && !dir.path().join("stack.lock").exists());
        assert!(!fits(&json!({ "data": 1 }), &output_schema()) && !fits(&json!({ "ok": false, "error": { "code": 1 } }), &output_schema()));
    }

    #[test]
    fn every_tool_schema_is_closed_and_uses_only_what_validation_checks() {
        fn walk(property: &Value) {
            match property["type"].as_str() {
                Some("string") => assert!(property["pattern"].is_null() || property["pattern"] == SECRET_NAME_PATTERN, "{property}"),
                Some("boolean" | "integer") => {}
                Some("array") => walk(&property["items"]),
                other => panic!("unchecked property type {other:?}: {property}"),
            }
        }
        for tool in tools().as_array().unwrap() {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["additionalProperties"], false, "{tool}");
            schema["properties"].as_object().unwrap().values().for_each(walk);
        }
    }

    #[test]
    fn arguments_are_checked_before_a_project_is_opened() {
        for args in [json!(["command"]), json!("echo hi"), json!(5)] {
            let e = validate("stack_exec", &args).unwrap_err();
            assert_eq!(e.code, "usage", "{args}");
            assert!(e.message.contains("arguments must be an object"), "{}", e.message);
        }
        assert!(validate("stack_status", &Value::Null).unwrap().is_empty(), "no arguments is none");
        let e = validate("stack_status", &json!({ "dir": 5 })).unwrap_err();
        assert!(e.message.contains("`dir` must be a string"), "{}", e.message);
        // An unknown tool is named as such, whatever its arguments, before any directory is read.
        let called = call(&json!({ "name": "stack_nope", "arguments": { "dir": "/does/not/exist" } }), None);
        assert_eq!(called["structuredContent"]["error"]["code"], "unknown_tool");
        let called = call(&json!({ "name": "stack_status", "arguments": { "dir": "/does/not/exist", "extra": 1 } }), None);
        assert_eq!(called["structuredContent"]["error"]["code"], "usage");
    }

    #[test]
    fn misspelled_arguments_suggest_the_accepted_name() {
        let known = ["command", "require", "require_all", "secrets", "timeout_secs", "dir"];
        assert_eq!(closest("secret", &known), Some("secrets"));
        assert_eq!(closest("timeout", &known), Some("timeout_secs"));
        assert_eq!(closest("Command", &known), Some("command"));
        assert_eq!(closest("requir_all", &known), Some("require_all"));
        assert_eq!(closest("environment", &known), None);
    }

    #[test]
    fn the_project_is_an_absolute_dir_or_the_only_root_never_the_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(project("stack_status", &json!({ "dir": dir.path() }), None).unwrap(), Some(root.clone()));
        let only = [dir.path().to_path_buf()];
        assert_eq!(project("stack_status", &json!({}), Some(&only)).unwrap(), Some(root.clone()));
        // An explicit dir wins over roots.
        assert_eq!(project("stack_status", &json!({ "dir": dir.path() }), Some(&[PathBuf::from("/a"), PathBuf::from("/b")])).unwrap(), Some(root.clone()));
        for roots in [None, Some(&[][..]), Some(&[PathBuf::from("/a"), PathBuf::from("/b")][..])] {
            let e = project("stack_down", &json!({}), roots).unwrap_err();
            assert_eq!(e.code, "usage", "{roots:?}");
            assert!(e.hint.unwrap().contains("absolute path"));
        }
        let e = project("stack_down", &json!({ "dir": "." }), None).unwrap_err();
        assert!(e.message.contains("absolute"), "{}", e.message);
        assert_eq!(project("stack_down", &json!({ "dir": "/does/not/exist" }), None).unwrap_err().code, "dir_not_found");
        // Machine-wide tools need none, and take the only root when there is one.
        assert_eq!(project("stack_gc", &json!({}), None).unwrap(), None);
        assert_eq!(project("stack_ps", &json!({}), Some(&only)).unwrap(), Some(root.clone()));
        let called = call(&json!({ "name": "stack_status", "arguments": { "dir": "/does/not/exist" } }), None);
        assert!(called["structuredContent"].get("project").is_none(), "an unresolved dir is not reported as the project");
        // Whatever the server's working directory, a call without `dir` names none.
        let called = call(&json!({ "name": "stack_down", "arguments": {} }), None);
        assert_eq!(called["structuredContent"]["error"]["code"], "usage", "{called}");
        // A machine-wide mv given relative paths and no `dir` has nothing to resolve them against.
        let called = call(&json!({ "name": "stack_mv", "arguments": { "from": "a", "to": "b" } }), None);
        assert!(called["structuredContent"]["error"]["message"].as_str().unwrap().contains("must be an absolute path"), "{called}");
        // Every result names the checkout it acted on.
        std::fs::write(dir.path().join("stack.toml"), "[env]\nA = 'b'\n").unwrap();
        let called = call(&json!({ "name": "stack_inspect", "arguments": { "dir": dir.path() } }), None);
        assert_eq!(called["structuredContent"]["project"], json!(root), "{called}");
        // `dir` is optional in the schema (roots may supply it) but never defaults to the cwd.
        for tool in tools().as_array().unwrap() {
            let dir = tool["inputSchema"]["properties"]["dir"]["description"].as_str().unwrap();
            assert!(!dir.contains("cwd") && !dir.contains("default"), "{}: {dir}", tool["name"]);
        }
    }

    #[test]
    fn timeouts_take_seconds_or_a_duration_but_not_both() {
        let default = Duration::from_secs(600);
        assert_eq!(timeout_arg(&json!({ "timeout": "90s" }), default).unwrap(), Duration::from_secs(90));
        assert_eq!(timeout_arg(&json!({ "timeout": "5m" }), default).unwrap(), Duration::from_secs(300));
        assert_eq!(timeout_arg(&json!({ "timeout": "45" }), default).unwrap(), Duration::from_secs(45));
        assert_eq!(timeout_arg(&json!({ "timeout_secs": 7 }), default).unwrap(), Duration::from_secs(7));
        assert_eq!(timeout_arg(&json!({ "timeout": null, "timeout_secs": null }), default).unwrap(), default);
        assert_eq!(timeout_arg(&json!({}), default).unwrap(), default);
        for bad in [json!({ "timeout": "0s" }), json!({ "timeout": "soon" }), json!({ "timeout": "5x" }), json!({ "timeout": 5 })] {
            assert_eq!(timeout_arg(&bad, default).unwrap_err().code, "usage", "{bad}");
        }
        for tool in ["stack_up", "stack_restart", "stack_exec", "stack_run"] {
            let mut args = json!({ "timeout": "5m", "timeout_secs": 300 });
            if tool == "stack_exec" {
                args["command"] = json!(["true"]);
            }
            if tool == "stack_run" {
                args["task"] = json!("test");
            }
            let e = validate(tool, &args).unwrap_err();
            assert!(e.message.contains("cannot be given together"), "{tool}: {}", e.message);
            args.as_object_mut().unwrap().remove("timeout_secs");
            assert!(validate(tool, &args).is_ok(), "{tool}");
        }
    }

    #[test]
    fn every_tool_says_what_it_does_and_how_it_behaves() {
        for tool in tools().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let description = tool["description"].as_str().unwrap();
            assert!(description.len() < 400, "{name}: description is {} bytes", description.len());
            assert!(description.contains("Use it") || description.contains("Start here"), "{name}: say when to use it");
            assert!(tool["title"].is_string() && tool["annotations"]["title"] == tool["title"], "{name}");
            for hint in ["readOnlyHint", "idempotentHint", "openWorldHint"] {
                assert!(tool["annotations"][hint].is_boolean(), "{name}: {hint}");
            }
            if tool["annotations"]["readOnlyHint"] == true {
                assert!(tool["annotations"].get("destructiveHint").is_none(), "{name}: a read-only tool destroys nothing");
            }
            for (key, property) in tool["inputSchema"]["properties"].as_object().unwrap() {
                assert!(property["description"].as_str().is_some_and(|d| !d.is_empty()), "{name}.{key} has no description");
            }
        }
        let annotation = |name: &str, hint: &str| tools().as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["annotations"][hint].clone();
        for name in ["stack_inspect", "stack_status", "stack_logs", "stack_skill", "stack_doctor", "stack_ps", "stack_env"] {
            assert_eq!(annotation(name, "readOnlyHint"), true, "{name}");
        }
        for name in ["stack_down", "stack_gc"] {
            assert_eq!(annotation(name, "destructiveHint"), true, "{name}");
        }
        for name in ["stack_up", "stack_down", "stack_compile", "stack_install"] {
            assert_eq!(annotation(name, "idempotentHint"), true, "{name}");
        }
    }

    #[test]
    fn up_and_restart_advertise_their_timeout() {
        let tools = tools();
        for name in ["stack_up", "stack_restart"] {
            let tool = tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["inputSchema"]["properties"]["timeout_secs"]["minimum"], 1, "{tool}");
        }
    }

    #[test]
    fn watching_is_described_as_reporting_never_as_reloading() {
        assert!(INSTRUCTIONS.contains("only makes stack_status, stack_exec and stack_run report changed files"), "{INSTRUCTIONS}");
        let tools = tools();
        let description = |name: &str| {
            tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap()["description"].as_str().unwrap().to_string()
        };
        assert!(description("stack_up").contains("kept, not restarted"));
        assert!(description("stack_status").contains("nothing reloads them, so call stack_restart"));
        assert!(description("stack_restart").contains("it never reloads"));
    }
}

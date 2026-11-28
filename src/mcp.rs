//! MCP server over stdio. Tools return the same `{ok, data | error}` envelope as `--json`.

use crate::discover::Discovery;
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

/// What the server tells a client about using its tools, once, at initialization. The
/// envelope's meaning lives here and in `stack://guide`, not in every tool's `outputSchema`.
const INSTRUCTIONS: &str = "Pass `dir`, the absolute path of the checkout you work in, to every tool (it may be omitted only when this client exposes exactly one root; the server's own working directory is never used). A `dir` below the checkout also works: the nearest stack.toml at or above it, up to the Git worktree root, is used, and every result names that checkout as `project`; stack_init alone writes stack.toml in exactly `dir`. Use stack_up before work that needs services, stack_run for the project's tasks (such as tests) and stack_exec for other commands (unverified service endpoints are withheld), stack_restart after editing code a running service loaded, stack_status to diagnose, and stack_down when finished. On stack_status, stack_logs and stack_down, `project: true`, `labels` or `all: true` act across multiple checkouts (e.g. every worktree of dir's repository, not just dir). Nothing reloads services for you: a service's `watch` list only makes stack_status, stack_exec and stack_run report changed files, and stack_up leaves a running service with unchanged configuration as it is, so call stack_restart after such edits (changed: true restarts just the services whose watched files changed). Every result is the `--json` envelope: `ok` says whether Stack did what was asked; `passed` is also false when stack_exec or stack_run exited non-zero and when stack_status finds a started checkout unhealthy; on failure `error` carries `code`, `message`, `hint`, `retryable` (whether the same call can succeed unchanged), `next` (the operations that fix it) and `details`. Tools in this stack may ship agent skills; `stack_inspect` lists them under `skills` and `stack_skill` returns one. The resource stack://guide is the full agent guide.";

/// Which tools a server offers: `full` (every tool, the default) or `core`, the daily loop
/// only, for clients that pay for every tool definition in their context.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Profile {
    #[default]
    Full,
    Core,
}

/// The tools `--profile core` offers.
const CORE: &[&str] = &["stack_up", "stack_status", "stack_run", "stack_exec", "stack_restart", "stack_logs", "stack_down", "stack_ps"];

impl Profile {
    fn offers(self, tool: &str) -> bool {
        self == Profile::Full || CORE.contains(&tool)
    }

    /// [`INSTRUCTIONS`], and under `core` which tools are left out.
    fn instructions(self) -> String {
        match self {
            Profile::Full => INSTRUCTIONS.to_string(),
            Profile::Core => format!(
                "{INSTRUCTIONS} This server runs `--profile core` and offers only {}; the other tools need the default `stack mcp --profile full`.",
                CORE.join(", ")
            ),
        }
    }
}

fn startup_timeout_schema() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Seconds allowed (600)" })
}

/// `timeout`: the same bound as `timeout_secs`, as a duration string like the CLI's `--timeout`.
fn duration_schema(what: &str) -> Value {
    json!({ "type": "string", "description": format!("{what}, e.g. \"5m\"") })
}

/// What a server lets its client do beyond its tools' own checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// `stack_down` may purge checkouts selected machine-wide (by `labels`, without `project`):
    /// `stack mcp --allow-host-purge`. Off by default in every profile.
    pub host_purge: bool,
}

/// Serve the tools of `profile` over stdio until the client closes stdin. Requests run
/// concurrently (see [`server`]); every response and notification is written as one line.
pub fn serve(profile: Profile, policy: Policy) -> std::io::Result<()> {
    crate::commands::serving_mcp();
    server::serve(std::io::BufReader::new(std::io::stdin()), std::io::stdout(), profile, policy)
}

/// Refuse a call `policy` does not allow, before anything runs: a purge of checkouts selected
/// machine-wide. A client's roots are where it works, not a sandbox, so a selection by labels
/// reaches every checkout on the machine; deleting their data takes the server's consent.
/// Purging one checkout, or `dir`'s project (`project: true`), is not affected.
fn permitted(policy: Policy, name: &str, args: &Value) -> Result<()> {
    if name == "stack_down" && args["purge"] == true && machine_wide(name, args) && !policy.host_purge {
        return Err(StackError::new(
            "host_purge_disabled",
            "stack_down: purge with a `labels` selection would delete the service data of matching checkouts anywhere on this machine",
        ));
    }
    Ok(())
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
const DIR_DESCRIPTION: &str = "Absolute path";

/// `dir` of `stack_init`, which is never searched upward.
const INIT_DIR: &str = "Absolute dir to write it in";

fn schema(props: Value, required: &[&str]) -> Value {
    let mut p = props;
    p["dir"] = json!({ "type": "string", "description": DIR_DESCRIPTION });
    let mut schema = json!({ "type": "object", "properties": p, "additionalProperties": false });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// The schema of a tool whose `dir` means something other than the checkout to search from.
fn dir_schema(props: Value, required: &[&str], dir: &str) -> Value {
    let mut schema = schema(props, required);
    schema["properties"]["dir"]["description"] = json!(dir);
    schema
}

/// Tools that act machine-wide: they need no checkout.
const MACHINE_WIDE: &[&str] = &["stack_gc", "stack_ps", "stack_mv", "stack_errors"];

/// Tools that act on one checkout, or on several given `project`, `all` or `labels`.
const FLEET: &[&str] = &["stack_status", "stack_logs", "stack_down"];

/// Whether a call needs no checkout: a machine-wide tool (except `stack_ps` when scoped to a
/// project or repository's worktrees), or a fleet call that selects checkouts by `all` or
/// `labels` without `project` (which is `dir`'s project).
fn machine_wide(name: &str, args: &Value) -> bool {
    if name == "stack_ps" {
        return args["project"] != true && args["worktrees"] != true;
    }
    MACHINE_WIDE.contains(&name)
        || (FLEET.contains(&name)
            && args["project"] != true
            && (args["all"] == true || args["labels"].as_object().is_some_and(|m| !m.is_empty())))
}

/// `labels` of a tool that takes them: label keys to string values.
fn labels_schema(what: &str) -> Value {
    json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": what })
}

/// The fleet selection of `stack_status`, `stack_logs` and `stack_down`.
fn fleet_props(mut props: Value, verb: &str, all: bool) -> Value {
    props["project"] = json!({ "type": "boolean", "description": format!("{verb} ALL worktrees of dir's repo (not just dir)") });
    props["labels"] = labels_schema("Match labels");
    if all {
        props["all"] = json!({ "type": "boolean", "description": format!("{verb} all checkouts") });
    }
    props
}

/// `dir` of a machine-wide tool that only reports it.
const REPORTED_DIR: &str = "Only echoed as `project`";

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
    ("stack_status", "all", "project"),
    ("stack_down", "forget_unrecorded", "project"),
    ("stack_down", "forget_unrecorded", "labels"),
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
        if args.get(key).is_none_or(Value::is_null) {
            return Err(usage(format!("`{key}` is required")));
        }
    }
    let given = |key: &str| match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
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
        "object" => {
            let map = value.as_object().ok_or_else(|| format!("must be an object, not {}", shown(value)))?;
            for (key, item) in map {
                check_value(item, &property["additionalProperties"]).map_err(|why| format!("key {key:?}: {why}"))?;
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
/// result (see each command's documentation); `error` is set instead when `ok` is false. The
/// shape only: what each field means is in [`INSTRUCTIONS`] and `stack://guide`, said once
/// rather than in every tool's definition.
fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "passed": { "type": "boolean" },
            "command": { "type": "string" },
            "stack_version": { "type": "string" },
            "schema": { "type": "integer" },
            "duration_ms": { "type": "integer" },
            "data": {},
            "error": {
                "type": "object",
                "properties": {
                    "code": { "type": "string" },
                    "message": { "type": "string" },
                    "hint": { "type": "string" },
                    "retryable": { "type": "boolean" },
                    "next": { "type": "array" },
                    "details": { "type": "array" },
                },
                "required": ["code", "message"],
            },
        },
        "required": ["ok", "passed", "command", "stack_version", "schema", "duration_ms"],
    })
}

/// Every tool, each with its `inputSchema` and the [`output_schema`] its results follow.
fn tools() -> Value {
    tools_for(Profile::Full)
}

/// The tools `profile` offers, as `tools/list` lists them. `core` leaves out the
/// [`output_schema`], which would be most of its size: its results are the same envelope.
fn tools_for(profile: Profile) -> Value {
    let mut tools = tool_definitions();
    if let Some(all) = tools.as_array_mut() {
        all.retain(|t| profile.offers(t["name"].as_str().unwrap_or_default()));
        if profile == Profile::Full {
            for tool in all {
                tool["outputSchema"] = output_schema();
            }
        }
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

/// One tool: its name, description, annotations and input schema. Its title is in its
/// annotations only, where clients look when a tool has no top-level `title`.
fn tool(name: &str, annotations: Value, description: &str, input: Value) -> Value {
    json!({ "name": name, "description": description, "annotations": annotations, "inputSchema": input })
}

/// `up` and `ttl` of `stack_exec` and `stack_run`.
fn prepare_schemas(props: &mut Value) {
    props["up"] = json!({ "type": "boolean", "description": "Run stack_up first if needed" });
    props["ttl"] = json!({ "type": "string", "description": "With up: its lease, e.g. \"30m\"" });
}

fn tool_definitions() -> Value {
    let mut exec = json!({
        "command": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "argv (no shell); use [\"sh\",\"-c\",...] for shell" },
        "require": { "type": "array", "items": { "type": "string" }, "description": "Required services" },
        "require_all": { "type": "boolean", "description": "Require all services" },
        "secrets": { "type": "array", "items": { "type": "string", "pattern": SECRET_NAME_PATTERN }, "description": "fnox secrets (redacted, not ACL)" },
        "timeout_secs": { "type": "integer", "minimum": 1, "description": "Timeout seconds (600)" },
        "timeout": duration_schema("Duration"),
        "install": { "type": "boolean", "description": "Install missing pins" }
    });
    prepare_schemas(&mut exec);
    let mut run = json!({
        "task": { "type": "string" },
        "args": { "type": "array", "items": { "type": "string" }, "description": "Extra args" },
        "timeout_secs": { "type": "integer", "minimum": 1, "description": "Timeout seconds (600)" },
        "timeout": duration_schema("Duration")
    });
    prepare_schemas(&mut run);
    json!([
        tool("stack_inspect", annotations("Inspect stack", true, None, true, false),
            "Show tools, env, services, tasks, ports and `skills`. Use it to inspect a checkout.",
            schema(json!({ "all_skills": { "type": "boolean", "description": "Include supervisor skills" } }), &[])),
        tool("stack_skill", annotations("Read tool skill", true, None, true, false),
            "Return SKILL.md of a tool in `skills`. Use it to learn a tool; read, never run.",
            schema(json!({ "tool": { "type": "string", "description": "Tool name" }, "name": { "type": "string", "description": "Skill name" } }), &["tool"])),
        tool("stack_init", annotations("Init stack.toml", false, Some(false), false, false),
            "Write stack.toml in `dir` (never overwrites). Use it on manifest_missing, then stack_compile.",
            dir_schema(json!({
                "tools": { "type": "array", "items": { "type": "string" }, "description": "NAME[=VER]" },
                "services": { "type": "array", "items": { "type": "string" }, "description": "Presets, e.g. postgres@17" },
                "detect": { "type": "boolean", "description": "Detect from project files" }
            }), &[], INIT_DIR)),
        tool("stack_compile", annotations("Compile stack.lock", false, Some(false), true, true),
            "Resolve versions and write stack.lock. Use it after editing stack.toml or on lock_outdated.",
            schema(json!({
                "update": { "type": "boolean", "description": "Update pins" },
                "locked": { "type": "boolean", "description": "Check only" },
                "reassign_ports": { "type": "boolean", "description": "New ports" }
            }), &[])),
        tool("stack_up", annotations("Start services", false, Some(false), true, true),
            "Start and verify services. Use it before work needing them; unchanged services are kept, not restarted (call stack_restart after edits).",
            schema(json!({
                "ttl": { "type": "string", "description": "Idle lease TTL" },
                "owner_pid": { "type": "integer", "minimum": 1, "maximum": session::MAX_OWNER_PID, "description": "Lease owner PID" },
                "owner_ancestor": { "type": "string", "description": "Lease ancestor name" },
                "reassign_ports": { "type": "boolean", "description": "Move conflicting ports" },
                "timeout_secs": startup_timeout_schema(),
                "timeout": duration_schema("Duration"),
                "no_init": { "type": "boolean" },
                "reinit": { "type": "array", "items": { "type": "string" }, "description": "Re-run init tasks" },
                "from_snapshot": { "type": "string", "description": "Seed from snapshot" },
                "labels": labels_schema("Session labels")
            }), &[])),
        tool("stack_snapshot_save", annotations("Save snapshot", false, Some(false), false, false),
            "Save postgres/redis data for worktrees to restore. Use it after seeding.",
            schema(json!({
                "name": { "type": "string", "description": "Snapshot name" },
                "services": { "type": "array", "items": { "type": "string" } },
                "force": { "type": "boolean" }
            }), &["name"])),
        tool("stack_snapshot_restore", annotations("Restore snapshot", false, Some(true), true, false),
            "Replace checkout service data with a snapshot. Use it to reset DB state.",
            schema(json!({ "name": { "type": "string" } }), &["name"])),
        tool("stack_snapshot_list", annotations("List snapshots", true, None, true, false),
            "List project snapshots. Use it to find one to restore.",
            schema(json!({}), &[])),
        tool("stack_snapshot_rm", annotations("Remove snapshot", false, Some(true), true, false),
            "Delete a project snapshot. Use it to free disk.",
            schema(json!({ "name": { "type": "string" } }), &["name"])),
        tool("stack_install", annotations("Install tools", false, Some(false), true, true),
            "Install locked tools without starting services. Use it on tools_not_installed.",
            schema(json!({}), &[])),
        tool("stack_restart", annotations("Restart services", false, Some(false), false, false),
            "Restart and verify services. Use it after code edits: `watch` only reports, it never reloads.",
            schema(json!({
                "services": { "type": "array", "items": { "type": "string" } },
                "changed": { "type": "boolean", "description": "Watched-changed only" },
                "timeout_secs": startup_timeout_schema(),
                "timeout": duration_schema("Duration")
            }), &[])),
        tool("stack_logs", annotations("Service logs", true, None, true, false),
            "Return recent service output. Use it when a check fails.",
            schema(fleet_props(json!({
                "service": { "type": "string" },
                "tail": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Lines (100)" },
                "since_start": { "type": "boolean", "description": "Current run only" },
                "since": { "type": "string", "description": "Span, e.g. 10m" },
                "grep": { "type": "string", "description": "Substring" },
                "ignore_case": { "type": "boolean" },
                "cursor": { "type": "string", "description": "A result's cursor: only newer output (\"\" to begin)" }
            }), "Read", false), &[])),
        tool("stack_env", annotations("Command env", true, None, true, false),
            "Return stack_exec env. Use it for unwrapped tools; prefer stack_exec.",
            schema(json!({}), &[])),
        tool("stack_status", annotations("Check services", true, None, true, false),
            "Verify services and renew lease. Start here; edited `watch` files: nothing reloads them, so call stack_restart.",
            schema(fleet_props(json!({}), "Check", true), &[])),
        tool("stack_exec", annotations("Run command", false, None, false, true),
            "Run argv (no shell) with stack env; unverified endpoints withheld. Use it outside tasks; check exit_code.",
            schema(exec, &["command"])),
        tool("stack_run", annotations("Run task", false, None, false, true),
            "Run a stack.toml task with declared secrets once services verify. Use it for tests; check exit_code.",
            schema(run, &["task"])),
        tool("stack_renew", annotations("Renew lease", false, Some(false), true, false),
            "Renew session lease. Use it during idle work.",
            schema(json!({ "ttl": { "type": "string", "description": "New TTL" } }), &[])),
        tool("stack_down", annotations("Stop services", false, Some(true), true, false),
            "Stop services; stop_unconfirmed while a started command runs. Use it when finished; keeps data unless purge.",
            schema(fleet_props(json!({
                "stop_supervisor": { "type": "boolean", "description": "Stop idle supervisor (true)" },
                "purge": { "type": "boolean", "description": "Delete service data (with labels: server must allow)" },
                "forget_unrecorded": { "type": "boolean", "description": "Forget dead unrecorded cmds" }
            }), "Stop", false), &[])),
        tool("stack_gc", annotations("Reclaim sessions", false, Some(true), true, false),
            "Reclaim expired sessions and deleted checkouts machine-wide. Use it for cleanup only.",
            dir_schema(json!({
                "reclaim_data": { "type": "boolean", "description": "Delete their data" },
                "stop_orphans": { "type": "boolean", "description": "Stop verified orphans" },
                "dry_run": { "type": "boolean" }
            }), &[], REPORTED_DIR)),
        tool("stack_ps", annotations("List checkouts", true, None, true, false),
            "List checkouts machine-wide. Use it to see what runs where.",
            dir_schema(json!({
                "no_size": { "type": "boolean" },
                "project": { "type": "boolean", "description": "Only dir's project" },
                "state": { "type": "string", "description": "Filter by state" },
                "labels": labels_schema("Match labels"),
                "worktrees": { "type": "boolean", "description": "Include unstarted worktrees" }
            }), &[], "Base dir for project/worktrees")),
        tool("stack_mv", annotations("Move checkout", false, Some(false), false, false),
            "Move checkout records, ports and data to a new path. Use it after moving one.",
            dir_schema(json!({
                "from": { "type": "string" },
                "to": { "type": "string" }
            }), &["from", "to"], "Base for relative from/to")),
        tool("stack_doctor", annotations("Check setup", true, None, true, false),
            "Check providers and project compile. Use it on setup errors.",
            schema(json!({}), &[])),
        tool("stack_errors", annotations("List error codes", true, None, true, false),
            "List error codes and fixes. Use it to inspect a code.",
            dir_schema(json!({}), &[], REPORTED_DIR)),
    ])
}

/// Run one `tools/call` with every tool offered. `roots` are the client's MCP roots: `None`
/// when it offers none.
#[cfg(test)]
fn call(params: &Value, roots: Option<&[PathBuf]>) -> Value {
    call_in(Profile::Full, Policy::default(), params, roots)
}

/// Run one `tools/call` on a server offering `profile`'s tools under `policy`.
fn call_in(profile: Profile, policy: Policy, params: &Value, roots: Option<&[PathBuf]>) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let mut call = crate::contract::Call::new(name.strip_prefix("stack_").unwrap_or(name));
    let mut resolved = None;
    let outcome = offered(profile, name)
        .and_then(|()| validate(name, &params["arguments"]))
        .and_then(|args| {
            let args = Value::Object(args);
            permitted(policy, name, &args)?;
            let found = locate(name, &args, roots)?;
            resolved = found.as_ref().map(|d| d.root.clone());
            let here = found.as_ref().map(|d| d.from.clone()).filter(|from| Some(from) != resolved.as_ref());
            dispatch(name, &args, &ctx(resolved.as_deref()), here).map_err(|e| match &found {
                Some(d) => explain(profile, d, e),
                None => e,
            })
        })
        .map_err(|e| with_usage_hint(e, "see this tool's inputSchema in tools/list"));
    call.dir = resolved.clone();
    let mut envelope = call.outcome(&outcome);
    if profile == Profile::Core {
        if let Some(steps) = envelope.get_mut("error").and_then(|e| e.get_mut("next")).and_then(Value::as_array_mut) {
            for step in steps {
                if step.get("tool").and_then(Value::as_str).is_some_and(|t| !profile.offers(t)) {
                    step["tool"] = Value::Null;
                }
            }
        }
    }
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

/// A tool `profile` leaves out is unknown to this server, with a hint saying where it is.
fn offered(profile: Profile, name: &str) -> Result<()> {
    // A name no profile has is left to `validate`, which names it unknown as a full server does.
    if profile.offers(name) || !tools().as_array().into_iter().flatten().any(|t| t["name"] == name) {
        return Ok(());
    }
    Err(StackError::new("unknown_tool", format!("no tool named '{name}' in this server's --profile core"))
        .hint(format!("{name} is available under the default `--profile full`: register the server as `stack mcp` without `--profile core`")))
}

/// Say where an upward search for `stack.toml` looked, on a `manifest_missing` error.
fn explain(profile: Profile, found: &Discovery, mut e: StackError) -> StackError {
    if found.found || e.code != "manifest_missing" || found.from == found.stop {
        return e;
    }
    e.message = format!("no stack.toml in {} or any directory above it up to {}", found.from.display(), found.stop.display());
    e.hint = Some(if profile.offers("stack_init") {
        "call stack_init with this `dir` to create one, or pass the `dir` of the checkout that holds stack.toml".into()
    } else {
        "run `stack init` in this directory to create one, or pass the `dir` of the checkout that holds stack.toml".into()
    });
    e
}

/// The checkout a call acts on: see [`locate`].
fn project(name: &str, args: &Value, roots: Option<&[PathBuf]>) -> Result<Option<PathBuf>> {
    Ok(locate(name, args, roots)?.map(|d| d.root))
}

/// The checkout a call acts on: its `dir`, which must be absolute, else the client's only root,
/// then the nearest `stack.toml` at or above it up to the Git worktree root, as the CLI finds
/// one from its working directory. Never the server's working directory: one server is shared
/// by agents in many checkouts, so that directory says nothing about which one a call means.
/// `stack_init` (which writes `stack.toml` in exactly `dir`) and machine-wide tools search
/// nothing. `root` is the directory itself when no `stack.toml` was found, so the tool reports
/// `manifest_missing` as usual. `None` for a machine-wide tool given neither.
fn locate(name: &str, args: &Value, roots: Option<&[PathBuf]>) -> Result<Option<Discovery>> {
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
            _ if machine_wide(name, args) => return Ok(None),
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
    let start = dir
        .canonicalize()
        .map_err(|e| StackError::new("dir_not_found", format!("{}: {e}", dir.display())).hint("pass `dir` as the absolute path of an existing checkout"))?;
    if !start.is_dir() {
        return Err(StackError::new("usage", format!("{name}: `dir` must be a directory, not {}", dir.display()))
            .hint("pass `dir` as the absolute path of a checkout directory"));
    }
    if name == "stack_init" || (MACHINE_WIDE.contains(&name) && machine_wide(name, args)) {
        let found = start.join(crate::manifest::PROJECT_FILE).is_file();
        return Ok(Some(Discovery { root: start.clone(), found, from: start.clone(), stop: start }));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).and_then(|h| h.canonicalize().ok());
    Ok(Some(crate::discover::find(&start, home.as_deref())))
}

/// Whether a call needs the client's roots to find its checkout: it is offered by `profile`,
/// names no `dir`, and acts on one checkout (or a project-scoped fleet/ps call). Unoffered
/// tools or arguments that fail validation need nothing; the call reports them immediately.
fn needs_roots(profile: Profile, params: &Value) -> bool {
    let name = params["name"].as_str().unwrap_or_default();
    if !profile.offers(name) {
        return false;
    }
    validate(name, &params["arguments"]).is_ok_and(|args| args.get("dir").is_none_or(Value::is_null) && !machine_wide(name, &Value::Object(args)))
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

/// `here` is the directory a call named below its checkout's root, where `stack_exec` runs its
/// command, as the CLI's `exec` runs where it was called from.
fn dispatch(name: &str, args: &Value, ctx: &Ctx, here: Option<PathBuf>) -> Result<Value> {
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
            crate::init::init(&ctx.root, &strings("tools"), &strings("services"), args["detect"] == true).map(to_value)
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
                    ..LeaseOptions::default()
                },
                timeout,
                session::UpOptions {
                    reassign_ports: args["reassign_ports"] == true,
                    no_init: args["no_init"] == true,
                    reinit: strings(args, "reinit")?,
                    from_snapshot: args["from_snapshot"].as_str().map(String::from),
                    labels: crate::labels::from_json(&args["labels"])?,
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
                cursor: match &args["cursor"] {
                    Value::Null => None,
                    Value::String(c) => Some(crate::logs::Cursor::parse(c)?),
                    v => return Err(StackError::new("usage", format!("cursor {v} must be a string a previous stack_logs result gave"))),
                },
            };
            let fleet = fleet_filter(args, ctx)?;
            if fleet.active() && q.cursor.is_some() {
                return Err(StackError::new("usage", "cursor reads one checkout's logs").hint("drop `cursor`, or drop project/labels/all"));
            }
            if fleet.active() {
                return crate::fleet::logs(&ctx.state, &ctx.cache, &fleet, &q).map(to_value);
            }
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
        "stack_status" => match fleet_filter(args, ctx)? {
            fleet if fleet.active() => crate::fleet::status(&ctx.state, &ctx.cache, &fleet).map(to_value),
            _ => session::status(ctx).map(to_value),
        },
        "stack_env" => crate::shell::plan(ctx).map(to_value),
        "stack_renew" => {
            let ttl_secs = args["ttl"].as_str().map(parse_duration).transpose()?;
            session::renew_with(ctx, ttl_secs).map(to_value)
        }
        "stack_down" => match fleet_filter(args, ctx)? {
            fleet if fleet.active() => crate::fleet::down(&ctx.state, &ctx.cache, &fleet, args["stop_supervisor"] != false, args["purge"] == true).map(to_value),
            _ => session::down_forgetting(ctx, args["stop_supervisor"] != false, args["purge"] == true, args["forget_unrecorded"] == true).map(to_value),
        },
        "stack_gc" if args["dry_run"] == true => session::gc_plan(&ctx.state, args["reclaim_data"] == true).map(to_value),
        "stack_gc" => session::gc_checked(&ctx.state, args["reclaim_data"] == true, args["stop_orphans"] == true).map(to_value),
        "stack_ps" => {
            let given = (!ctx.root.as_os_str().is_empty()).then(|| ctx.root.clone());
            let project = match (args["project"] == true, &given) {
                (false, _) => None,
                (true, Some(root)) => Some(root.clone()),
                (true, None) => return Err(StackError::new("usage", "stack_ps: `project` needs `dir`").hint("pass `dir` as the absolute path of a checkout of the project")),
            };
            let opts = crate::fleet::PsOptions {
                project,
                state: args["state"].as_str().map(String::from),
                labels: crate::labels::from_json(&args["labels"])?,
                worktrees: args["worktrees"] == true,
                cwd: given,
            };
            crate::fleet::ps_with(&ctx.state, args["no_size"] != true, &opts).map(to_value)
        }
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
        "stack_exec" => exec(args, ctx, here),
        "stack_run" => run(args, ctx),
        _ => Err(StackError::new(
            "unknown_tool",
            format!("no tool named '{name}'"),
        )),
    }
}

/// The fleet selection a `stack_status`, `stack_logs` or `stack_down` call makes: none (one
/// checkout, as before) unless it gives `project`, `all` or `labels`.
fn fleet_filter(args: &Value, ctx: &Ctx) -> Result<crate::fleet::FleetFilter> {
    let project = match args["project"] == true {
        false => None,
        true if ctx.root.as_os_str().is_empty() => {
            return Err(StackError::new("usage", "`project` needs `dir`").hint("pass `dir` as the absolute path of a checkout of the project"))
        }
        true => Some(ctx.root.clone()),
    };
    Ok(crate::fleet::FleetFilter { project, labels: crate::labels::from_json(&args["labels"])?, all: args["all"] == true })
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

fn exec(args: &Value, ctx: &Ctx, here: Option<PathBuf>) -> Result<Value> {
    let command = strings(args, "command")?;
    let secrets = strings(args, "secrets")?;
    let require = if args["require_all"] == true { Require::All } else { Require::Only(strings(args, "require")?) };
    let prepare = prepare(args, args["install"] == true)?;
    captured(args, ctx, || session::plan_prepared(ctx, prepare, || {
        session::plan_exec_with(ctx, &command, &require, &Grant { keys: secrets, captured: true }).map(|mut plan| {
            plan.dir = here;
            plan
        })
    }))
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
    let _recorded = plan.record_spawn();
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
        let properties = schema["properties"].as_object().into_iter().flatten().all(|(k, s)| value.get(k).is_none_or(|v| fits(v, s)));
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
                Some("object") => assert_eq!(property["additionalProperties"], json!({ "type": "string" }), "{property}"),
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
            assert!(tool["annotations"]["title"].is_string() && tool.get("title").is_none(), "{name}: the title is in the annotations, once");
            for hint in ["readOnlyHint", "idempotentHint", "openWorldHint"] {
                assert!(tool["annotations"][hint].is_boolean(), "{name}: {hint}");
            }
            if tool["annotations"]["readOnlyHint"] == true {
                assert!(tool["annotations"].get("destructiveHint").is_none(), "{name}: a read-only tool destroys nothing");
            }
            // Only an argument whose name says it all goes without a description.
            const SELF_EXPLANATORY: &[&str] = &["force", "dry_run", "from", "to", "task", "service", "services", "ignore_case", "no_size", "no_init", "name"];
            for (key, property) in tool["inputSchema"]["properties"].as_object().unwrap() {
                let described = property["description"].as_str().is_some_and(|d| !d.is_empty());
                assert!(described || SELF_EXPLANATORY.contains(&key.as_str()), "{name}.{key} has no description");
                assert!(property["description"].as_str().map_or(0, str::len) <= 80, "{name}.{key}: one short line");
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
    fn labels_are_an_object_of_strings_checked_before_anything_runs() {
        assert!(validate("stack_up", &json!({ "labels": { "agent": "claude" } })).is_ok());
        let e = validate("stack_up", &json!({ "labels": { "agent": 1 } })).unwrap_err();
        assert!(e.message.contains("`labels` key \"agent\": must be a string"), "{}", e.message);
        assert!(validate("stack_up", &json!({ "labels": ["agent=claude"] })).unwrap_err().message.contains("must be an object"));
        // A key the CLI's --label would refuse is refused the same way, before any project is opened.
        let called = call(&json!({ "name": "stack_up", "arguments": { "dir": "/", "labels": { "bad key": "v" } } }), None);
        assert_eq!(called["structuredContent"]["error"]["code"], "usage", "{called}");
        assert!(called["structuredContent"]["error"]["hint"].as_str().unwrap().contains("KEY=VALUE"), "{called}");
        for (tool, args) in [("stack_ps", json!({ "labels": { "a": "b" } })), ("stack_status", json!({ "labels": { "a": "b" } })), ("stack_logs", json!({ "labels": {} })), ("stack_down", json!({ "labels": { "a": "b" } }))] {
            assert!(validate(tool, &args).is_ok(), "{tool}");
        }
    }

    #[test]
    fn a_machine_wide_purge_needs_the_servers_consent_and_a_scoped_one_does_not() {
        let by_label = json!({ "labels": { "agent": "claude" }, "purge": true });
        let allowed = Policy { host_purge: true };
        for profile in [Profile::Full, Profile::Core] {
            // Refused before anything is located or stopped: no dir, no roots, nothing runs.
            let called = call_in(profile, Policy::default(), &json!({ "name": "stack_down", "arguments": by_label }), None);
            let error = &called["structuredContent"]["error"];
            assert_eq!(error["code"], "host_purge_disabled", "{profile:?}: {called}");
            assert!(error["hint"].as_str().unwrap().contains("--allow-host-purge"), "{called}");
            assert_eq!(error["retryable"], false, "{called}");
        }
        let refused = |args: Value| permitted(Policy::default(), "stack_down", &args).is_err();
        assert!(refused(by_label.clone()));
        assert!(refused(json!({ "dir": "/w", "labels": { "a": "b" }, "purge": true })), "dir does not scope a label selection");
        // Scoped to one checkout or dir's project, or not purging: as before.
        assert!(!refused(json!({ "dir": "/w", "purge": true })));
        assert!(!refused(json!({ "dir": "/w", "project": true, "purge": true })));
        assert!(!refused(json!({ "dir": "/w", "project": true, "labels": { "a": "b" }, "purge": true })));
        assert!(!refused(json!({ "labels": { "a": "b" } })));
        assert!(!refused(json!({ "labels": {}, "purge": true })), "an empty selection selects nothing machine-wide");
        assert!(permitted(allowed, "stack_down", &by_label).is_ok());
        assert!(permitted(Policy::default(), "stack_gc", &json!({ "reclaim_data": true })).is_ok(), "gc reclaims only deleted checkouts");
    }

    #[test]
    fn fleet_calls_by_label_or_all_need_no_checkout_and_by_project_need_one() {
        let needs = |name: &str, args: Value| needs_roots(Profile::Full, &json!({ "name": name, "arguments": args }));
        assert!(needs("stack_status", json!({})));
        assert!(!needs("stack_status", json!({ "all": true })));
        assert!(!needs("stack_status", json!({ "labels": { "agent": "claude" } })));
        assert!(needs("stack_status", json!({ "labels": {} })), "an empty selection selects nothing");
        assert!(needs("stack_down", json!({ "project": true, "labels": { "a": "b" } })), "project is dir's project");
        assert!(!needs("stack_logs", json!({ "labels": { "a": "b" } })));
        assert!(needs("stack_logs", json!({ "project": true })));
        assert!(!needs("stack_ps", json!({})));
        assert!(needs("stack_ps", json!({ "project": true })), "project-scoped ps uses dir or sole root");
        assert!(needs("stack_ps", json!({ "worktrees": true })), "worktree-scoped ps uses dir or sole root");
        assert!(!needs_roots(Profile::Core, &json!({ "name": "stack_inspect", "arguments": {} })), "excluded core tools never wait for roots");
        assert_eq!(project("stack_down", &json!({ "labels": { "a": "b" } }), None).unwrap(), None);
        assert_eq!(project("stack_down", &json!({ "project": true }), None).unwrap_err().code, "usage");
        assert!(validate("stack_status", &json!({ "all": true, "project": true })).unwrap_err().message.contains("cannot be given together"));
        assert!(validate("stack_down", &json!({ "forget_unrecorded": true, "labels": { "a": "b" } })).is_err());
        // Without a selection a fleet tool acts on one checkout, as before.
        let ctx = ctx(None);
        assert!(!fleet_filter(&json!({}), &ctx).unwrap().active());
        assert_eq!(fleet_filter(&json!({ "project": true }), &ctx).unwrap_err().code, "usage");
        let called = call(&json!({ "name": "stack_ps", "arguments": { "project": true } }), None);
        assert_eq!(called["structuredContent"]["error"]["code"], "usage", "{called}");
        let called = call(&json!({ "name": "stack_ps", "arguments": { "state": "happy" } }), None);
        assert_eq!(called["structuredContent"]["error"]["code"], "usage", "{called}");
    }

    #[test]
    fn up_and_restart_advertise_their_timeout() {
        let tools = tools();
        for name in ["stack_up", "stack_restart"] {
            let tool = tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["inputSchema"]["properties"]["timeout_secs"]["minimum"], 1, "{tool}");
        }
    }

    /// The names of the tools `profile` lists.
    fn names(profile: Profile) -> Vec<String> {
        tools_for(profile).as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn tools_list_stays_small_and_keeps_every_tool() {
        let size = |profile| json!({ "tools": tools_for(profile) }).to_string().len();
        let (full, core) = (size(Profile::Full), size(Profile::Core));
        assert!(full < 26_000, "full tools/list is {full} bytes");
        assert!(core < 10_000, "core tools/list is {core} bytes");
        let mut all = names(Profile::Full);
        all.sort();
        let mut expected = vec![
            "stack_inspect", "stack_skill", "stack_init", "stack_compile", "stack_up", "stack_snapshot_save", "stack_snapshot_restore", "stack_snapshot_list",
            "stack_snapshot_rm", "stack_install", "stack_restart", "stack_logs", "stack_env", "stack_status", "stack_exec", "stack_run", "stack_renew", "stack_down",
            "stack_gc", "stack_ps", "stack_mv", "stack_doctor", "stack_errors",
        ];
        expected.sort();
        assert_eq!(all, expected);
        // The envelope keeps its whole shape, without the prose said once in the instructions.
        let schema = output_schema();
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&schema["properties"]), ["ok", "passed", "command", "stack_version", "schema", "duration_ms", "data", "error"]);
        assert_eq!(keys(&schema["properties"]["error"]["properties"]), ["code", "message", "hint", "retryable", "next", "details"]);
        assert_eq!(schema["required"], json!(["ok", "passed", "command", "stack_version", "schema", "duration_ms"]));
        assert!(!schema.to_string().contains("description"), "{schema}");
        for word in ["`passed`", "`retryable`", "`next`", "`details`"] {
            assert!(INSTRUCTIONS.contains(word), "the instructions explain {word}");
        }
    }

    #[test]
    fn the_core_profile_offers_the_daily_loop_and_names_the_rest_unknown() {
        assert_eq!(names(Profile::Core), ["stack_up", "stack_restart", "stack_logs", "stack_status", "stack_exec", "stack_run", "stack_down", "stack_ps"]);
        // The same definitions as the full profile, without the output schema.
        let full = tools_for(Profile::Full);
        for tool in tools_for(Profile::Core).as_array().unwrap() {
            let mut same = full.as_array().unwrap().iter().find(|t| t["name"] == tool["name"]).unwrap().clone();
            same.as_object_mut().unwrap().remove("outputSchema");
            assert_eq!(*tool, same);
        }
        let refused = call_in(Profile::Core, Policy::default(), &json!({ "name": "stack_inspect", "arguments": { "dir": "/does/not/exist" } }), None);
        let error = &refused["structuredContent"]["error"];
        assert_eq!((refused["isError"].clone(), error["code"].clone()), (json!(true), json!("unknown_tool")), "{refused}");
        assert!(error["hint"].as_str().unwrap().contains("--profile full"), "{error}");
        assert!(refused["structuredContent"].get("project").is_none(), "nothing was resolved: {refused}");
        // A name no profile has is unknown as on a full server; core tools are checked as ever.
        let unknown = call_in(Profile::Core, Policy::default(), &json!({ "name": "stack_nope", "arguments": {} }), None);
        assert_eq!(unknown["structuredContent"]["error"]["code"], "unknown_tool");
        assert!(!unknown["structuredContent"]["error"]["hint"].as_str().unwrap_or_default().contains("--profile"), "{unknown}");
        let relative = call_in(Profile::Core, Policy::default(), &json!({ "name": "stack_status", "arguments": { "dir": "relative" } }), None);
        assert_eq!(relative["structuredContent"]["error"]["code"], "usage", "{relative}");
        assert!(Profile::Core.instructions().contains("--profile core") && !Profile::Full.instructions().contains("--profile core"));
    }

    #[test]
    fn dir_finds_the_nearest_manifest_up_to_the_worktree_root_except_for_init() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        let repo = top.join("repo");
        let deep = repo.join("src/deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        // Above the worktree root does not count: the call keeps its own `dir`.
        std::fs::write(top.join("stack.toml"), "[env]\nA = 'b'\n").unwrap();
        assert_eq!(project("stack_status", &json!({ "dir": deep }), None).unwrap(), Some(deep.clone()));
        let missing = call(&json!({ "name": "stack_inspect", "arguments": { "dir": deep } }), None);
        let error = &missing["structuredContent"]["error"];
        assert_eq!(error["code"], "manifest_missing", "{missing}");
        assert!(error["message"].as_str().unwrap().contains(&format!("up to {}", repo.display())), "{error}");
        assert!(error["hint"].as_str().unwrap().contains("stack_init"), "{error}");
        assert_eq!(missing["structuredContent"]["project"], json!(deep));

        std::fs::write(repo.join("stack.toml"), "[env]\nA = 'b'\n").unwrap();
        assert_eq!(project("stack_status", &json!({ "dir": deep }), None).unwrap(), Some(repo.clone()));
        assert_eq!(project("stack_status", &json!({ "dir": repo }), None).unwrap(), Some(repo.clone()));
        // A single root inside the checkout finds it too.
        assert_eq!(project("stack_down", &json!({}), Some(std::slice::from_ref(&deep))).unwrap(), Some(repo.clone()));
        let found = call(&json!({ "name": "stack_inspect", "arguments": { "dir": deep } }), None);
        assert_eq!(found["structuredContent"]["ok"], true, "{found}");
        assert_eq!(found["structuredContent"]["project"], json!(repo), "the envelope names the checkout found");
        // The nearest wins.
        std::fs::write(repo.join("src/stack.toml"), "").unwrap();
        assert_eq!(project("stack_status", &json!({ "dir": deep }), None).unwrap(), Some(repo.join("src")));
        // Project-scoped `stack_ps` walks upward just like `stack_status`.
        assert_eq!(project("stack_ps", &json!({ "dir": deep, "project": true }), None).unwrap(), Some(repo.join("src")));

        // stack_init writes in exactly `dir`; machine-wide tools search nothing either.
        assert_eq!(project("stack_init", &json!({ "dir": deep }), None).unwrap(), Some(deep.clone()));
        assert_eq!(project("stack_mv", &json!({ "dir": deep, "from": "a", "to": "b" }), None).unwrap(), Some(deep.clone()));
        let init = call(&json!({ "name": "stack_init", "arguments": { "dir": deep } }), None);
        assert_eq!(init["structuredContent"]["ok"], true, "{init}");
        assert_eq!(init["structuredContent"]["project"], json!(deep));
        assert!(deep.join("stack.toml").is_file());

        // Still absolute, still existing, and never a regular file.
        assert_eq!(project("stack_status", &json!({ "dir": "repo/src" }), None).unwrap_err().code, "usage");
        assert_eq!(project("stack_status", &json!({ "dir": repo.join("nope") }), None).unwrap_err().code, "dir_not_found");
        assert_eq!(project("stack_status", &json!({ "dir": repo.join("stack.toml") }), None).unwrap_err().code, "usage");
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

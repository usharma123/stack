//! MCP server over stdio. Tools return the same `{ok, data | error}` envelope as `--json`.

use crate::error::{Result, StackError};
use crate::project::{self, default_cache_dir, Options};
use crate::secrets::Grant;
use crate::session::{self, Ctx, LeaseOptions, Require};
use crate::source::Mode;
use crate::state::default_state_dir;
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const OUTPUT_LIMIT: usize = 64 * 1024;
const DEFAULT_EXEC_TIMEOUT: u64 = 600;

fn startup_timeout_schema() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "Seconds the whole startup may take (default 600)" })
}

pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(
                    &mut stdout,
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } }),
                )?;
                continue;
            }
        };
        // Notifications (no id) need no response.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg["method"].as_str().unwrap_or_default();
        let reply = match method {
            "initialize" => Ok(json!({
                "protocolVersion": negotiate(msg["params"]["protocolVersion"].as_str()),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "stack", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Use stack_up before work that needs services, stack_run for the project's tasks (such as tests) and stack_exec for other commands (unverified service endpoints are withheld), stack_restart after editing code a running service loaded, stack_status to diagnose, and stack_down when finished. Tools in this stack may ship agent skills; `stack_inspect` lists them under `skills` and `stack_skill` returns one.",
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => Ok(call(&msg["params"])),
            _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
        };
        let response = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        send(&mut stdout, response)?;
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

fn schema(props: Value, required: &[&str]) -> Value {
    let mut p = props;
    p["dir"] = json!({ "type": "string", "description": "Project directory containing stack.toml (default: server cwd)" });
    json!({ "type": "object", "properties": p, "required": required, "additionalProperties": false })
}

/// The one `pattern` the tool schemas use: a fnox secret name.
const SECRET_NAME_PATTERN: &str = "^[A-Z_][A-Z0-9_]*$";

fn secret_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase() || c == '_') && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Arguments of one tool that cannot be given together, as on the command line.
const CONFLICTS: &[(&str, &str, &str)] = &[("stack_compile", "update", "locked"), ("stack_exec", "require", "require_all")];

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

fn tools() -> Value {
    json!([
        { "name": "stack_inspect", "description": "Show the composed stack (bundles, tools, env, services, tasks, ports, origins) without changing anything. `skills` lists the agent skills of the exact releases stack.lock pins (status available, no_skill, not_installed or unavailable); all_skills also lists the provider's own under provider_skills, for people.",
          "inputSchema": schema(json!({ "all_skills": { "type": "boolean" } }), &[]) },
        { "name": "stack_skill", "description": "The SKILL.md text (at most 64 KiB) of one available skill that stack_inspect lists under `skills`, at the release stack.lock pins. It is the tool's own documentation; read it, do not run it.",
          "inputSchema": schema(json!({ "tool": { "type": "string" }, "name": { "type": "string" } }), &["tool", "name"]) },
        { "name": "stack_compile", "description": "Resolve bundles and exact tool/service versions, update stack.lock and the generated provider config. Pins are kept unless their request changed; update re-resolves everything; locked fails instead of changing stack.lock; reassign_ports gives this checkout fresh ports (after a port_conflict).",
          "inputSchema": schema(json!({ "update": { "type": "boolean" }, "locked": { "type": "boolean" }, "reassign_ports": { "type": "boolean" } }), &[]) },
        { "name": "stack_up", "description": "Start and verify services; records a session. Optional lease: ttl like '30m', or owner_pid. Startup as a whole (lock, compile, install, start, readiness, verification) must finish within timeout_secs (default 600) or the call fails with timed_out: the step cut short and the progress so far are in the error details, and anything launched stays recorded for stack_status, stack_down and a retried stack_up.",
          "inputSchema": schema(json!({ "ttl": { "type": "string" }, "owner_pid": { "type": "integer", "minimum": 1, "maximum": session::MAX_OWNER_PID }, "timeout_secs": startup_timeout_schema() }), &[]) },
        { "name": "stack_install", "description": "Install the locked tools and service binaries without starting services or recording a session. Locked: a missing or stale pin fails with lock_outdated.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_restart", "description": "Restart services of the running session (all when services is omitted) after editing code they loaded, then verify the whole stack again. Other services keep running and the session keeps its id. Fails with session_stale when the configuration changed; use stack_up then. Bounded by timeout_secs (default 600) like stack_up.",
          "inputSchema": schema(json!({ "services": { "type": "array", "items": { "type": "string" } }, "timeout_secs": startup_timeout_schema() }), &[]) },
        { "name": "stack_logs", "description": "The last lines a service wrote, as kept by the supervisor (bounded; never follows). The supervisor keeps output across restarts; since_start returns only the current process's lines.",
          "inputSchema": schema(json!({ "service": { "type": "string" }, "tail": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Lines from the end (default 100)" }, "since_start": { "type": "boolean" } }), &["service"]) },
        { "name": "stack_status", "description": "Live verification of every service, plus session and lease state.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_exec", "description": "Run a command with the stack's tools and env. command is an argv list run without a shell: `$VAR`, pipes and globs are not expanded, so use [\"sh\", \"-c\", \"...\"] for those. Connection variables of services that fail verification are withheld; required services must verify or the command does not run. A command still running at timeout_secs (default 600) is killed and the call fails with timed_out; its output so far is in the error details. secrets grants named fnox secrets to this command only; no granted value appears literally in the parsed stdout or stderr strings, including the timed_out error's. Key names are not secret: they appear under secrets and in [redacted:KEY] markers. Values the command transforms (encoded, split, or only matching after the result's JSON escaping) are not caught.",
          "inputSchema": schema(json!({
              "command": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
              "require": { "type": "array", "items": { "type": "string" } },
              "require_all": { "type": "boolean" },
              "secrets": { "type": "array", "items": { "type": "string", "pattern": SECRET_NAME_PATTERN }, "description": "fnox secret names to grant this command, resolved through the stack's pinned fnox. Literal values are replaced by [redacted:KEY] ([redacted] where naming the key could spell out a value) in the parsed stdout and stderr strings; key names are listed under secrets; values shorter than 8 bytes or ones a marker could spell out are refused (secret_unsupported)" },
              "timeout_secs": { "type": "integer", "minimum": 1, "description": "Seconds before the command is killed (default 600)" }
          }), &["command"]) },
        { "name": "stack_run", "description": "Run a task declared in stack.toml ([tasks.<name>]) through mise's task runner, with the stack's tools and env. Every service of the project must verify or it does not run (mise gives a task every service's endpoint); use stack_exec for commands that should run with services down. Output is captured like stack_exec. The task receives exactly the secrets it declares (secrets = [...] in stack.toml), redacted from its output as in stack_exec; no others can be added here.",
          "inputSchema": schema(json!({
              "task": { "type": "string" },
              "args": { "type": "array", "items": { "type": "string" }, "description": "Appended to the task's command" },
              "timeout_secs": { "type": "integer", "minimum": 1, "description": "Seconds before the task is killed (default 600)" }
          }), &["task"]) },
        { "name": "stack_renew", "description": "Renew this project's session lease.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_down", "description": "Stop services; succeeds only once their processes are confirmed gone.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_gc", "description": "Reclaim sessions with expired leases, and services of deleted projects, machine-wide. Fails (gc_incomplete) if any could not be confirmed stopped; ownership records are then kept.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_doctor", "description": "Check that the providers stack needs (mise, git, tar) are installed, that Pitchfork's socket path fits this platform, and that the project compiles.", "inputSchema": schema(json!({}), &[]) },
    ])
}

fn call(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let outcome = validate(name, &params["arguments"]).and_then(|args| {
        let args = Value::Object(args);
        ctx(&args).and_then(|ctx| dispatch(name, &args, &ctx))
    });
    let envelope = match outcome {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let is_error = envelope["ok"] == false;
    json!({
        "content": [{ "type": "text", "text": envelope.to_string() }],
        "structuredContent": envelope,
        "isError": is_error,
    })
}

fn ctx(args: &Value) -> Result<Ctx> {
    let dir = args["dir"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = dir
        .canonicalize()
        .map_err(|e| StackError::new("dir_not_found", format!("{}: {e}", dir.display())))?;
    Ok(Ctx {
        root,
        cache: default_cache_dir(),
        state: default_state_dir(),
    })
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
            let timeout = startup_timeout(args)?;
            session::up(
                ctx,
                LeaseOptions {
                    ttl_secs,
                    owner_pid,
                },
                timeout,
            )
            .map(to_value)
        }
        "stack_install" => session::install(ctx).map(to_value),
        "stack_logs" => {
            let service = args["service"].as_str().ok_or_else(|| StackError::new("usage", "service is required"))?;
            let tail = match &args["tail"] {
                Value::Null => 100,
                v => v.as_u64().filter(|n| (1..=10_000).contains(n)).ok_or_else(|| StackError::new("usage", format!("tail {v} must be 1 to 10000")))? as usize,
            };
            session::logs(ctx, service, tail, args["since_start"] == true).map(to_value)
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
            session::restart(ctx, &services, timeout).map(to_value)
        }
        "stack_status" => session::status(ctx).map(to_value),
        "stack_renew" => session::renew(ctx).map(to_value),
        "stack_down" => session::down(ctx).map(to_value),
        "stack_gc" => session::gc_checked(&ctx.state).map(to_value),
        "stack_doctor" => crate::doctor::run(&ctx.root, &ctx.cache, &ctx.state).map(to_value),
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
    let field = |key: &str| {
        args[key].as_str().filter(|s| !s.is_empty()).ok_or_else(|| StackError::new("usage", format!("{key} is required")))
    };
    let (tool, name) = (field("tool")?, field("name")?);
    let report = project::compile(&Options {
        root: ctx.root.clone(),
        mode: project::inspect_mode(&ctx.root),
        write: false,
        cache: ctx.cache.clone(),
        state: ctx.state.clone(),
        reassign_ports: false,
        resolver: None,
        locker: None,
    })?;
    let found = report.discover_skills(&ctx.cache);
    crate::skills::read(&found, tool, name)
}

/// `timeout_secs` of `stack_up` and `stack_restart`: a whole number of seconds, at least 1.
/// Omitted (or null) means the default. Checked before any lifecycle work.
fn startup_timeout(args: &Value) -> Result<Duration> {
    match &args["timeout_secs"] {
        Value::Null => Ok(session::DEFAULT_STARTUP_TIMEOUT),
        v => v
            .as_u64()
            .filter(|secs| *secs >= 1)
            .map(Duration::from_secs)
            .ok_or_else(|| StackError::new("usage", format!("timeout_secs {v} must be a whole number of seconds, at least 1"))),
    }
}

fn exec(args: &Value, ctx: &Ctx) -> Result<Value> {
    let command = strings(args, "command")?;
    let secrets = strings(args, "secrets")?;
    let require = if args["require_all"] == true { Require::All } else { Require::Only(strings(args, "require")?) };
    captured(args, ctx, || session::plan_exec_with(ctx, &command, &require, &Grant { keys: secrets, captured: true }))
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
    captured(args, ctx, || session::plan_task(ctx, task, &extra, true))
}

fn captured(args: &Value, ctx: &Ctx, plan: impl FnOnce() -> Result<session::ExecPlan>) -> Result<Value> {
    let plan = plan()?;
    let timeout = Duration::from_secs(
        args["timeout_secs"]
            .as_u64()
            .unwrap_or(DEFAULT_EXEC_TIMEOUT),
    );
    let result = run_captured(ctx, &plan, timeout)?;
    if result["timed_out"] == true {
        return Err(timed_out(timeout, result, "raise timeout_secs"));
    }
    Ok(result)
}

/// A command stack killed at its deadline did not do its job: `timed_out`, with the captured
/// result (output so far, checks) as the error's only detail.
pub fn timed_out(timeout: Duration, result: Value, raise: &str) -> StackError {
    StackError::new("timed_out", format!("command did not finish within {}s and was killed", timeout.as_secs()))
        .hint(format!("{raise} if it needs longer; its output so far is in details"))
        .details(vec![result])
}

/// Run a planned command with bounded output capture; shared by MCP and `stack exec --json`.
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
            .current_dir(&ctx.root),
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
        "unverified": plan.checks.iter().filter(|c| !c.ready).map(|c| &c.service).collect::<Vec<_>>(),
        "checks": plan.checks,
    });
    // Names only, and only for commands granted secrets, so other results keep their shape.
    let mut warnings = Vec::new();
    if !plan.secrets.is_empty() {
        result["secrets"] = json!(plan.secrets);
        warnings.extend(plan.secret_warnings.iter().cloned());
    }
    // `mise run` installs a task's missing tools against the rendered lock, and a refusal ends
    // in mise's advice to edit that lock. The output stays as the task wrote it; this says what
    // the remedy is under stack.
    if plan.task_config.is_some() && output.exit_code != Some(0) {
        if let Some(first) = crate::provider::mise::refusals(&output.stderr).first() {
            warnings.push(format!(
                "artifact_mismatch: {} (while `mise run` installed the task's tools); mise.lock is rendered from stack.lock, so do not edit it: {}",
                crate::provider::mise::refusal_message(first),
                crate::provider::mise::MISMATCH_HINT
            ));
        }
    }
    if !plan.secrets.is_empty() || !warnings.is_empty() {
        result["warnings"] = json!(warnings);
    }
    Ok(result)
}

/// `90`, `90s`, `30m`, `2h`, `1d`.
pub fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| StackError::new("usage", format!("invalid duration '{s}'")))?;
    let mult = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => {
            return Err(StackError::new(
                "usage",
                format!("invalid duration unit in '{s}'; use s, m, h or d"),
            ))
        }
    };
    n.checked_mul(mult)
        .ok_or_else(|| StackError::new("usage", "duration is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let called = call(&json!({ "name": "stack_nope", "arguments": { "dir": "/does/not/exist" } }));
        assert_eq!(called["structuredContent"]["error"]["code"], "unknown_tool");
        let called = call(&json!({ "name": "stack_status", "arguments": { "dir": "/does/not/exist", "extra": 1 } }));
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
    fn up_and_restart_advertise_their_timeout() {
        let tools = tools();
        for name in ["stack_up", "stack_restart"] {
            let tool = tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["inputSchema"]["properties"]["timeout_secs"]["minimum"], 1, "{tool}");
        }
    }
}

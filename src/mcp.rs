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
    json!({ "type": "object", "properties": p, "required": required })
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
        { "name": "stack_exec", "description": "Run a command with the stack's tools and env. command is an argv list run without a shell: `$VAR`, pipes and globs are not expanded, so use [\"sh\", \"-c\", \"...\"] for those. Connection variables of services that fail verification are withheld; required services must verify or the command does not run. A command still running at timeout_secs (default 600) is killed and the call fails with timed_out; its output so far is in the error details. secrets grants named fnox secrets to this command only; their values never appear in the result.",
          "inputSchema": schema(json!({
              "command": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
              "require": { "type": "array", "items": { "type": "string" } },
              "require_all": { "type": "boolean" },
              "secrets": { "type": "array", "items": { "type": "string", "pattern": "^[A-Z_][A-Z0-9_]*$" }, "description": "fnox secret names to grant this command, resolved through the stack's pinned fnox. Values are replaced by [redacted:KEY] in stdout and stderr; values shorter than 8 bytes are refused (secret_unsupported)" },
              "timeout_secs": { "type": "integer" }
          }), &["command"]) },
        { "name": "stack_run", "description": "Run a task declared in stack.toml ([tasks.<name>]) through mise's task runner, with the stack's tools and env. Every service of the project must verify or it does not run (mise gives a task every service's endpoint); use stack_exec for commands that should run with services down. Output is captured like stack_exec. The task receives exactly the secrets it declares (secrets = [...] in stack.toml), redacted from its output; no others can be added here.",
          "inputSchema": schema(json!({
              "task": { "type": "string" },
              "args": { "type": "array", "items": { "type": "string" }, "description": "Appended to the task's command" },
              "timeout_secs": { "type": "integer" }
          }), &["task"]) },
        { "name": "stack_renew", "description": "Renew this project's session lease.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_down", "description": "Stop services; succeeds only once their processes are confirmed gone.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_gc", "description": "Reclaim sessions with expired leases, and services of deleted projects, machine-wide. Fails (gc_incomplete) if any could not be confirmed stopped; ownership records are then kept.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_doctor", "description": "Check that the providers stack needs (mise, git, tar) are installed, that Pitchfork's socket path fits this platform, and that the project compiles.", "inputSchema": schema(json!({}), &[]) },
    ])
}

fn call(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];
    let outcome = ctx(args).and_then(|ctx| dispatch(name, args, &ctx));
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
    })?;
    let found = crate::skills::discover(&ctx.cache, &report.lock, &report.versions);
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
    let command: Vec<String> = args["command"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let secrets = secret_names(&args["secrets"])?;
    let require = if args["require_all"] == true {
        Require::All
    } else {
        Require::Only(
            args["require"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        )
    };
    captured(args, ctx, &command, &require, secrets)
}

/// `secrets` of `stack_exec`: omitted, or an array of names. Anything else is refused rather
/// than read as no grant.
fn secret_names(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|v| v.as_str().map(String::from).ok_or_else(|| StackError::new("usage", format!("secret name {v} is not a string"))))
            .collect(),
        v => Err(StackError::new("usage", format!("secrets must be an array of secret names, not {v}"))),
    }
}

fn run(args: &Value, ctx: &Ctx) -> Result<Value> {
    let task = args["task"].as_str().ok_or_else(|| StackError::new("usage", "task is required"))?;
    let extra: Vec<String> = args["args"]
        .as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if !args["secrets"].is_null() {
        return Err(StackError::new("usage", "stack_run grants exactly the secrets the task declares and accepts no others")
            .hint("declare them under [tasks.<name>] secrets = [...] in stack.toml, or use stack_exec with secrets"));
    }
    let (command, require, secrets) = session::task_command(ctx, task, &extra)?;
    captured(args, ctx, &command, &require, secrets)
}

fn captured(args: &Value, ctx: &Ctx, command: &[String], require: &Require, secrets: Vec<String>) -> Result<Value> {
    let plan = session::plan_exec_with(ctx, command, require, &Grant { keys: secrets, captured: true })?;
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
    if !plan.secrets.is_empty() {
        result["secrets"] = json!(plan.secrets);
        result["warnings"] = json!(plan.secret_warnings);
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
    fn up_and_restart_advertise_their_timeout() {
        let tools = tools();
        for name in ["stack_up", "stack_restart"] {
            let tool = tools.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(tool["inputSchema"]["properties"]["timeout_secs"]["minimum"], 1, "{tool}");
        }
    }
}

//! MCP server over stdio. Tools return the same `{ok, data | error}` envelope as `--json`.

use crate::error::{Result, StackError};
use crate::project::{self, default_cache_dir, Options};
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
                "instructions": "Use stack_up before work that needs services, stack_exec to run commands (unverified service endpoints are withheld), stack_status to diagnose, and stack_down when finished.",
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
        { "name": "stack_inspect", "description": "Show the composed stack (bundles, tools, env, services, tasks, ports, origins) without changing anything.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_compile", "description": "Resolve bundles, update stack.lock and the generated provider config.",
          "inputSchema": schema(json!({ "update": { "type": "boolean" }, "locked": { "type": "boolean" } }), &[]) },
        { "name": "stack_up", "description": "Start and verify services; records a session. Optional lease: ttl like '30m', or owner_pid.",
          "inputSchema": schema(json!({ "ttl": { "type": "string" }, "owner_pid": { "type": "integer" } }), &[]) },
        { "name": "stack_status", "description": "Live verification of every service, plus session and lease state.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_exec", "description": "Run a command with the stack's tools and env. Connection variables of services that fail verification are withheld; required services must verify or the command does not run.",
          "inputSchema": schema(json!({
              "command": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
              "require": { "type": "array", "items": { "type": "string" } },
              "require_all": { "type": "boolean" },
              "timeout_secs": { "type": "integer" }
          }), &["command"]) },
        { "name": "stack_renew", "description": "Renew this project's session lease.", "inputSchema": schema(json!({}), &[]) },
        { "name": "stack_down", "description": "Stop services; succeeds only once their processes are confirmed gone.", "inputSchema": schema(json!({}), &[]) },
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
    let compile = |mode, write| {
        project::compile(&Options {
            root: ctx.root.clone(),
            mode,
            write,
            cache: ctx.cache.clone(),
            state: ctx.state.clone(),
            reassign_ports: false,
        })
    };
    match name {
        "stack_inspect" => compile(Mode::Frozen, false).map(to_value),
        "stack_compile" => {
            let mode = if args["update"] == true {
                Mode::Update
            } else if args["locked"] == true {
                Mode::Frozen
            } else {
                Mode::UseLock
            };
            compile(mode, true).map(to_value)
        }
        "stack_up" => {
            let ttl_secs = args["ttl"].as_str().map(parse_duration).transpose()?;
            let owner_pid = args["owner_pid"].as_u64().map(|p| p as u32);
            session::up(
                ctx,
                LeaseOptions {
                    ttl_secs,
                    owner_pid,
                },
            )
            .map(to_value)
        }
        "stack_status" => session::status(ctx).map(to_value),
        "stack_renew" => session::renew(ctx).map(to_value),
        "stack_down" => session::down(ctx).map(to_value),
        "stack_exec" => exec(args, ctx),
        _ => Err(StackError::new(
            "unknown_tool",
            format!("no tool named '{name}'"),
        )),
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
    let plan = session::plan_exec(ctx, &command, &require)?;
    let timeout = Duration::from_secs(
        args["timeout_secs"]
            .as_u64()
            .unwrap_or(DEFAULT_EXEC_TIMEOUT),
    );

    let output = crate::process::capture(
        Command::new(&plan.program)
            .args(&plan.args)
            .env_clear()
            .envs(std::env::vars().filter(|(k, _)| !plan.removed.contains(k)))
            .envs(&plan.env)
            .current_dir(&ctx.root),
        timeout,
        OUTPUT_LIMIT,
    )
    .map_err(|e| {
        StackError::new(
            "exec_failed",
            format!("cannot execute {}: {e}", plan.program.display()),
        )
    })?;
    Ok(json!({
        "exit_code": output.exit_code,
        "timed_out": output.timed_out,
        "stdout": output.stdout,
        "stderr": output.stderr,
        "unverified": plan.checks.iter().filter(|c| !c.ready).map(|c| &c.service).collect::<Vec<_>>(),
        "checks": plan.checks,
    }))
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

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use stack::error::{Result, StackError};
use stack::manifest::read_bundle;
use stack::mcp::{self, parse_duration};
use stack::oci::{self, Reference};
use stack::project::{self, default_cache_dir, Options, Report};
use stack::compose::LoadedBundle;
use stack::session::{self, Ctx, LeaseOptions, Require};
use stack::source::Mode;
use stack::state::default_state_dir;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Parser)]
#[command(name = "stack", version, about = "Reusable, agent-safe development stacks")]
struct Cli {
    /// Project directory containing stack.toml
    #[arg(short = 'C', long = "dir", global = true, default_value = ".")]
    dir: PathBuf,
    /// Machine-readable output: a single JSON object on stdout
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Resolve bundles, write stack.lock and the generated provider config
    Compile {
        /// Re-resolve every bundle ref (tags and branches may move)
        #[arg(long)]
        update: bool,
        /// Fail instead of changing stack.lock
        #[arg(long, conflicts_with = "update")]
        locked: bool,
        /// Give this checkout fresh ports (e.g. after a port was taken by another program)
        #[arg(long)]
        reassign_ports: bool,
    },
    /// Show the composed stack without writing anything
    Inspect,
    /// Start services, verify each one, and record a session
    Up {
        /// Reclaim the session after this long without activity (e.g. 30m, 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Reclaim the session when this process exits (e.g. an agent runner)
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(session::MAX_OWNER_PID)))]
        owner_pid: Option<u32>,
    },
    /// Verify every service now, and show session and lease state
    Status,
    /// Run a command with the stack's tools and env; unverified endpoints are withheld
    Exec {
        /// Fail unless this service verifies (repeatable)
        #[arg(long = "require", value_name = "SERVICE")]
        require: Vec<String>,
        /// Fail unless every service verifies
        #[arg(long, conflicts_with = "require")]
        require_all: bool,
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Stop services; succeeds only once their processes are gone
    Down,
    /// Renew this project's session lease
    Renew,
    /// Reclaim sessions with expired leases or deleted projects (machine-wide)
    Gc,
    /// Publish a bundle directory to an OCI registry
    Publish {
        /// Directory containing bundle.toml
        bundle: PathBuf,
        /// Target, e.g. oci:ghcr.io/acme/pybase:1.0.0
        target: String,
    },
    /// Serve the stack tools over MCP (stdio)
    Mcp,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Cmd::Mcp = cli.cmd {
        return match mcp::serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(false, StackError::new("io", e.to_string())),
        };
    }
    let root = match cli.dir.canonicalize() {
        Ok(root) => root,
        Err(e) => return fail(cli.json, StackError::new("dir_not_found", format!("{}: {e}", cli.dir.display()))),
    };
    let ctx = Ctx { root: root.clone(), cache: default_cache_dir(), state: default_state_dir() };
    let opts = |mode, write, reassign_ports| Options {
        root: root.clone(),
        mode,
        write,
        cache: ctx.cache.clone(),
        state: ctx.state.clone(),
        reassign_ports,
    };

    let result: Result<ExitCode> = match &cli.cmd {
        Cmd::Compile { update, locked, reassign_ports } => {
            let mode = if *update { Mode::Update } else if *locked { Mode::Frozen } else { Mode::UseLock };
            project::compile(&opts(mode, true, *reassign_ports)).map(|r| report(cli.json, &r))
        }
        Cmd::Inspect => project::compile(&opts(Mode::Frozen, false, false)).map(|r| report(cli.json, &r)),
        Cmd::Up { ttl, owner_pid } => ttl
            .as_deref()
            .map(parse_duration)
            .transpose()
            .and_then(|ttl_secs| session::up(&ctx, LeaseOptions { ttl_secs, owner_pid: *owner_pid }))
            .map(|r| {
                emit(cli.json, &r, || {
                    for c in &r.checks {
                        println!("{:<12} port {:<5}  {}", c.service, c.port.unwrap_or(0), identity_label(c.identity));
                    }
                    println!("session {}", r.session.id);
                })
            }),
        Cmd::Status => session::status(&ctx).map(|r| {
            let healthy = !r.stale && r.checks.iter().all(|c| c.ready);
            let code = emit(cli.json, &r, || {
                match &r.session {
                    Some(s) => println!("session {}{}", s.id, if r.stale { " (stale: compiled configuration changed)" } else { "" }),
                    None => println!("no session"),
                }
                if let Some(why) = &r.lease_expired {
                    println!("lease: {why}");
                }
                for c in &r.checks {
                    match &c.reason {
                        None => println!("{:<12} ready  port {}  {}", c.service, c.port.unwrap_or(0), identity_label(c.identity)),
                        Some(reason) => println!("{:<12} NOT READY  {reason}", c.service),
                    }
                }
            });
            if healthy { code } else { ExitCode::FAILURE }
        }),
        Cmd::Exec { require, require_all, cmd } => {
            let req = if *require_all {
                Require::All
            } else if require.is_empty() {
                Require::Nothing
            } else {
                Require::Only(require.clone())
            };
            exec(&ctx, cmd, &req)
        }
        Cmd::Down => session::down(&ctx).map(|r| emit(cli.json, &r, || println!("stopped {} service(s); confirmed", r.stopped.len()))),
        Cmd::Renew => session::renew(&ctx).map(|s| emit(cli.json, &s, || println!("renewed session {}", s.id))),
        Cmd::Gc => session::gc(&ctx.state).map(|r| {
            emit(cli.json, &r, || {
                for e in &r {
                    println!("{}: {} ({})", e.project.display(), e.reason, if e.stopped { "stopped" } else { "not stopped" });
                }
                if r.is_empty() {
                    println!("nothing to reclaim");
                }
            })
        }),
        Cmd::Publish { bundle, target } => publish(bundle, target).map(|r| {
            emit(cli.json, &r, || println!("published {}\nuse: bundle = \"{}\"", r["digest"], r["pinned"].as_str().unwrap_or_default()))
        }),
        Cmd::Mcp => unreachable!("handled above"),
    };
    match result {
        Ok(code) => code,
        Err(e) => fail(cli.json, e),
    }
}

fn emit<T: Serialize>(as_json: bool, value: &T, human: impl FnOnce()) -> ExitCode {
    if as_json {
        println!("{}", json!({ "ok": true, "data": value }));
    } else {
        human();
    }
    ExitCode::SUCCESS
}

fn identity_label(identity: Option<session::Identity>) -> &'static str {
    match identity {
        Some(session::Identity::Instance) => "verified instance",
        Some(session::Identity::Liveness) => "liveness only",
        None => "-",
    }
}

fn report(as_json: bool, r: &Report) -> ExitCode {
    emit(as_json, r, || {
        for b in &r.bundles {
            let pin = b
                .commit
                .as_deref()
                .or(b.digest.as_deref())
                .map(|c| &c[..19.min(c.len())])
                .unwrap_or("local");
            println!("bundle {} @ {pin}  ({})", b.name, b.source);
            if let Some(from) = &b.moved_from {
                println!("  moved from {}", &from[..19.min(from.len())]);
            }
        }
        let s = &r.stack;
        println!("{} tools, {} env, {} services, {} tasks", s.tools.len(), s.env.len(), s.services.len(), s.tasks.len());
        for (name, port) in &r.ports {
            println!("port {name} = {port}");
        }
        for o in &s.overrides {
            let replaced = if o.replaced.is_empty() { "nothing".into() } else { o.replaced.join(", ") };
            println!("override {}.{} (replaced: {replaced})", o.kind, o.key);
        }
        if r.written {
            println!("wrote {}{}", r.output.display(), if r.lock_changed { " and stack.lock" } else { "" });
        }
    })
}

fn exec(ctx: &Ctx, cmd: &[String], require: &Require) -> Result<ExitCode> {
    let plan = session::plan_exec(ctx, cmd, require)?;
    for c in plan.checks.iter().filter(|c| !c.ready) {
        eprintln!(
            "stack: {} not verified ({}); endpoint withheld: {}",
            c.service,
            c.reason.as_deref().unwrap_or("unknown"),
            if c.withheld.is_empty() { "nothing".into() } else { c.withheld.join(", ") }
        );
    }
    let mut command = Command::new(&plan.program);
    for var in &plan.removed {
        command.env_remove(var);
    }
    command.args(&plan.args).envs(&plan.env).current_dir(&ctx.root);
    let status = command
        .status()
        .map_err(|e| StackError::new("exec_failed", format!("cannot start {}: {e}", plan.program.display())))?;
    Ok(ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8))
}

fn publish(bundle: &Path, target: &str) -> Result<serde_json::Value> {
    let dir = bundle
        .canonicalize()
        .map_err(|e| StackError::new("bundle_not_found", format!("{}: {e}", bundle.display())))?;
    let spec = target.strip_prefix("oci:").ok_or_else(|| {
        StackError::new("source_invalid", "publish target must start with oci:").hint("e.g. oci:ghcr.io/acme/pybase:1.0.0")
    })?;
    let reference = Reference::parse(spec)?;
    let manifest = read_bundle(&dir, &dir.display().to_string())?;
    let (name, version) = (manifest.bundle.name.clone(), manifest.bundle.version.clone());
    LoadedBundle::new(manifest, dir.clone())?; // same validation as consumers apply
    let digest = oci::Client::default().push(&dir, &reference, &name, version.as_deref())?;
    Ok(json!({
        "name": name,
        "digest": digest,
        "reference": target,
        "pinned": format!("oci:{}/{}@{digest}", reference.registry, reference.repository),
    }))
}

fn fail(as_json: bool, e: StackError) -> ExitCode {
    if as_json {
        println!("{}", json!({ "ok": false, "error": e }));
    } else {
        eprintln!("error[{}]: {}", e.code, e.message);
        if let Some(hint) = &e.hint {
            eprintln!("  hint: {hint}");
        }
        for d in &e.details {
            eprintln!("  {d}");
        }
    }
    ExitCode::FAILURE
}

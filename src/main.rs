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
use std::time::Duration;

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
    /// Install the locked tools and service binaries without starting anything
    Install,
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
        /// With --json: stop the command after this long (e.g. 10m). Default: no limit
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Stop services; succeeds only once their processes are gone
    Down,
    /// Renew this project's session lease
    Renew,
    /// Reclaim sessions with expired leases or deleted projects (machine-wide)
    Gc {
        /// Keep running in the foreground and collect every --interval (for a supervisor such as
        /// systemd or launchd; stack installs no service). With --json, one object per pass
        #[arg(long)]
        watch: bool,
        /// Time between passes with --watch (default 60s, at least 1s)
        #[arg(long, value_name = "DURATION", requires = "watch")]
        interval: Option<String>,
        /// Stop after this many passes with --watch (default: run until terminated)
        #[arg(long, value_name = "N", requires = "watch", value_parser = clap::value_parser!(u64).range(1..))]
        max_passes: Option<u64>,
    },
    /// Publish a bundle directory to an OCI registry
    Publish {
        /// Directory containing bundle.toml
        bundle: PathBuf,
        /// Target, e.g. oci:ghcr.io/acme/pybase:1.0.0
        target: String,
        /// Move the tag even if it already points to different content
        #[arg(long)]
        force: bool,
    },
    /// Check that mise, git and tar are usable and the project compiles
    Doctor,
    /// Serve the stack tools over MCP (stdio)
    Mcp,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return usage_error(e),
    };
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
        resolver: None,
    };

    let result: Result<ExitCode> = match &cli.cmd {
        Cmd::Compile { update, locked, reassign_ports } => {
            let mode = if *update { Mode::Update } else if *locked { Mode::Frozen } else { Mode::UseLock };
            project::compile(&opts(mode, true, *reassign_ports)).map(|r| report(cli.json, &r))
        }
        Cmd::Inspect => {
            project::compile(&opts(project::inspect_mode(&root), false, false)).map(|r| report(cli.json, &r))
        }
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
        Cmd::Install => session::install(&ctx).map(|r| {
            emit(cli.json, &r, || {
                for v in &r.versions {
                    println!("{:<12} {}", v.name, v.resolved.as_deref().unwrap_or("-"));
                }
                println!("installed; nothing started");
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
        Cmd::Exec { require, require_all, timeout, cmd } => {
            let req = if *require_all {
                Require::All
            } else if require.is_empty() {
                Require::Nothing
            } else {
                Require::Only(require.clone())
            };
            if cli.json {
                exec_json(&ctx, cmd, &req, timeout.as_deref())
            } else if timeout.is_some() {
                Err(StackError::new("usage", "--timeout applies only with --json")
                    .hint("without --json the command keeps the terminal; use your shell's `timeout`"))
            } else {
                exec(&ctx, cmd, &req)
            }
        }
        Cmd::Down => session::down(&ctx).map(|r| {
            emit(cli.json, &r, || {
                println!("stopped {} service(s); confirmed", r.stopped.len());
                for c in &r.conflicts {
                    println!("note: port {} reserved for {} is held by another program{}", c.port, c.service,
                        c.holder.as_ref().map(|h| format!(" (pid {} {})", h.pid, h.command)).unwrap_or_default());
                }
            })
        }),
        Cmd::Renew => session::renew(&ctx).map(|s| emit(cli.json, &s, || println!("renewed session {}", s.id))),
        Cmd::Gc { watch: true, interval, max_passes } => gc_watch(&ctx.state, cli.json, interval.as_deref(), *max_passes),
        Cmd::Gc { .. } => session::gc_checked(&ctx.state).map(|r| {
            emit(cli.json, &r, || print_gc(&r))
        }),
        Cmd::Publish { bundle, target, force } => publish(bundle, target, *force).map(|r| {
            emit(cli.json, &r, || println!("published {}\nuse: bundle = \"{}\"", r["digest"], r["pinned"].as_str().unwrap_or_default()))
        }),
        Cmd::Doctor => stack::doctor::run(&root, &ctx.cache, &ctx.state).map(|checks| {
            emit(cli.json, &checks, || {
                for c in &checks {
                    println!("ok    {:<13} {}", c.name, c.detail);
                }
            })
        }),
        Cmd::Mcp => unreachable!("handled above"),
    };
    match result {
        Ok(code) => code,
        Err(e) => fail(cli.json, e),
    }
}

fn print_gc(entries: &[session::GcEntry]) {
    for e in entries {
        println!("{}: {} ({})", e.project.display(), e.reason, if e.stopped { "stopped" } else { "not stopped" });
        if let Some(error) = &e.error {
            println!("  {error}");
        }
    }
    if entries.is_empty() {
        println!("nothing to reclaim");
    }
}

/// Foreground periodic collection. Each pass is reported as it completes (one JSON object per
/// line with --json). A failing pass is reported and retried at the next interval; the exit
/// code after --max-passes reflects the last pass.
fn gc_watch(state: &Path, as_json: bool, interval: Option<&str>, max_passes: Option<u64>) -> Result<ExitCode> {
    let interval = interval.map(parse_duration).transpose()?.unwrap_or(60);
    if interval == 0 {
        return Err(StackError::new("usage", "--interval must be at least 1s"));
    }
    let mut pass = 0u64;
    loop {
        pass += 1;
        let result = session::gc_checked(state);
        let ok = result.is_ok();
        if as_json {
            let line = match &result {
                Ok(entries) => json!({ "ok": true, "data": { "pass": pass, "at": stack::state::now(), "reclaimed": entries } }),
                Err(e) => json!({ "ok": false, "error": e, "pass": pass, "at": stack::state::now() }),
            };
            println!("{line}");
        } else {
            match &result {
                Ok(entries) if entries.is_empty() => {}
                Ok(entries) => print_gc(entries),
                Err(e) => eprintln!("error[{}]: {}", e.code, e.message),
            }
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
        if max_passes.is_some_and(|max| pass >= max) {
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
        std::thread::sleep(Duration::from_secs(interval));
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
        for v in &r.versions {
            let resolved = v.resolved.as_deref().unwrap_or("(not locked yet)");
            let moved = v.moved_from.as_deref().map(|m| format!("  (moved from {m})")).unwrap_or_default();
            println!("{} {} {} -> {resolved}{moved}", v.kind, v.name, v.requested);
        }
        for (name, port) in &r.ports {
            println!("port {name} = {port}");
        }
        for o in &s.overrides {
            let replaced = if o.replaced.is_empty() { "nothing".into() } else { o.replaced.join(", ") };
            println!("override {}.{} (replaced: {replaced})", o.kind, o.key);
        }
        for w in &r.warnings {
            eprintln!("warning: {w}");
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

/// One JSON object on stdout: the command's bounded output and exit code, never its raw stream.
/// The process exits with the command's code (124 on timeout, like `timeout(1)`).
fn exec_json(ctx: &Ctx, cmd: &[String], require: &Require, timeout: Option<&str>) -> Result<ExitCode> {
    // Large enough to mean "no limit" without overflowing deadline arithmetic.
    let timeout = match timeout {
        Some(t) => Duration::from_secs(parse_duration(t)?),
        None => Duration::from_secs(365 * 24 * 3600),
    };
    let plan = session::plan_exec(ctx, cmd, require)?;
    let result = mcp::run_captured(ctx, &plan, timeout)?;
    let code = if result["timed_out"] == true {
        124
    } else {
        result["exit_code"].as_i64().unwrap_or(1).clamp(0, 255) as u8
    };
    println!("{}", json!({ "ok": true, "data": result }));
    Ok(ExitCode::from(code))
}

fn publish(bundle: &Path, target: &str, force: bool) -> Result<serde_json::Value> {
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
    let digest = oci::Client::default().push(&dir, &reference, &name, version.as_deref(), force)?;
    Ok(json!({
        "name": name,
        "digest": digest,
        "reference": target,
        "pinned": format!("oci:{}/{}@{digest}", reference.registry, reference.repository),
    }))
}

/// Argument errors honour `--json` too; help and version output are not errors.
fn usage_error(e: clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand) {
        e.exit();
    }
    // `--json` after `--` belongs to the command being run, not to stack.
    // Compared as OS strings: arguments need not be Unicode, and this must not panic on them.
    let as_json = std::env::args_os().skip(1).take_while(|a| a != "--").any(|a| a == "--json");
    if !as_json {
        e.exit();
    }
    let rendered = e.render().to_string();
    let message = rendered
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_string();
    fail(true, StackError::new("usage", message).hint("run `stack --help` for usage"));
    ExitCode::from(2)
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

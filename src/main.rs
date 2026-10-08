use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use stack::error::{Result, StackError};
use stack::manifest::read_bundle;
use stack::mcp::{self, parse_duration};
use stack::oci::{self, Reference};
use stack::project::{self, default_cache_dir, Options, Report};
use stack::compose::LoadedBundle;
use stack::secrets::Grant;
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
    Inspect {
        /// Also list skills of tools stack adds for its provider (Pitchfork), under
        /// `provider_skills`. For people: an agent should not drive the supervisor directly
        #[arg(long)]
        all_skills: bool,
    },
    /// Install the locked tools and service binaries without starting anything
    Install,
    /// Show the last lines a service wrote, as kept by the supervisor
    Logs {
        /// Service name from the composed stack
        service: String,
        /// Number of lines from the end (default 100)
        #[arg(long, value_name = "N", default_value_t = 100, value_parser = clap::value_parser!(u64).range(1..=10_000))]
        tail: u64,
        /// Only output of the current process, not of earlier runs the supervisor kept
        #[arg(long)]
        since_start: bool,
    },
    /// Start services, verify each one, and record a session
    Up {
        /// Reclaim the session after this long without activity (e.g. 30m, 2h)
        #[arg(long)]
        ttl: Option<String>,
        /// Reclaim the session when this process exits. Pass a long-lived process such as the
        /// agent runner or CI job, not a shell that exits after this command
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(session::MAX_OWNER_PID)))]
        owner_pid: Option<u32>,
        /// Give up after this long (e.g. 5m), exit 124 and keep what was launched recorded for
        /// `status` and `down`. Covers the whole startup. Default 10m
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
    },
    /// Restart services of the running session (all when none are named), then verify again
    Restart {
        /// Services to restart; the others keep running
        services: Vec<String>,
        /// Give up after this long (e.g. 5m) and exit 124, like `up --timeout`. Default 10m
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
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
        /// Kill the command after this long (e.g. 10m) and exit 124. Default: no limit
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
        /// Grant this fnox secret to the command (repeatable), resolved through the stack's
        /// pinned fnox. With --json its value is redacted from the captured output (and values
        /// under 8 bytes are refused); without --json the command owns the terminal and its
        /// output is shown as written, unredacted
        #[arg(long = "secret", value_name = "KEY")]
        secret: Vec<String>,
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Run a task from stack.toml once every service verifies, granted the secrets it declares
    Run {
        /// Task name, as in [tasks.<name>]
        task: String,
        /// Kill the task after this long (e.g. 10m) and exit 124. Default: no limit
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
        /// Extra arguments, appended to the task's command
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
    /// Download mise into stack's data directory unless one is already on PATH
    Setup {
        /// Install stack's pinned mise even when another mise is on PATH
        #[arg(long)]
        force: bool,
    },
    /// Check that mise, git and tar are usable and the project compiles
    Doctor,
    /// Serve the stack tools over MCP (stdio)
    Mcp,
}

fn main() -> ExitCode {
    stack::setup::use_managed_tools();
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
        locker: None,
    };

    let result: Result<ExitCode> = match &cli.cmd {
        Cmd::Compile { update, locked, reassign_ports } => {
            let mode = if *update { Mode::Update } else if *locked { Mode::Frozen } else { Mode::UseLock };
            project::compile(&opts(mode, true, *reassign_ports)).map(|mut r| {
                r.attach_skills(&ctx.cache, false);
                report(cli.json, &r)
            })
        }
        Cmd::Inspect { all_skills } => project::compile(&opts(project::inspect_mode(&root), false, false)).map(|mut r| {
            r.warnings.extend(session::stale_session(&ctx, &r));
            r.attach_skills(&ctx.cache, *all_skills);
            report(cli.json, &r)
        }),
        Cmd::Up { ttl, owner_pid, timeout } => ttl
            .as_deref()
            .map(parse_duration)
            .transpose()
            .and_then(|ttl_secs| Ok((ttl_secs, startup_timeout(timeout.as_deref())?)))
            .and_then(|(ttl_secs, timeout)| session::up(&ctx, LeaseOptions { ttl_secs, owner_pid: *owner_pid }, timeout))
            .map(|r| {
                emit(cli.json, &r, || {
                    for c in &r.checks {
                        println!("{:<12} port {:<5}  {}", c.service, c.port.unwrap_or(0), identity_label(c.identity));
                    }
                    println!("session {}", r.session.id);
                    for w in &r.warnings {
                        eprintln!("warning: {w}");
                    }
                })
            }),
        Cmd::Install => session::install(&ctx).map(|r| {
            emit(cli.json, &r, || {
                for v in &r.versions {
                    println!("{:<12} {}", v.name, v.resolved.as_deref().unwrap_or("-"));
                }
                if let Some(detail) = r.steps.iter().find(|s| s["step"] == "install").map(|s| &s["detail"]) {
                    let names = |key: &str| detail[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                    println!("checked (mise install --locked): {}", names("locked"));
                    println!("unchecked (mise install): {}", names("plain"));
                }
                println!("installed; nothing started");
                for w in &r.warnings {
                    eprintln!("warning: {w}");
                }
            })
        }),
        Cmd::Logs { service, tail, since_start } => session::logs(&ctx, service, *tail as usize, *since_start).map(|r| {
            emit(cli.json, &r, || {
                // On stderr, so the lines themselves stay pipeable.
                if let (Some(at), false) = (r.started_at, r.since_start) {
                    eprintln!("# current {} process started {}s ago; --since-start shows only its output",
                        r.service, stack::state::now().saturating_sub(at));
                }
                for line in &r.lines {
                    println!("{line}");
                }
            })
        }),
        Cmd::Restart { services, timeout } => startup_timeout(timeout.as_deref())
            .and_then(|timeout| session::restart(&ctx, services, timeout))
            .map(|r| {
                emit(cli.json, &r, || {
                    for c in &r.checks {
                        let note = if r.restarted.contains(&c.service) { "restarted" } else { "" };
                        println!("{:<12} port {:<5}  {:<18} {note}", c.service, c.port.unwrap_or(0), identity_label(c.identity));
                    }
                    println!("session {}", r.session.id);
                })
            }),
        Cmd::Status => session::status(&ctx).map(|r| {
            let healthy = r.healthy;
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
                warn_changed(&r.checks);
            });
            if healthy { code } else { ExitCode::FAILURE }
        }),
        Cmd::Exec { require, require_all, timeout, secret, cmd } => {
            let req = if *require_all {
                Require::All
            } else if require.is_empty() {
                Require::Nothing
            } else {
                Require::Only(require.clone())
            };
            run_command(&ctx, cli.json, cmd, &req, secret.clone(), timeout.as_deref())
        }
        Cmd::Run { task, timeout, args } => session::task_command(&ctx, task, args)
            .and_then(|(cmd, req, secrets)| run_command(&ctx, cli.json, &cmd, &req, secrets, timeout.as_deref())),
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
        Cmd::Setup { force } => stack::setup::run(*force).map(|r| {
            emit(cli.json, &r, || {
                let how = if r.installed { "installed" } else { "already available" };
                println!("mise {} {how} at {}", r.version, r.mise.display());
                println!("ready; run `stack doctor` to check the rest");
            })
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
        // Like `timeout(1)`, and like `exec --timeout`.
        Err(e) if e.code == "timed_out" => {
            fail(cli.json, e);
            ExitCode::from(124)
        }
        Err(e) => fail(cli.json, e),
    }
}

/// `--timeout` of `up` and `restart`: at least one second, 10 minutes when omitted.
fn startup_timeout(timeout: Option<&str>) -> Result<Duration> {
    match timeout.map(parse_duration).transpose()? {
        None => Ok(session::DEFAULT_STARTUP_TIMEOUT),
        Some(0) => Err(StackError::new("usage", "--timeout must be at least 1s")),
        Some(secs) => Ok(Duration::from_secs(secs)),
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
                Err(e) => print_error(e),
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
        // Counts what is listed below, including the supervisor tool stack adds for services.
        let tools = r.versions.iter().filter(|v| v.kind == "tool").count().max(s.tools.len());
        println!("{tools} tools, {} env, {} services, {} tasks", s.env.len(), s.services.len(), s.tasks.len());
        for v in &r.versions {
            let resolved = v.resolved.as_deref().unwrap_or("(not locked yet)");
            let moved = v.moved_from.as_deref().map(|m| format!("  (moved from {m})")).unwrap_or_default();
            println!("{} {} {} -> {resolved}{moved}", v.kind, v.name, v.requested);
            if let Some(artifacts) = &v.artifacts {
                let states: Vec<String> = artifacts
                    .iter()
                    .map(|(platform, a)| format!("{platform} {}{}", a.state.name(), a.change.map(|c| format!(" ({c})")).unwrap_or_default()))
                    .collect();
                println!("  artifacts: {}", states.join(", "));
            }
        }
        // Custom services have no release to resolve; list them too.
        for (name, entry) in &s.services {
            if !r.versions.iter().any(|v| v.kind == "service" && v.name == *name) {
                let kind = if entry.value.preset.is_some() { "preset" } else { "custom" };
                println!("service {name} ({kind}, {})", entry.origin);
            }
        }
        for (name, port) in &r.ports {
            println!("port {name} = {port}");
        }
        for o in &s.overrides {
            let replaced = if o.replaced.is_empty() { "nothing".into() } else { o.replaced.join(", ") };
            println!("override {}.{} (replaced: {replaced})", o.kind, o.key);
        }
        for (list, label) in [(&r.skills, "skill"), (&r.provider_skills, "provider skill")] {
            for k in list.iter().flatten() {
                let at = k.version.as_deref().map(|v| format!("@{v}")).unwrap_or_default();
                let status = serde_json::to_value(k.status).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
                match (&k.name, &k.entrypoint) {
                    (Some(name), Some(entry)) if k.status == stack::skills::Status::Available => {
                        println!("{label} {}{at} {name}: {}", k.tool, entry.display())
                    }
                    _ => println!("{label} {}{at} {status}{}", k.tool, k.reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default()),
                }
            }
        }
        for w in &r.warnings {
            eprintln!("warning: {w}");
        }
        if r.written {
            println!("wrote {}{}", r.output.display(), if r.lock_changed { " and stack.lock" } else { "" });
        }
    })
}

fn run_command(ctx: &Ctx, as_json: bool, cmd: &[String], require: &Require, secrets: Vec<String>, timeout: Option<&str>) -> Result<ExitCode> {
    let timeout = timeout.map(parse_duration).transpose()?.map(Duration::from_secs);
    // Captured output is redacted, so values too short to redact are refused there; on the
    // terminal nothing is captured or redacted.
    let grant = Grant { keys: secrets, captured: as_json };
    if as_json {
        exec_json(ctx, cmd, require, &grant, timeout)
    } else {
        exec(ctx, cmd, require, &grant, timeout)
    }
}

/// Unverified services, one line per distinct reason.
fn warn_unverified(checks: &[session::Check]) {
    let mut reasons: Vec<(&str, Vec<&str>, Vec<&str>)> = Vec::new();
    for c in checks.iter().filter(|c| !c.ready) {
        let reason = c.reason.as_deref().unwrap_or("unknown");
        let i = reasons.iter().position(|(r, _, _)| *r == reason).unwrap_or_else(|| {
            reasons.push((reason, Vec::new(), Vec::new()));
            reasons.len() - 1
        });
        reasons[i].1.push(&c.service);
        reasons[i].2.extend(c.withheld.iter().map(String::as_str));
    }
    for (reason, services, withheld) in reasons {
        let withheld = if withheld.is_empty() { "nothing".into() } else { withheld.join(", ") };
        eprintln!("stack: not verified: {} ({reason}); withheld: {withheld}", services.join(", "));
    }
}

/// Services whose watched files changed after they started.
fn warn_changed(checks: &[session::Check]) {
    for c in checks.iter().filter(|c| c.watch_incomplete) {
        eprintln!("stack: {}'s watch paths hold too many entries to check them all; narrow `watch`", c.service);
    }
    for c in checks.iter().filter(|c| !c.changed_since_start.is_empty()) {
        eprintln!(
            "stack: {} changed after {} started; run `stack restart {}` to load it",
            c.changed_since_start.join(", "),
            c.service,
            c.service
        );
    }
}

fn exec(ctx: &Ctx, cmd: &[String], require: &Require, grant: &Grant, timeout: Option<Duration>) -> Result<ExitCode> {
    let plan = session::plan_exec_with(ctx, cmd, require, grant)?;
    warn_unverified(&plan.checks);
    warn_changed(&plan.checks);
    for warning in &plan.secret_warnings {
        eprintln!("stack: {warning}");
    }
    let mut command = Command::new(&plan.program);
    for var in &plan.removed {
        command.env_remove(var);
    }
    command.args(&plan.args).envs(&plan.env).current_dir(&ctx.root);
    let start_error = |e: std::io::Error| StackError::new("exec_failed", format!("cannot start {}: {e}", plan.program.display()));
    let code = match timeout {
        None => command.status().map_err(start_error)?.code(),
        Some(timeout) => {
            let (code, timed_out) = stack::process::run_with_deadline(&mut command, timeout).map_err(start_error)?;
            if timed_out {
                eprintln!("stack: command did not finish within {}s and was killed", timeout.as_secs());
                return Ok(ExitCode::from(124));
            }
            code
        }
    };
    Ok(ExitCode::from(code.unwrap_or(1).clamp(0, 255) as u8))
}

/// One JSON object on stdout: the command's bounded output and exit code, never its raw stream.
/// The process exits with the command's code (124 on timeout, like `timeout(1)`).
fn exec_json(ctx: &Ctx, cmd: &[String], require: &Require, grant: &Grant, timeout: Option<Duration>) -> Result<ExitCode> {
    // Large enough to mean "no limit" without overflowing deadline arithmetic.
    let timeout = timeout.unwrap_or(Duration::from_secs(365 * 24 * 3600));
    let plan = session::plan_exec_with(ctx, cmd, require, grant)?;
    let result = mcp::run_captured(ctx, &plan, timeout)?;
    if result["timed_out"] == true {
        println!("{}", json!({ "ok": false, "error": mcp::timed_out(timeout, result, "raise --timeout") }));
        return Ok(ExitCode::from(124));
    }
    let code = result["exit_code"].as_i64().unwrap_or(1).clamp(0, 255) as u8;
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
        print_error(&e);
    }
    ExitCode::FAILURE
}

/// An error for people, on stderr: the message, then the hint and details that say what to do.
fn print_error(e: &StackError) {
    eprintln!("error[{}]: {}", e.code, e.message);
    if let Some(hint) = &e.hint {
        eprintln!("  hint: {hint}");
    }
    for d in &e.details {
        eprintln!("  {}", human_detail(d));
    }
}

/// One error detail for people: `up`'s progress record as a step list, objects as
/// `key: value` pairs. `--json` keeps the structured form.
fn human_detail(detail: &serde_json::Value) -> String {
    use serde_json::Value;
    let scalar = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let Value::Object(map) = detail else { return scalar(detail) };
    // What a startup deadline cut short, as `timed_out` reports it.
    if let Some(cause) = map.get("cause").filter(|c| c["code"].is_string()) {
        let mut lines = vec![format!("cut short: error[{}]: {}", scalar(&cause["code"]), scalar(&cause["message"]))];
        lines.extend(cause["details"].as_array().into_iter().flatten().map(|d| format!("    {}", human_detail(d))));
        return lines.join("\n");
    }
    if let Some(Value::Array(steps)) = map.get("steps") {
        let steps: Vec<String> = steps
            .iter()
            .map(|s| match s["code"].as_str() {
                Some(code) => format!("{} {} ({code})", scalar(&s["step"]), scalar(&s["status"])),
                None => format!("{} {}", scalar(&s["step"]), scalar(&s["status"])),
            })
            .collect();
        return format!(
            "steps: {}; retry safe: {}; changed: {}",
            steps.join(", "),
            scalar(&map["retry_safe"]),
            scalar(&map["changed"])
        );
    }
    map.iter()
        .map(|(k, v)| match v {
            Value::Object(o) => format!(
                "{k}: {}",
                o.iter().map(|(k, v)| format!("{k} {}", scalar(v))).collect::<Vec<_>>().join(", ")
            ),
            v => format!("{k}: {}", scalar(v)),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

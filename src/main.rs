use clap::{Parser, Subcommand};
use serde_json::json;
use stack::error::{Result, StackError};
use stack::project::{self, default_cache_dir, Options, Report};
use stack::provider::mise;
use stack::source::Mode;
use std::path::PathBuf;
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
    },
    /// Show the composed stack without writing anything
    Inspect,
    /// Start the stack's services (delegates to `mise daemons start`)
    Up,
    /// Stop the stack's services (delegates to `mise daemons stop`)
    Down,
    /// Run a command with the stack's tools and environment
    Exec {
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = match cli.dir.canonicalize() {
        Ok(root) => root,
        Err(e) => return fail(cli.json, StackError::new("dir_not_found", format!("{}: {e}", cli.dir.display()))),
    };
    let opts = |mode, write| Options { root: root.clone(), mode, write, cache: default_cache_dir() };

    let result = match &cli.cmd {
        Cmd::Compile { update, locked } => {
            let mode = if *update { Mode::Update } else if *locked { Mode::Frozen } else { Mode::UseLock };
            project::compile(&opts(mode, true)).map(|r| report(cli.json, &r))
        }
        Cmd::Inspect => project::compile(&opts(Mode::Frozen, false)).map(|r| report(cli.json, &r)),
        Cmd::Up => delegate(&opts(Mode::Frozen, true), &["daemons", "start"], &[]),
        Cmd::Down => delegate(&opts(Mode::Frozen, true), &["daemons", "stop"], &[]),
        Cmd::Exec { cmd } => delegate(&opts(Mode::Frozen, true), &["exec", "--"], cmd),
    };
    match result {
        Ok(code) => code,
        Err(e) => fail(cli.json, e),
    }
}

fn report(as_json: bool, r: &Report) -> ExitCode {
    if as_json {
        println!("{}", json!({ "ok": true, "data": r }));
        return ExitCode::SUCCESS;
    }
    for b in &r.bundles {
        let pin = b.commit.as_deref().map(|c| &c[..12.min(c.len())]).unwrap_or("local");
        println!("bundle {} @ {pin}  ({})", b.name, b.source);
        if let Some(from) = &b.moved_from {
            println!("  moved from {}", &from[..12.min(from.len())]);
        }
    }
    let s = &r.stack;
    println!(
        "{} tools, {} env, {} services, {} tasks",
        s.tools.len(),
        s.env.len(),
        s.services.len(),
        s.tasks.len()
    );
    for o in &s.overrides {
        println!("override {}.{} (replaced: {})", o.kind, o.key, display_list(&o.replaced));
    }
    if r.written {
        println!("wrote {}{}", r.output.display(), if r.lock_changed { " and stack.lock" } else { "" });
    }
    ExitCode::SUCCESS
}

/// Run mise against the compiled stack. Refuses to run on a stale or missing lock.
fn delegate(opts: &Options, mise_args: &[&str], extra: &[String]) -> Result<ExitCode> {
    project::compile(opts)?;
    let config = mise::output_path(&opts.root);
    let mise_status = |args: &[&str]| {
        Command::new("mise")
            .args(args)
            .current_dir(&opts.root)
            .status()
            .map_err(|e| StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
                .hint("install mise: https://mise.jdx.dev"))
    };
    mise_status(&["trust", "--quiet", &config.to_string_lossy()])?;
    let mut args: Vec<&str> = mise_args.to_vec();
    args.extend(extra.iter().map(String::as_str));
    let status = mise_status(&args)?;
    Ok(ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8))
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

fn display_list(items: &[String]) -> String {
    if items.is_empty() { "nothing".into() } else { items.join(", ") }
}

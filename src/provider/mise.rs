//! mise installs tools; Pitchfork (via `mise daemons`) supervises services.

use crate::compose::Composed;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

/// Pinned so service behaviour only changes when stack is upgraded.
pub const PITCHFORK_VERSION: &str = "2.29.0";

/// mise auto-loads `conf.d/*.toml`, so plain `mise` commands see the stack too.
pub fn output_path(root: &Path) -> PathBuf {
    root.join(".config/mise/conf.d/stack.toml")
}

/// Exact versions rendered in place of the composed requests.
#[derive(Debug, Default)]
pub struct Versions {
    pub tools: IndexMap<String, String>,
    pub services: IndexMap<String, String>,
}

/// The provider tool a service preset installs, for presets whose mapping stack has verified
/// against mise (a `postgres` preset with `version = "17"` installs `postgres@17`).
pub fn preset_tool(preset: &str) -> Option<&'static str> {
    match preset {
        "postgres" => Some("postgres"),
        "redis" => Some("redis"),
        "cockroachdb" => Some("cockroach"),
        "nats" => Some("nats-server"),
        "spicedb" => Some("spicedb"),
        _ => None,
    }
}

/// Requests that name something other than a release, so no version can be locked.
pub fn unversioned(request: &str) -> bool {
    let r = request.trim();
    r == "system" || ["path:", "ref:"].iter().any(|p| r.starts_with(p))
}

/// Finds the exact release a version request currently means.
pub trait Resolver: Send + Sync {
    fn resolve(&self, tool: &str, request: &str) -> Result<String>;
}

/// `mise latest <tool>@<request>`, run outside any project so project configuration cannot
/// change which release a request means.
pub struct MiseResolver {
    pub cwd: PathBuf,
}

const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl Resolver for MiseResolver {
    fn resolve(&self, tool: &str, request: &str) -> Result<String> {
        std::fs::create_dir_all(&self.cwd).map_err(|e| crate::error::io_error(self.cwd.display(), e))?;
        // `latest` already interprets a bare version as a prefix; it rejects the explicit
        // `prefix:` spelling that other mise commands accept.
        let request = request.strip_prefix("prefix:").unwrap_or(request);
        let spec = if request.trim() == "latest" { tool.to_string() } else { format!("{tool}@{request}") };
        let mut command = Command::new("mise");
        configure_command(&mut command, &self.cwd);
        command.env("MISE_NO_CONFIG", "1")
            .args(["latest", &spec])
            .current_dir(&self.cwd)
            .env("MISE_YES", "1")
            .env("NO_COLOR", "1");
        let out = crate::process::capture(&mut command, RESOLVE_TIMEOUT, 16 * 1024).map_err(|e| {
            StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
                .hint(crate::setup::MISE_INSTALL_HINT)
        })?;
        let fail = |why: String| {
            StackError::new("resolve_failed", format!("cannot resolve {spec}: {why}"))
                .hint("check the tool name and version; `mise ls-remote <tool>` lists releases")
        };
        if out.timed_out && crate::process::expired() {
            return Err(StackError::new("timed_out", format!("resolving {spec} was cut short by the deadline")));
        }
        if out.timed_out {
            return Err(fail(format!("mise did not answer within {}s", RESOLVE_TIMEOUT.as_secs())));
        }
        if out.exit_code != Some(0) {
            let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("mise failed");
            return Err(fail(err.to_string()));
        }
        parse_resolved(&out.stdout).filter(|v| exact_release(tool, v)).ok_or_else(|| {
            // mise exits 0 with no output when no release matches the prefix.
            fail(if out.stdout.trim().is_empty() {
                "no release matches".to_string()
            } else {
                format!("unexpected output {:?}", out.stdout.trim())
            })
        })
    }
}

/// One version on one line; anything else is not an answer stack can lock.
pub fn parse_resolved(stdout: &str) -> Option<String> {
    let lines: Vec<&str> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    match lines.as_slice() {
        [v] if v.len() <= 128 && !v.contains(char::is_whitespace) && *v != "latest" => Some(v.to_string()),
        _ => None,
    }
}

/// Offline validation of release pins. Known providers have different release shapes:
/// PostgreSQL and jq use two numeric components, while Python/Node/Redis require three.
/// Other backends may use calendar or named releases, but never floating selectors.
pub fn exact_release(tool: &str, version: &str) -> bool {
    if version.is_empty() || version.len() > 128
        || !version.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        || !version.bytes().any(|b| b.is_ascii_digit())
        || version.starts_with("sub-") || version.starts_with("lts-")
    {
        return false;
    }
    let name = tool.rsplit([':', '/']).next().unwrap_or(tool);
    let components = match name {
        "python" | "node" | "nodejs" | "ruby" | "rust" | "uv" | "redis" | "pitchfork"
        | "cockroach" | "nats-server" | "spicedb" => 3,
        "postgres" | "postgresql" | "jq" | "go" => 2,
        _ => return !version.bytes().all(|b| b.is_ascii_digit()) || version.len() >= 8,
    };
    let numeric = version.trim_start_matches(|c: char| !c.is_ascii_digit());
    numeric.split('.').take(components).filter(|part| part.starts_with(|c: char| c.is_ascii_digit())).count() == components
}

/// `ports` are this checkout's assigned ports; every service gets a concrete one. `versions`
/// replace composed version requests with the exact versions stack.lock records.
/// `identities` are instance tokens, exported as `STACK_IDENTITY_<NAME>` for the service to
/// report back to its identity probe.
/// Environment the provider config adds beyond what the stack declares, given the exact
/// Python release stack.lock pins (if any). Part of the session fingerprint, so a change here
/// restarts services the way a declared change would.
pub fn implicit_env(stack: &Composed, python: Option<&str>) -> IndexMap<String, String> {
    let mut env = IndexMap::new();
    if !stack.tools.contains_key("python") {
        return env;
    }
    // uv prefers an active Conda environment, then Python builds it manages itself, over the
    // locked interpreter on PATH; `uv run` and `uv sync` would quietly use another release.
    // Name the locked release and allow only installed interpreters, so uv finds this one on
    // PATH (or a virtualenv built on it). A project's own `[env]` value wins for each variable.
    let mut add = |key: &str, value: &str| {
        if !stack.env.contains_key(key) {
            env.insert(key.to_string(), value.to_string());
        }
    };
    add("UV_PYTHON_PREFERENCE", "only-system");
    if let Some(version) = python.filter(|v| exact_release("python", v)) {
        add("UV_PYTHON", version);
    }
    env
}

pub fn render(
    stack: &Composed,
    ports: &IndexMap<String, u16>,
    versions: &Versions,
    identities: &IndexMap<String, String>,
) -> String {
    let mut doc = Table::new();
    let has_services = !stack.services.is_empty();

    if has_services {
        let mut settings = Table::new();
        settings.insert("experimental".into(), Value::Boolean(true));
        doc.insert("settings".into(), Value::Table(settings));
    }

    let mut tools: Table = stack
        .tools
        .iter()
        .map(|(k, e)| {
            let version = versions.tools.get(k).unwrap_or(&e.value);
            (k.clone(), Value::String(version.clone()))
        })
        .collect();
    if has_services && !tools.contains_key("pitchfork") {
        let version = versions.tools.get("pitchfork").map_or(PITCHFORK_VERSION, String::as_str);
        tools.insert("pitchfork".into(), Value::String(version.into()));
    }
    if !tools.is_empty() {
        doc.insert("tools".into(), Value::Table(tools));
    }

    let mut env: Table = stack
        .env
        .iter()
        .map(|(k, e)| (k.clone(), Value::String(e.value.clone())))
        .collect();
    for (key, value) in implicit_env(stack, versions.tools.get("python").map(String::as_str)) {
        env.insert(key, Value::String(value));
    }
    for (service, token) in identities {
        env.insert(crate::manifest::identity_var(service), Value::String(token.clone()));
    }
    if !stack.bin_paths.is_empty() {
        let paths = stack
            .bin_paths
            .iter()
            .map(|p| Value::String(p.to_string_lossy().into()))
            .collect();
        let mut directives = Table::new();
        directives.insert("path".into(), Value::Array(paths));
        env.insert("_".into(), Value::Table(directives));
    }
    if !env.is_empty() {
        doc.insert("env".into(), Value::Table(env));
    }

    if has_services {
        let daemons = stack
            .services
            .iter()
            .map(|(name, e)| {
                let mut t = Table::new();
                let s = &e.value;
                put(&mut t, "preset", s.preset.clone().map(Value::String));
                let version = versions.services.get(name).or(s.version.as_ref());
                put(&mut t, "version", version.cloned().map(Value::String));
                put(&mut t, "run", s.run.clone().map(Value::String));
                put(&mut t, "ready_cmd", s.ready_cmd.clone().map(Value::String));
                put(&mut t, "ready_port", s.ready_port.map(|p| Value::Integer(p.into())));
                if let Some(port) = ports.get(name) {
                    t.insert("port".into(), Value::Integer((*port).into()));
                    // Pitchfork accepts the first successful readiness check. Let TCP unblock
                    // a hanging preset probe only where stack subsequently verifies instance
                    // identity through the service protocol. Other presets need their own
                    // functional readiness checks; a listening port is not enough.
                    if matches!(s.preset.as_deref(), Some("postgres" | "redis"))
                        && s.ready_cmd.is_none() && s.ready_port.is_none()
                    {
                        t.insert("ready_port".into(), Value::Integer((*port).into()));
                    }
                }
                (name.clone(), Value::Table(t))
            })
            .collect();
        doc.insert("daemons".into(), Value::Table(daemons));
    }

    if !stack.tasks.is_empty() {
        let tasks = stack
            .tasks
            .iter()
            .map(|(name, e)| {
                let mut t = Table::new();
                let task = &e.value;
                t.insert("run".into(), Value::String(task.run.clone()));
                put(&mut t, "description", task.description.clone().map(Value::String));
                if !task.services.is_empty() {
                    let daemons = task.services.iter().cloned().map(Value::String).collect();
                    t.insert("daemons".into(), Value::Array(daemons));
                }
                (name.clone(), Value::Table(t))
            })
            .collect();
        doc.insert("tasks".into(), Value::Table(tasks));
    }

    let body = toml::to_string_pretty(&doc).expect("mise config serializes");
    format!("# Generated by `stack compile` from stack.toml + stack.lock. Do not edit.\n{body}")
}

fn put(t: &mut Table, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        t.insert(key.into(), v);
    }
}

// ---- runtime -------------------------------------------------------------------------------

use crate::error::{Result, StackError};
use serde::Deserialize;
use std::process::{Command, Output};

/// One supervised service as Pitchfork reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct DaemonStatus {
    /// Qualified supervisor id (`<namespace>/<name>`); valid without the project directory.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub data_dir: Option<String>,
}

/// Inherited provider settings may redefine aliases, backends or the config search path.
/// Keep storage and network credentials, but reserve configuration selection to Stack.
fn config_key(key: &str) -> bool {
    (key.starts_with("MISE_") && !matches!(key,
        "MISE_DATA_DIR" | "MISE_CACHE_DIR" | "MISE_STATE_DIR" | "MISE_GITHUB_TOKEN"))
        || key.starts_with("__MISE")
}

pub fn inherited_config_keys() -> Vec<String> {
    std::env::vars_os().filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| config_key(key)).collect()
}

/// One boundary for resolution, install, supervisor operations and nested mise commands.
pub fn config_env(root: &Path) -> IndexMap<String, String> {
    let isolated = root.join(".stack/provider-config");
    [
        ("MISE_CONFIG_DIR", isolated.clone()),
        ("MISE_SYSTEM_CONFIG_DIR", isolated.clone()),
        ("MISE_GLOBAL_CONFIG_FILE", isolated.join("global.toml")),
        ("MISE_SYSTEM_CONFIG_FILE", isolated.join("system.toml")),
        ("MISE_CEILING_PATHS", root.parent().unwrap_or(root).to_path_buf()),
    ].into_iter().map(|(key, value)| (key.into(), value.to_string_lossy().into_owned()))
        .chain([
            ("MISE_OVERRIDE_CONFIG_FILENAMES".into(), ".config/mise/conf.d/stack.toml".into()),
            ("MISE_OVERRIDE_TOOL_VERSIONS_FILENAMES".into(), "none".into()),
            ("MISE_IDIOMATIC_VERSION_FILE_ENABLE_TOOLS".into(), "".into()),
            ("MISE_ENV".into(), "".into()),
            ("MISE_AUTO_ENV".into(), "false".into()),
            ("MISE_YES".into(), "1".into()),
        ]).collect()
}

pub fn configure_command(command: &mut Command, root: &Path) {
    for key in inherited_config_keys() {
        command.env_remove(key);
    }
    command.envs(config_env(root));
}

/// A mise command, bounded by the caller's deadline (see `process::output`).
fn mise(root: &Path, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("mise");
    configure_command(&mut command, root);
    command.args(args)
        .current_dir(root)
        .env("MISE_YES", "1")
        .env("NO_COLOR", "1");
    crate::process::output(&mut command).map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut => StackError::new("timed_out", format!("mise {}: {e}", args.join(" "))),
        _ => StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
            .hint(crate::setup::MISE_INSTALL_HINT),
    })
}

/// Make sure Pitchfork's supervisor runs before a supervisor request under a deadline.
///
/// A request client that finds no supervisor starts one as its own child, in its own process
/// group. Under a deadline that group is killed when time runs out, and the supervisor of every
/// project using this state directory with it. Started here first, in a session of its own that
/// stack never signals, it is already running when the request comes. Best effort: should this
/// fail, the request starts the supervisor as it always did.
fn detach_supervisor(root: &Path) {
    let Some(wait) = crate::process::remaining() else { return };
    let mut command = Command::new("mise");
    configure_command(&mut command, root);
    command.args(["x", "--", "pitchfork", "supervisor", "start"])
        .current_dir(root)
        .env("MISE_YES", "1")
        .env("NO_COLOR", "1");
    let _ = crate::process::run_detached(&mut command, wait);
}

fn checked(root: &Path, args: &[&str], code: &'static str) -> Result<Output> {
    let out = mise(root, args)?;
    if out.status.success() {
        return Ok(out);
    }
    let failed = if crate::process::expired() { "was cut short by the deadline" } else { "failed" };
    Err(StackError::new(code, format!("mise {} {failed}", args.join(" ")))
        .details(vec![serde_json::json!({ "output": tail(&out) })]))
}

/// Lines of provider output kept in an error: enough for a traceback and the final error.
const TAIL_LINES: usize = 20;

/// Last lines of combined output, as a terminal would have left them.
pub fn tail(out: &Output) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    readable_tail(&text, TAIL_LINES)
}

/// The last `max` lines of terminal output in readable form: escape sequences removed, each
/// carriage-return redraw reduced to what it finally showed, blank lines dropped, and spinner
/// frames dropped unless they are all there is. The supervisor animates progress even when
/// it is captured, between the lines a failing service printed.
fn readable_tail(text: &str, max: usize) -> String {
    let plain = strip_escapes(text);
    let lines: Vec<&str> = plain
        .split('\n')
        .filter_map(|line| line.split('\r').rev().find(|s| !s.trim().is_empty()))
        .map(str::trim_end)
        .collect();
    let content: Vec<&str> = lines.iter().copied().filter(|l| !spinner_frame(l)).collect();
    let kept = if content.is_empty() { &lines[lines.len().saturating_sub(1)..] } else { &content[..] };
    kept[kept.len().saturating_sub(max)..].join("\n")
}

/// A progress line: it starts with a Braille spinner glyph.
fn spinner_frame(line: &str) -> bool {
    line.trim_start().chars().next().is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
}

/// `text` without ANSI escape sequences (CSI such as colours and line clearing, OSC such as
/// titles and hyperlinks, and two-character escapes) or other control characters. Newlines,
/// carriage returns and tabs are kept.
fn strip_escapes(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || matches!(c, '\n' | '\r' | '\t') {
                clean.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte.
            Some('[') => {
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST (ESC \\).
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    clean
}

pub fn trust(root: &Path) -> Result<()> {
    checked(root, &["trust", "--quiet", &output_path(root).to_string_lossy()], "provider_failed").map(|_| ())
}

pub fn install(root: &Path) -> Result<()> {
    checked(root, &["install", "--yes", "--quiet"], "install_failed").map(|_| ())
}

/// The environment mise would give a command: tools on PATH, env, service connection vars.
pub fn env(root: &Path) -> Result<IndexMap<String, String>> {
    let out = checked(root, &["env", "--json"], "provider_failed")?;
    let mut env: IndexMap<String, String> = serde_json::from_slice(&out.stdout)
        .map_err(|e| StackError::new("provider_failed", format!("unexpected `mise env --json` output: {e}")))?;
    env.retain(|key, _| !config_key(key));
    env.extend(config_env(root));
    Ok(env)
}

pub fn daemons(root: &Path) -> Result<Vec<DaemonStatus>> {
    let out = checked(root, &["daemons", "--json"], "provider_failed")?;
    serde_json::from_slice(&out.stdout)
        .map_err(|e| StackError::new("provider_failed", format!("unexpected `mise daemons --json` output: {e}")))
}

/// `env` and `daemons` at once. Both only read provider state for the same configuration, so
/// they run concurrently; errors are reported in the order the two used to run.
pub fn env_and_daemons(root: &Path) -> Result<(IndexMap<String, String>, Vec<DaemonStatus>)> {
    let (env, daemons) = std::thread::scope(|scope| {
        let statuses = scope.spawn(|| daemons(root));
        let env = env(root);
        let statuses = statuses
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (env, statuses)
    });
    Ok((env?, daemons?))
}

/// The Pitchfork binary mise runs for this project, so stopping does not need the project.
pub fn which_pitchfork(root: &Path) -> Option<PathBuf> {
    let out = mise(root, &["which", "pitchfork"]).ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (out.status.success() && path.is_absolute() && path.is_file()).then_some(path)
}

/// What Pitchfork reports about one qualified daemon id.
#[derive(Debug, Clone, PartialEq)]
pub enum Supervised {
    NotFound,
    Found { status: String, pid: Option<u32>, port: Option<u16> },
}

const SUPERVISOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn pitchfork(bin: &Path, state_dir: &Path, args: &[&str]) -> std::result::Result<crate::process::Captured, String> {
    let mut command = Command::new(bin);
    command
        .args(args)
        .env("PITCHFORK_STATE_DIR", state_dir)
        .env("NO_COLOR", "1");
    let out = crate::process::capture(&mut command, SUPERVISOR_TIMEOUT, 64 * 1024)
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if out.timed_out {
        return Err(format!("pitchfork {} did not answer within {}s", args.join(" "), SUPERVISOR_TIMEOUT.as_secs()));
    }
    Ok(out)
}

/// `pitchfork status --json <id>` against the recorded supervisor state directory.
pub fn supervised(bin: &Path, state_dir: &Path, id: &str) -> std::result::Result<Supervised, String> {
    let out = pitchfork(bin, state_dir, &["status", "--json", id])?;
    if out.exit_code != Some(0) {
        if out.stderr.contains("not found") {
            return Ok(Supervised::NotFound);
        }
        let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed");
        return Err(format!("pitchfork status {id}: {err}"));
    }
    let v: serde_json::Value = serde_json::from_str(&out.stdout)
        .map_err(|e| format!("unexpected `pitchfork status --json` output: {e}"))?;
    if v["id"].as_str() != Some(id) {
        return Err(format!("pitchfork status {id} described '{}'", v["id"]));
    }
    Ok(Supervised::Found {
        status: v["status"].as_str().unwrap_or_default().to_string(),
        pid: v["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
        port: v["active_port"].as_u64().and_then(|p| u16::try_from(p).ok()),
    })
}

const LOGS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const LOGS_LIMIT: usize = 1024 * 1024;

/// `mise daemons logs -- <service> -n <tail> --raw --no-pager`: the supervisor's stored output
/// for one daemon, through the same isolated configuration as every other provider call.
/// Bounded by a deadline and an output cap; the pager and follow modes are never used.
/// `since` (Unix seconds) keeps only lines the supervisor stored from that second onwards.
pub fn logs(root: &Path, service: &str, tail: usize, since: Option<u64>) -> Result<crate::process::Captured> {
    let n = tail.to_string();
    let mut command = Command::new("mise");
    configure_command(&mut command, root);
    command.args(["daemons", "logs", "--", service, "-n", &n, "--raw", "--no-pager"]);
    if let Some(at) = since {
        command.args(["--since", &since_argument(at, crate::state::now())]);
    }
    command
        .current_dir(root)
        .env("MISE_YES", "1")
        .env("NO_COLOR", "1");
    let out = crate::process::capture(&mut command, LOGS_TIMEOUT, LOGS_LIMIT).map_err(|e| {
        StackError::new("provider_unavailable", format!("cannot run mise: {e}")).hint(crate::setup::MISE_INSTALL_HINT)
    })?;
    if out.timed_out {
        return Err(StackError::new("logs_failed", format!("mise daemons logs did not finish within {}s", LOGS_TIMEOUT.as_secs())));
    }
    if out.exit_code != Some(0) {
        let detail = out.stderr.lines().chain(out.stdout.lines()).map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("failed").to_string();
        return Err(StackError::new("logs_failed", format!("mise daemons logs {service} failed"))
            .hint("the service may never have started here; `stack status` shows what the supervisor knows")
            .details(vec![serde_json::json!({ "output": detail })]));
    }
    Ok(out)
}

/// Pitchfork's `--since` for the instant `at`: local wall-clock time, which it reads as an
/// exact instant. A wall-clock time that names two instants (repeated when clocks go back, by
/// any amount up to three hours in 15-minute steps, which covers every zone in use) or that
/// cannot be formatted falls back to a relative age, rounded up so the start second is kept.
fn since_argument(at: u64, now: u64) -> String {
    since_argument_in(at, now, local_datetime)
}

fn since_argument_in(at: u64, now: u64, local_datetime: impl Fn(u64) -> Option<String>) -> String {
    let local = local_datetime(at);
    let ambiguous = local.is_none()
        || (1..=12u64).map(|k| k * 900).any(|shift| {
            local == local_datetime(at + shift) || local == at.checked_sub(shift).and_then(&local_datetime)
        });
    match local {
        Some(local) if !ambiguous => local,
        _ => format!("{}s", now.saturating_sub(at) + 1),
    }
}

/// `YYYY-MM-DD HH:MM:SS` in local time, the form Pitchfork's `--since` takes for an instant.
#[cfg(unix)]
fn local_datetime(secs: u64) -> Option<String> {
    let t = libc::time_t::try_from(secs).ok()?;
    // SAFETY: localtime_r writes only the provided struct.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

#[cfg(not(unix))]
fn local_datetime(_: u64) -> Option<String> {
    None
}

pub fn start(root: &Path) -> Result<()> {
    detach_supervisor(root);
    checked(root, &["daemons", "start"], "start_failed").map(|_| ())
}

pub fn stop(root: &Path) -> Result<()> {
    detach_supervisor(root);
    checked(root, &["daemons", "stop"], "stop_failed").map(|_| ())
}

/// Start only the named daemons of this project.
pub fn start_daemons(root: &Path, names: &[String]) -> Result<()> {
    let mut args = vec!["daemons", "start", "--"];
    args.extend(names.iter().map(String::as_str));
    detach_supervisor(root);
    checked(root, &args, "start_failed").map(|_| ())
}

/// Stop only the named daemons of this project.
pub fn stop_daemons(root: &Path, names: &[String]) -> Result<()> {
    let mut args = vec!["daemons", "stop", "--"];
    args.extend(names.iter().map(String::as_str));
    detach_supervisor(root);
    checked(root, &args, "stop_failed").map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `mise daemons start` printed for a service that failed on a bad edit, captured
    /// with NO_COLOR and CI set (DX evaluation of 0.1.18, log 130), with a shorter path.
    const FAILED_START: &str = "\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api] Traceback (most recent call last):\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api]   File \"/p/server.py\", line 2, in <module>\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api]     raise RuntimeError(\"bad edit\")\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api] RuntimeError: bad edit\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{280b} [app-9df2/api] waiting for delay (3s)...\n\rpitchfork ERROR Daemon app-9df2/api failed to start\n\u{2717} [app-9df2/api] failed (exit code 1): Daemon app-9df2/api failed with exit code 1";

    #[test]
    fn captured_spinner_frames_leave_the_traceback_and_the_final_error() {
        assert_eq!(
            readable_tail(FAILED_START, TAIL_LINES),
            "\u{2022} [app-9df2/api] Traceback (most recent call last):
\u{2022} [app-9df2/api]   File \"/p/server.py\", line 2, in <module>
\u{2022} [app-9df2/api]     raise RuntimeError(\"bad edit\")
\u{2022} [app-9df2/api] RuntimeError: bad edit
pitchfork ERROR Daemon app-9df2/api failed to start
\u{2717} [app-9df2/api] failed (exit code 1): Daemon app-9df2/api failed with exit code 1"
        );
    }

    #[test]
    fn redraws_keep_what_the_terminal_finally_showed() {
        assert_eq!(readable_tail("Downloading 10%\rDownloading 100%\r\nError: disk full\r\n", 5), "Downloading 100%\nError: disk full");
        assert_eq!(readable_tail("\u{280b} starting\r\u{2819} starting\r\u{2839} starting\n", 5), "\u{2839} starting");
        assert_eq!(readable_tail("", 5), "");
    }

    #[test]
    fn escape_sequences_and_controls_are_removed_but_text_is_kept() {
        let text = "\u{1b}[31mError:\u{1b}[0m port \u{1b}]8;;https://x/\u{7}5432\u{1b}]8;;\u{7} in use\u{1b}[2K\u{8}\n\u{1b}]0;title\u{1b}\\\tdétail ✓\u{1b}7";
        assert_eq!(readable_tail(text, 5), "Error: port 5432 in use\n\tdétail ✓");
    }

    #[test]
    fn only_the_last_lines_are_kept_with_the_error_last() {
        let text: String = (1..=30).map(|n| format!("\u{280b} wait\nline {n}\n")).collect::<String>() + "Error: final";
        let tail = readable_tail(&text, TAIL_LINES);
        assert_eq!(tail.lines().count(), TAIL_LINES);
        assert!(tail.starts_with("line 12\n") && tail.ends_with("line 30\nError: final"), "{tail}");
    }

    #[test]
    fn resolved_versions_are_one_exact_line() {
        assert_eq!(parse_resolved("3.13.16\n").as_deref(), Some("3.13.16"));
        assert_eq!(parse_resolved("\n  17.11  \n").as_deref(), Some("17.11"));
        for bad in ["", "\n", "latest\n", "3.13.1\n3.13.2\n", "a b\n"] {
            assert_eq!(parse_resolved(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn log_cutoffs_avoid_wall_clock_times_that_name_two_instants() {
        // A zone whose clocks go back an hour at 7200: 3600..7200 and 7200..10800 read alike.
        let wall = |t: u64| Some(format!("wall {}", if (7200..10800).contains(&t) { t - 3600 } else { t }));
        assert_eq!(since_argument_in(1000, 2000, wall), "wall 1000");
        assert_eq!(since_argument_in(5000, 9000, wall), "4001s", "first pass through the repeated hour");
        assert_eq!(since_argument_in(8000, 9000, wall), "1001s", "second pass");
        assert_eq!(since_argument_in(20_000, 20_005, wall), "wall 20000");
        assert_eq!(since_argument_in(5, 10, |_| None), "6s");
        // A 30-minute rollback at 7200 (as on Lord Howe Island): both passes are ambiguous.
        let half = |t: u64| Some(format!("wall {}", if (7200..9000).contains(&t) { t - 1800 } else { t }));
        assert_eq!(since_argument_in(6000, 9500, half), "3501s");
        assert_eq!(since_argument_in(7800, 9500, half), "1701s");
        assert_eq!(since_argument_in(5000, 9500, half), "wall 5000");
        assert!(local_datetime(1_791_404_602).is_some_and(|t| t.len() == 19));
    }

    #[test]
    fn unversioned_requests_are_recognised() {
        for r in ["system", "path:/opt/x", "ref:main"] {
            assert!(unversioned(r), "{r}");
        }
        for r in ["3.13", "latest", "lts", "17", "prefix:3", "sub-1:latest"] {
            assert!(!unversioned(r), "{r}");
        }
    }
}

// ---- supervisor socket ---------------------------------------------------------------------

/// Where Pitchfork will put its supervisor socket, and whether it fits the platform's
/// `sockaddr_un.sun_path`. Pitchfork refuses to start otherwise, after tools are installed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SocketPath {
    pub path: PathBuf,
    pub bytes: usize,
    pub limit: usize,
    /// Which setting decided the location.
    pub source: &'static str,
}

impl SocketPath {
    pub fn fits(&self) -> bool {
        self.bytes <= self.limit
    }

    pub fn error(&self) -> StackError {
        StackError::new(
            "socket_path_too_long",
            format!(
                "Pitchfork's supervisor socket {} is {} bytes; this platform allows {}",
                self.path.display(),
                self.bytes,
                self.limit
            ),
        )
        .hint("set PITCHFORK_STATE_DIR to a shorter absolute directory, e.g. PITCHFORK_STATE_DIR=/tmp/pitchfork-$USER")
        .with_detail(serde_json::to_value(self).expect("socket path serializes"))
    }
}

/// Bytes `sockaddr_un.sun_path` holds: 104 on macOS and the BSDs, 108 on Linux. Pitchfork
/// makes the same check before binding.
#[cfg(unix)]
pub fn socket_capacity() -> usize {
    // SAFETY: `sockaddr_un` is plain data; all-zero bytes are a valid value.
    let sun: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sun.sun_path.len()
}

/// The environment Pitchfork resolves its state directory from.
pub struct SocketEnv {
    /// `PITCHFORK_STATE_DIR` as the provider config sets it; mise passes config `[env]` to
    /// Pitchfork, so it takes precedence over stack's own environment.
    pub configured: Option<String>,
    pub cwd: Option<PathBuf>,
    pub state_dir: Option<std::ffi::OsString>,
    pub home: Option<std::ffi::OsString>,
    pub xdg_state_home: Option<std::ffi::OsString>,
    pub root: bool,
    /// Pitchfork reads `XDG_STATE_HOME` only where the `dirs` crate does: not on macOS.
    pub xdg: bool,
}

impl SocketEnv {
    pub fn current(configured: Option<String>) -> Self {
        Self {
            configured,
            cwd: std::env::current_dir().ok(),
            state_dir: std::env::var_os("PITCHFORK_STATE_DIR"),
            home: std::env::var_os("HOME"),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME"),
            #[cfg(unix)]
            // SAFETY: geteuid has no preconditions.
            root: unsafe { libc::geteuid() } == 0,
            #[cfg(not(unix))]
            root: false,
            xdg: !cfg!(target_os = "macos"),
        }
    }

    /// Overlay every location input with the environment that mise actually supplies.
    pub fn effective(root: &Path, env: &IndexMap<String, String>) -> Self {
        let mut socket = Self::current(env.get("PITCHFORK_STATE_DIR").cloned());
        socket.cwd = Some(root.to_path_buf());
        if let Some(home) = env.get("HOME") { socket.home = Some(home.into()); }
        if let Some(xdg) = env.get("XDG_STATE_HOME") { socket.xdg_state_home = Some(xdg.into()); }
        socket
    }

}

/// Pitchfork 2.29.0's rule (`src/env.rs`, `src/ipc/mod.rs`): `$PITCHFORK_STATE_DIR` (a leading
/// `~` expanded, and only if it is valid Unicode), else the XDG state directory on Linux when
/// `XDG_STATE_HOME` is absolute, else `$HOME/.local/state`, then `/pitchfork/sock/main.sock`.
/// `Err` explains why the path cannot be predicted (root uses `SUDO_USER`'s home).
pub fn socket_path(env: &SocketEnv, limit: usize) -> std::result::Result<SocketPath, String> {
    let home = || {
        env.home
            .as_ref()
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set; cannot predict Pitchfork's state directory".to_string())
    };
    let expand = |value: &str| -> std::result::Result<PathBuf, String> {
        match value.strip_prefix('~') {
            Some("") => home(),
            Some(rest) if rest.starts_with('/') => Ok(home()?.join(&rest[1..])),
            _ => Ok(PathBuf::from(value)),
        }
    };
    let (state, source) = if let Some(dir) = &env.configured {
        (expand(dir)?, "PITCHFORK_STATE_DIR in the provider config [env]")
    } else if let Some(dir) = env.state_dir.as_ref().and_then(|d| d.to_str()) {
        (expand(dir)?, "PITCHFORK_STATE_DIR")
    } else if env.root {
        return Err("running as root without PITCHFORK_STATE_DIR: the SUDO_USER fallback is not checked".into());
    } else if let Some(xdg) = env.xdg_state_home.as_ref().map(PathBuf::from).filter(|p| env.xdg && p.is_absolute()) {
        (xdg.join("pitchfork"), "XDG_STATE_HOME")
    } else {
        (home()?.join(".local/state/pitchfork"), "HOME")
    };
    let state = if state.is_absolute() { state } else {
        env.cwd.as_ref().filter(|cwd| cwd.is_absolute())
            .ok_or_else(|| "cannot resolve relative Pitchfork state without an absolute provider directory".to_string())?
            .join(state)
    };
    let path = state.join("sock").join("main.sock");
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    };
    #[cfg(not(unix))]
    let bytes = path.as_os_str().len();
    Ok(SocketPath { path, bytes, limit, source })
}

#[cfg(test)]
mod socket_tests {
    use super::*;

    fn env(home: &str) -> SocketEnv {
        SocketEnv { configured: None, cwd: Some("/project".into()), state_dir: None, home: Some(home.into()), xdg_state_home: None, root: false, xdg: true }
    }

    #[test]
    fn socket_path_follows_pitchfork_precedence() {
        let mut e = env("/home/u");
        assert_eq!(socket_path(&e, 108).unwrap().path, PathBuf::from("/home/u/.local/state/pitchfork/sock/main.sock"));
        e.xdg_state_home = Some("relative/state".into());
        assert_eq!(socket_path(&e, 108).unwrap().source, "HOME", "relative XDG paths are ignored");
        e.xdg_state_home = Some("/x".into());
        assert_eq!(socket_path(&e, 108).unwrap().path, PathBuf::from("/x/pitchfork/sock/main.sock"));
        e.xdg = false; // macOS
        assert_eq!(socket_path(&e, 104).unwrap().source, "HOME");
        e.state_dir = Some("~/pf".into());
        assert_eq!(socket_path(&e, 104).unwrap().path, PathBuf::from("/home/u/pf/sock/main.sock"));
        e.configured = Some("/c".into());
        let p = socket_path(&e, 104).unwrap();
        assert_eq!((p.path, p.source), (PathBuf::from("/c/sock/main.sock"), "PITCHFORK_STATE_DIR in the provider config [env]"));
        e.root = true;
        assert_eq!(socket_path(&e, 104).unwrap().path, PathBuf::from("/c/sock/main.sock"));
        e.configured = None;
        e.state_dir = None;
        assert!(socket_path(&e, 104).is_err());
    }

    #[test]
    fn effective_environment_and_relative_paths_use_the_provider_directory() {
        let mut e = env("/caller");
        e.home = Some("/effective".into());
        e.state_dir = Some("~/pf".into());
        assert_eq!(socket_path(&e, 108).unwrap().path, PathBuf::from("/effective/pf/sock/main.sock"));
        e.state_dir = Some("relative/pf".into());
        assert_eq!(socket_path(&e, 108).unwrap().path, PathBuf::from("/project/relative/pf/sock/main.sock"));
        e.state_dir = None;
        e.xdg_state_home = Some("/effective-state".into());
        assert_eq!(socket_path(&e, 108).unwrap().path, PathBuf::from("/effective-state/pitchfork/sock/main.sock"));
        let inputs = [("HOME".into(), "/configured-home".into()), ("XDG_STATE_HOME".into(), "/configured-state".into())].into_iter().collect();
        let effective = SocketEnv::effective(Path::new("/application"), &inputs);
        assert_eq!(effective.home, Some("/configured-home".into()));
        assert_eq!(effective.xdg_state_home, Some("/configured-state".into()));
        assert_eq!(effective.cwd, Some("/application".into()));
    }

    #[test]
    #[cfg(unix)]
    fn non_unicode_state_dirs_are_ignored_as_pitchfork_does() {
        use std::os::unix::ffi::OsStringExt;
        let mut e = env("/h");
        e.state_dir = Some(std::ffi::OsString::from_vec(b"/bad\xff".to_vec()));
        assert_eq!(socket_path(&e, 104).unwrap().source, "HOME");
    }

    #[test]
    fn length_is_counted_in_bytes_at_the_exact_boundary() {
        let suffix = "/sock/main.sock".len();
        for limit in [104usize, 108] {
            let mut e = env("/h");
            // 'é' is two bytes: a path of `limit` bytes fits, one more byte does not.
            let fill = |n: usize| format!("/{}", "é".repeat(n / 2) + &"a".repeat(n % 2));
            e.state_dir = Some(fill(limit - suffix - 1).into());
            let at = socket_path(&e, limit).unwrap();
            assert_eq!(at.bytes, limit);
            assert!(at.fits());
            e.state_dir = Some(fill(limit - suffix).into());
            let over = socket_path(&e, limit).unwrap();
            assert_eq!(over.bytes, limit + 1);
            assert!(!over.fits());
            assert!(over.path.to_str().unwrap().chars().count() < limit, "characters undercount bytes");
            assert_eq!(over.error().code, "socket_path_too_long");
        }
    }

    #[test]
    #[cfg(unix)]
    fn capacity_matches_the_platform() {
        assert_eq!(socket_capacity(), if cfg!(target_os = "macos") { 104 } else { 108 });
    }
}

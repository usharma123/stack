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
        _ => None,
    }
}

/// Requests that name something other than a release, so no version can be locked.
pub fn unversioned(request: &str) -> bool {
    let r = request.trim();
    r == "system" || ["path:", "ref:", "prefix:", "sub-"].iter().any(|p| r.starts_with(p))
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
        let spec = if request.trim() == "latest" { tool.to_string() } else { format!("{tool}@{request}") };
        let mut command = Command::new("mise");
        command
            .args(["latest", &spec])
            .current_dir(&self.cwd)
            .env("MISE_YES", "1")
            .env("NO_COLOR", "1");
        let out = crate::process::capture(&mut command, RESOLVE_TIMEOUT, 16 * 1024).map_err(|e| {
            StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
                .hint("install mise: https://mise.jdx.dev")
        })?;
        let fail = |why: String| {
            StackError::new("resolve_failed", format!("cannot resolve {spec}: {why}"))
                .hint("check the tool name and version; `mise ls-remote <tool>` lists releases")
        };
        if out.timed_out {
            return Err(fail(format!("mise did not answer within {}s", RESOLVE_TIMEOUT.as_secs())));
        }
        if out.exit_code != Some(0) {
            let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("mise failed");
            return Err(fail(err.to_string()));
        }
        parse_resolved(&out.stdout).ok_or_else(|| {
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

/// `ports` are this checkout's assigned ports; every service gets a concrete one. `versions`
/// replace composed version requests with the exact versions stack.lock records.
pub fn render(
    stack: &Composed,
    ports: &IndexMap<String, u16>,
    versions: &Versions,
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
use std::process::{Command, Output, Stdio};

/// One supervised service as Pitchfork reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct DaemonStatus {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub data_dir: Option<String>,
}

fn mise(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("mise")
        .args(args)
        .current_dir(root)
        .env("MISE_YES", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
                .hint("install mise: https://mise.jdx.dev")
        })
}

fn checked(root: &Path, args: &[&str], code: &'static str) -> Result<Output> {
    let out = mise(root, args)?;
    if out.status.success() {
        return Ok(out);
    }
    Err(StackError::new(code, format!("mise {} failed", args.join(" ")))
        .details(vec![serde_json::json!({ "output": tail(&out) })]))
}

/// Last lines of combined output, without terminal escape codes.
pub fn tail(out: &Output) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip CSI sequences: ESC [ params final-byte
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            continue;
        }
        clean.push(c);
    }
    let lines: Vec<&str> = clean.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(12)..].join("\n")
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
    serde_json::from_slice(&out.stdout)
        .map_err(|e| StackError::new("provider_failed", format!("unexpected `mise env --json` output: {e}")))
}

pub fn daemons(root: &Path) -> Result<Vec<DaemonStatus>> {
    let out = checked(root, &["daemons", "--json"], "provider_failed")?;
    serde_json::from_slice(&out.stdout)
        .map_err(|e| StackError::new("provider_failed", format!("unexpected `mise daemons --json` output: {e}")))
}

pub fn start(root: &Path) -> Result<()> {
    checked(root, &["daemons", "start"], "start_failed").map(|_| ())
}

pub fn stop(root: &Path) -> Result<()> {
    checked(root, &["daemons", "stop"], "stop_failed").map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_versions_are_one_exact_line() {
        assert_eq!(parse_resolved("3.13.16\n").as_deref(), Some("3.13.16"));
        assert_eq!(parse_resolved("\n  17.11  \n").as_deref(), Some("17.11"));
        for bad in ["", "\n", "latest\n", "3.13.1\n3.13.2\n", "a b\n"] {
            assert_eq!(parse_resolved(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn unversioned_requests_are_recognised() {
        for r in ["system", "path:/opt/x", "ref:main", "prefix:3", "sub-1:latest"] {
            assert!(unversioned(r), "{r}");
        }
        for r in ["3.13", "latest", "lts", "17"] {
            assert!(!unversioned(r), "{r}");
        }
    }
}

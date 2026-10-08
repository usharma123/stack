//! Per-task and per-command secret grants, resolved through the stack's pinned fnox.
//!
//! A task declares the names of the secrets it needs (`[tasks.x] secrets = [...]`); `stack exec
//! --secret KEY` and MCP `stack_exec.secrets` grant names for one command. Names are checked
//! when the stack compiles and when a command is planned: they must be environment variable
//! names, and none may name a variable stack itself controls (service endpoints, `[env]`,
//! `PATH`, `STACK_*`, `MISE_*`, `__MISE*`, `PG*`).
//!
//! Values are resolved only after services are verified, by running the fnox release stack.lock
//! pins, twice: `fnox env --json --describe` (value-free) to refuse keys stack cannot inject,
//! then `fnox env --json --keys <names>`. Both are non-interactive, bounded in time and output,
//! and killed with their process group at the deadline. The whole response is validated before
//! anything is applied. Nothing fnox prints (stdout, stderr, or a message inside its protocol)
//! is forwarded anywhere: errors carry stack's own text, key names, fnox's error kind, the exit
//! status and whether the deadline passed.
//!
//! Values never enter stack.lock, generated configuration, session records, timings or any
//! report. They live in the planned environment of one command and in the [`Redactor`] that
//! replaces them in captured output.

use crate::compose::Composed;
use crate::error::{Result, StackError};
use crate::process::{self, Captured, Redactor};
use crate::provider::mise::{self, Pin, ScratchRoot};
use crate::tool::ToolSpec;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The tool that resolves secrets. A project that grants any must list it in `[tools]`.
pub const FNOX: &str = "fnox";

/// Shortest value stack accepts when output is captured. Replacing every occurrence of a
/// shorter string would mangle output; leaving it would leak it. A usability threshold, not a
/// confidentiality property: a longer value is not safer, only legible to replace.
pub const MIN_CAPTURED_LEN: usize = 8;

/// Each fnox (and the provider query locating it) must answer within this, or the deadline.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Most bytes of fnox's stdout stack reads; a larger answer is refused, not truncated.
pub const RESOLVE_OUTPUT_LIMIT: usize = 64 * 1024;

/// `[A-Z_][A-Z0-9_]*`.
pub fn valid_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Why a grant may not name `key` in this stack, if it may not. Every variable of every service
/// (whether or not a task uses it), `[env]` keys, variables the provider config adds, and the
/// prefixes stack and mise reserve.
pub fn protected(key: &str, stack: &Composed) -> Option<String> {
    if key == "PATH" {
        return Some("PATH is set by the stack".into());
    }
    for (prefix, owner) in [("STACK_", "stack"), ("MISE_", "mise"), ("__MISE", "mise"), ("PG", "libpq (service endpoints)")] {
        if key.starts_with(prefix) {
            return Some(format!("{prefix}* is reserved for {owner}"));
        }
    }
    if let Some(e) = stack.env.get(key) {
        return Some(format!("defined in [env] ({})", e.origin));
    }
    if stack.tools.contains_key("python") && ["UV_PYTHON", "UV_PYTHON_PREFERENCE"].contains(&key) {
        return Some("set by stack for the locked Python".into());
    }
    for (name, e) in &stack.services {
        if service_vars(name, e.value.preset.as_deref()).iter().any(|v| v == key) {
            return Some(format!("an endpoint variable of service '{name}'"));
        }
    }
    None
}

/// The variables that point a command at a service, as stack withholds them.
fn service_vars(name: &str, preset: Option<&str>) -> Vec<String> {
    let mut vars: Vec<String> = match preset {
        Some("postgres") => vec!["DATABASE_URL".into()],
        Some("redis") => vec!["REDIS_URL".into()],
        _ => Vec::new(),
    };
    let folded = name.to_uppercase().replace('-', "_");
    vars.extend(["HOST", "PORT", "URL"].map(|s| format!("{folded}_{s}")));
    vars
}

/// How a grant was given, for error details.
#[derive(Clone, Copy)]
pub enum Origin<'a> {
    Task(&'a str),
    Command,
}

/// Check one grant's names against the stack: format, duplicates, protected variables, and
/// fnox among the tools at an exact release. `invalid_secret` lists every problem.
pub fn validate(keys: &[String], stack: &Composed, origin: Origin<'_>) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut problems: Vec<Value> = Vec::new();
    let mut detail = |key: &str, reason: String| {
        let mut d = json!({ "key": key, "operation": "declare", "reason": reason });
        if let Origin::Task(task) = origin {
            d["task"] = task.into();
        }
        problems.push(d);
    };
    for (i, key) in keys.iter().enumerate() {
        if !valid_name(key) {
            detail(key, "not an environment variable name ([A-Z_][A-Z0-9_]*)".into());
        } else if keys[..i].contains(key) {
            detail(key, "listed twice".into());
        } else if let Some(why) = protected(key, stack) {
            detail(key, format!("protected: {why}"));
        }
    }
    let fnox = stack.tools.get(FNOX);
    let where_ = match origin {
        Origin::Task(task) => format!("task '{task}'"),
        Origin::Command => "this command".into(),
    };
    if !problems.is_empty() {
        let first = problems[0]["key"].as_str().unwrap_or_default().to_string();
        return Err(StackError::new(
            "invalid_secret",
            format!("{} secret name(s) of {where_} cannot be granted; first: {first}", problems.len()),
        )
        .hint("secret names are upper-case variable names that no service, [env] entry or stack/mise variable uses")
        .details(problems));
    }
    match fnox {
        None => Err(StackError::new("invalid_secret", format!("{where_} is granted secrets, but fnox is not among the stack's tools"))
            .hint("add `fnox = \"<version>\"` to [tools] in stack.toml; stack resolves secrets only through the fnox release stack.lock pins")
            .details(keys.iter().map(|k| json!({ "key": k, "operation": "declare", "reason": "fnox is not in [tools]" })).collect())),
        Some(e) if mise::unversioned(&e.value.version) => Err(StackError::new(
            "invalid_secret",
            format!("tools.fnox = \"{}\" ({}) names no release, so stack cannot bind secrets to a pinned fnox", e.value.version, e.origin),
        )
        .hint("pin fnox to a version, e.g. fnox = \"1.39.0\"")
        .details(keys.iter().map(|k| json!({ "key": k, "operation": "declare", "reason": "fnox is not pinned" })).collect())),
        Some(_) => Ok(()),
    }
}

/// Every task's declared secrets, checked when the stack compiles.
pub fn validate_tasks(stack: &Composed) -> Result<()> {
    for (name, e) in &stack.tasks {
        validate(&e.value.secrets, stack, Origin::Task(name))
            .map_err(|err| StackError { message: format!("{} ({})", err.message, e.origin), ..err })?;
    }
    Ok(())
}

/// What a command is granted, and whether its output is captured (which refuses short values).
#[derive(Debug, Clone, Default)]
pub struct Grant {
    pub keys: Vec<String>,
    pub captured: bool,
}

/// Resolved values for one command. Neither `Debug` nor `Serialize`: it holds values.
pub struct Resolved {
    /// Granted names, in the order requested.
    pub keys: Vec<String>,
    /// Values to set, in the order requested.
    pub set: Vec<(String, String)>,
    /// Variables fnox asked to remove that stack may remove.
    pub remove: Vec<String>,
    /// Requests stack declined (protected variables fnox asked to remove).
    pub warnings: Vec<String>,
    pub redactor: Redactor,
}

/// Names only: values never reach a debug rendering.
impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved").field("keys", &self.keys).field("remove", &self.remove).field("warnings", &self.warnings).finish_non_exhaustive()
    }
}

/// Where and how fnox runs: the project, the cache for the provider query, the pinned release,
/// and the environment the command itself will get.
pub struct Request<'a> {
    pub root: &'a Path,
    pub cache: &'a Path,
    /// fnox at the exact release stack.lock pins, with its options.
    pub pin: ToolSpec,
    pub env: &'a IndexMap<String, String>,
    pub removed: &'a [String],
}

fn unavailable(message: impl Into<String>, detail: Value) -> StackError {
    StackError::new("secret_unavailable", message).with_detail(detail)
}

/// Locate the pinned fnox, ask it what it can inject, resolve `grant`'s keys and validate the
/// whole answer. `protected` says whether stack controls a variable in this command's
/// environment (a superset of the compile-time check: everything the planned environment sets
/// or withholds).
pub fn resolve(request: &Request<'_>, grant: &Grant, protected: impl Fn(&str) -> bool) -> Result<Resolved> {
    let dir = install_dir(request)?;
    let exe = locate(request, &dir)?;
    let described = run(request, &exe, &["env", "--json", "--describe"], "describe", &grant.keys)?;
    check_described(&described, &grant.keys)?;
    let joined = grant.keys.join(",");
    let answer = run(request, &exe, &["env", "--json", "--keys", &joined], "keys", &grant.keys)?;
    interpret(answer, grant, &protected)
}

/// The install directory mise reports for fnox at the pinned release, asked in a scratch root
/// whose only configuration is that one tool (no project template is evaluated).
fn install_dir(request: &Request<'_>) -> Result<PathBuf> {
    pinned_install_dir(request.cache, &request.pin)
}

fn pinned_install_dir(cache: &Path, pin: &ToolSpec) -> Result<PathBuf> {
    let version = pin.version.clone();
    let failed = |kind: &str, why: String| {
        unavailable(format!("cannot locate fnox {version}: {why}"), json!({ "step": "locate", "kind": kind, "timed_out": kind == "timed_out" }))
    };
    let scratch = ScratchRoot::create(cache, "secrets")?;
    scratch.write_tools(&[Pin { tool: FNOX.into(), spec: pin.clone() }])?;
    let mut command = scratch.command(&["ls", "--json", FNOX]);
    let out = process::capture(&mut command, RESOLVE_TIMEOUT, RESOLVE_OUTPUT_LIMIT)
        .map_err(|e| failed("provider", format!("cannot run mise: {e}")))?;
    if out.timed_out {
        return Err(failed("timed_out", "`mise ls` did not answer in time".into()));
    }
    if out.exit_code != Some(0) || out.stdout_truncated {
        return Err(failed("provider", format!("`mise ls --json fnox` failed (exit {})", exit_text(out.exit_code))));
    }
    let rows: Value = serde_json::from_str(out.stdout.trim()).map_err(|_| failed("provider", "`mise ls --json fnox` printed something stack cannot read".into()))?;
    // An array for one tool; older releases nest it under the tool's name.
    let rows = match &rows {
        Value::Array(rows) => rows.as_slice(),
        Value::Object(map) => map.get(FNOX).and_then(Value::as_array).map_or(&[][..], Vec::as_slice),
        _ => &[],
    };
    let row = rows.iter().find(|r| r["version"].as_str() == Some(version.as_str()));
    match row {
        Some(r) if r["installed"] == true => r["install_path"].as_str().map(PathBuf::from).ok_or_else(|| failed("provider", "mise reported no install path".into())),
        _ => Err(unavailable(
            format!("fnox {version} (pinned by stack.lock) is not installed"),
            json!({ "step": "locate", "kind": "not_installed", "timed_out": false }),
        )
        .hint("run `stack install`")),
    }
}

/// The fnox the command's PATH finds, accepted only as the pinned release: its path, before
/// links are resolved, lies in the pinned install directory, and after resolving links it is a
/// regular file still inside it. Returns the resolved file, which is what stack runs.
fn locate(request: &Request<'_>, dir: &Path) -> Result<PathBuf> {
    let version = &request.pin.version;
    let not_pinned = |path: &Path| {
        unavailable(
            format!("fnox on PATH is not the stack's pinned release (fnox {version})"),
            json!({ "step": "locate", "kind": "not_pinned", "path": path, "expected_dir": dir, "timed_out": false }),
        )
        .hint("another fnox comes first on the stack's PATH; remove it, or run `stack install`")
    };
    let found = crate::session::which_in(request.env.get("PATH").map(String::as_str), FNOX).ok_or_else(|| {
        unavailable("fnox is not on the stack's PATH", json!({ "step": "locate", "kind": "not_on_path", "timed_out": false }))
            .hint("run `stack install`")
    })?;
    let canonical_dir = dir.canonicalize().map_err(|_| not_pinned(&found))?;
    if !(found.starts_with(dir) || found.starts_with(&canonical_dir)) {
        return Err(not_pinned(&found));
    }
    let resolved = found.canonicalize().map_err(|_| not_pinned(&found))?;
    let regular = std::fs::metadata(&resolved).is_ok_and(|m| m.is_file());
    if !regular || !resolved.starts_with(&canonical_dir) {
        return Err(not_pinned(&found));
    }
    Ok(resolved)
}

fn exit_text(code: Option<i32>) -> String {
    code.map_or("by signal".into(), |c| c.to_string())
}

/// Run fnox once in the command's environment, non-interactively, and return its protocol
/// object. Every failure is reported without anything fnox printed.
fn run(request: &Request<'_>, exe: &Path, args: &[&str], step: &str, requested: &[String]) -> Result<serde_json::Map<String, Value>> {
    run_within(request, exe, args, step, requested, RESOLVE_TIMEOUT)
}

fn run_within(request: &Request<'_>, exe: &Path, args: &[&str], step: &str, requested: &[String], timeout: Duration) -> Result<serde_json::Map<String, Value>> {
    let mut command = Command::new(exe);
    for var in request.removed {
        command.env_remove(var);
    }
    command
        .envs(request.env)
        .env("FNOX_NON_INTERACTIVE", "1")
        .env("NO_COLOR", "1")
        // fnox's resolution daemon detaches into a session of its own, so the process-group
        // kill at the deadline would not reach it, and it keeps resolved values cached in
        // memory for hours. Stack resolves in the foreground only.
        .args(["--non-interactive", "--no-daemon"])
        .args(args)
        .current_dir(request.root);
    let out = process::capture(&mut command, timeout, RESOLVE_OUTPUT_LIMIT).map_err(|e| {
        unavailable(format!("cannot run fnox: {}", e.kind()), json!({ "step": step, "kind": "spawn", "timed_out": false }))
    })?;
    protocol(out, step, requested, timeout)
}

/// fnox's answer as a protocol object, or the error it maps to.
fn protocol(out: Captured, step: &str, requested: &[String], timeout: Duration) -> Result<serde_json::Map<String, Value>> {
    let detail = |kind: &str| json!({ "step": step, "kind": kind, "exit_code": out.exit_code, "timed_out": out.timed_out });
    if out.timed_out {
        return Err(unavailable(
            format!("fnox env ({step}) did not answer within {}s and was killed", process::bounded(timeout).as_secs().max(1)),
            detail("timed_out"),
        )
        .hint("fnox must resolve without prompting; check its provider can answer non-interactively"));
    }
    if out.stdout_truncated {
        return Err(unavailable(format!("fnox env ({step}) printed more than {RESOLVE_OUTPUT_LIMIT} bytes"), detail("oversized")));
    }
    let violation = || {
        unavailable(format!("fnox env ({step}) did not answer with its JSON protocol (exit {})", exit_text(out.exit_code)), detail("protocol"))
            .hint("run `fnox env --json --describe` in the project yourself to see fnox's own message")
    };
    // serde's messages can quote the input, so none is kept.
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(out.stdout.trim()) else {
        return Err(violation());
    };
    if object.get("schema") != Some(&json!(1)) {
        return Err(violation());
    }
    if let Some(error) = object.get("error") {
        let kind = error["kind"].as_str().filter(|k| safe_kind(k)).unwrap_or("unknown");
        if kind == "invalid_keys" {
            let unknown: Vec<&String> = requested
                .iter()
                .filter(|k| error["unknown"].as_array().is_some_and(|u| u.iter().any(|v| v.as_str() == Some(k.as_str()))))
                .collect();
            let listed: Vec<&String> = if unknown.is_empty() { requested.iter().collect() } else { unknown };
            return Err(StackError::new(
                "secret_missing",
                format!("fnox does not know {}", listed.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")),
            )
            .hint("define the key in fnox.toml for the active profile (FNOX_PROFILE), or remove the grant")
            .details(listed.iter().map(|k| json!({ "key": k, "reason": "unknown" })).collect()));
        }
        return Err(unavailable(format!("fnox env ({step}) failed with a {kind} error (exit {})", exit_text(out.exit_code)), detail(kind))
            .hint("run `fnox env --json --describe` in the project yourself to see fnox's own message; stack never shows it because it may quote secret values"));
    }
    if out.exit_code != Some(0) {
        return Err(violation());
    }
    Ok(object)
}

/// fnox error kinds are short identifiers; anything else is not echoed.
fn safe_kind(kind: &str) -> bool {
    (1..=40).contains(&kind.len()) && kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// What `stack doctor` reports about fnox: whether the stack lists it, whether the pinned
/// release is installed, and fnox's own value-free description of the declared keys. Never
/// resolves a value. `None` when the stack does not use fnox.
pub struct DoctorReport {
    pub ok: bool,
    pub detail: String,
}

pub fn doctor(root: &Path, cache: &Path, stack: &Composed, resolved: Option<&str>) -> Option<DoctorReport> {
    let declared: Vec<String> = stack.tasks.values().flat_map(|e| e.value.secrets.iter().cloned()).fold(Vec::new(), |mut acc, k| {
        if !acc.contains(&k) {
            acc.push(k);
        }
        acc
    });
    let entry = stack.tools.get(FNOX)?;
    let report = |ok: bool, detail: String| Some(DoctorReport { ok, detail });
    let Some(version) = resolved else {
        return report(true, format!("fnox {} ({}) is not locked yet; `stack compile` pins it", entry.value.version, entry.origin));
    };
    let pin = entry.value.at(version);
    let dir = match pinned_install_dir(cache, &pin) {
        Ok(dir) => dir,
        Err(e) if e.details.first().is_some_and(|d| d["kind"] == "not_installed") => {
            return report(true, format!("fnox {version} is pinned but not installed; `stack install` installs it"));
        }
        Err(e) => return report(false, e.message),
    };
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    let exe = [".mise-bins/fnox", "bin/fnox", "fnox"]
        .iter()
        .filter_map(|p| dir.join(p).canonicalize().ok())
        .find(|p| p.starts_with(&canonical) && std::fs::metadata(p).is_ok_and(|m| m.is_file()));
    let Some(exe) = exe else {
        return report(false, format!("fnox {version} is installed at {} but stack finds no fnox executable in it", dir.display()));
    };
    let env = IndexMap::new();
    let removed = mise::inherited_config_keys();
    let request = Request { root, cache, pin, env: &env, removed: &removed };
    let described = match run(&request, &exe, &["env", "--json", "--describe"], "describe", &declared) {
        Ok(described) => described,
        Err(e) => return report(false, format!("fnox {version}: {}", e.message)),
    };
    let count = described.get("keys").and_then(Value::as_array).map_or(0, Vec::len);
    match check_described(&described, &declared) {
        Ok(()) => report(true, format!("fnox {version} at {}; {count} key(s) described, {} declared by tasks and injectable", exe.display(), declared.len())),
        Err(e) => report(false, format!("fnox {version}: {}", e.message)),
    }
}

#[derive(Deserialize)]
struct Described {
    keys: Vec<DescribedKey>,
    #[serde(default)]
    dynamic_leases: Vec<Value>,
}

/// fnox 1.39.0 omits optional fields (`as_file`, `env`, `lease`) when they are not set.
#[derive(Deserialize)]
struct DescribedKey {
    key: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    lease: Option<Value>,
    #[serde(default)]
    as_file: bool,
    injectable: Injectable,
}

#[derive(Deserialize)]
struct Injectable {
    exec: bool,
}

/// Whether `value` names `key` anywhere (a lease is described by name, in some shape).
fn mentions(value: &Value, key: &str) -> bool {
    match value {
        Value::String(s) => s == key,
        Value::Array(items) => items.iter().any(|v| mentions(v, key)),
        Value::Object(map) => map.values().any(|v| mentions(v, key)),
        _ => false,
    }
}

/// Refuse, before any value is requested, keys fnox does not know or cannot inject into an
/// environment: file secrets, keys that are not exec-injectable, and leases.
fn check_described(object: &serde_json::Map<String, Value>, requested: &[String]) -> Result<()> {
    let described: Described = serde_json::from_value(Value::Object(object.clone())).map_err(|_| {
        unavailable("fnox env --describe did not answer with its JSON protocol", json!({ "step": "describe", "kind": "protocol", "timed_out": false }))
    })?;
    let mut missing = Vec::new();
    let mut unsupported = Vec::new();
    for key in requested {
        match described.keys.iter().find(|k| k.key == *key) {
            None => missing.push(json!({ "key": key, "reason": "unknown" })),
            Some(k) if k.kind.as_deref() == Some("lease") || k.lease.is_some() || described.dynamic_leases.iter().any(|l| mentions(l, key)) => {
                unsupported.push(json!({ "key": key, "reason": "supplied by a lease; use `stack exec -- fnox exec -- ...`" }));
            }
            Some(k) if k.as_file => unsupported.push(json!({ "key": key, "reason": "a file secret (as_file); use `stack exec -- fnox exec -- ...`" })),
            Some(k) if !k.injectable.exec => unsupported.push(json!({ "key": key, "reason": "not injectable into a command's environment (env = false or exec injection disabled)" })),
            Some(_) => {}
        }
    }
    if !missing.is_empty() {
        let names = key_list(&missing);
        return Err(StackError::new("secret_missing", format!("fnox does not know {names}"))
            .hint("define the key in fnox.toml for the active profile (FNOX_PROFILE), or remove the grant")
            .details(missing));
    }
    if !unsupported.is_empty() {
        let names = key_list(&unsupported);
        return Err(StackError::new("secret_unsupported", format!("stack cannot inject {names} into a command's environment"))
            .hint("`stack exec -- fnox exec -- <command>` supports file secrets and leases")
            .details(unsupported));
    }
    Ok(())
}

fn key_list(details: &[Value]) -> String {
    details.iter().filter_map(|d| d["key"].as_str()).collect::<Vec<_>>().join(", ")
}

#[derive(Deserialize)]
struct Answer {
    set: IndexMap<String, String>,
    #[serde(default)]
    files: serde_json::Map<String, Value>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    missing: Vec<Value>,
    #[serde(default)]
    leases: Vec<Value>,
}

/// Validate fnox's whole answer, then decide what to set and remove. Nothing is applied by
/// the caller unless this succeeds.
fn interpret(object: serde_json::Map<String, Value>, grant: &Grant, protected: &impl Fn(&str) -> bool) -> Result<Resolved> {
    let answer: Answer = serde_json::from_value(Value::Object(object)).map_err(|_| {
        unavailable("fnox env --keys did not answer with its JSON protocol", json!({ "step": "keys", "kind": "protocol", "timed_out": false }))
    })?;
    let requested = &grant.keys;
    let named = |values: &[Value]| -> Vec<String> { requested.iter().filter(|k| values.iter().any(|v| mentions(v, k))).cloned().collect() };

    if !answer.leases.is_empty() || !answer.files.is_empty() {
        let mut keys = named(&answer.leases);
        let files: Vec<String> = requested.iter().filter(|k| answer.files.contains_key(k.as_str()) && !keys.contains(k)).cloned().collect();
        keys.extend(files);
        let details = if keys.is_empty() {
            vec![json!({ "key": null, "reason": "fnox returned files or leases stack cannot inject" })]
        } else {
            keys.iter().map(|k| json!({ "key": k, "reason": "returned as a file or lease" })).collect()
        };
        return Err(StackError::new("secret_unsupported", "fnox answered with files or leases, which stack cannot inject")
            .hint("`stack exec -- fnox exec -- <command>` supports file secrets and leases")
            .details(details));
    }
    let mut unresolved = named(&answer.missing);
    if !answer.missing.is_empty() && unresolved.is_empty() {
        unresolved = requested.clone();
    }
    for key in requested {
        if !answer.set.contains_key(key) && !unresolved.contains(key) {
            unresolved.push(key.clone());
        }
    }
    if !unresolved.is_empty() {
        return Err(StackError::new("secret_missing", format!("fnox could not resolve {}", unresolved.join(", ")))
            .hint("check the key's provider can answer non-interactively (`fnox get <KEY>` in the project)")
            .details(unresolved.iter().map(|k| json!({ "key": k, "reason": "unresolved" })).collect()));
    }
    let touched: Vec<Value> = answer
        .set
        .keys()
        .filter(|k| protected(k))
        .map(|k| json!({ "key": if valid_name(k) { k.as_str() } else { "<not a variable name>" }, "operation": "set", "reason": "protected: stack controls this variable" }))
        .collect();
    if !touched.is_empty() {
        return Err(StackError::new("invalid_secret", format!("fnox tried to set {}, which stack controls; nothing was applied", key_list(&touched)))
            .hint("rename the secret in fnox.toml so it does not shadow a service endpoint or stack variable")
            .details(touched));
    }
    let mut refused = Vec::new();
    for key in requested {
        let value = &answer.set[key];
        if value.contains('\0') {
            refused.push(json!({ "key": key, "reason": "the value contains a NUL byte" }));
        } else if grant.captured && value.len() < MIN_CAPTURED_LEN {
            refused.push(json!({ "key": key, "reason": format!("the value is shorter than {MIN_CAPTURED_LEN} bytes, too short to redact from captured output") }));
        }
    }
    if !refused.is_empty() {
        return Err(StackError::new("secret_unsupported", format!("stack cannot grant {} here", key_list(&refused)))
            .hint("run without --json (the command then owns the terminal and nothing is captured), or use a longer value")
            .details(refused));
    }
    let mut warnings = Vec::new();
    let mut remove = Vec::new();
    for name in &answer.remove {
        if name.is_empty() || name.contains(['=', '\0']) || requested.contains(name) {
            continue;
        }
        if protected(name) {
            if valid_name(name) {
                warnings.push(format!("fnox asked to remove {name}; kept"));
            }
        } else if !remove.contains(name) {
            remove.push(name.clone());
        }
    }
    let mut redactor = Redactor::default();
    let mut set = Vec::new();
    for key in requested {
        let value = answer.set[key].clone();
        redactor.add(key, value.as_bytes());
        set.push((key.clone(), value));
    }
    // Dependencies fnox resolved along the way are never given to the command; replacing them
    // too costs nothing.
    for (key, value) in answer.set.iter().filter(|(k, v)| !requested.contains(k) && v.len() >= MIN_CAPTURED_LEN) {
        redactor.add(key, value.as_bytes());
    }
    Ok(Resolved { keys: requested.clone(), set, remove, warnings, redactor })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::Entry;
    use crate::manifest::{Service, Task};

    fn stack() -> Composed {
        let mut stack = Composed::default();
        stack.tools.insert("fnox".into(), Entry { value: ToolSpec::new("1.39.0"), origin: "project".into() });
        stack.env.insert("APP_MODE".into(), Entry { value: "dev".into(), origin: "bundle:base".into() });
        let service = |preset: Option<&str>, run: Option<&str>| Service {
            preset: preset.map(Into::into),
            version: None,
            run: run.map(Into::into),
            ready_cmd: None,
            ready_port: None,
            port: None,
            identity: None,
            watch: vec![],
        };
        stack.services.insert("db".into(), Entry { value: service(Some("postgres"), None), origin: "project".into() });
        stack.services.insert("cache".into(), Entry { value: service(Some("redis"), None), origin: "project".into() });
        stack.services.insert("search-api".into(), Entry { value: service(None, Some("x")), origin: "project".into() });
        stack
    }

    #[test]
    fn names_follow_the_variable_pattern() {
        for ok in ["A", "_X", "DEPLOY_KEY", "K2"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "lower", "2X", "A-B", "A B", "Ä", "A=B"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn every_service_env_and_reserved_variable_is_protected() {
        let s = stack();
        for key in [
            "PATH", "STACK_SESSION", "STACK_IDENTITY_DB", "MISE_DATA_DIR", "__MISE_DIFF", "PGHOST", "PGPASSWORD", "DATABASE_URL",
            "REDIS_URL", "DB_URL", "DB_PORT", "CACHE_HOST", "SEARCH_API_URL", "SEARCH_API_PORT", "APP_MODE",
        ] {
            assert!(protected(key, &s).is_some(), "{key}");
        }
        for key in ["DEPLOY_KEY", "SENTRY_DSN", "UV_PYTHON"] {
            assert!(protected(key, &s).is_none(), "{key}");
        }
    }

    #[test]
    fn invalid_grants_fail_early_listing_every_problem() {
        let s = stack();
        let keys: Vec<String> = ["DEPLOY_KEY", "bad-name", "DATABASE_URL", "DEPLOY_KEY"].map(String::from).to_vec();
        let e = validate(&keys, &s, Origin::Task("deploy")).unwrap_err();
        assert_eq!(e.code, "invalid_secret");
        let reasons: Vec<(&str, &str)> = e.details.iter().map(|d| (d["key"].as_str().unwrap(), d["operation"].as_str().unwrap())).collect();
        assert_eq!(reasons, [("bad-name", "declare"), ("DATABASE_URL", "declare"), ("DEPLOY_KEY", "declare")]);
        assert_eq!(e.details[0]["task"], "deploy");
        validate(&["DEPLOY_KEY".into()], &s, Origin::Command).unwrap();
    }

    #[test]
    fn grants_need_fnox_pinned_among_the_tools() {
        let mut s = stack();
        s.tools.shift_remove("fnox");
        let e = validate(&["DEPLOY_KEY".into()], &s, Origin::Command).unwrap_err();
        assert_eq!(e.code, "invalid_secret");
        assert!(e.message.contains("fnox is not among"), "{}", e.message);
        s.tools.insert("fnox".into(), Entry { value: ToolSpec::new("system"), origin: "project".into() });
        assert_eq!(validate(&["DEPLOY_KEY".into()], &s, Origin::Command).unwrap_err().code, "invalid_secret");
        // No grant, no requirement.
        validate(&[], &s, Origin::Command).unwrap();
        s.tasks.insert("t".into(), Entry { value: Task { run: "x".into(), description: None, services: vec![], secrets: vec!["K".into()] }, origin: "bundle:b".into() });
        let e = validate_tasks(&s).unwrap_err();
        assert!(e.message.ends_with("(bundle:b)"), "{}", e.message);
    }

    fn captured(stdout: &str, exit_code: Option<i32>) -> Captured {
        Captured { exit_code, timed_out: false, stdout: stdout.into(), stderr: "SENTINEL-STDERR quoted line".into(), stdout_truncated: false }
    }

    fn assert_clean(e: &StackError) {
        let text = serde_json::to_string(e).unwrap() + &e.to_string();
        for sentinel in ["SENTINEL", "quoted", "super-secret"] {
            assert!(!text.contains(sentinel), "{text}");
        }
    }

    #[test]
    fn protocol_errors_never_carry_what_fnox_printed() {
        let keys = vec!["DEPLOY_KEY".to_string()];
        let cases = [
            (r#"{"schema":1,"error":{"kind":"config","message":"SENTINEL super-secret = 1"}}"#, Some(1), "secret_unavailable"),
            (r#"{"schema":1,"error":{"kind":"SENTINEL Weird Kind","message":"x"}}"#, Some(1), "secret_unavailable"),
            (r#"{"schema":1,"error":{"kind":"invalid_keys","message":"SENTINEL","unknown":["DEPLOY_KEY","SENTINEL"]}}"#, Some(1), "secret_missing"),
            ("SENTINEL not json", Some(0), "secret_unavailable"),
            (r#"{"schema":2,"set":{}}"#, Some(0), "secret_unavailable"),
            (r#"{"schema":1,"set":{"DEPLOY_KEY":"super-secret-value"}}"#, Some(3), "secret_unavailable"),
            ("", None, "secret_unavailable"),
        ];
        for (stdout, code, expected) in cases {
            let e = protocol(captured(stdout, code), "keys", &keys, RESOLVE_TIMEOUT).unwrap_err();
            assert_eq!(e.code, expected, "{stdout}");
            assert_clean(&e);
        }
        let e = protocol(captured(r#"{"schema":1,"error":{"kind":"SENTINEL","message":"x"}}"#, Some(1)), "keys", &keys, RESOLVE_TIMEOUT).unwrap_err();
        assert_eq!(e.details[0]["kind"], "unknown");
        let e = protocol(captured(r#"{"schema":1,"error":{"kind":"invalid_keys","unknown":["DEPLOY_KEY"]}}"#, Some(1)), "keys", &keys, RESOLVE_TIMEOUT).unwrap_err();
        assert_eq!(e.details, vec![json!({ "key": "DEPLOY_KEY", "reason": "unknown" })]);
        let mut timed_out = captured("{", None);
        timed_out.timed_out = true;
        let e = protocol(timed_out, "describe", &keys, RESOLVE_TIMEOUT).unwrap_err();
        assert_eq!((e.code, e.details[0]["timed_out"].clone()), ("secret_unavailable", json!(true)));
        let mut big = captured("{}", Some(0));
        big.stdout_truncated = true;
        assert_eq!(protocol(big, "keys", &keys, RESOLVE_TIMEOUT).unwrap_err().details[0]["kind"], "oversized");
    }

    fn object(text: &str) -> serde_json::Map<String, Value> {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn describe_refuses_unknown_file_lease_and_non_exec_keys_before_values_are_requested() {
        let described = object(
            r#"{"schema":1,"keys":[
                {"key":"OK","kind":"secret","env":true,"as_file":false,"injectable":{"exec":true,"shell":true}},
                {"key":"FILE","kind":"secret","env":true,"as_file":true,"injectable":{"exec":true,"shell":true}},
                {"key":"HIDDEN","kind":"secret","env":false,"as_file":false,"injectable":{"exec":false,"shell":false}},
                {"key":"LEASED","kind":"lease","lease":"aws","injectable":{"exec":true,"shell":true}},
                {"key":"BY_LEASE","kind":"secret","lease":"aws","injectable":{"exec":true,"shell":true}},
                {"key":"MINIMAL","kind":"secret","injectable":{"exec":true,"shell":true}}],
               "dynamic_leases":["DYNAMIC"]}"#,
        );
        // Optional fields fnox omits when unset are not required.
        check_described(&described, &["OK".into(), "MINIMAL".into()]).unwrap();
        for (key, code) in [("NOPE", "secret_missing"), ("FILE", "secret_unsupported"), ("HIDDEN", "secret_unsupported"), ("LEASED", "secret_unsupported"), ("BY_LEASE", "secret_unsupported")] {
            let e = check_described(&described, &[key.into()]).unwrap_err();
            assert_eq!((e.code, e.details[0]["key"].as_str()), (code, Some(key)));
        }
        let e = check_described(&object(r#"{"schema":1}"#), &["OK".into()]).unwrap_err();
        assert_eq!(e.code, "secret_unavailable");
    }

    fn grant(keys: &[&str], captured: bool) -> Grant {
        Grant { keys: keys.iter().map(|k| k.to_string()).collect(), captured }
    }

    #[test]
    fn answers_apply_only_requested_keys_and_never_touch_protected_variables() {
        let protected = |k: &str| ["DATABASE_URL", "PGHOST", "PATH", "MISE_X", "__MISE_Y", "STACK_Z"].contains(&k);
        let answer = object(
            r#"{"schema":1,"set":{"DEPLOY_KEY":"deploy-value-123","DEP":"dependency-value-9"},"files":{},
                "remove":["HIDDEN","DATABASE_URL","PGHOST","PATH","MISE_X","__MISE_Y","STACK_Z","bad=name"],"missing":[],"leases":[]}"#,
        );
        let r = interpret(answer, &grant(&["DEPLOY_KEY"], true), &protected).unwrap();
        assert_eq!(r.keys, ["DEPLOY_KEY"]);
        assert_eq!(r.set, [("DEPLOY_KEY".to_string(), "deploy-value-123".to_string())]);
        assert_eq!(r.remove, ["HIDDEN"]);
        assert_eq!(r.warnings.len(), 6, "{:?}", r.warnings);
        assert_eq!(r.warnings[0], "fnox asked to remove DATABASE_URL; kept");
        assert_eq!(r.redactor.redact("deploy-value-123 dependency-value-9"), "[redacted:DEPLOY_KEY] [redacted:DEP]");

        let bad = object(r#"{"schema":1,"set":{"DEPLOY_KEY":"deploy-value-123","PGHOST":"evil.example"},"files":{},"remove":[],"missing":[],"leases":[]}"#);
        let e = interpret(bad, &grant(&["DEPLOY_KEY"], true), &protected).unwrap_err();
        assert_eq!((e.code, e.details[0]["operation"].as_str()), ("invalid_secret", Some("set")));
        assert!(!e.to_string().contains("evil") && !serde_json::to_string(&e).unwrap().contains("deploy-value"));
    }

    #[test]
    fn missing_files_leases_and_short_values_map_to_their_codes() {
        let none = |_: &str| false;
        let cases = [
            (r#"{"schema":1,"set":{},"missing":["DEPLOY_KEY"]}"#, "secret_missing", "unresolved"),
            (r#"{"schema":1,"set":{}}"#, "secret_missing", "unresolved"),
            (r#"{"schema":1,"set":{"DEPLOY_KEY":"long-enough-1"},"files":{"DEPLOY_KEY":"/tmp/x"}}"#, "secret_unsupported", "returned as a file or lease"),
            (r#"{"schema":1,"set":{"DEPLOY_KEY":"long-enough-1"},"leases":[{"key":"DEPLOY_KEY"}]}"#, "secret_unsupported", "returned as a file or lease"),
            (r#"{"schema":1,"set":{"DEPLOY_KEY":"seven77"}}"#, "secret_unsupported", "the value is shorter than 8 bytes, too short to redact from captured output"),
            (r#"{"schema":1,"set":{"DEPLOY_KEY":"nul\u0000inside-it"}}"#, "secret_unsupported", "the value contains a NUL byte"),
            (r#"{"schema":1,"set":{"DEPLOY_KEY":7}}"#, "secret_unavailable", ""),
        ];
        for (text, code, reason) in cases {
            let e = interpret(object(text), &grant(&["DEPLOY_KEY"], true), &none).unwrap_err();
            assert_eq!(e.code, code, "{text}");
            if !reason.is_empty() {
                assert_eq!(e.details[0]["reason"], reason, "{text}");
            }
            assert!(!serde_json::to_string(&e).unwrap().contains("seven77"));
        }
        // Terminal mode has no minimum.
        let r = interpret(object(r#"{"schema":1,"set":{"DEPLOY_KEY":"seven77"}}"#), &grant(&["DEPLOY_KEY"], false), &none).unwrap();
        assert_eq!(r.set[0].1, "seven77");
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_fnox_is_killed_with_its_process_group_and_reported_without_its_output() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("fnox");
        let pid_file = dir.path().join("child");
        std::fs::write(&exe, format!("#!/bin/sh\necho SENTINEL-partial\necho SENTINEL-err >&2\nsleep 30 &\necho $! >{}\nwait\n", pid_file.display())).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = IndexMap::new();
        let request = Request { root: dir.path(), cache: dir.path(), pin: ToolSpec::new("1.39.0"), env: &env, removed: &[] };
        let start = std::time::Instant::now();
        let e = run_within(&request, &exe, &["env", "--json", "--describe"], "describe", &["K".into()], Duration::from_millis(1500)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(e.code, "secret_unavailable");
        assert_eq!((e.details[0]["timed_out"].clone(), e.details[0]["kind"].clone()), (json!(true), json!("timed_out")));
        assert_clean(&e);
        let child: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while crate::state::pid_alive(child as u32) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!crate::state::pid_alive(child as u32), "fnox's descendant survived the deadline");
    }
}

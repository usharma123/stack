//! Init tasks: `[services.<name>] init = ["migrate", "seed"]`.
//!
//! A service's `setup` (see [`crate::session`]) runs before every start of the service, so it
//! must be safe to repeat. Init tasks are its counterpart for work that must happen once: `up`
//! runs a service's init tasks, in order, after every service verifies (so they reach the
//! running, healthy service, as migrations against a live Postgres need), and only when no
//! completed-init marker matches the service's current data directory. The marker lives in
//! machine state (never inside the data directory), keyed by checkout and service, and records
//! where the data directory resolved and which directory it is, as [`crate::datadirs`] records
//! them, and a digest of the init list and the task definitions it ran. So:
//!
//! - a data directory deleted and recreated (a new inode) runs init again;
//! - `up` on an initialised database never runs it again;
//! - a changed init list or task definition is reported as `stale` and nothing runs until
//!   `up --reinit <service>`;
//! - a failed task is recorded; the next `up` resumes from it, skipping the tasks before it
//!   whose definitions have not changed.
//!
//! A service without a data directory (a plain command) is keyed by checkout and service only:
//! its init runs once per checkout, until `--reinit`.

use crate::compose::{Composed, Entry};
use crate::error::{Result, StackError};
use crate::hash::sha256_hex;
use crate::manifest::Service;
use crate::session::{self, Ctx, UpOptions};
use crate::state::{now, project_key, read_json, write_json, FileLock};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Split one layer's service tables into definitions and init lists. A table that sets only
/// `init` contributes no definition.
pub fn split(services: &IndexMap<String, Service>) -> (IndexMap<String, Service>, IndexMap<String, Vec<String>>) {
    let mut definitions = IndexMap::new();
    let mut init = IndexMap::new();
    for (name, service) in services {
        if let Some(list) = &service.init {
            init.insert(name.clone(), list.clone());
        }
        if !service.init_only() {
            definitions.insert(name.clone(), Service { init: None, ..service.clone() });
        }
    }
    (definitions, init)
}

fn invalid(message: String, hint: &str, detail: Value) -> StackError {
    StackError::new("init_invalid", message).hint(hint).with_detail(detail)
}

/// Compile-time checks of the composed init lists.
pub fn validate(stack: &Composed) -> Result<()> {
    for (service, entry) in &stack.init {
        let origin = &entry.origin;
        if !stack.services.contains_key(service) {
            return Err(invalid(
                format!("services.{service}.init ({origin}) names a service no layer defines"),
                "a table with only `init` adds init tasks to a service a bundle or the project defines; check the service name",
                json!({ "service": service, "origin": origin }),
            ));
        }
        let mut seen: Vec<&String> = Vec::new();
        for task in &entry.value {
            let detail = json!({ "service": service, "task": task, "origin": origin });
            if seen.contains(&task) {
                return Err(invalid(
                    format!("services.{service}.init ({origin}) lists task '{task}' twice"),
                    "list each init task once; they run in order",
                    detail,
                ));
            }
            seen.push(task);
            let Some(def) = stack.tasks.get(task) else {
                let known: Vec<&str> = stack.tasks.keys().map(String::as_str).collect();
                return Err(invalid(
                    format!("services.{service}.init ({origin}) names task '{task}', which no layer defines"),
                    &if known.is_empty() {
                        "define it under [tasks.<name>] in stack.toml".to_string()
                    } else {
                        format!("define [tasks.{task}], or use one of: {}", known.join(", "))
                    },
                    detail,
                ));
            };
            if !def.value.services.is_empty() && !def.value.services.contains(service) {
                return Err(invalid(
                    format!(
                        "task '{task}' ({}) is an init task of service '{service}' but its `services` list does not include it",
                        def.origin
                    ),
                    &format!("add \"{service}\" to [tasks.{task}] services, or remove '{task}' from services.{service}.init"),
                    detail,
                ));
            }
        }
    }
    Ok(())
}

/// Digest of one task definition as init runs it.
fn task_digest(stack: &Composed, task: &str) -> String {
    let def = stack.tasks.get(task).map(|e| &e.value);
    sha256_hex(json!({ "task": task, "definition": def }).to_string().as_bytes())[..16].to_string()
}

/// Digest of a service's init list and every task definition in it.
pub fn list_digest(stack: &Composed, service: &str) -> Option<String> {
    let list = declared(stack, service)?;
    let parts: Vec<(String, String)> = list.iter().map(|t| (t.clone(), task_digest(stack, t))).collect();
    Some(sha256_hex(json!(parts).to_string().as_bytes())[..16].to_string())
}

/// The service's non-empty init list.
pub fn declared<'a>(stack: &'a Composed, service: &str) -> Option<&'a Vec<String>> {
    stack.init.get(service).map(|e: &Entry<Vec<String>>| &e.value).filter(|l| !l.is_empty())
}

// ---- markers ------------------------------------------------------------------------------

/// Which data directory a marker describes, as [`crate::datadirs`] records one: where it
/// resolved, with every link resolved, and which directory it is (device, inode). A directory
/// recreated at the same path is a different one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirIdentity {
    pub location: PathBuf,
    pub identity: [u64; 2],
}

impl DirIdentity {
    /// `None` when the directory does not exist (or is not a directory).
    pub fn of(path: &Path) -> Option<Self> {
        match crate::datadirs::locate(path) {
            (Some(location), Some(identity)) => Some(Self { location, identity }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedTask {
    pub task: String,
    pub digest: String,
}

/// What Stack recorded about one service's init in this checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    /// The data directory it describes; `None` for a service without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<DirIdentity>,
    /// `done` or `failed`.
    pub status: String,
    /// Digest of the init list and task definitions this data satisfies (when `done`).
    pub digest: String,
    #[serde(default)]
    pub completed: Vec<CompletedTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_task: Option<String>,
    /// What wrote it: `up`.
    pub source: String,
    pub at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Markers {
    #[serde(default)]
    services: IndexMap<String, Marker>,
}

fn markers_file(state: &Path, root: &Path) -> PathBuf {
    state.join("init").join(format!("{}.json", project_key(root)))
}

/// Held for a short read-modify-write of the markers, never while taking another lock.
fn markers_lock(state: &Path, root: &Path) -> Result<FileLock> {
    FileLock::acquire(&state.join("init").join(format!("{}.markers.lock", project_key(root))))
}

/// Serializes init runs of one checkout. Taken before the project
/// lock, never while holding it.
pub fn run_lock(state: &Path, root: &Path) -> Result<FileLock> {
    FileLock::acquire(&state.join("init").join(format!("{}.lock", project_key(root))))
}

pub fn read_markers(state: &Path, root: &Path) -> Result<IndexMap<String, Marker>> {
    Ok(read_json::<Markers>(&markers_file(state, root))?.services)
}

/// Replace (or with `None` remove) one service's marker.
pub fn set_marker(state: &Path, root: &Path, service: &str, marker: Option<Marker>) -> Result<()> {
    let _guard = markers_lock(state, root)?;
    let path = markers_file(state, root);
    let mut all: Markers = read_json(&path)?;
    match marker {
        Some(m) => {
            all.services.insert(service.to_string(), m);
        }
        None => {
            all.services.shift_remove(service);
        }
    }
    write_json(&path, &all)
}

/// Forget a checkout's markers, except those of the services in `kept` (whose data stays):
/// `down --purge` removed the rest of its data, so their init runs again on the next `up`.
pub fn forget(state: &Path, root: &Path, kept: &[String]) -> Result<()> {
    let _guard = markers_lock(state, root)?;
    let path = markers_file(state, root);
    let mut all: Markers = read_json(&path)?;
    all.services.retain(|service, _| kept.contains(service));
    if all.services.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(crate::error::io_error(path.display(), e)),
            _ => Ok(()),
        };
    }
    write_json(&path, &all)
}

/// A checkout moved from `old` to `new` (`stack mv`): its markers follow it, and a marker whose
/// data directory was moved (`moved` lists where each was and where it is now) names the new
/// place. A rename keeps the directory's identity, so its init still counts as done. Best
/// effort: without it init would only run again.
pub fn rekey(state: &Path, old: &Path, new: &Path, moved: &[(PathBuf, PathBuf)]) {
    let (from, to) = (markers_file(state, old), markers_file(state, new));
    let Ok(mut all) = read_json::<Markers>(&from) else { return };
    if all.services.is_empty() || to.exists() {
        return;
    }
    for marker in all.services.values_mut() {
        if let Some(dir) = marker.data_dir.as_mut() {
            if let Some((_, now)) = moved.iter().find(|(was, _)| *was == dir.location) {
                dir.location = crate::datadirs::locate(now).0.unwrap_or_else(|| now.clone());
            }
        }
    }
    if write_json(&to, &all).is_ok() {
        let _ = std::fs::remove_file(from);
    }
}

// ---- status -------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitState {
    /// Every init task ran against this data directory, from the current definitions.
    Done,
    /// Init has not completed against this data directory (or it is not known yet).
    Pending,
    /// A task failed; the next `up` resumes from it.
    Failed,
    /// Init completed, but the init list or a task definition changed since.
    Stale,
    /// The service declares no init tasks.
    None,
}

impl std::fmt::Display for InitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            InitState::Done => "done",
            InitState::Pending => "pending",
            InitState::Failed => "failed",
            InitState::Stale => "stale",
            InitState::None => "none",
        };
        f.write_str(s)
    }
}

/// The marker that applies to the service's current data directory, if any. `data_dir` is
/// what the supervisor reports for it; `None` for a service without one.
fn applicable<'a>(marker: Option<&'a Marker>, data_dir: Option<&str>) -> Option<&'a Marker> {
    let marker = marker?;
    match (data_dir, &marker.data_dir) {
        (None, None) => Some(marker),
        (Some(dir), Some(recorded)) => DirIdentity::of(Path::new(dir)).filter(|now| now == recorded).map(|_| marker),
        _ => None,
    }
}

pub fn classify(stack: &Composed, service: &str, marker: Option<&Marker>, data_dir: Option<&str>) -> InitState {
    let Some(digest) = list_digest(stack, service) else { return InitState::None };
    match applicable(marker, data_dir) {
        None => InitState::Pending,
        Some(m) if m.status == "failed" => InitState::Failed,
        Some(m) if m.digest == digest => InitState::Done,
        Some(_) => InitState::Stale,
    }
}

/// Per-service init state, given the data directory the supervisor reports for each service.
/// Empty when no service declares init tasks.
pub fn statuses(state: &Path, root: &Path, stack: &Composed, data_dirs: &IndexMap<String, Option<String>>) -> IndexMap<String, InitState> {
    if !stack.init.values().any(|e| !e.value.is_empty()) {
        return IndexMap::new();
    }
    let markers = read_markers(state, root).unwrap_or_default();
    stack
        .services
        .keys()
        .map(|service| {
            let dir = data_dirs.get(service).cloned().flatten();
            (service.clone(), classify(stack, service, markers.get(service), dir.as_deref()))
        })
        .collect()
}

// ---- running ------------------------------------------------------------------------------

/// What `up` did about one service's init tasks.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub status: InitState,
    /// Tasks this call ran, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ran: Vec<String>,
    /// Tasks skipped because an earlier, failed `up` already completed them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resumed_after: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Check `--reinit` names against the compiled stack before anything starts.
pub fn check_options(stack: &Composed, options: &UpOptions) -> Result<()> {
    for service in &options.reinit {
        if !stack.services.contains_key(service) {
            let known: Vec<&str> = stack.services.keys().map(String::as_str).collect();
            return Err(StackError::new("unknown_service", format!("no service named '{service}'"))
                .hint(format!("services: {}", known.join(", "))));
        }
        if declared(stack, service).is_none() {
            return Err(StackError::new("init_invalid", format!("service '{service}' declares no init tasks"))
                .hint(format!("add `init = [\"<task>\", ...]` under [services.{service}] in stack.toml, or drop --reinit {service}")));
        }
    }
    Ok(())
}

/// Run every pending init task after `up` verified the stack. Steps are appended to
/// `report.steps` and outcomes to `report.init`. A failing task fails with `init_failed`,
/// leaving services running and the failure recorded for the next `up` to resume from.
pub fn run(ctx: &Ctx, report: &mut session::UpReport, stack: &Composed, options: &UpOptions) -> Result<()> {
    let services: Vec<String> = stack.services.keys().filter(|s| declared(stack, s).is_some()).cloned().collect();
    if services.is_empty() {
        return Ok(());
    }
    let data_dir = |service: &str| report.session.services.get(service).and_then(|r| r.data_dir.clone());
    if options.no_init {
        let markers = read_markers(&ctx.state, &ctx.root)?;
        for service in &services {
            let status = classify(stack, service, markers.get(service), data_dir(service).as_deref());
            report.init.insert(service.clone(), Outcome { status, ran: Vec::new(), resumed_after: Vec::new(), note: Some("skipped: --no-init".into()) });
        }
        report.steps.push(json!({ "step": "init", "status": "skipped", "detail": { "reason": "--no-init" } }));
        return Ok(());
    }
    let _run = run_lock(&ctx.state, &ctx.root).map_err(|e| progress(report, "init", e, None))?;
    for service in &services {
        let dir = data_dir(service);
        let list = declared(stack, service).cloned().unwrap_or_default();
        let digest = list_digest(stack, service).unwrap_or_default();
        // Read again under the run lock: another `up` may have just finished it.
        let markers = read_markers(&ctx.state, &ctx.root).map_err(|e| progress(report, "init", e, None))?;
        let marker = applicable(markers.get(service.as_str()), dir.as_deref());
        let forced = options.reinit.contains(service);
        let state = classify(stack, service, markers.get(service.as_str()), dir.as_deref());
        let mut outcome = Outcome { status: state, ran: Vec::new(), resumed_after: Vec::new(), note: None };
        match state {
            InitState::Done if !forced => {
                report.init.insert(service.clone(), outcome);
                continue;
            }
            InitState::Stale if !forced => {
                outcome.note = Some(format!("the init list or a task changed since it ran; `stack up --reinit {service}` runs it again"));
                report.warnings.push(format!("init: {service}: init is stale; run `stack up --reinit {service}` to run it again"));
                report.init.insert(service.clone(), outcome);
                continue;
            }
            _ => {}
        }
        // Resume after a failure: skip the leading tasks it completed with the same definitions.
        let mut completed: Vec<CompletedTask> = Vec::new();
        if let (false, Some(m)) = (forced, marker.filter(|m| m.status == "failed")) {
            for (task, done) in list.iter().zip(&m.completed) {
                if done.task == *task && done.digest == task_digest(stack, task) {
                    completed.push(done.clone());
                } else {
                    break;
                }
            }
        }
        outcome.resumed_after = completed.iter().map(|c| c.task.clone()).collect();
        // The data directory as it is now; recorded with every marker this run writes.
        let identity = dir.as_deref().map(Path::new).and_then(DirIdentity::of);
        if dir.is_some() && identity.is_none() {
            let e = StackError::new("init_failed", format!("service '{service}' reports data directory {} but it does not exist", dir.clone().unwrap_or_default()))
                .hint("run `stack status`; the service may have been reset while `up` ran");
            return Err(progress(report, "init", e, None));
        }
        for task in list.iter().skip(completed.len()) {
            let started = Instant::now();
            match run_task(ctx, task) {
                Ok(result) => {
                    completed.push(CompletedTask { task: task.clone(), digest: task_digest(stack, task) });
                    outcome.ran.push(task.clone());
                    let marker = Marker { data_dir: identity.clone(), status: "failed".into(), digest: digest.clone(), completed: completed.clone(), failed_task: None, source: "up".into(), at: now() };
                    // Durable after every task: an interrupted `up` resumes after the last one.
                    if completed.len() < list.len() {
                        set_marker(&ctx.state, &ctx.root, service, Some(marker)).map_err(|e| progress(report, "init", e, None))?;
                    }
                    report.steps.push(json!({ "step": "init", "status": "ok", "detail": {
                        "service": service, "task": task, "exit_code": result["exit_code"], "secs": started.elapsed().as_secs()
                    } }));
                }
                Err(failure) => {
                    let (e, result) = *failure;
                    let marker = Marker { data_dir: identity.clone(), status: "failed".into(), digest: digest.clone(), completed: completed.clone(), failed_task: Some(task.clone()), source: "up".into(), at: now() };
                    let recorded = set_marker(&ctx.state, &ctx.root, service, Some(marker));
                    outcome.status = InitState::Failed;
                    report.init.insert(service.clone(), outcome);
                    let mut e = e.with_detail(json!({ "service": service, "task": task, "completed": completed.iter().map(|c| &c.task).collect::<Vec<_>>() }));
                    if let Some(result) = result {
                        e = e.with_detail(json!({ "exit_code": result["exit_code"], "stdout": tail(&result["stdout"]), "stderr": tail(&result["stderr"]) }));
                    }
                    if let Err(r) = recorded {
                        e = e.with_detail(json!({ "marker_error": r.to_string() }));
                    }
                    return Err(progress(report, "init", e, Some(service)));
                }
            }
        }
        let marker = Marker { data_dir: identity, status: "done".into(), digest: digest.clone(), completed, failed_task: None, source: "up".into(), at: now() };
        set_marker(&ctx.state, &ctx.root, service, Some(marker)).map_err(|e| progress(report, "init", e, None))?;
        outcome.status = InitState::Done;
        report.init.insert(service.clone(), outcome);
    }
    Ok(())
}

/// The last 4 KiB of a captured stream.
fn tail(v: &Value) -> Value {
    let s = v.as_str().unwrap_or_default();
    let mut start = s.len().saturating_sub(4096);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    json!(&s[start..])
}

/// One init task, exactly as `stack run <task>` runs it (captured, with its declared secrets),
/// within what is left of `up`'s deadline. On failure, the error and the captured result.
fn run_task(ctx: &Ctx, task: &str) -> std::result::Result<Value, Box<(StackError, Option<Value>)>> {
    let remaining = crate::process::deadline().map_or(session::DEFAULT_STARTUP_TIMEOUT, |d| d.saturating_duration_since(Instant::now()));
    if remaining.is_zero() {
        return Err(Box::new((StackError::new("timed_out", format!("no time left to run init task '{task}'")), None)));
    }
    let failed = |e: StackError| {
        let message = format!("init task '{task}' could not start: {}", e.message);
        Box::new((StackError::new("init_failed", message).hint(retry_hint(task)).with_detail(json!({ "cause": e })), None))
    };
    let mut plan = session::plan_task(ctx, task, &[], true).map_err(failed)?;
    let result = crate::mcp::run_and_finish(ctx, &mut plan, remaining.max(Duration::from_secs(1))).map_err(failed)?;
    if result["timed_out"] == true {
        return Err(Box::new((StackError::new("timed_out", format!("init task '{task}' did not finish before `up`'s deadline")), Some(result))));
    }
    if result["exit_code"] != 0 {
        let code = result["exit_code"].clone();
        return Err(Box::new((StackError::new("init_failed", format!("init task '{task}' exited with {code}")).hint(retry_hint(task)), Some(result))));
    }
    Ok(result)
}

fn retry_hint(task: &str) -> String {
    format!("services are still running; fix the task, then `stack up` resumes from '{task}' (`stack run {task}` runs it by hand, `stack up --no-init` skips init)")
}

/// `err` with `up`'s progress record attached, as `up` reports every failure after its
/// steps begin: services verified and stay running.
fn progress(report: &session::UpReport, step: &str, err: StackError, service: Option<&String>) -> StackError {
    let mut steps = report.steps.clone();
    let mut failed = json!({ "step": step, "status": "failed", "code": err.code });
    if let Some(service) = service {
        failed["service"] = json!(service);
    }
    steps.push(failed);
    err.with_detail(json!({ "steps": steps, "retry_safe": true, "changed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(data_dir: Option<DirIdentity>) -> Marker {
        Marker { data_dir, status: "done".into(), digest: "d".into(), completed: Vec::new(), failed_task: None, source: "up".into(), at: 0 }
    }

    #[test]
    fn markers_follow_a_moved_checkout_and_its_moved_data() {
        let state = tempfile::tempdir().unwrap();
        let (old, new) = (Path::new("/work/old"), Path::new("/work/new"));
        let dir = |path: &str| DirIdentity { location: path.into(), identity: [1, 2] };
        set_marker(state.path(), old, "db", Some(marker(Some(dir("/data/old/db"))))).unwrap();
        set_marker(state.path(), old, "worker", Some(marker(None))).unwrap();
        rekey(state.path(), old, new, &[(PathBuf::from("/data/old/db"), PathBuf::from("/data/new/db"))]);
        assert!(read_markers(state.path(), old).unwrap().is_empty());
        let moved = read_markers(state.path(), new).unwrap();
        // The same directory at its new place: init still counts as done.
        assert_eq!(moved["db"].data_dir, Some(dir("/data/new/db")));
        assert!(moved.contains_key("worker"));
        // A purge that kept db's data forgets only the others.
        forget(state.path(), new, &["db".to_string()]).unwrap();
        assert_eq!(read_markers(state.path(), new).unwrap().keys().collect::<Vec<_>>(), ["db"]);
        forget(state.path(), new, &[]).unwrap();
        assert!(read_markers(state.path(), new).unwrap().is_empty());
        forget(state.path(), new, &[]).unwrap();
    }

    #[test]
    fn identities_tell_a_recreated_directory_apart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        std::fs::create_dir(&path).unwrap();
        let first = DirIdentity::of(&path).unwrap();
        assert_eq!(first, DirIdentity::of(&path).unwrap());
        // Keep the old directory so its inode cannot be reused immediately.
        std::fs::rename(&path, dir.path().join("keep")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_ne!(first, DirIdentity::of(&path).unwrap());
        assert!(DirIdentity::of(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn markers_apply_only_to_the_directory_they_describe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        std::fs::create_dir(&path).unwrap();
        let p = path.to_str().unwrap();
        let recorded = marker(DirIdentity::of(&path));
        assert!(applicable(Some(&recorded), Some(p)).is_some());
        assert!(applicable(Some(&recorded), None).is_none());
        assert!(applicable(Some(&recorded), Some(dir.path().to_str().unwrap())).is_none());
        let checkout = marker(None);
        assert!(applicable(Some(&checkout), None).is_some());
        assert!(applicable(Some(&checkout), Some(p)).is_none());
    }
}

//! What mise answers about a checkout, kept so the commands agents run constantly (`exec`,
//! `run`, `env`, `status`, `up` of a running stack) need not start mise every time.
//!
//! Two questions are kept together: `mise env --json` (the environment a command gets) and
//! whether every pinned release is installed (`mise ls --json` in a scratch root naming exactly
//! the pins, see [`scratch::installed`]). An answer is kept only when the second one is "all of
//! them", so a kept environment always has every pinned release on its PATH.
//!
//! An answer is a function of what mise reads. Under Stack's isolation (see
//! [`super::mise::config_env`]) that is, as checked against mise 2026.10.3:
//!
//! - the mise binary it runs (the first executable `mise` on PATH, by resolved path, device,
//!   inode, size, modification and change time), only when that is a native executable (a
//!   script runs some other file, which identifies nothing) and no empty or relative PATH
//!   entry comes at or before it (mise starts in the checkout, where such an entry resolves);
//! - its whole environment, exactly as Stack starts it: what Stack inherited without the
//!   variables it removes, plus the isolation it sets. Data, cache and state directories,
//!   `HOME`, `XDG_*`, the caller's `PATH` (mise keeps it after the tools' directories, minus
//!   those directories) and anything a backend reads from the environment are all in it;
//! - the checkout's path and the files mise loads there: the generated configuration and the
//!   rendered lock (by content), and the isolated configuration directory
//!   `.stack/provider-config`, which must be absent or a real, empty directory (it normally
//!   does not exist; settings there could make mise read files nothing here covers). No other
//!   file of the checkout or above it is read (`mise.toml`, `.mise.toml`, `.tool-versions`,
//!   other `conf.d` files and the isolated directory's `config.toml` or `conf.d` were each
//!   checked to change nothing);
//! - the installed releases: each pin's install directory as mise reported it, and the
//!   directory above it (the tool's), each a real directory (not a link, whose target could
//!   change unseen) with every entry directly inside (name, kind, device, inode, size,
//!   modification and change time), so installing, removing or reinstalling a release, or
//!   adding one beside it, is seen. One dependency is not recorded: mise counts a release as
//!   not installed while `$MISE_CACHE_DIR/<tool>/<version>/incomplete` exists; mise replaces
//!   the install directory whenever it creates that marker, which the record does see.
//!
//! The first three, with the pins, make up the key an answer is found by; the install
//! directories are recorded
//! with the answer and must look the same when it is used. Anything else mise could depend on
//! makes an answer not kept at all, and mise is asked every time:
//!
//! - template syntax anywhere in the configuration (`{{`, `{%`, `{#`: `exec()`, `env.X` and
//!   friends run code and read the caller), or `$` in an `[env]` value (mise's shell expansion);
//! - a configured tool that is not one of the pins, a pin without an entry in the rendered lock,
//!   or a backend outside those whose environment is a function of the install directory
//!   (asdf and vfox plugins run scripts);
//! - a value in the answer Stack cannot account for: every variable but `PATH` must be the
//!   configuration's own literal value or lie under a pin's install directory, and every `PATH`
//!   entry must be one of the caller's, under an install directory, or a declared `_.path`
//!   entry. So nothing is written that was not already on disk (the generated configuration)
//!   or in the caller's `PATH`; secrets Stack grants are resolved later and never reach mise.
//! - a change while mise was asked: the key is computed again afterwards and must be the same,
//!   and every recorded directory entry must have changed more than [`SETTLE`] before mise was
//!   asked, so a change racing the question (whose change time cannot be earlier) is never
//!   recorded as the state it answered for.
//!
//! The file is `$STACK_STATE_DIR/projects/<checkout key>.provider-env.json`, mode 0600, replaced
//! atomically (a reader sees one whole version), holding the last few answers by key. Losing it
//! costs one mise call. Concurrent commands may each ask and write; the last write wins, and an
//! answer already kept is not written again. The last eight answers written are kept (a hit
//! does not move one up), so more callers with different environments than that evict each
//! other, at one mise call each. The file is used as read: like the session and port records,
//! the state directory is trusted, and whatever can write it can set later commands' PATH.
//!
//! The release `mise version` printed is recorded the same way, for the binary that printed it
//! (`$STACK_STATE_DIR/mise-version.json`, see [`recorded_version`]).

use crate::provider::scratch::{self, Pin};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Bumped whenever what an answer depends on, or how it is recorded, changes.
const SCHEMA: &str = "stack-provider-env-2";
/// Answers kept per checkout (one per distinct caller environment, mostly).
const MAX_ENTRIES: usize = 8;
/// A recorded directory entry must be at least this much older than the question. Covers
/// coarse kernel clocks and filesystems with 1 or 2 second timestamps.
pub const SETTLE: Duration = Duration::from_secs(2);

/// Backends whose environment is a function of the install directory (and the environment).
const PURE_BACKENDS: &[&str] = &[
    "core", "aqua", "ubi", "github", "gitlab", "forgejo", "http", "conda", "cargo", "npm", "pipx", "go", "gem", "spm",
    "dotnet", "packslip",
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    schema: String,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    key: String,
    /// Directories whose state the answer depends on, and a digest of that state.
    watched: Vec<PathBuf>,
    snapshot: String,
    /// `mise env --json` exactly as mise printed it (parsed).
    env: IndexMap<String, String>,
}

/// Where a checkout's answers are kept.
pub fn path(state: &Path, root: &Path) -> PathBuf {
    state.join("projects").join(format!("{}.provider-env.json", crate::state::project_key(root)))
}

/// Forget every answer kept for `root` (after Stack installed something, say). Best effort:
/// the answers would not match anyway.
pub fn forget(state: &Path, root: &Path) {
    let _ = std::fs::remove_file(path(state, root));
}

/// The environment Stack runs with, which the mise commands it starts inherit (see
/// [`child_env`]). Tests give their own.
#[derive(Debug, Clone)]
pub struct Caller(Vec<(OsString, OsString)>);

impl Caller {
    pub fn current() -> Self {
        Self(std::env::vars_os().collect())
    }

    fn var(&self, name: &str) -> Option<&OsStr> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_os_str())
    }
}

/// A question about to be put to mise, found in no answer: what [`remember`] needs.
#[derive(Debug)]
pub struct Pending {
    key: Option<String>,
    asked_at: SystemTime,
}

impl Pending {
    /// Whether an answer to this question could be kept at all (mise and the configuration
    /// could be identified).
    pub fn keepable(&self) -> bool {
        self.key.is_some()
    }
}

/// The answer kept for `root` and `pins` now, if there is a current one, else what
/// [`remember`] needs once mise has been asked.
pub fn lookup(caller: &Caller, state: &Path, root: &Path, pins: &[Pin]) -> Result<IndexMap<String, String>, Pending> {
    let asked_at = SystemTime::now();
    let Some(key) = key(caller, root, pins) else { return Err(Pending { key: None, asked_at }) };
    let found = read(state, root).entries.into_iter().find(|e| e.key == key);
    if let Some(entry) = found {
        if snapshot(&entry.watched).is_some_and(|(digest, _)| digest == entry.snapshot) {
            return Ok(entry.env);
        }
    }
    // A configuration whose answer would never be kept: nothing to ask for keeping it.
    let key = eligible(root, pins).map(|_| key);
    Err(Pending { key, asked_at })
}

/// Keep what mise answered for `pending` (`env`, and the install directory of every pin, all
/// installed), when it can be kept (see the module documentation). Best effort.
pub fn remember(caller: &Caller, state: &Path, root: &Path, pins: &[Pin], pending: Pending, env: &IndexMap<String, String>, installs: &[PathBuf]) {
    let Some(key) = pending.key else { return };
    if installs.len() != pins.len() || !accountable(caller, root, pins, env, installs) {
        return;
    }
    // Only real directories: a link's target (a release `mise link` points elsewhere, a tool
    // directory that is a link) could go or change with nothing here changing.
    let real_dir = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    if !installs.iter().all(|dir| real_dir(dir) && dir.parent().is_some_and(real_dir)) {
        return;
    }
    let mut watched: Vec<PathBuf> = Vec::new();
    for dir in installs {
        for path in [Some(dir.as_path()), dir.parent()].into_iter().flatten() {
            if !watched.iter().any(|w| w == path) {
                watched.push(path.to_path_buf());
            }
        }
    }
    let Some((digest, newest)) = snapshot(&watched) else { return };
    let settled = pending.asked_at.checked_sub(SETTLE).is_some_and(|limit| newest.is_none_or(|n| n < limit));
    if !settled || self::key(caller, root, pins).as_deref() != Some(key.as_str()) {
        return;
    }
    let mut file = read(state, root);
    let entry = Entry { key, watched, snapshot: digest, env: env.clone() };
    // Kept already (another command asked at the same time): nothing to write.
    if file.schema == SCHEMA && file.entries.first().is_some_and(|e| e == &entry) {
        return;
    }
    file.schema = SCHEMA.into();
    file.entries.retain(|e| e.key != entry.key);
    file.entries.insert(0, entry);
    file.entries.truncate(MAX_ENTRIES);
    let _ = crate::state::write_json_unsynced(&path(state, root), &file);
}

fn read(state: &Path, root: &Path) -> CacheFile {
    std::fs::read(path(state, root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok())
        .filter(|f| f.schema == SCHEMA)
        .unwrap_or_default()
}

#[cfg(unix)]
/// The key an answer is found by (see the module documentation), or `None` when mise cannot be
/// found or the configuration cannot be read.
pub(crate) fn key(caller: &Caller, root: &Path, pins: &[Pin]) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut put = |bytes: &[u8]| {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    put(SCHEMA.as_bytes());
    put(env!("CARGO_PKG_VERSION").as_bytes());
    put(root.as_os_str().as_encoded_bytes());
    let env = child_env(caller, root);
    for (k, v) in &env {
        put(k.as_encoded_bytes());
        put(v.as_encoded_bytes());
    }
    let path = env.get(OsStr::new("PATH"))?;
    let mise = find_mise(path)?;
    put(mise.as_os_str().as_encoded_bytes());
    put(describe(&std::fs::metadata(&mise).ok()?).as_bytes());
    let config = std::fs::read(super::mise::output_path(root)).ok()?;
    put(crate::hash::sha256_hex(&config).as_bytes());
    match std::fs::read(lock_path(root)) {
        Ok(lock) => put(crate::hash::sha256_hex(&lock).as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => put(b"no lock"),
        Err(_) => return None,
    }
    put(isolated_config(&root.join(".stack/provider-config"))?.as_bytes());
    put(scratch::tools_config(pins).as_bytes());
    Some(crate::hash::hex(&h.finalize()))
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(".config/mise/mise.lock")
}

/// The environment Stack starts mise with in `root` (see [`super::mise::configure_command`] and
/// `mise_with`), sorted.
fn child_env(caller: &Caller, root: &Path) -> std::collections::BTreeMap<OsString, OsString> {
    let mut env: std::collections::BTreeMap<_, _> = caller
        .0
        .iter()
        .filter(|(k, _)| {
            k.to_str().is_none_or(|k| !super::mise::config_key(k) && k != super::mise::OWNER_VAR && k != super::mise::BINDING_VAR)
        })
        .cloned()
        .collect();
    for (k, v) in super::mise::config_env(root) {
        env.insert(k.into(), v.into());
    }
    env.insert("MISE_YES".into(), "1".into());
    env.insert("NO_COLOR".into(), "1".into());
    env
}

#[cfg(unix)]
/// The `mise` a command started by name runs: the first executable file of that name on PATH,
/// links resolved, and only when that is a native executable (Mach-O or ELF). A script (a
/// wrapper or shim) would run some other file Stack does not see, so it identifies nothing.
fn find_mise(path: &OsStr) -> Option<PathBuf> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    // An empty or relative entry names a directory relative to where mise starts (the
    // checkout), not to where Stack runs: before the match, it could hold the file that runs.
    let mut found = None;
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() || dir.is_relative() {
            return None;
        }
        let candidate = dir.join("mise");
        if std::fs::metadata(&candidate).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
            found = Some(candidate);
            break;
        }
    }
    let found = found?.canonicalize().ok()?;
    let mut magic = [0u8; 4];
    std::fs::File::open(&found).ok()?.read_exact(&mut magic).ok()?;
    let native = [
        [0x7f, b'E', b'L', b'F'],
        [0xfe, 0xed, 0xfa, 0xce],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0xbe, 0xba, 0xfe, 0xca],
    ];
    native.contains(&magic).then_some(found)
}

#[cfg(unix)]
/// One entry's identity and times, without following a link.
fn describe(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    let kind = if meta.file_type().is_symlink() {
        'l'
    } else if meta.is_dir() {
        'd'
    } else if meta.is_file() {
        'f'
    } else {
        'o'
    };
    format!(
        "{kind}:{}:{}:{}:{}:{}.{}:{}.{}",
        meta.mode(),
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    )
}

#[cfg(unix)]
fn changed_at(meta: &std::fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let secs = u64::try_from(meta.ctime()).ok()?;
    let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

#[cfg(unix)]
/// The isolated configuration directory when it holds nothing mise could read: absent, or a
/// real, empty directory. `None` otherwise (a file there, or a link anywhere): its content, and
/// the settings in it, could make mise's answer depend on files the key does not cover.
fn isolated_config(dir: &Path) -> Option<String> {
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some("absent".into()),
        Ok(meta) if meta.is_dir() && std::fs::read_dir(dir).ok()?.next().is_none() => Some(format!("empty {}", describe(&meta))),
        _ => None,
    }
}

#[cfg(unix)]
/// A digest of each of `paths` and every entry directly inside it, and the latest change time
/// among them. `None` when one cannot be read.
fn snapshot(paths: &[PathBuf]) -> Option<(String, Option<SystemTime>)> {
    let mut text = String::new();
    let mut newest: Option<SystemTime> = None;
    let mut note = |meta: &std::fs::Metadata| {
        let at = changed_at(meta);
        newest = match (newest, at) {
            (Some(n), Some(a)) => Some(n.max(a)),
            (n, a) => n.or(a),
        };
    };
    for path in paths {
        let meta = std::fs::symlink_metadata(path).ok()?;
        note(&meta);
        text.push_str(&format!("{}\0{}\n", path.display(), describe(&meta)));
        if !meta.is_dir() {
            continue;
        }
        let mut children = Vec::new();
        for entry in std::fs::read_dir(path).ok()? {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            note(&meta);
            children.push(format!("{}\0{}", entry.file_name().to_string_lossy(), describe(&meta)));
        }
        children.sort();
        for child in children {
            text.push('\t');
            text.push_str(&child);
            text.push('\n');
        }
    }
    Some((crate::hash::sha256_hex(text.as_bytes()), newest))
}

/// What the configuration declares, when an answer for it could be kept at all: no template
/// syntax, every configured tool a pin with a locked pure backend, `[env]` literal strings and
/// `_.path` directives only. Checked before mise is asked (so a question whose answer would
/// never be kept asks only what the caller needs) and again when the answer is kept.
struct Eligible {
    declared: toml::Table,
    paths: Vec<PathBuf>,
}

fn eligible(root: &Path, pins: &[Pin]) -> Option<Eligible> {
    let Ok(text) = std::fs::read_to_string(super::mise::output_path(root)) else { return None };
    if crate::tool::templated(&text) {
        return None;
    }
    let Ok(config) = text.parse::<toml::Table>() else { return None };
    // Every configured tool is a pin at that version, with an entry of a pure backend.
    let configured = configured_tools(&config)?;
    let mut pinned: Vec<(String, String)> = pins.iter().map(|p| (p.tool.clone(), p.spec.version.clone())).collect();
    let mut listed = configured.clone();
    pinned.sort();
    pinned.dedup();
    listed.sort();
    listed.dedup();
    if pinned != listed {
        return None;
    }
    if !listed.is_empty() {
        let Ok(lock) = std::fs::read_to_string(lock_path(root)).map(|t| t.parse::<toml::Table>()) else { return None };
        let Ok(lock) = lock else { return None };
        let tools = lock.get("tools").and_then(|t| t.as_table());
        let pure = |tool: &str, version: &str| {
            tools.and_then(|t| t.get(tool)).and_then(|e| e.as_array()).is_some_and(|entries| {
                entries.iter().any(|e| {
                    e.get("version").and_then(|v| v.as_str()) == Some(version)
                        && e.get("backend").and_then(|b| b.as_str()).is_some_and(|b| {
                            PURE_BACKENDS.contains(&b.split(':').next().unwrap_or_default())
                        })
                })
            })
        };
        if !listed.iter().all(|(tool, version)| pure(tool, version)) {
            return None;
        }
    }
    // `[env]`: literal strings, no shell expansion; `_` holds only `path`.
    let empty = toml::Table::new();
    let declared = config.get("env").map_or(Some(&empty), |e| e.as_table());
    let declared = declared?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for (k, v) in declared {
        if k == "_" {
            let directives = v.as_table()?;
            for (d, value) in directives {
                let entries = value.as_array().filter(|_| d == "path");
                let entries = entries?;
                for entry in entries {
                    let entry = entry.as_str()?;
                    paths.push(root.join(entry));
                }
            }
            continue;
        }
        if v.as_str().is_none_or(|v| v.contains('$')) {
            return None;
        }
    }
    Some(Eligible { declared: declared.clone(), paths })
}

/// Whether mise's answer is a function of the inputs [`key`] and the snapshot cover, and holds
/// nothing that was not already on disk or in the caller's PATH (see the module documentation).
fn accountable(caller: &Caller, root: &Path, pins: &[Pin], env: &IndexMap<String, String>, installs: &[PathBuf]) -> bool {
    let Some(Eligible { declared, paths }) = eligible(root, pins) else { return false };
    let under_install = |value: &str| installs.iter().any(|dir| Path::new(value).starts_with(dir));
    let caller: Vec<PathBuf> = caller.var("PATH").map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    for (k, v) in env {
        if k == "PATH" {
            let ok = std::env::split_paths(v).all(|entry| caller.contains(&entry) || paths.contains(&entry) || under_install(&entry.to_string_lossy()));
            if !ok {
                return false;
            }
            continue;
        }
        let literal = declared.get(k).and_then(|d| d.as_str()) == Some(v.as_str());
        let tool_value = Path::new(v).is_absolute() && under_install(v);
        if !(literal || tool_value) {
            return false;
        }
    }
    true
}

/// `(tool, version)` of every `[tools]` entry: a version, a list of them, or tables with one.
fn configured_tools(config: &toml::Table) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let Some(tools) = config.get("tools") else { return Some(out) };
    for (tool, value) in tools.as_table()? {
        let values = match value {
            toml::Value::Array(values) => values.iter().collect(),
            other => vec![other],
        };
        for value in values {
            let version = match value {
                toml::Value::String(v) => v.as_str(),
                toml::Value::Table(t) => t.get("version")?.as_str()?,
                _ => return None,
            };
            out.push((tool.clone(), version.to_string()));
        }
    }
    Some(out)
}

/// The mise binary a command started by name runs now (see [`find_mise`]), by resolved path,
/// device, inode, size and times, and whether it last changed long enough ago (see
/// [`SETTLE`]) for an answer about it to be recorded.
#[cfg(unix)]
fn binary_identity(caller: &Caller) -> Option<(String, bool)> {
    let mise = find_mise(caller.var("PATH")?)?;
    let meta = std::fs::metadata(&mise).ok()?;
    let settled = SystemTime::now().checked_sub(SETTLE).zip(changed_at(&meta)).is_some_and(|(limit, at)| at < limit);
    Some((format!("{}\0{}", mise.display(), describe(&meta)), settled))
}

#[cfg(not(unix))]
fn binary_identity(_: &Caller) -> Option<(String, bool)> {
    None
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordedVersion {
    schema: String,
    binary: String,
    version: String,
}

fn version_path(state: &Path) -> PathBuf {
    state.join("mise-version.json")
}

/// What `mise version` printed for the binary a command would run now, if Stack recorded it for
/// exactly that file (same resolved path, device, inode, size and times). A release is a
/// property of the binary; replacing or upgrading it changes the file.
pub fn recorded_version(caller: &Caller, state: &Path) -> Option<String> {
    let (binary, _) = binary_identity(caller)?;
    let recorded: RecordedVersion = serde_json::from_slice(&std::fs::read(version_path(state)).ok()?).ok()?;
    (recorded.schema == SCHEMA && recorded.binary == binary).then_some(recorded.version)
}

/// Record what `mise version` printed, when the binary is the one [`recorded_version`] looks
/// for, was the same before it was asked (`before`), and has settled. Best effort.
pub fn record_version(caller: &Caller, state: &Path, before: Option<&str>, version: &str) {
    let Some((binary, settled)) = binary_identity(caller) else { return };
    if settled && before == Some(binary.as_str()) {
        let recorded = RecordedVersion { schema: SCHEMA.into(), binary, version: version.into() };
        let _ = crate::state::write_json_unsynced(&version_path(state), &recorded);
    }
}

/// The identity [`record_version`] compares, taken before mise is asked.
pub fn version_identity(caller: &Caller) -> Option<String> {
    binary_identity(caller).map(|(binary, _)| binary)
}

/// Without unix metadata nothing can be fingerprinted: mise is asked every time.
#[cfg(not(unix))]
pub(crate) fn key(_: &Caller, _: &Path, _: &[Pin]) -> Option<String> {
    None
}

#[cfg(not(unix))]
fn snapshot(_: &[PathBuf]) -> Option<(String, Option<SystemTime>)> {
    None
}

#[cfg(test)]
mod tests;

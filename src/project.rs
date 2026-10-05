//! `stack compile`: resolve bundles → lock → compose → render provider config.

use crate::compose::{compose, Composed, LoadedBundle};
use crate::error::{io_error, Result, StackError};
use crate::lock::{self, LockedBundle, Lockfile};
use crate::manifest::{read_bundle, read_project};
use crate::ports::{self, Request};
use crate::provider::mise;
use crate::source::{Mode, Source};
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Options {
    pub root: PathBuf,
    pub mode: Mode,
    /// Write stack.lock and the provider config. `false` for `stack inspect`.
    pub write: bool,
    pub cache: PathBuf,
    /// Machine-wide state (port reservations, session index).
    pub state: PathBuf,
    /// Drop this project's port reservations and assign fresh ones.
    pub reassign_ports: bool,
}

#[derive(Debug, Serialize)]
pub struct BundleReport {
    pub name: String,
    pub version: Option<String>,
    pub source: String,
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub content_hash: String,
    pub dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub bundles: Vec<BundleReport>,
    pub stack: Composed,
    /// Ports assigned to this checkout. Machine-specific; never written to bundles or stack.lock.
    pub ports: IndexMap<String, u16>,
    pub lock_changed: bool,
    pub provider: &'static str,
    pub output: PathBuf,
    pub written: bool,
    /// Valid but risky choices, such as tools that are not pinned to a version.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// `stack inspect` previews a project that has no lock yet; once locked it reports drift.
pub fn inspect_mode(root: &Path) -> Mode {
    if root.join(lock::LOCK_FILE).exists() {
        Mode::Frozen
    } else {
        Mode::UseLock
    }
}

/// Presets `mise daemons` documents. Newer mise releases may add more, so others only warn.
const KNOWN_PRESETS: &[&str] = &["cockroachdb", "nats", "postgres", "redis", "spicedb"];

fn warnings(stack: &Composed) -> Vec<String> {
    let mut out = unpinned_tools(stack);
    for (name, e) in &stack.services {
        if let Some(preset) = e.value.preset.as_deref().filter(|p| !KNOWN_PRESETS.contains(p)) {
            out.push(format!(
                "services.{name} ({}) uses preset '{preset}', which mise does not document (known: {}); `stack up` will fail if your mise lacks it",
                e.origin,
                KNOWN_PRESETS.join(", ")
            ));
        }
    }
    out
}

/// Versions that resolve differently over time, so stack.lock cannot pin them.
fn unpinned_tools(stack: &Composed) -> Vec<String> {
    stack
        .tools
        .iter()
        .filter(|(_, e)| matches!(e.value.trim(), "latest" | "lts" | "*" | ""))
        .map(|(name, e)| {
            format!("tools.{name} = \"{}\" ({}) is not pinned; installs can change between machines", e.value, e.origin)
        })
        .collect()
}

pub fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STACK_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("stack");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("stack")
}

pub fn compile(opts: &Options) -> Result<Report> {
    // Inspect must remain read-only, including on a new machine.
    if !opts.write {
        return compile_locked(opts);
    }
    let _guard = crate::state::project_lock(&opts.state, &opts.root)?;
    compile_locked(opts)
}

/// The caller holds the project lock when publishing configuration or changing a session.
pub(crate) fn compile_locked(opts: &Options) -> Result<Report> {
    let project = read_project(&opts.root)?;
    let previous = lock::read(&opts.root)?;
    if opts.mode == Mode::Frozen && previous.is_none() {
        return Err(
            StackError::new("lock_outdated", "stack.lock does not exist")
                .hint("run `stack compile` and commit stack.lock"),
        );
    }

    let mut loaded = Vec::new();
    let mut reports = Vec::new();
    let mut locked = Vec::new();
    let mut names = HashSet::new();

    for entry in &project.uses {
        let spec = entry.bundle.as_str();
        let source = Source::parse(spec, &opts.root)?;
        let prior = previous.as_ref().and_then(|l| l.find(spec));
        let fetched = source.fetch(spec, prior, opts.mode, &opts.cache)?;
        let manifest = read_bundle(&fetched.dir, spec)?;
        let name = manifest.bundle.name.clone();
        if !names.insert(name.clone()) {
            return Err(StackError::new(
                "duplicate_bundle",
                format!("two [[use]] entries resolve to a bundle named '{name}'"),
            ));
        }

        locked.push(LockedBundle {
            source: spec.to_string(),
            name: name.clone(),
            commit: fetched.commit.clone(),
            digest: fetched.digest.clone(),
            content_hash: fetched.content_hash.clone(),
        });
        reports.push(BundleReport {
            name,
            version: manifest.bundle.version.clone(),
            source: spec.to_string(),
            commit: fetched.commit.clone(),
            digest: fetched.digest.clone(),
            content_hash: fetched.content_hash.clone(),
            dir: fetched.dir.clone(),
            moved_from: fetched.moved_from.clone(),
        });
        loaded.push(LoadedBundle::new(manifest, fetched.dir)?);
    }

    let stack = compose(&loaded, &project)?;
    let new_lock = Lockfile::new(locked);
    let lock_changed = previous.as_ref() != Some(&new_lock);
    if opts.mode == Mode::Frozen && lock_changed {
        return Err(
            StackError::new("lock_outdated", "stack.toml and stack.lock disagree")
                .hint("run `stack compile` and commit stack.lock"),
        );
    }

    let output = mise::output_path(&opts.root);
    let ports = if opts.write {
        let requests: Vec<Request> = stack
            .services
            .iter()
            .map(|(name, e)| Request {
                service: name.clone(),
                fixed: e.value.fixed_port(),
            })
            .collect();
        let ports = ports::assign(&opts.state, &opts.root, &requests, opts.reassign_ports)?;
        if lock_changed {
            lock::write(&opts.root, &new_lock)?;
        }
        write_if_changed(&output, &mise::render(&stack, &ports))?;
        ports
    } else {
        ports::lookup(&opts.state, &opts.root)?
    };

    let warnings = warnings(&stack);
    Ok(Report {
        bundles: reports,
        warnings,
        stack,
        ports,
        lock_changed,
        provider: "mise",
        output,
        written: opts.write,
    })
}

fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    }
    fs::write(path, contents).map_err(|e| io_error(path.display(), e))
}

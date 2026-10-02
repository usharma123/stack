//! Where a bundle comes from, and how it is pinned.

use crate::error::{Result, StackError};
use crate::git;
use crate::hash::hash_dir;
use crate::lock::LockedBundle;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Git { url: String, reference: String },
    Path { path: PathBuf },
}

/// How compile treats the lockfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Use locked commits; resolve only bundles that are new to the lock.
    UseLock,
    /// Re-resolve every ref.
    Update,
    /// Fail rather than change the lock (CI, agents).
    Frozen,
}

#[derive(Debug)]
pub struct Fetched {
    pub dir: PathBuf,
    pub commit: Option<String>,
    pub content_hash: String,
    /// Set when `--update` moved a bundle to a different commit.
    pub moved_from: Option<String>,
}

impl Source {
    pub fn parse(spec: &str, project_root: &Path) -> Result<Self> {
        if let Some(path) = spec.strip_prefix("path:") {
            return Ok(Source::Path {
                path: project_root.join(path),
            });
        }
        if let Some(rest) = spec.strip_prefix("git+") {
            let (url, query) = rest.split_once('?').unwrap_or((rest, ""));
            let reference = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("ref="))
                .filter(|r| !r.is_empty())
                .ok_or_else(|| {
                    StackError::new("ref_required", format!("'{spec}' has no ?ref="))
                        .hint("pin a tag, branch or commit, e.g. git+https://host/repo?ref=v1")
                })?;
            return Ok(Source::Git {
                url: url.to_string(),
                reference: reference.to_string(),
            });
        }
        Err(StackError::new("source_invalid", format!("unrecognised bundle source '{spec}'"))
            .hint("use git+<url>?ref=<ref> or path:<dir>"))
    }

    pub fn fetch(
        &self,
        spec: &str,
        locked: Option<&LockedBundle>,
        mode: Mode,
        cache: &Path,
    ) -> Result<Fetched> {
        match self {
            Source::Git { url, reference } => fetch_git(spec, url, reference, locked, mode, cache),
            Source::Path { path } => fetch_path(spec, path, locked, mode),
        }
    }
}

fn fetch_git(
    spec: &str,
    url: &str,
    reference: &str,
    locked: Option<&LockedBundle>,
    mode: Mode,
    cache: &Path,
) -> Result<Fetched> {
    let locked_commit = locked.and_then(|l| l.commit.clone());
    let (commit, moved_from) = match (mode, locked_commit) {
        (Mode::UseLock | Mode::Frozen, Some(commit)) => {
            let mut mirror = git::ensure_mirror(cache, url, false)?;
            if !git::has_commit(&mirror, &commit) {
                mirror = git::ensure_mirror(cache, url, true)?;
            }
            if !git::has_commit(&mirror, &commit) {
                return Err(StackError::new(
                    "locked_commit_missing",
                    format!("{spec}: locked commit {commit} no longer exists upstream"),
                )
                .hint("run `stack compile --update` to re-resolve, then review the change"));
            }
            (commit, None)
        }
        (Mode::Frozen, None) => {
            return Err(StackError::new(
                "lock_outdated",
                format!("{spec} is not in stack.lock"),
            )
            .hint("run `stack compile` and commit stack.lock"));
        }
        (_, previous) => {
            let mirror = git::ensure_mirror(cache, url, true)?;
            let commit = git::resolve(&mirror, reference)?;
            let moved = previous.filter(|p| *p != commit);
            (commit, moved)
        }
    };

    let mirror = git::mirror_path(cache, url);
    let dir = cache.join("bundles").join(&commit);
    git::materialize(&mirror, &commit, &dir)?;
    let content_hash = hash_dir(&dir)?;

    if let Some(locked) = locked {
        if locked.commit.as_deref() == Some(commit.as_str()) && locked.content_hash != content_hash {
            return Err(StackError::new(
                "content_hash_mismatch",
                format!("{spec}: contents of {commit} do not match stack.lock"),
            )
            .hint(format!("delete {} and retry; if it persists, the lock was edited", dir.display())));
        }
    }

    Ok(Fetched {
        dir,
        commit: Some(commit),
        content_hash,
        moved_from,
    })
}

fn fetch_path(spec: &str, path: &Path, locked: Option<&LockedBundle>, mode: Mode) -> Result<Fetched> {
    let dir = path.canonicalize().map_err(|e| {
        StackError::new("bundle_not_found", format!("{spec}: {e}"))
    })?;
    let content_hash = hash_dir(&dir)?;
    if mode == Mode::Frozen && locked.map(|l| &l.content_hash) != Some(&content_hash) {
        return Err(StackError::new(
            "lock_outdated",
            format!("{spec} changed since stack.lock was written"),
        )
        .hint("run `stack compile` and commit stack.lock"));
    }
    Ok(Fetched {
        dir,
        commit: None,
        content_hash,
        moved_from: None,
    })
}

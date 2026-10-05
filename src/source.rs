//! Where a bundle comes from, and how it is pinned.

use crate::error::{io_error, Result, StackError};
use crate::git;
use crate::hash::hash_dir;
use crate::lock::LockedBundle;
use crate::oci::{self, Reference};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// `dir` selects a bundle in a subdirectory of the repository.
    Git { url: String, reference: String, dir: Option<PathBuf> },
    Oci(Reference),
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
    pub digest: Option<String>,
    pub content_hash: String,
    /// Set when `--update` moved a bundle to a different commit or digest.
    pub moved_from: Option<String>,
}

impl Source {
    pub fn parse(spec: &str, project_root: &Path) -> Result<Self> {
        if let Some(path) = spec.strip_prefix("path:") {
            return Ok(Source::Path {
                path: project_root.join(path),
            });
        }
        if let Some(rest) = spec.strip_prefix("oci:") {
            return Ok(Source::Oci(Reference::parse(rest)?));
        }
        if let Some(rest) = spec.strip_prefix("git+") {
            let (url, query) = rest.split_once('?').unwrap_or((rest, ""));
            let mut reference = None;
            let mut dir = None;
            for pair in query.split('&').filter(|p| !p.is_empty()) {
                let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
                // Decoded once, after splitting, so `%26` and `%3D` can name a literal `&` or `=`.
                let value = String::from_utf8(crate::session::percent_decode(raw)).map_err(|_| {
                    StackError::new("source_invalid", format!("'{spec}': {key} is not valid UTF-8 once decoded"))
                })?;
                let slot = match key {
                    "ref" => &mut reference,
                    "dir" => &mut dir,
                    _ => {
                        return Err(StackError::new(
                            "source_invalid",
                            format!("'{spec}' has unknown parameter '{key}'"),
                        )
                        .hint("git sources accept only ?ref=<ref> and &dir=<subdirectory>"));
                    }
                };
                if slot.replace(value).is_some() {
                    return Err(StackError::new(
                        "source_invalid",
                        format!("'{spec}' sets '{key}' more than once"),
                    ));
                }
            }
            let reference = reference.filter(|r| !r.is_empty()).ok_or_else(|| {
                StackError::new("ref_required", format!("'{spec}' has no ?ref="))
                    .hint("pin a tag, branch or commit, e.g. git+https://host/repo?ref=v1")
            })?;
            let dir = dir.map(|d| subdirectory(spec, &d)).transpose()?;
            return Ok(Source::Git {
                url: url.to_string(),
                reference,
                dir,
            });
        }
        Err(StackError::new("source_invalid", format!("unrecognised bundle source '{spec}'"))
            .hint("use git+<url>?ref=<ref>, oci:<registry>/<repo>:<tag> or path:<dir>"))
    }

    pub fn fetch(
        &self,
        spec: &str,
        locked: Option<&LockedBundle>,
        mode: Mode,
        cache: &Path,
    ) -> Result<Fetched> {
        match self {
            Source::Git { url, reference, dir } => {
                fetch_git(spec, url, reference, dir.as_deref(), locked, mode, cache)
            }
            Source::Oci(r) => fetch_oci(spec, r, locked, mode, cache),
            Source::Path { path } => fetch_path(spec, path, locked, mode),
        }
    }
}

/// A relative path inside the repository. Rejects anything that could leave the checkout.
fn subdirectory(spec: &str, dir: &str) -> Result<PathBuf> {
    let path = Path::new(dir);
    let inside = !dir.is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir));
    if !inside {
        return Err(StackError::new(
            "source_invalid",
            format!("'{spec}': dir '{dir}' must be a relative path inside the repository"),
        ));
    }
    Ok(path.to_path_buf())
}

fn fetch_git(
    spec: &str,
    url: &str,
    reference: &str,
    subdir: Option<&Path>,
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
    let checkout = cache.join("bundles").join(&commit);
    git::materialize(&mirror, &commit, &checkout)?;
    let dir = match subdir {
        None => checkout,
        Some(sub) => {
            // Symlinks in the repository must not move the bundle outside its checkout.
            let base = checkout.canonicalize().map_err(|e| io_error(checkout.display(), e))?;
            let real = checkout.join(sub).canonicalize().ok();
            real.filter(|d| d.starts_with(&base) && d.is_dir()).ok_or_else(|| {
                StackError::new(
                    "bundle_not_found",
                    format!("{spec}: commit {commit} has no directory '{}'", sub.display()),
                )
            })?
        }
    };
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
        digest: None,
        content_hash,
        moved_from,
    })
}

fn fetch_oci(
    spec: &str,
    r: &Reference,
    locked: Option<&LockedBundle>,
    mode: Mode,
    cache: &Path,
) -> Result<Fetched> {
    let client = oci::Client::default();
    let locked_digest = locked.and_then(|l| l.digest.clone());
    let (digest, moved_from) = match (mode, locked_digest) {
        (Mode::UseLock | Mode::Frozen, Some(d)) => (d, None),
        (Mode::Frozen, None) => {
            return Err(StackError::new("lock_outdated", format!("{spec} is not in stack.lock"))
                .hint("run `stack compile` and commit stack.lock"));
        }
        (_, previous) => {
            let digest = client.resolve(r)?;
            let moved = previous.filter(|p| *p != digest);
            (digest, moved)
        }
    };

    let dir = cache.join("bundles").join(format!("oci-{}", digest.trim_start_matches("sha256:")));
    client.pull(r, &digest, &dir)?;
    let content_hash = hash_dir(&dir)?;
    if let Some(locked) = locked {
        if locked.digest.as_deref() == Some(digest.as_str()) && locked.content_hash != content_hash {
            return Err(StackError::new("content_hash_mismatch", format!("{spec}: contents of {digest} do not match stack.lock"))
                .hint(format!("delete {} and retry; if it persists, the lock was edited", dir.display())));
        }
    }
    Ok(Fetched { dir, commit: None, digest: Some(digest), content_hash, moved_from })
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
        digest: None,
        content_hash,
        moved_from: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(spec: &str) -> Result<Source> {
        Source::parse(spec, Path::new("/p"))
    }

    #[test]
    fn git_sources_accept_ref_and_dir() {
        assert_eq!(
            git("git+https://h/r?ref=v1&dir=bundles/py").unwrap(),
            Source::Git {
                url: "https://h/r".into(),
                reference: "v1".into(),
                dir: Some(PathBuf::from("bundles/py")),
            }
        );
        assert_eq!(
            git("git+https://h/r?dir=./py&ref=v1").unwrap(),
            Source::Git { url: "https://h/r".into(), reference: "v1".into(), dir: Some(PathBuf::from("./py")) }
        );
    }

    #[test]
    fn git_sources_reject_unknown_repeated_and_escaping_parameters() {
        for spec in [
            "git+https://h/r?ref=v1&dir=%2E%2E/x",
            "git+https://h/r?ref=v1&dir=a%2F..%2F..%2Fx",
            "git+https://h/r?ref=v1&dir=%FF",
            "git+https://h/r?ref=v1&sub=x",
            "git+https://h/r?ref=v1&ref=v2",
            "git+https://h/r?ref=v1&dir=../x",
            "git+https://h/r?ref=v1&dir=/etc",
            "git+https://h/r?ref=v1&dir=",
            "git+https://h/r?ref=v1&dir=a/../../x",
        ] {
            assert_eq!(git(spec).unwrap_err().code, "source_invalid", "{spec}");
        }
        assert_eq!(git("git+https://h/r?dir=x").unwrap_err().code, "ref_required");
    }

    #[test]
    fn git_parameters_are_percent_decoded_once() {
        let Source::Git { reference, dir, .. } = git("git+https://h/r?ref=feat%2Fa%2Bb&dir=a%26b/c%20d/%2541").unwrap() else {
            panic!("not git");
        };
        assert_eq!(reference, "feat/a+b");
        assert_eq!(dir, Some(PathBuf::from("a&b/c d/%41")));
    }
}

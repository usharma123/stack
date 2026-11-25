//! Where a bundle comes from, and how it is pinned.

use crate::error::{io_error, Result, StackError};
use crate::git;
use crate::hash::hash_keeping;
use crate::lock::LockedBundle;
use crate::manifest::BUNDLE_FILE;
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
    /// The bytes of the bundle's `bundle.toml` exactly as `content_hash` covered them (see
    /// [`crate::manifest::parse_bundle`]); `None` when the tree has none.
    pub manifest: Option<Vec<u8>>,
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
        let err = StackError::new("source_invalid", format!("unrecognised bundle source '{spec}'"));
        let local = ["./", "../", "/", "~"].iter().any(|p| spec.starts_with(p)) || project_root.join(spec).is_dir();
        Err(if local {
            let value = toml::Value::String(format!("path:{spec}"));
            err.hint(format!("a local bundle directory is written bundle = {value}"))
        } else {
            err.hint("use git+<url>?ref=<ref>, oci:<registry>/<repo>:<tag> or path:<dir>")
        })
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
            // It names a cache directory and is handed to git: only a commit id may.
            if !is_commit(&commit) {
                let shown: String = commit.chars().take(80).collect();
                return Err(StackError::new("lock_invalid", format!("{spec}: the commit stack.lock records is not a commit id: \"{}\"", shown.escape_debug()))
                    .hint("run `stack compile --update` to re-resolve, then review the change"));
            }
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
            if !is_commit(&commit) {
                return Err(StackError::new("materialize_failed", format!("{spec}: git resolved {reference} to something that is not a commit id")));
            }
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
    let (content_hash, manifest) = hash_keeping(&dir, BUNDLE_FILE)?;

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
        manifest,
        moved_from,
    })
}

/// Whether `value` is a full Git object id (SHA-1 or SHA-256), lowercase hex.
fn is_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
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
        (Mode::UseLock | Mode::Frozen, Some(d)) => {
            oci::digest(&d, "lock_invalid", &format!("{spec}: the digest stack.lock records"))?;
            (d, None)
        }
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

    // Only a digest may name the cache directory (the registry's answer is hashed here, so it
    // is one; a reference's and the lock's are checked above).
    oci::digest(&digest, "oci_failed", &format!("{spec}: the manifest digest"))?;
    let dir = cache.join("bundles").join(format!("oci-{}", digest.trim_start_matches("sha256:")));
    client.pull(r, &digest, &dir)?;
    // A layer with any of these is refused when expanded; a cached tree with one was filled
    // otherwise (an earlier Stack compared names exactly), with content no hash covers.
    if let Some(found) = crate::hash::first_excluded(&dir)? {
        return Err(StackError::new(
            "bundle_invalid",
            format!("{spec}: the bundle cached at {} holds {}, which bundles cannot contain", dir.display(), found.display()),
        )
        .hint(format!("delete {} and retry", dir.display())));
    }
    let (content_hash, manifest) = hash_keeping(&dir, BUNDLE_FILE)?;
    // The cached tree is reused only with evidence that it is what `digest` holds: stack.lock
    // pinning this digest with this content hash, or what this Stack recorded when it installed
    // it (both checked offline); otherwise the registry is asked again (see
    // `oci::Client::verify_cached`).
    match locked.filter(|l| l.digest.as_deref() == Some(digest.as_str())) {
        Some(locked) if locked.content_hash != content_hash => {
            return Err(StackError::new("content_hash_mismatch", format!("{spec}: contents of {digest} do not match stack.lock"))
                .hint(format!("delete {} and retry; if it persists, the lock was edited", dir.display())));
        }
        Some(_) => {}
        None if oci::Evidence::matches(&dir, &digest, &content_hash) => {}
        None => client.verify_cached(r, &digest, &dir, &content_hash)?,
    }
    Ok(Fetched { dir, commit: None, digest: Some(digest), content_hash, manifest, moved_from })
}

fn fetch_path(spec: &str, path: &Path, locked: Option<&LockedBundle>, mode: Mode) -> Result<Fetched> {
    let dir = path.canonicalize().map_err(|e| {
        StackError::new("bundle_not_found", format!("{spec}: {e}"))
    })?;
    let (content_hash, manifest) = hash_keeping(&dir, BUNDLE_FILE)?;
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
        manifest,
        moved_from: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(spec: &str) -> Result<Source> {
        Source::parse(spec, Path::new("/p"))
    }

    /// The manifest a compile parses is the one its content hash covered: changed after the
    /// tree was hashed, the bytes kept are still the ones hashed.
    #[test]
    fn the_manifest_parsed_is_the_one_hashed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bundle.toml"), "[bundle]\nname = 'hashed'\n").unwrap();
        let fetched = Source::parse(&format!("path:{}", dir.path().display()), Path::new("/")).unwrap().fetch("path:b", None, Mode::UseLock, dir.path()).unwrap();
        std::fs::write(dir.path().join("bundle.toml"), "[bundle]\nname = 'changed'\n").unwrap();
        let manifest = crate::manifest::parse_bundle(fetched.manifest.as_deref(), &fetched.dir, "path:b").unwrap();
        assert_eq!(manifest.bundle.name, "hashed");
        assert_ne!(crate::hash::hash_dir(dir.path()).unwrap(), fetched.content_hash, "the hash was of the bytes parsed");
        let e = crate::manifest::parse_bundle(None, dir.path(), "path:b").unwrap_err();
        assert_eq!(e.code, "bundle_invalid");
    }

    /// A local bundle edited after stack.lock recorded it: locked mode refuses it (lock_outdated)
    /// until an ordinary compile hashes it again, and that hash is the edited content's.
    #[test]
    fn a_local_bundle_edited_after_its_lock_is_outdated_until_compiled_again() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bundle.toml"), "[bundle]\nname = 'b'\n").unwrap();
        let source = Source::parse(&format!("path:{}", dir.path().display()), Path::new("/")).unwrap();
        let first = source.fetch("path:b", None, Mode::UseLock, dir.path()).unwrap();
        let locked = LockedBundle { source: "path:b".into(), name: "b".into(), commit: None, digest: None, content_hash: first.content_hash.clone() };
        assert_eq!(source.fetch("path:b", Some(&locked), Mode::Frozen, dir.path()).unwrap().content_hash, first.content_hash);

        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/tool"), "edited").unwrap();
        let e = source.fetch("path:b", Some(&locked), Mode::Frozen, dir.path()).unwrap_err();
        assert_eq!(e.code, "lock_outdated");
        let again = source.fetch("path:b", Some(&locked), Mode::UseLock, dir.path()).unwrap();
        assert_ne!(again.content_hash, first.content_hash);
        assert_eq!(again.content_hash, crate::hash::hash_dir(dir.path()).unwrap());
    }

    #[test]
    fn only_commit_ids_name_git_bundle_checkouts() {
        assert!(is_commit(&"a".repeat(40)) && is_commit(&"0".repeat(64)));
        for bad in ["", "abc", "../../x", "-p", &"A".repeat(40), &"a".repeat(41)] {
            assert!(!is_commit(bad), "{bad}");
        }
        let locked = LockedBundle { source: "git+https://example.invalid/r?ref=v1".into(), name: "b".into(), commit: Some("../../dummy".into()), digest: None, content_hash: String::new() };
        let cache = tempfile::tempdir().unwrap();
        let source = git("git+https://example.invalid/r?ref=v1").unwrap();
        let e = source.fetch("git+https://example.invalid/r?ref=v1", Some(&locked), Mode::UseLock, cache.path()).unwrap_err();
        assert_eq!(e.code, "lock_invalid", "{e:?}");
        assert!(!cache.path().join("bundles").exists());
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
    fn a_bare_local_path_is_answered_with_its_path_form() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("team bundle")).unwrap();
        for (spec, hint) in [
            ("../bundles/team", r#"bundle = "path:../bundles/team""#),
            ("/abs/b", r#"bundle = "path:/abs/b""#),
            ("team bundle", r#"bundle = "path:team bundle""#),
            (r#"./say "hi""#, r#"bundle = 'path:./say "hi"'"#),
        ] {
            let err = Source::parse(spec, dir.path()).unwrap_err();
            assert_eq!(err.code, "source_invalid");
            assert!(err.hint.as_deref().unwrap().ends_with(hint), "{spec}: {:?}", err.hint);
        }
        let err = git("github.com/o/r").unwrap_err();
        assert!(err.hint.unwrap().starts_with("use git+"));
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

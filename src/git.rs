//! Thin wrapper over the `git` CLI, so every transport git supports works (https, ssh, file).

use crate::error::{io_error, Result, StackError};
use crate::hash::sha256_hex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    // Never block on a credential prompt: agents can't answer it.
    cmd.args(args).env("GIT_TERMINAL_PROMPT", "0");
    // Bounded by the caller's deadline, as when `up` fetches a locked bundle.
    let out = crate::process::output(&mut cmd)
        .map_err(|e| StackError::new("git_unavailable", format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(StackError::new(
            "git_failed",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn mirror_path(cache: &Path, url: &str) -> PathBuf {
    cache.join("git").join(&sha256_hex(url.as_bytes())[..16])
}

/// Clone a bare mirror if missing; refresh branches and tags when `fetch` is set.
pub fn ensure_mirror(cache: &Path, url: &str, fetch: bool) -> Result<PathBuf> {
    let mirror = mirror_path(cache, url);
    if !mirror.exists() {
        let parent = mirror.parent().expect("mirror has parent");
        fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
        // Cloned aside first: a clone cut short (by a deadline, say) must not pass for a mirror.
        let staging = mirror.with_extension("tmp");
        let _ = fs::remove_dir_all(&staging);
        let target = staging.to_string_lossy().to_string();
        git(None, &["clone", "--bare", "--quiet", url, &target])?;
        fs::rename(&staging, &mirror).map_err(|e| io_error(mirror.display(), e))?;
    } else if fetch {
        git(
            Some(&mirror),
            &[
                "fetch",
                "--quiet",
                "--force",
                "--prune",
                url,
                "+refs/heads/*:refs/heads/*",
                "+refs/tags/*:refs/tags/*",
            ],
        )?;
    }
    Ok(mirror)
}

pub fn resolve(mirror: &Path, reference: &str) -> Result<String> {
    git(Some(mirror), &["rev-parse", "--verify", "--quiet", &format!("{reference}^{{commit}}")])
        .map_err(|_| {
            StackError::new("ref_not_found", format!("ref '{reference}' not found"))
                .hint("check the ?ref= value; it must be a branch, tag or commit")
        })
}

pub fn has_commit(mirror: &Path, commit: &str) -> bool {
    git(Some(mirror), &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

/// Extract the tree at `commit` into `dest` (atomically, via a temp dir).
pub fn materialize(mirror: &Path, commit: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let tmp = dest.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| io_error(tmp.display(), e))?;

    // Through a file rather than a pipe, so each step is bounded by the caller's deadline.
    // Absolute, since git resolves `-o` inside the mirror.
    let archive = std::path::absolute(dest.with_extension("tar")).map_err(|e| io_error(dest.display(), e))?;
    let extracted = git(Some(mirror), &["archive", "--format=tar", "-o", &archive.to_string_lossy(), commit])
        .and_then(|_| {
            crate::process::output(Command::new("tar").arg("-x").arg("-f").arg(&archive).arg("-C").arg(&tmp))
                .map_err(|e| StackError::new("tar_unavailable", format!("cannot run tar: {e}")))
        });
    let _ = fs::remove_file(&archive);
    match extracted {
        Ok(out) if out.status.success() => {}
        failed => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(match failed {
                Err(e) if e.code.ends_with("_unavailable") => e,
                _ => StackError::new("materialize_failed", format!("could not extract commit {commit}")),
            });
        }
    }
    fs::rename(&tmp, dest).map_err(|e| io_error(dest.display(), e))
}

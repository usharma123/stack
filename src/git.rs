//! Thin wrapper over the `git` CLI, so every transport git supports works (https, ssh, file).

use crate::error::{io_error, Result, StackError};
use crate::hash::sha256_hex;
use crate::state::FileLock;
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
    // A mirror only ever appears whole, so one that exists can be read without the lock.
    let missing = !mirror.exists();
    if !missing && !fetch {
        return Ok(mirror);
    }
    let _lock = lock(&mirror)?;
    if !mirror.exists() {
        // Cloned aside first: a clone cut short (by a deadline, say) must not pass for a mirror.
        let staging = Staging::new(&mirror)?;
        let target = staging.dir.join("mirror");
        git(None, &["clone", "--bare", "--quiet", url, &target.to_string_lossy()])?;
        fs::rename(&target, &mirror).map_err(|e| io_error(mirror.display(), e))?;
    } else if fetch && !missing {
        // When it was missing, whoever held the lock meanwhile just cloned it fresh.
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

/// Exclusive use of `dest` and its staging area among stack processes. Waiting for another
/// holder is bounded by the caller's deadline.
fn lock(dest: &Path) -> Result<FileLock> {
    FileLock::acquire(&dest.with_extension("lock")).map_err(|e| match e.code {
        "lock_busy" => e.hint("another stack command is fetching the same bundle; retry when it finishes"),
        _ => e,
    })
}

/// A directory to build `dest` in, removed with everything in it when dropped. Hold `dest`'s
/// lock for as long as this lives: the staging area is the lock holder's alone, so whatever
/// it holds beforehand was left by a holder that was killed, and is removed first.
struct Staging {
    root: PathBuf,
    /// Unique, so a process left running by a killed holder cannot write into it.
    dir: PathBuf,
}

impl Staging {
    fn new(dest: &Path) -> Result<Self> {
        let root = dest.with_extension("staging");
        match fs::remove_dir_all(&root) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io_error(root.display(), e)),
            _ => {}
        }
        fs::create_dir_all(&root).map_err(|e| io_error(root.display(), e))?;
        // Built in place, so the root is removed again if the next step fails.
        let mut staging = Self { root, dir: PathBuf::new() };
        staging.dir = tempfile::tempdir_in(&staging.root).map_err(|e| io_error(staging.root.display(), e))?.keep();
        Ok(staging)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
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

/// Extract the tree at `commit` into `dest` (atomically, via a staging directory).
pub fn materialize(mirror: &Path, commit: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let _lock = lock(dest)?;
    if dest.exists() {
        return Ok(());
    }
    let staging = Staging::new(dest)?;
    let tree = staging.dir.join("tree");
    fs::create_dir(&tree).map_err(|e| io_error(tree.display(), e))?;

    // Through a file rather than a pipe, so each step is bounded by the caller's deadline.
    // Absolute, since git resolves `-o` inside the mirror.
    let archive = std::path::absolute(staging.dir.join("tree.tar")).map_err(|e| io_error(dest.display(), e))?;
    let extracted = git(Some(mirror), &["archive", "--format=tar", "-o", &archive.to_string_lossy(), commit])
        .and_then(|_| {
            crate::process::output(Command::new("tar").arg("-x").arg("-f").arg(&archive).arg("-C").arg(&tree))
                .map_err(|e| StackError::new("tar_unavailable", format!("cannot run tar: {e}")))
        });
    match extracted {
        Ok(out) if out.status.success() => {}
        Err(e) if e.code.ends_with("_unavailable") => return Err(e),
        _ => return Err(StackError::new("materialize_failed", format!("could not extract commit {commit}"))),
    }
    fs::rename(&tree, dest).map_err(|e| io_error(dest.display(), e))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    /// A repository with one commit, and that commit.
    fn upstream(dir: &Path) -> (String, String) {
        let repo = dir.join("upstream");
        fs::create_dir(&repo).unwrap();
        fs::write(repo.join("bundle.toml"), "[bundle]\nname = 'b'\n").unwrap();
        for args in [&["init", "-q"][..], &["add", "-A"], &["commit", "-q", "-m", "v1"]] {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        let commit = git(Some(&repo), &["rev-parse", "HEAD"]).unwrap();
        (repo.to_string_lossy().into_owned(), commit)
    }

    fn entries(dir: &Path) -> BTreeSet<String> {
        fs::read_dir(dir).map_or_else(|_| BTreeSet::new(), |d| d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
    }

    /// What a finished operation may leave beside `dest`: `dest` itself and its lock file.
    fn only_published(dest: &Path) -> BTreeSet<String> {
        let name = dest.file_name().unwrap().to_string_lossy();
        [name.to_string(), format!("{name}.lock")].into()
    }

    #[test]
    fn concurrent_first_fetches_publish_one_complete_mirror_and_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(tmp.path());
        let cache = tmp.path().join("cache");
        let dest = cache.join("bundles").join(&commit);
        let start = Arc::new(Barrier::new(8));
        let callers: Vec<_> = (0..8)
            .map(|i| {
                let (cache, url, commit, dest, start) = (cache.clone(), url.clone(), commit.clone(), dest.clone(), start.clone());
                thread::spawn(move || {
                    start.wait();
                    let mirror = ensure_mirror(&cache, &url, i % 2 == 0)?;
                    // Each caller can use what it was handed, whoever cloned it.
                    assert!(has_commit(&mirror, &commit), "caller {i} got a mirror without {commit}");
                    materialize(&mirror, &commit, &dest)?;
                    assert!(dest.join("bundle.toml").is_file(), "caller {i} got an incomplete bundle");
                    Ok::<_, StackError>(mirror)
                })
            })
            .collect();
        let mirrors: BTreeSet<PathBuf> = callers.into_iter().map(|c| c.join().unwrap().unwrap()).collect();
        assert_eq!(mirrors, [mirror_path(&cache, &url)].into());
        assert_eq!(entries(&cache.join("git")), only_published(&mirror_path(&cache, &url)));
        assert_eq!(entries(&cache.join("bundles")), only_published(&dest));
    }

    #[test]
    fn a_mirror_published_while_waiting_for_the_lock_is_used_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(tmp.path());
        let cache = tmp.path().join("cache");
        let mirror = mirror_path(&cache, &url);
        let held = FileLock::acquire(&mirror.with_extension("lock")).unwrap();
        let waiter = {
            let (cache, url) = (cache.clone(), url.clone());
            thread::spawn(move || ensure_mirror(&cache, &url, false))
        };
        thread::sleep(Duration::from_millis(300));
        assert!(!waiter.is_finished(), "cloned without the mirror's lock");
        assert_eq!(entries(&cache.join("git")), [format!("{}.lock", mirror.file_name().unwrap().to_string_lossy())].into(), "staged without the lock");

        // Another holder publishes the mirror, then lets go.
        git(None, &["clone", "--bare", "--quiet", &url, &mirror.to_string_lossy()]).unwrap();
        fs::write(mirror.join("published-by-the-holder"), "").unwrap();
        drop(held);
        assert_eq!(waiter.join().unwrap().unwrap(), mirror);
        assert!(mirror.join("published-by-the-holder").exists(), "the published mirror was replaced");
        assert!(has_commit(&mirror, &commit));
        assert_eq!(entries(&cache.join("git")), only_published(&mirror));
    }

    #[test]
    fn waiting_for_a_busy_mirror_or_bundle_ends_at_the_deadline_and_touches_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(tmp.path());
        let cache = tmp.path().join("cache");
        let mirror = mirror_path(&cache, &url);
        let dest = cache.join("bundles").join(&commit);
        let _held = (FileLock::acquire(&mirror.with_extension("lock")).unwrap(), FileLock::acquire(&dest.with_extension("lock")).unwrap());
        let source = ensure_mirror(tmp.path().join("other").as_path(), &url, false).unwrap();
        for (attempt, dir) in [("mirror", cache.join("git")), ("bundle", cache.join("bundles"))] {
            let before = entries(&dir);
            let start = Instant::now();
            let busy = {
                let _deadline = crate::process::deadline_scope(Some(start + Duration::from_millis(300)));
                match attempt {
                    "mirror" => ensure_mirror(&cache, &url, true).map(drop),
                    _ => materialize(&source, &commit, &dest),
                }
                .expect_err("the lock is held")
            };
            assert_eq!(busy.code, "lock_busy", "{attempt}: {busy}");
            assert!(busy.hint.as_deref().is_some_and(|h| h.contains("same bundle")), "{attempt}: {busy}");
            assert!(start.elapsed() < Duration::from_secs(3), "{attempt}: {:?}", start.elapsed());
            assert_eq!(entries(&dir), before, "{attempt}");
        }
    }

    #[test]
    fn a_clone_cut_short_publishes_nothing_and_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        // Accepts connections and never answers, as a stalled remote would.
        let silent = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}/repo.git", silent.local_addr().unwrap().port());
        let start = Instant::now();
        let cut = {
            let _deadline = crate::process::deadline_scope(Some(start + Duration::from_millis(500)));
            ensure_mirror(&cache, &url, false).expect_err("the remote never answers")
        };
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}: {cut}", start.elapsed());
        let mirror = mirror_path(&cache, &url);
        assert_eq!(entries(&cache.join("git")), [format!("{}.lock", mirror.file_name().unwrap().to_string_lossy())].into(), "{cut}");
    }

    #[test]
    fn what_a_killed_holder_left_staged_is_removed_and_never_published() {
        let tmp = tempfile::tempdir().unwrap();
        let (url, commit) = upstream(tmp.path());
        let cache = tmp.path().join("cache");
        let mirror = mirror_path(&cache, &url);
        let dest = cache.join("bundles").join(&commit);
        for left in [mirror.with_extension("staging"), dest.with_extension("staging")] {
            fs::create_dir_all(left.join("tmp-of-a-killed-holder/mirror")).unwrap();
            fs::write(left.join("tmp-of-a-killed-holder/mirror/partial"), "").unwrap();
        }
        ensure_mirror(&cache, &url, false).unwrap();
        materialize(&mirror, &commit, &dest).unwrap();
        assert!(!mirror.join("partial").exists() && has_commit(&mirror, &commit));
        assert!(dest.join("bundle.toml").is_file());
        assert_eq!(entries(&cache.join("git")), only_published(&mirror));
        assert_eq!(entries(&cache.join("bundles")), only_published(&dest));
    }
}

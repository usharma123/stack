//! What a checkout's Git directory says, read from its files without running `git`: where the
//! worktree's Git directory and the repository's common directory are, and which branch and
//! commit `HEAD` names. `ps` reads this for every checkout it lists, so it must cost a few file
//! reads, never a process.
//!
//! Only what Git itself writes is understood: a `.git` directory, or a `.git` file holding
//! `gitdir: <path>` (a linked worktree or a submodule), whose Git directory may name the common
//! directory in `commondir`; `HEAD` holding `ref: <ref>` or an object name; refs as loose files
//! or in `packed-refs`. Anything else (a reftable repository, `GIT_DIR` set in the environment)
//! reads as unknown, never as something wrong.

use std::fs;
use std::path::{Path, PathBuf};

/// Where a checkout's Git data is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirs {
    /// The worktree's top directory: the one holding `.git`.
    pub toplevel: PathBuf,
    /// This worktree's own Git directory (its `HEAD`).
    pub git_dir: PathBuf,
    /// The directory every worktree of the repository shares (branches, `packed-refs`),
    /// canonical: the same path `git rev-parse --git-common-dir` gives.
    pub common_dir: PathBuf,
}

/// The Git directories of the worktree holding `start` (canonical), searching upward for `.git`
/// as Git does. `None` outside Git or when what `.git` holds cannot be read.
pub fn locate(start: &Path) -> Option<GitDirs> {
    let mut dir = start;
    loop {
        let dot = dir.join(".git");
        if let Ok(meta) = fs::metadata(&dot) {
            let git_dir = if meta.is_dir() {
                dot
            } else {
                let text = fs::read_to_string(&dot).ok()?;
                let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
                dir.join(target)
            };
            let common = match fs::read_to_string(git_dir.join("commondir")) {
                Ok(text) => git_dir.join(text.trim()),
                Err(_) => git_dir.clone(),
            };
            return Some(GitDirs {
                toplevel: fs::canonicalize(dir).ok()?,
                git_dir: fs::canonicalize(&git_dir).ok()?,
                common_dir: fs::canonicalize(common).ok()?,
            });
        }
        dir = dir.parent()?;
    }
}

/// What `HEAD` names: the branch (without `refs/heads/`; `None` when detached) and the commit,
/// abbreviated to 7 characters (`None` when it cannot be read, as on a branch with no commit).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Head {
    pub branch: Option<String>,
    pub commit: Option<String>,
}

/// Most `ref:` indirections followed, as Git limits symbolic ref chains.
const MAX_DEPTH: usize = 5;

/// [`Head`] of the worktree whose directories are `dirs`.
pub fn head(dirs: &GitDirs) -> Head {
    let Ok(text) = fs::read_to_string(dirs.git_dir.join("HEAD")) else { return Head::default() };
    let text = text.trim();
    match text.strip_prefix("ref:") {
        Some(name) => {
            let name = name.trim();
            Head { branch: Some(name.strip_prefix("refs/heads/").unwrap_or(name).to_string()), commit: resolve(dirs, name, 0).map(short) }
        }
        None => Head { branch: None, commit: object_name(text).map(short) },
    }
}

/// The object name a ref points at, following `ref:` indirections.
fn resolve(dirs: &GitDirs, name: &str, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH || name.split('/').any(|part| part.is_empty() || part == "..") {
        return None;
    }
    // A worktree's own refs (`refs/bisect`, `refs/worktree`) are in its Git directory, the
    // shared ones in the common directory.
    for base in [&dirs.git_dir, &dirs.common_dir] {
        if let Ok(text) = fs::read_to_string(base.join(name)) {
            let text = text.trim();
            return match text.strip_prefix("ref:") {
                Some(next) => resolve(dirs, next.trim(), depth + 1),
                None => object_name(text).map(String::from),
            };
        }
    }
    packed(&dirs.common_dir, name)
}

/// `name`'s object in `packed-refs`: lines of `<object> <ref>`, with `#` comments and `^` peeled
/// lines (which follow the tag they peel).
fn packed(common: &Path, name: &str) -> Option<String> {
    let text = fs::read_to_string(common.join("packed-refs")).ok()?;
    text.lines().filter(|l| !l.starts_with('#') && !l.starts_with('^')).find_map(|l| {
        let (object, rest) = l.split_once(' ')?;
        (rest.trim() == name).then(|| object_name(object).map(String::from)).flatten()
    })
}

/// `text` when it is a full object name (SHA-1 or SHA-256, hexadecimal).
fn object_name(text: &str) -> Option<&str> {
    (matches!(text.len(), 40 | 64) && text.chars().all(|c| c.is_ascii_hexdigit())).then_some(text)
}

fn short(object: impl AsRef<str>) -> String {
    object.as_ref()[..7].to_string()
}

/// One worktree as `git worktree list --porcelain` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub bare: bool,
}

/// The repository's worktrees, as `git worktree list --porcelain` run at `dir` lists them (one
/// process for the whole repository). Empty outside Git or when git cannot be run.
pub fn worktrees(dir: &Path) -> Vec<Worktree> {
    let mut command = std::process::Command::new("git");
    command.arg("-C").arg(dir).args(["worktree", "list", "--porcelain"]).env("GIT_TERMINAL_PROMPT", "0");
    let Ok(out) = crate::process::capture(&mut command, std::time::Duration::from_secs(10), 1024 * 1024) else { return Vec::new() };
    if out.exit_code != Some(0) || out.timed_out {
        return Vec::new();
    }
    parse_worktrees(&out.stdout)
}

/// The porcelain format: blocks separated by empty lines, each starting `worktree <path>`.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(Worktree { path: PathBuf::from(path), bare: false });
        } else if line == "bare" {
            if let Some(last) = out.last_mut() {
                last.bare = true;
            }
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn git(at: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(at)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[test]
    fn branch_and_commit_are_read_from_files_in_a_repository_and_its_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        // No commit yet: the branch is known, the commit is not.
        let dirs = locate(&repo).unwrap();
        assert_eq!(head(&dirs), Head { branch: Some("main".into()), commit: None });
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]);
        let common = PathBuf::from(git(&repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])).canonicalize().unwrap();
        assert_eq!(dirs.common_dir, common);
        assert_eq!(dirs.toplevel, repo);
        assert_eq!(head(&dirs), Head { branch: Some("main".into()), commit: Some(sha[..7].to_string()) });

        // A subdirectory finds the worktree above it.
        fs::create_dir_all(repo.join("svc/deep")).unwrap();
        assert_eq!(locate(&repo.join("svc/deep")).unwrap(), dirs);

        // A linked worktree on its own branch: HEAD in its Git directory, the branch shared.
        let wt = repo.parent().unwrap().join("wt");
        git(&repo, &["worktree", "add", "-q", "-b", "feature/x", wt.to_str().unwrap()]);
        let wdirs = locate(&wt).unwrap();
        assert_eq!(wdirs.common_dir, common, "one common directory for every worktree");
        assert_ne!(wdirs.git_dir, dirs.git_dir);
        assert_eq!(head(&wdirs), Head { branch: Some("feature/x".into()), commit: Some(sha[..7].to_string()) });

        // Packed refs: a branch whose loose file is gone is found in packed-refs.
        git(&wt, &["commit", "-q", "--allow-empty", "-m", "y"]);
        let wsha = git(&wt, &["rev-parse", "HEAD"]);
        git(&repo, &["pack-refs", "--all"]);
        assert!(!common.join("refs/heads/feature/x").exists(), "packed");
        assert_eq!(head(&wdirs).commit.as_deref(), Some(&wsha[..7]));
        assert_eq!(head(&dirs).commit.as_deref(), Some(&sha[..7]));

        // Detached: no branch, the commit from HEAD itself.
        git(&wt, &["checkout", "-q", "--detach", &sha]);
        assert_eq!(head(&wdirs), Head { branch: None, commit: Some(sha[..7].to_string()) });

        // The porcelain listing names both worktrees.
        let listed: Vec<PathBuf> = worktrees(&repo).into_iter().map(|w| w.path.canonicalize().unwrap()).collect();
        assert_eq!(listed, vec![repo.clone(), wt.clone()]);

        // Outside Git: nothing.
        let lone = tmp.path().canonicalize().unwrap().join("lone");
        fs::create_dir(&lone).unwrap();
        // (A temporary directory may itself sit inside a repository; only assert when it does not.)
        if locate(tmp.path()).is_none() {
            assert_eq!(locate(&lone), None);
            assert!(worktrees(&lone).is_empty());
        }
    }

    #[test]
    fn what_cannot_be_read_is_unknown_never_wrong() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let git_dir = root.join(".git");
        fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let dirs = locate(&root).unwrap();
        assert_eq!(head(&dirs), Head { branch: Some("main".into()), commit: None });
        fs::write(git_dir.join("refs/heads/main"), "not-a-sha\n").unwrap();
        assert_eq!(head(&dirs).commit, None);
        // A ref chain that loops ends.
        fs::write(git_dir.join("refs/heads/main"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(head(&dirs).commit, None);
        // A ref escaping the Git directory is not followed.
        fs::write(git_dir.join("HEAD"), "ref: ../../etc/passwd\n").unwrap();
        assert_eq!(head(&dirs).commit, None);
        // A packed ref with a peeled tag line after it.
        let sha = "0123456789abcdef0123456789abcdef01234567";
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/dev\n").unwrap();
        fs::write(git_dir.join("packed-refs"), format!("# pack-refs with: peeled\n{sha} refs/heads/dev\n^{}\n", "f".repeat(40))).unwrap();
        assert_eq!(head(&dirs), Head { branch: Some("dev".into()), commit: Some("0123456".into()) });
        // A `.git` file that says nothing usable.
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        fs::write(other.join(".git"), "nonsense").unwrap();
        assert_eq!(locate(&other), None);
    }

    #[test]
    fn porcelain_listings_are_parsed() {
        let text = "worktree /r\nHEAD 0123\nbranch refs/heads/main\n\nworktree /r/wt\nHEAD 4567\ndetached\n\nworktree /bare\nbare\n";
        assert_eq!(
            parse_worktrees(text),
            vec![
                Worktree { path: "/r".into(), bare: false },
                Worktree { path: "/r/wt".into(), bare: false },
                Worktree { path: "/bare".into(), bare: true }
            ]
        );
    }
}

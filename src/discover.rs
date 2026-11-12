//! Which project a command means when `-C` is not given: the nearest `stack.toml` at or above
//! the working directory, searching no higher than the Git worktree root (the directory that
//! holds `.git`), or `$HOME` / the filesystem root outside Git.

use crate::error::StackError;
use crate::manifest::PROJECT_FILE;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Discovery {
    /// The directory holding `stack.toml`, or the starting directory when none was found.
    pub root: PathBuf,
    pub found: bool,
    /// Where the search started.
    pub from: PathBuf,
    /// The last directory searched.
    pub stop: PathBuf,
}

/// Search upward from `start` (already canonical). `home` bounds the search outside Git.
pub fn find(start: &Path, home: Option<&Path>) -> Discovery {
    let mut dir = start;
    loop {
        if dir.join(PROJECT_FILE).is_file() {
            return Discovery { root: dir.to_path_buf(), found: true, from: start.to_path_buf(), stop: dir.to_path_buf() };
        }
        let boundary = dir.join(".git").exists() || home.is_some_and(|h| h == dir);
        match dir.parent() {
            Some(parent) if !boundary => dir = parent,
            _ => return Discovery { root: start.to_path_buf(), found: false, from: start.to_path_buf(), stop: dir.to_path_buf() },
        }
    }
}

/// [`find`] from the process's working directory and `$HOME`.
pub fn from_cwd() -> std::io::Result<Discovery> {
    let cwd = std::env::current_dir()?.canonicalize()?;
    let home = std::env::var_os("HOME").map(PathBuf::from).and_then(|h| h.canonicalize().ok());
    Ok(find(&cwd, home.as_deref()))
}

impl Discovery {
    /// Say where a search that found nothing looked, on a `manifest_missing` error.
    pub fn explain(&self, mut e: StackError) -> StackError {
        if self.found || e.code != "manifest_missing" {
            return e;
        }
        e.message = if self.from == self.stop {
            format!("no {PROJECT_FILE} in {}", self.from.display())
        } else {
            format!("no {PROJECT_FILE} in {} or any directory above it up to {}", self.from.display(), self.stop.display())
        };
        e.hint = Some(format!(
            "run `stack init` to create one here, or pass `-C <dir>` for a project elsewhere (searched from {} up to {})",
            self.from.display(),
            self.stop.display()
        ));
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_nearest_manifest_wins_and_the_search_stops_at_the_worktree_root() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        let repo = top.join("repo");
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        fs::write(top.join(PROJECT_FILE), "").unwrap();

        // Above the worktree root does not count.
        let d = find(&repo.join("src/deep"), None);
        assert!(!d.found);
        assert_eq!(d.root, repo.join("src/deep"));
        assert_eq!(d.stop, repo);

        fs::write(repo.join(PROJECT_FILE), "").unwrap();
        let d = find(&repo.join("src/deep"), None);
        assert!(d.found);
        assert_eq!(d.root, repo);

        fs::write(repo.join("src").join(PROJECT_FILE), "").unwrap();
        assert_eq!(find(&repo.join("src/deep"), None).root, repo.join("src"));
    }

    #[test]
    fn outside_git_the_search_stops_at_home() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        let home = top.join("home");
        fs::create_dir_all(home.join("work/app")).unwrap();
        fs::write(top.join(PROJECT_FILE), "").unwrap();
        let d = find(&home.join("work/app"), Some(&home));
        assert!(!d.found);
        assert_eq!(d.stop, home);
        let e = d.explain(StackError::new("manifest_missing", "cannot read"));
        assert!(e.message.contains("up to"), "{}", e.message);
        assert!(e.hint.unwrap().contains("stack init"));
    }
}

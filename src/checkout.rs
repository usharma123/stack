//! Which directory a checkout is, beyond its path.
//!
//! Agents delete worktrees and create new ones at the same path all the time, and everything
//! keyed by path alone (mise's daemon state, so each preset's data) would pass from one to the
//! next. A checkout's token is derived from its path together with the directory's device,
//! inode and, where the filesystem records one, creation time: the same directory always has
//! the same token, across restarts and without any state of its own, and a directory recreated
//! at that path has another. Filesystems without creation times can reuse an inode, so there a
//! recreated directory may still match; nothing better can be known about it.
//!
//! The token is given to every service in its own environment (never to `exec` or `run`
//! commands), so a process carrying it was launched by the supervisor for this checkout, and it
//! marks the data directories this checkout's services use. It tells checkouts apart; it is
//! not a credential.

use crate::hash::sha256_hex;
use std::path::Path;

/// The variable through which each service receives its checkout's token.
pub const TOKEN_VAR: &str = "STACK_CHECKOUT_TOKEN";

/// The token of the directory at `root` now, if it exists, whichever way its path is spelled.
pub fn token(root: &Path) -> Option<String> {
    let root = &root.canonicalize().ok()?;
    let (dev, ino) = crate::session::dir_id(root)?;
    let created = std::fs::metadata(root)
        .ok()
        .and_then(|m| m.created().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(String::new, |d| format!("{}.{:09}", d.as_secs(), d.subsec_nanos()));
    Some(sha256_hex(format!("{}\0{dev}\0{ino}\0{created}", root.display()).as_bytes())[..32].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_keeps_its_token_and_one_recreated_at_its_path_gets_another() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let first = token(&root).unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(token(&root).unwrap(), first, "stable for the same directory");
        // Hold the old inode so the filesystem cannot hand it to the new directory.
        let held = parent.path().join("held");
        std::fs::rename(&root, &held).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert_ne!(token(&root).unwrap(), first, "a new directory at the same path");
        assert!(token(&parent.path().join("missing")).is_none());
    }
}

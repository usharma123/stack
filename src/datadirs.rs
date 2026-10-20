//! Machine-wide record of the data directories this machine's checkouts' services used.
//!
//! mise keeps a service's data under its own state directory, keyed by the project's path, and
//! nothing removes it when a checkout is deleted. `up` records each service's data directory
//! here with the checkout's token (see [`crate::checkout`]), which it also writes beside the
//! directory as its marker. `stack gc` reports the data of checkouts that are gone, and removes
//! it only when asked (`--reclaim-data`) and only while the directory still carries the marker
//! of the checkout recorded here and the supervisor recorded with it confirms the service is
//! not running, so data another checkout took over, data a service may still use, or data
//! stack never recorded, is never touched.

use crate::error::Result;
use crate::state::{read_json, write_json, FileLock};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    entries: Vec<Entry>,
}

/// One service's data directory, as `up` found it for a checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub project: PathBuf,
    pub service: String,
    pub dir: PathBuf,
    /// The checkout's token, as written into the directory's marker.
    pub checkout: String,
    /// The supervisor that runs the service, to confirm it stopped before its data goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<Supervisor>,
}

/// A service's supervisor identity: the Pitchfork binary, its state directory and the daemon id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supervisor {
    pub pitchfork: PathBuf,
    pub state_dir: PathBuf,
    pub id: String,
}

fn paths(state: &Path) -> (PathBuf, PathBuf) {
    (state.join("data.json"), state.join("data.lock"))
}

/// Record (or replace) where `entry`'s service keeps its data.
pub fn record(state: &Path, entry: Entry) -> Result<()> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    if ledger.entries.contains(&entry) {
        return Ok(());
    }
    ledger.entries.retain(|e| !(e.project == entry.project && e.service == entry.service) && e.dir != entry.dir);
    ledger.entries.push(entry);
    write_json(&file, &ledger)
}

/// Every recorded entry.
pub fn entries(state: &Path) -> Result<Vec<Entry>> {
    Ok(read_json::<Ledger>(&paths(state).0)?.entries)
}

/// Drop the entries `gone` selects, under the ledger's lock.
pub fn forget(state: &Path, gone: impl Fn(&Entry) -> bool) -> Result<()> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut ledger: Ledger = read_json(&file)?;
    let before = ledger.entries.len();
    ledger.entries.retain(|e| !gone(e));
    if ledger.entries.len() != before {
        write_json(&file, &ledger)?;
    }
    Ok(())
}

/// Bytes of regular files under `dir`, never following links.
pub fn size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(project: &str, service: &str, dir: &str, checkout: &str) -> Entry {
        Entry { project: project.into(), service: service.into(), dir: dir.into(), checkout: checkout.into(), supervisor: None }
    }

    #[test]
    fn a_service_has_one_entry_and_a_directory_one_owner() {
        let state = tempfile::tempdir().unwrap();
        record(state.path(), entry("/a", "pg", "/d/a", "t1")).unwrap();
        record(state.path(), entry("/a", "pg", "/d/a", "t1")).unwrap();
        record(state.path(), entry("/a", "redis", "/d/ar", "t1")).unwrap();
        // The same checkout path recreated: its new token replaces the old entry.
        record(state.path(), entry("/a", "pg", "/d/a", "t2")).unwrap();
        // Another project took the directory over: the earlier claim on it goes.
        record(state.path(), entry("/b", "pg", "/d/ar", "t3")).unwrap();
        assert_eq!(entries(state.path()).unwrap(), [entry("/a", "pg", "/d/a", "t2"), entry("/b", "pg", "/d/ar", "t3")]);
        forget(state.path(), |e| e.project == Path::new("/a")).unwrap();
        assert_eq!(entries(state.path()).unwrap(), [entry("/b", "pg", "/d/ar", "t3")]);
    }

    #[test]
    fn size_counts_files_but_never_follows_links() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a"), b"123").unwrap();
        std::fs::write(dir.path().join("sub/b"), b"45").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("big"), vec![0u8; 1000]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert_eq!(size(dir.path()), 5);
    }
}

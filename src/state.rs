//! Machine-wide state: port reservations and the session index. Lives outside any project.

use crate::error::{io_error, Result, StackError};
use crate::hash::sha256_hex;
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn default_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STACK_STATE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("stack");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("state").join("stack")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Short stable key for a project directory.
pub fn project_key(root: &Path) -> String {
    sha256_hex(root.to_string_lossy().as_bytes())[..16].to_string()
}

/// Cross-process exclusive lock held for the lifetime of the guard.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
        }
        // Keep the inode: unlinking a lock file lets other processes lock a different inode.
        // The kernel releases this lock even if its holder crashes.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| io_error(path.display(), e))?;
        file.lock_exclusive()
            .map_err(|e| io_error(path.display(), e))?;
        Ok(Self { _file: file })
    }
}

/// Serialize compile and lifecycle changes for this project, outside its mutable directory.
pub fn project_lock(state: &Path, root: &Path) -> Result<FileLock> {
    FileLock::acquire(
        &state
            .join("projects")
            .join(format!("{}.lock", project_key(root))),
    )
}

pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            StackError::new("state_invalid", format!("{}: {e}", path.display()))
                .hint("delete the file to reset it")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(io_error(path.display(), e)),
    }
}

/// Atomic write: temp file then rename.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    }
    let text = serde_json::to_string_pretty(value).expect("state serializes");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| io_error(path.display(), e))?;
    tmp.write_all(text.as_bytes())
        .map_err(|e| io_error(path.display(), e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| io_error(path.display(), e))?;
    tmp.persist(path)
        .map_err(|e| io_error(path.display(), e.error))?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn pid_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        // Zombies still have a /proc entry; treat them as gone.
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

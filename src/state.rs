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
        if crate::process::deadline().is_none() {
            file.lock_exclusive().map_err(|e| io_error(path.display(), e))?;
            return Ok(Self { _file: file });
        }
        // Under a deadline, wait for another holder only as long as it allows.
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(e) if e.raw_os_error() != fs2::lock_contended_error().raw_os_error() => {
                    return Err(io_error(path.display(), e))
                }
                Err(_) if crate::process::expired() => {
                    return Err(StackError::new("lock_busy", format!("{} is still locked by another stack command", path.display()))
                        .hint("another `stack up`, `restart` or `down` may be running for this project; retry when it finishes"))
                }
                Err(_) => std::thread::sleep(crate::process::bounded(std::time::Duration::from_millis(50))),
            }
        }
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

/// Same answer as `kill -0 <pid>`: the process exists and may be signalled by us.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn pid_alive(pid: u32) -> bool {
    // 0 and values that wrap negative would address process groups, not one process.
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs only the existence and permission checks; nothing is delivered.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

#[cfg(not(unix))]
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_is_waited_for_only_until_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.lock");
        let held = FileLock::acquire(&path).unwrap();
        let start = std::time::Instant::now();
        let busy = {
            let _deadline = crate::process::deadline_scope(Some(start + std::time::Duration::from_millis(300)));
            FileLock::acquire(&path).err().expect("the lock is held")
        };
        assert_eq!(busy.code, "lock_busy");
        assert!(busy.message.contains("project.lock"), "{busy}");
        let waited = start.elapsed();
        assert!(waited >= std::time::Duration::from_millis(300) && waited < std::time::Duration::from_secs(3), "{waited:?}");
        drop(held);
        let _deadline = crate::process::deadline_scope(Some(std::time::Instant::now() + std::time::Duration::from_secs(5)));
        FileLock::acquire(&path).unwrap();
    }

    #[test]
    fn pid_alive_sees_this_process() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn pid_alive_rejects_ids_that_would_address_process_groups() {
        assert!(!pid_alive(0) && !pid_alive(u32::MAX));
    }

    #[test]
    fn pid_alive_is_false_for_a_reaped_child() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!pid_alive(pid));
    }
}

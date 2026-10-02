//! Machine-wide state: port reservations and the session index. Lives outside any project.

use crate::error::{io_error, Result, StackError};
use crate::hash::sha256_hex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn default_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STACK_STATE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("stack");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("state").join("stack")
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Short stable key for a project directory.
pub fn project_key(root: &Path) -> String {
    sha256_hex(root.to_string_lossy().as_bytes())[..16].to_string()
}

/// Cross-process exclusive lock held for the lifetime of the guard.
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
        }
        for _ in 0..200 {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(_) => return Ok(Self { path: path.to_path_buf() }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A crashed holder leaves the file behind; locks are held for milliseconds.
                    let stale = fs::metadata(path)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(30))
                        .unwrap_or(false);
                    if stale {
                        let _ = fs::remove_file(path);
                    } else {
                        sleep(Duration::from_millis(50));
                    }
                }
                Err(e) => return Err(io_error(path.display(), e)),
            }
        }
        Err(StackError::new("state_locked", format!("timed out waiting for {}", path.display())))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(value).expect("state serializes");
    fs::write(&tmp, text).map_err(|e| io_error(tmp.display(), e))?;
    fs::rename(&tmp, path).map_err(|e| io_error(path.display(), e))
}

#[cfg(target_os = "linux")]
pub fn pid_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        // Zombies still have a /proc entry; treat them as gone.
        Ok(stat) => !stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
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

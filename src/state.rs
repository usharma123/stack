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

/// A lock that many holders may share, and one holder may take alone (see [`SharedLock`]).
fn open_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io_error(path.display(), e))
}

/// Cross-process lock, shared or exclusive, held for the lifetime of the guard. Like
/// [`FileLock`], its file is never removed and the kernel releases it with its holder. A file
/// removed or replaced at its path while it was being locked (its directory recreated, say)
/// locks nothing a later command opens, so it is opened again and locked anew.
pub struct SharedLock {
    file: File,
    path: PathBuf,
}

impl SharedLock {
    /// Share the lock, waiting for an exclusive holder as long as the caller's deadline allows.
    pub fn shared(path: &Path) -> Result<Self> {
        let mut file = open_lock(path)?;
        loop {
            let locked = if crate::process::deadline().is_none() {
                FileExt::lock_shared(&file).map(|()| true)
            } else {
                FileExt::try_lock_shared(&file).map(|()| true).or_else(|e| match e.raw_os_error() {
                    code if code == fs2::lock_contended_error().raw_os_error() => Ok(false),
                    _ => Err(e),
                })
            };
            match locked {
                Ok(true) if still_at(&file, path) => return Ok(Self { file, path: path.to_path_buf() }),
                Ok(true) => file = open_lock(path)?,
                Ok(false) if crate::process::expired() => {
                    return Err(StackError::new("lock_busy", format!("{} is still locked by another stack command", path.display()))
                        .hint("another checkout's `stack down` may be stopping this supervisor; retry when it finishes"))
                }
                Ok(false) => std::thread::sleep(crate::process::bounded(std::time::Duration::from_millis(50))),
                Err(e) => return Err(io_error(path.display(), e)),
            }
        }
    }

    /// Take the lock alone within `wait`; `None` while anyone else holds it.
    pub fn exclusive_within(path: &Path, wait: std::time::Duration) -> Result<Option<Self>> {
        let mut file = open_lock(path)?;
        let until = std::time::Instant::now() + crate::process::bounded(wait);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) if still_at(&file, path) => return Ok(Some(Self { file, path: path.to_path_buf() })),
                Ok(()) => file = open_lock(path)?,
                Err(e) if e.raw_os_error() != fs2::lock_contended_error().raw_os_error() => {
                    return Err(io_error(path.display(), e))
                }
                Err(_) if std::time::Instant::now() >= until => return Ok(None),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }

    /// Whether the locked file is still the one at its path: false once it was removed or
    /// replaced, after which a command opening the path locks another file.
    pub fn intact(&self) -> bool {
        still_at(&self.file, &self.path)
    }
}

/// Whether `file` is the file now at `path`.
fn still_at(file: &File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), fs::metadata(path)) {
            (Ok(open), Ok(now)) => open.dev() == now.dev() && open.ino() == now.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
        true
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

    /// Set in the process [`passes_alone`] starts.
    const ALONE: &str = "STACK_STATE_TEST_ALONE";

    /// Run ignored test `name` of this module in a process of its own, expecting it to pass.
    /// A process spawned while a lock's file is open shares that open file, and so the lock,
    /// until it execs; there, no other test's spawn can hold a lock a test just let go of.
    fn passes_alone(name: &str) {
        let name = format!("{}::{name}", module_path!().split_once("::").unwrap().1);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args([name.as_str(), "--exact", "--ignored", "--test-threads=1"]).env(ALONE, "1");
        let run = crate::process::capture(&mut command, std::time::Duration::from_secs(60), 1 << 20).unwrap();
        let shown = format!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        assert!(!run.timed_out, "{name} did not finish within 60s\n{shown}");
        assert!(run.exit_code == Some(0) && run.stdout.contains("1 passed"), "{shown}");
    }

    #[test]
    fn a_shared_lock_shuts_out_an_exclusive_holder_and_waits_for_one() {
        passes_alone("shared_lock_shuts_out_an_exclusive_holder");
    }

    #[test]
    #[ignore = "run by a_shared_lock_shuts_out_an_exclusive_holder_and_waits_for_one"]
    fn shared_lock_shuts_out_an_exclusive_holder() {
        if std::env::var_os(ALONE).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supervisors/x.lock");
        let first = SharedLock::shared(&path).unwrap();
        let second = SharedLock::shared(&path).unwrap();
        let start = std::time::Instant::now();
        assert!(SharedLock::exclusive_within(&path, std::time::Duration::from_millis(300)).unwrap().is_none());
        assert!(start.elapsed() >= std::time::Duration::from_millis(300));
        drop(first);
        assert!(SharedLock::exclusive_within(&path, std::time::Duration::ZERO).unwrap().is_none(), "one holder still shares it");
        drop(second);
        let alone = SharedLock::exclusive_within(&path, std::time::Duration::ZERO).unwrap().expect("nobody holds it");
        // A sharer waits for the exclusive holder, and gets it once that one is done.
        let (sent, got) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn({
            let path = path.clone();
            move || {
                let lock = SharedLock::shared(&path).unwrap();
                sent.send(()).unwrap();
                lock
            }
        });
        assert!(got.recv_timeout(std::time::Duration::from_millis(300)).is_err(), "shared while held alone");
        drop(alone);
        got.recv_timeout(std::time::Duration::from_secs(5)).expect("shared once released");
        let _shared = waiter.join().unwrap();
        // Under a deadline, a sharer gives up when it passes.
        let _alone = {
            drop(_shared);
            SharedLock::exclusive_within(&path, std::time::Duration::ZERO).unwrap().unwrap()
        };
        let _expired = crate::process::deadline_scope(Some(std::time::Instant::now() + std::time::Duration::from_millis(100)));
        assert_eq!(SharedLock::shared(&path).err().map(|e| e.code), Some("lock_busy"));
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_file_replaced_while_it_is_locked_is_locked_anew_and_its_holder_can_tell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pf/stack-supervisor.lock");
        let alone = SharedLock::exclusive_within(&path, std::time::Duration::ZERO).unwrap().unwrap();
        assert!(alone.intact());
        // A sharer waits for it; meanwhile the file is replaced (its directory recreated, say).
        let (sent, got) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn({
            let path = path.clone();
            move || {
                let _deadline = crate::process::deadline_scope(Some(std::time::Instant::now() + std::time::Duration::from_secs(10)));
                let lock = SharedLock::shared(&path).unwrap();
                sent.send(()).unwrap();
                lock
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        fs::remove_dir_all(dir.path().join("pf")).unwrap();
        fs::create_dir(dir.path().join("pf")).unwrap();
        File::create(&path).unwrap();
        assert!(!alone.intact(), "its holder sees the file it locked is gone");
        // The waiter is let in only by the holder of the file it opened: the new file is free,
        // yet it keeps waiting for the old one rather than slipping past.
        assert!(got.recv_timeout(std::time::Duration::from_millis(300)).is_err());
        drop(alone);
        got.recv_timeout(std::time::Duration::from_secs(5)).expect("shared once released");
        let shared = waiter.join().unwrap();
        // What it holds is the file now at the path, which shuts out anyone opening it next.
        assert!(shared.intact());
        assert!(SharedLock::exclusive_within(&path, std::time::Duration::ZERO).unwrap().is_none());
    }

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

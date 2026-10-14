//! What can be learned about another process of the same user without privileges: its parent,
//! process group, start time and environment. Each answer is `None` when the platform cannot
//! say, which never means "no".

/// One observation of a process. `started` tells a process from a later one reusing its PID;
/// its parent and group may change while it is the same process (an orphan is reparented).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    pub started: u64,
}

impl Process {
    /// Still the process observed: same PID and start time.
    pub fn same(&self, other: &Process) -> bool {
        self.pid == other.pid && self.started == other.started
    }

    /// The process `self` observed, if it is still running.
    pub fn current(&self) -> Option<Process> {
        process(self.pid).filter(|now| now.same(self))
    }
}

/// The process `pid` now. A zombie (exited, not yet reaped) is no process.
pub fn process(pid: u32) -> Option<Process> {
    imp::process(pid)
}

/// Whether `pid`'s environment, as it was when it started, sets `key` to exactly `value`.
/// macOS shows no environment for its own platform binaries (`/bin/sh`, say), even to their
/// owner, and an environment that cannot be told from the arguments is unknown too: only
/// `Some(true)` is an answer to rely on.
pub fn environ_has(pid: u32, key: &str, value: &str) -> Option<bool> {
    let wanted = format!("{key}={value}");
    imp::environ(pid).filter(|vars| !vars.is_empty()).map(|vars| vars.iter().any(|v| v.as_slice() == wanted.as_bytes()))
}

/// Every process whose environment sets `key` to `value` (see [`environ_has`]), other than this
/// one, each observed before and after its environment was read, so a process that replaced
/// another under its PID is never credited with what the other carried.
pub fn carrying(key: &str, value: &str) -> Vec<Process> {
    let me = std::process::id();
    imp::pids()
        .into_iter()
        .filter(|pid| *pid != me && *pid > 1)
        .filter_map(|pid| {
            let before = process(pid)?;
            (environ_has(pid, key, value) == Some(true) && before.current().is_some()).then_some(before)
        })
        .collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Process;

    pub fn pids() -> Vec<u32> {
        // SAFETY: with a null buffer proc_listallpids returns the count; then it writes at most
        // `size` bytes.
        unsafe {
            let count = libc::proc_listallpids(std::ptr::null_mut(), 0);
            if count <= 0 {
                return Vec::new();
            }
            let mut pids = vec![0 as libc::pid_t; count as usize * 2 + 64];
            let size = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
            let n = libc::proc_listallpids(pids.as_mut_ptr().cast(), size);
            pids.truncate(n.max(0) as usize);
            pids.into_iter().filter_map(|p| u32::try_from(p).ok()).collect()
        }
    }

    pub fn process(pid: u32) -> Option<Process> {
        let pid = libc::c_int::try_from(pid).ok()?;
        // SAFETY: proc_pidinfo writes at most `size` bytes into the zeroed struct.
        let info = unsafe {
            let mut info: libc::proc_bsdinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
            let n = libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size);
            if n != size {
                return None;
            }
            info
        };
        if info.pbi_status == libc::SZOMB {
            return None;
        }
        Some(Process {
            pid: info.pbi_pid,
            parent: info.pbi_ppid,
            group: info.pbi_pgid,
            started: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        })
    }

    /// `KERN_PROCARGS2`: argc, the executable path, padding, the arguments, then the environment,
    /// each NUL-terminated.
    pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let mut max: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        // SAFETY: sysctl writes at most `size` bytes into each buffer it is given.
        unsafe {
            if libc::sysctl(mib.as_mut_ptr(), 2, (&mut max as *mut libc::c_int).cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
                return None;
            }
        }
        let mut buf = vec![0u8; usize::try_from(max).ok()?];
        let mut size = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: as above.
        unsafe {
            if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
                return None;
            }
        }
        parse_procargs(buf.get(..size)?)
    }

    /// The environment in a `KERN_PROCARGS2` buffer: argc, the executable path padded with
    /// NULs to a pointer-size boundary (measured on macOS 15 arm64: arguments start at
    /// `round_up(len + 1, 8)` after argc, whatever they are), exactly argc arguments, then the
    /// environment up to an empty entry. A layout that does not match is unknown.
    pub(super) fn parse_procargs(buf: &[u8]) -> Option<Vec<Vec<u8>>> {
        let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
        let rest = buf.get(4..)?;
        let path_end = rest.iter().position(|b| *b == 0)?;
        let mut at = (path_end + 1).div_ceil(std::mem::size_of::<usize>()) * std::mem::size_of::<usize>();
        if !rest.get(path_end..at)?.iter().all(|b| *b == 0) {
            return None;
        }
        for _ in 0..argc {
            at += rest.get(at..)?.iter().position(|b| *b == 0)? + 1;
        }
        Some(rest.get(at..)?.split(|b| *b == 0).take_while(|f| !f.is_empty()).map(<[u8]>::to_vec).collect())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Process;

    pub fn pids() -> Vec<u32> {
        std::fs::read_dir("/proc")
            .map(|entries| entries.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect())
            .unwrap_or_default()
    }

    pub fn process(pid: u32) -> Option<Process> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name is parenthesized and may contain anything, so fields follow the last `)`.
        let fields: Vec<&str> = stat.get(stat.rfind(')')? + 2..)?.split(' ').collect();
        if fields.first() == Some(&"Z") {
            return None;
        }
        Some(Process {
            pid,
            parent: fields.get(1)?.parse().ok()?,
            group: fields.get(2)?.parse().ok()?,
            started: fields.get(19)?.parse().ok()?,
        })
    }

    pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
        let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
        Some(bytes.split(|b| *b == 0).filter(|f| !f.is_empty()).map(<[u8]>::to_vec).collect())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::Process;

    pub fn pids() -> Vec<u32> {
        Vec::new()
    }

    pub fn process(_: u32) -> Option<Process> {
        None
    }

    pub fn environ(_: u32) -> Option<Vec<Vec<u8>>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child to describe: this test binary (not a platform binary, whose environment macOS
    /// hides) running [`sleeper`].
    fn child() -> std::process::Child {
        child_with(|_| {})
    }

    fn child_with(adjust: impl FnOnce(&mut std::process::Command)) -> std::process::Child {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "procinfo::tests::sleeper", "--ignored", "--test-threads", "1"])
            .env("STACK_PROCINFO_TEST", "yes")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        adjust(&mut command);
        command.spawn().unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_environment_is_told_from_the_arguments() {
        // "/bin/x" and its NUL padded to 8 bytes.
        let parse = |argc: i32, args: &[u8]| {
            let mut buf = argc.to_ne_bytes().to_vec();
            buf.extend_from_slice(b"/bin/x\0\0");
            buf.extend_from_slice(args);
            buf.extend_from_slice(b"A=1\0B=2\0\0junk\0");
            imp::parse_procargs(&buf)
        };
        let env = Some(vec![b"A=1".to_vec(), b"B=2".to_vec()]);
        assert_eq!(parse(2, b"x\0y\0"), env);
        assert_eq!(parse(2, b"\0y\0"), env, "an empty argv[0]");
        assert_eq!(parse(3, b"\0\0\0"), env, "only empty arguments");
        // A variable passed as an argument is an argument.
        assert_eq!(parse(1, b"STACK_CHECKOUT_TOKEN=t\0"), env);
        // Padding that is not NULs is a layout this does not know.
        let mut odd = 1i32.to_ne_bytes().to_vec();
        odd.extend_from_slice(b"/bin/x\0zx\0A=1\0\0");
        assert_eq!(imp::parse_procargs(&odd), None);
    }

    #[test]
    #[ignore = "a long-running child for the other tests"]
    fn sleeper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    fn a_live_environment_is_read_past_an_empty_argv0_and_never_from_arguments() {
        use std::os::unix::process::CommandExt;
        let mut child = child_with(|c| {
            c.arg0("").arg("STACK_PROCINFO_ARG=yes").env("STACK_PROCINFO_TEST", "argv0");
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while environ_has(child.id(), "STACK_PROCINFO_TEST", "argv0") != Some(true) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(environ_has(child.id(), "STACK_PROCINFO_TEST", "argv0"), Some(true));
        assert_ne!(environ_has(child.id(), "STACK_PROCINFO_ARG", "yes"), Some(true));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_child_is_described_with_its_parent_group_and_environment() {
        let mut child = child();
        let found = process(child.id()).unwrap();
        assert_eq!(found.parent, std::process::id());
        assert_eq!(found.pid, child.id());
        assert!(found.started > 0);
        assert_eq!(process(child.id()), Some(found), "the same observation twice");
        // The environment is read once the child has exec'd; until then it is the parent's.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while environ_has(child.id(), "STACK_PROCINFO_TEST", "yes") != Some(true) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(environ_has(child.id(), "STACK_PROCINFO_TEST", "yes"), Some(true));
        assert_eq!(environ_has(child.id(), "STACK_PROCINFO_TEST", "no"), Some(false));
        assert_eq!(carrying("STACK_PROCINFO_TEST", "yes").iter().map(|p| p.pid).collect::<Vec<_>>(), [child.id()]);
        child.kill().unwrap();
        // Not yet reaped: a zombie is no process.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process(child.id()).is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(process(child.id()).is_none());
        child.wait().unwrap();
        assert!(environ_has(child.id(), "STACK_PROCINFO_TEST", "yes").is_none());
    }
}

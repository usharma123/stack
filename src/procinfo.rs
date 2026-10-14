//! What can be learned about another process of the same user without privileges: its parent,
//! process group, start time and environment. Each answer is `None` when the platform cannot
//! say, which never means "no".

/// One observation of a process. `started` tells a process from a later one reusing its PID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    pub started: u64,
}

/// The process `pid` now. A zombie (exited, not yet reaped) is no process.
pub fn process(pid: u32) -> Option<Process> {
    imp::process(pid)
}

/// Whether `pid`'s environment, as it was when it started, sets `key` to exactly `value`.
/// macOS shows no environment for its own platform binaries (`/bin/sh`, say), even to their
/// owner: an empty one is unknown, not proof of absence.
pub fn environ_has(pid: u32, key: &str, value: &str) -> Option<bool> {
    let wanted = format!("{key}={value}");
    imp::environ(pid).filter(|vars| !vars.is_empty()).map(|vars| vars.iter().any(|v| v.as_slice() == wanted.as_bytes()))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Process;

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
        let buf = buf.get(..size)?;
        let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
        let mut fields = buf[4..].split(|b| *b == 0).filter(|f| !f.is_empty());
        // The executable path, then the arguments.
        for _ in 0..=argc {
            fields.next()?;
        }
        Some(fields.map(<[u8]>::to_vec).collect())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Process;

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
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "procinfo::tests::sleeper", "--ignored", "--test-threads", "1"])
            .env("STACK_PROCINFO_TEST", "yes")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    #[ignore = "a long-running child for the other tests"]
    fn sleeper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
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

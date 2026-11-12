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

    /// When it started, in Unix milliseconds: exact on macOS; on Linux derived from the boot
    /// time, which the kernel reports in whole seconds, so up to a second early.
    pub fn started_unix_ms(&self) -> Option<u64> {
        imp::started_unix_ms(self.started)
    }
}

/// The process `pid` now. A zombie (exited, not yet reaped) is no process.
pub fn process(pid: u32) -> Option<Process> {
    imp::process(pid)
}

/// A process with the names it can be known by, as `--owner-pid ancestor:<name>` matches them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Named {
    pub pid: u32,
    pub parent: u32,
    /// The kernel's command name, which it truncates (15 bytes on Linux, 16 on macOS).
    pub comm: String,
    /// The basename of `argv[0]`, which a program may have rewritten (a process title), when it
    /// could be read and differs from `comm`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv0: Option<String>,
}

#[cfg(target_os = "linux")]
const COMM_LIMIT: usize = 15;
#[cfg(not(target_os = "linux"))]
const COMM_LIMIT: usize = 16;

impl Named {
    /// Whether `name` is this process's command name or the basename of its `argv[0]`. The
    /// kernel truncates `comm`, so a longer name also matches a `comm` it starts with when that
    /// `comm` is as long as the kernel keeps.
    pub fn named(&self, name: &str) -> bool {
        let truncated = self.comm.len() >= COMM_LIMIT && name.starts_with(self.comm.as_str());
        self.comm == name || truncated || self.argv0.as_deref() == Some(name)
    }
}

/// `pid` with its names, or `None` when it is not running or cannot be read.
pub fn named(pid: u32) -> Option<Named> {
    let process = process(pid)?;
    let comm = imp::comm(pid)?;
    let argv0 = imp::argv0(pid)
        .map(|a| {
            let a = String::from_utf8_lossy(&a).into_owned();
            a.rsplit('/').next().unwrap_or(&a).to_string()
        })
        .filter(|a| !a.is_empty() && *a != comm);
    Some(Named { pid, parent: process.parent, comm, argv0 })
}

/// Most ancestors [`ancestors`] walks, so a corrupt or cyclic answer cannot loop forever.
const ANCESTOR_LIMIT: usize = 64;

/// The parents of this process, nearest first, up to the first one whose parent is 0 or that
/// cannot be read.
pub fn ancestors() -> Vec<Named> {
    let mut chain: Vec<Named> = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 0 && chain.len() < ANCESTOR_LIMIT && !chain.iter().any(|p| p.pid == pid) {
        let Some(found) = named(pid) else { break };
        pid = found.parent;
        chain.push(found);
    }
    chain
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

    fn bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
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
        (info.pbi_status != libc::SZOMB).then_some(info)
    }

    pub fn process(pid: u32) -> Option<Process> {
        let info = bsdinfo(pid)?;
        Some(Process {
            pid: info.pbi_pid,
            parent: info.pbi_ppid,
            group: info.pbi_pgid,
            started: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        })
    }

    /// `started` is microseconds since the epoch.
    pub fn started_unix_ms(started: u64) -> Option<u64> {
        Some(started / 1000)
    }

    /// The kernel's command name (`p_comm`, at most 16 bytes).
    pub fn comm(pid: u32) -> Option<String> {
        let info = bsdinfo(pid)?;
        let bytes: Vec<u8> = info.pbi_comm.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn environ(pid: u32) -> Option<Vec<Vec<u8>>> {
        parse_procargs(&procargs(pid)?)
    }

    pub fn argv0(pid: u32) -> Option<Vec<u8>> {
        parse_argv0(&procargs(pid)?)
    }

    /// `KERN_PROCARGS2`: argc, the executable path, padding, the arguments, then the environment,
    /// each NUL-terminated.
    fn procargs(pid: u32) -> Option<Vec<u8>> {
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
        buf.truncate(size);
        Some(buf)
    }

    /// Where the arguments start in a `KERN_PROCARGS2` buffer (see [`parse_procargs`]), with argc.
    fn arguments(buf: &[u8]) -> Option<(usize, &[u8], usize)> {
        let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
        let rest = buf.get(4..)?;
        let path_end = rest.iter().position(|b| *b == 0)?;
        let at = (path_end + 1).div_ceil(std::mem::size_of::<usize>()) * std::mem::size_of::<usize>();
        if !rest.get(path_end..at)?.iter().all(|b| *b == 0) {
            return None;
        }
        Some((argc, rest, at))
    }

    /// The first argument in a `KERN_PROCARGS2` buffer, as the process holds it now.
    pub(super) fn parse_argv0(buf: &[u8]) -> Option<Vec<u8>> {
        let (argc, rest, at) = arguments(buf)?;
        if argc == 0 {
            return None;
        }
        let arg = rest.get(at..)?;
        Some(arg[..arg.iter().position(|b| *b == 0)?].to_vec())
    }

    /// The environment in a `KERN_PROCARGS2` buffer: argc, the executable path padded with
    /// NULs to a pointer-size boundary (measured on macOS 15 arm64: arguments start at
    /// `round_up(len + 1, 8)` after argc, whatever they are), exactly argc arguments, then the
    /// environment up to an empty entry. A layout that does not match is unknown.
    pub(super) fn parse_procargs(buf: &[u8]) -> Option<Vec<Vec<u8>>> {
        let (argc, rest, mut at) = arguments(buf)?;
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

    /// The kernel's command name (at most 15 bytes).
    pub fn comm(pid: u32) -> Option<String> {
        Some(std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim_end_matches('\n').to_string())
    }

    pub fn argv0(pid: u32) -> Option<Vec<u8>> {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        bytes.split(|b| *b == 0).next().map(<[u8]>::to_vec)
    }

    /// `started` is clock ticks after boot; the boot time (`btime`) is whole seconds.
    pub fn started_unix_ms(started: u64) -> Option<u64> {
        let boot: u64 = std::fs::read_to_string("/proc/stat").ok()?.lines().find_map(|l| l.strip_prefix("btime "))?.trim().parse().ok()?;
        // SAFETY: sysconf has no preconditions.
        let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok().filter(|h| *h > 0)?;
        Some(boot * 1000 + started * 1000 / hz)
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

    pub fn started_unix_ms(_: u64) -> Option<u64> {
        None
    }

    pub fn comm(_: u32) -> Option<String> {
        None
    }

    pub fn argv0(_: u32) -> Option<Vec<u8>> {
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
    fn a_child_started_now_has_a_start_time_now() {
        let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let before = now();
        // Its own marker: another test counts the children carrying the default one.
        let mut child = child_with(|c| {
            c.env("STACK_PROCINFO_TEST", "started");
        });
        let started = process(child.id()).and_then(|p| p.started_unix_ms());
        child.kill().unwrap();
        child.wait().unwrap();
        let started = started.expect("start time readable");
        // Linux rounds the boot time down to the second.
        assert!(started + 1500 >= before && started <= now(), "{started} vs {before}");
    }

    #[test]
    fn the_ancestor_chain_starts_at_the_parent_and_names_match_comm_or_argv0() {
        let chain = ancestors();
        let parent = chain.first().expect("the parent is readable");
        assert_eq!(parent.pid, std::os::unix::process::parent_id());
        assert!(!parent.comm.is_empty() && parent.named(&parent.comm));
        let me = named(std::process::id()).unwrap();
        assert_eq!(me.parent, parent.pid);
        let p = Named { pid: 2, parent: 1, comm: "node".into(), argv0: Some("claude".into()) };
        assert!(p.named("claude") && p.named("node") && !p.named("clau"));
        let long = Named { comm: "a".repeat(COMM_LIMIT), argv0: None, ..p };
        assert!(long.named(&format!("{}-server", "a".repeat(COMM_LIMIT))));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn argv0_is_the_first_argument() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/x\0\0claude\0y\0A=1\0\0");
        assert_eq!(imp::parse_argv0(&buf), Some(b"claude".to_vec()));
        let mut none = 0i32.to_ne_bytes().to_vec();
        none.extend_from_slice(b"/bin/x\0\0A=1\0\0");
        assert_eq!(imp::parse_argv0(&none), None);
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

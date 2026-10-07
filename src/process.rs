//! Bounded command capture with process-group cleanup on Unix.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Captured {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// More than `limit` bytes were written; `stdout` holds only the tail.
    pub stdout_truncated: bool,
}

struct Tail {
    bytes: VecDeque<u8>,
    limit: usize,
    truncated: bool,
}

impl Tail {
    fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            limit,
            truncated: false,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.bytes.len() == self.limit {
                self.bytes.pop_front();
                self.truncated = true;
            }
            if self.limit != 0 {
                self.bytes.push_back(byte);
            }
        }
    }

    fn text(self) -> String {
        let bytes: Vec<u8> = self.bytes.into_iter().collect();
        let text = String::from_utf8_lossy(&bytes);
        if self.truncated {
            format!("…[truncated]…{text}")
        } else {
            text.into_owned()
        }
    }
}

struct OwnedChild {
    child: Child,
    terminated: bool,
}

impl OwnedChild {
    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        #[cfg(unix)]
        {
            // SAFETY: the child was launched into its own process group. Negative PID
            // targets that owned group, including descendants that hold its output pipes.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: fd is a live pipe owned by the caller; fcntl only adjusts its flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read what is available without blocking. Returns `false` once the pipe reached EOF.
fn drain(pipe: &mut impl Read, tail: &mut Tail) -> io::Result<bool> {
    let mut buffer = [0; 8192];
    // Yield to deadline checks even when a child writes continuously.
    for _ in 0..16 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(n) => tail.append(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Longest wait between exit checks while a pipe is still open. Descendants that inherited the
/// pipes keep them open after the command exits; this bounds how late that exit is noticed.
const EXIT_CHECK_INTERVAL: Duration = Duration::from_millis(10);

/// Block until a pipe in `fds` has data or EOF, or `wait` elapses. Signals end the wait early;
/// the caller re-checks state either way.
#[cfg(unix)]
fn wait_readable(fds: &[std::os::fd::RawFd], wait: Duration) -> io::Result<()> {
    let mut polled: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    // Round up so a sub-millisecond remainder cannot become a busy loop.
    let millis = wait
        .as_micros()
        .div_ceil(1000)
        .clamp(1, libc::c_int::MAX as u128) as libc::c_int;
    // SAFETY: `polled` is a live, correctly sized array of pollfd for the duration of the call.
    let ready = unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, millis) };
    if ready < 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(())
}

/// Capture at most `limit` bytes per stream, draining pipes while the process runs.
/// Descendants in the command's group are stopped on completion or timeout. Nonblocking
/// reads also keep detached descendants from extending the response deadline.
///
/// Waiting is driven by pipe readiness: a command that exits (closing its pipes) is noticed
/// immediately rather than at the next fixed polling tick.
#[cfg(unix)]
pub fn capture(command: &mut Command, timeout: Duration, limit: usize) -> io::Result<Captured> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = OwnedChild {
        child: command.spawn()?,
        terminated: false,
    };
    let mut stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout pipe"))?;
    let mut stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr pipe"))?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let mut out = Tail::new(limit);
    let mut err = Tail::new(limit);
    let (mut out_open, mut err_open) = (true, true);
    // After both pipes close the command is normally exiting; reap it with a short backoff.
    let mut reap_backoff = Duration::from_micros(250);
    let (exit_code, timed_out) = loop {
        if out_open {
            out_open = drain(&mut stdout, &mut out)?;
        }
        if err_open {
            err_open = drain(&mut stderr, &mut err)?;
        }
        if let Some(status) = child.child.try_wait()? {
            break (status.code(), false);
        }
        let Some(remaining) = timeout
            .checked_sub(start.elapsed())
            .filter(|r| !r.is_zero())
        else {
            break (None, true);
        };
        let open: Vec<_> = [
            (out_open, stdout.as_raw_fd()),
            (err_open, stderr.as_raw_fd()),
        ]
        .into_iter()
        .filter_map(|(open, fd)| open.then_some(fd))
        .collect();
        if open.is_empty() {
            std::thread::sleep(reap_backoff.min(remaining));
            reap_backoff = (reap_backoff * 2).min(EXIT_CHECK_INTERVAL);
        } else {
            wait_readable(&open, EXIT_CHECK_INTERVAL.min(remaining))?;
        }
    };
    child.terminate();
    // These reads never wait for EOF. A detached child cannot keep the server blocked.
    if out_open {
        drain(&mut stdout, &mut out)?;
    }
    if err_open {
        drain(&mut stderr, &mut err)?;
    }
    Ok(Captured {
        exit_code,
        timed_out,
        stdout_truncated: out.truncated,
        stdout: out.text(),
        stderr: err.text(),
    })
}

/// Process group of the command `run_with_deadline` is waiting for, for its signal handler.
#[cfg(unix)]
static FORWARD_TO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn forward_signal(signal: libc::c_int) {
    let group = FORWARD_TO.load(std::sync::atomic::Ordering::SeqCst);
    if group > 0 {
        // SAFETY: kill is async-signal-safe; the group is the command's own.
        unsafe {
            libc::kill(-group, signal);
        }
    }
}

/// Run a command on the caller's stdout and stderr until it exits or `timeout` elapses, then
/// kill its whole process group. The command gets its own group so descendants are stopped
/// too; it therefore cannot read the terminal (stdin is empty). Interrupt, terminate and
/// hangup signals stack receives meanwhile are passed on to the group, unless ignored.
/// Returns the exit code (128 + the signal for a command a signal ended) and whether the
/// deadline passed.
#[cfg(unix)]
pub fn run_with_deadline(command: &mut Command, timeout: Duration) -> io::Result<(Option<i32>, bool)> {
    use std::os::unix::process::CommandExt;
    command.process_group(0).stdin(Stdio::null());
    let mut child = OwnedChild { child: command.spawn()?, terminated: false };
    FORWARD_TO.store(child.child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    let mut previous = Vec::new();
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: plain sigaction calls; the handler only reads an atomic and calls kill.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal, std::ptr::null(), &mut old);
            if old.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            let mut new: libc::sigaction = std::mem::zeroed();
            new.sa_sigaction = forward_signal as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut new.sa_mask);
            libc::sigaction(signal, &new, std::ptr::null_mut());
            previous.push((signal, old));
        }
    }
    let start = Instant::now();
    let outcome = loop {
        match child.child.try_wait() {
            // Like a shell: a command ended by a signal reports 128 + its number.
            Ok(Some(status)) => {
                use std::os::unix::process::ExitStatusExt;
                break Ok((status.code().or_else(|| status.signal().map(|s| 128 + s)), false));
            }
            Ok(None) if start.elapsed() >= timeout => break Ok((None, true)),
            Ok(None) => std::thread::sleep(EXIT_CHECK_INTERVAL),
            Err(e) => break Err(e),
        }
    };
    child.terminate();
    FORWARD_TO.store(0, std::sync::atomic::Ordering::SeqCst);
    for (signal, old) in previous {
        // SAFETY: restores the disposition read above.
        unsafe {
            libc::sigaction(signal, &old, std::ptr::null_mut());
        }
    }
    outcome
}

#[cfg(not(unix))]
pub fn run_with_deadline(_: &mut Command, _: Duration) -> io::Result<(Option<i32>, bool)> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "command deadlines currently require Unix"))
}

#[cfg(not(unix))]
pub fn capture(_: &mut Command, _: Duration, _: usize) -> io::Result<Captured> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded process capture currently requires Unix",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn timeout_terminates_descendants_without_waiting_for_their_pipes() {
        let start = Instant::now();
        let out = capture(
            Command::new("sh").args(["-c", "sleep 20 & echo $!; wait"]),
            Duration::from_millis(200),
            1024,
        )
        .unwrap();
        assert!(out.timed_out);
        assert!(start.elapsed() < Duration::from_secs(2));
        let pid = out.stdout.trim().parse::<u32>().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while crate::state::pid_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !crate::state::pid_alive(pid),
            "descendant {pid} survived timeout"
        );
    }

    #[test]
    fn successful_parent_cannot_leave_output_capture_waiting_on_background_children() {
        let start = Instant::now();
        let out = capture(
            Command::new("sh").args(["-c", "sleep 20 & echo done"]),
            Duration::from_secs(1),
            1024,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout.trim(), "done");
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn large_stdout_and_stderr_are_drained_but_only_their_tails_are_retained() {
        let out = capture(Command::new("sh").args(["-c", "head -c 262144 /dev/zero; printf stdout-end; head -c 262144 /dev/zero >&2; printf stderr-end >&2"]), Duration::from_secs(5), 4096).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.starts_with("…[truncated]…"));
        assert!(out.stdout.ends_with("stdout-end"));
        assert!(out.stderr.ends_with("stderr-end"));
        assert!(out.stdout.len() < 4200 && out.stderr.len() < 4200);
    }

    #[test]
    fn output_and_exit_code_of_a_short_command_are_complete() {
        let out = capture(
            Command::new("sh").args([
                "-c",
                "for i in 1 2 3; do echo $i; done; echo err >&2; exit 3",
            ]),
            Duration::from_secs(5),
            1024,
        )
        .unwrap();
        assert_eq!(
            (out.exit_code, out.stdout.as_str(), out.stderr.as_str()),
            (Some(3), "1\n2\n3\n", "err\n")
        );
    }

    #[test]
    fn timeout_still_applies_after_a_command_closes_its_pipes() {
        let start = Instant::now();
        let out = capture(
            Command::new("sh").args(["-c", "exec >/dev/null 2>&1; sleep 20"]),
            Duration::from_millis(200),
            1024,
        )
        .unwrap();
        assert!(
            out.timed_out && start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }
}

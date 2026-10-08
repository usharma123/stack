//! Bounded command capture with process-group cleanup on Unix, and operation deadlines.

use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, Read};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

// ---- operation deadlines -----------------------------------------------------------------

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Restores the enclosing deadline when dropped.
#[must_use = "the deadline applies only while the scope is alive"]
pub struct DeadlineScope {
    previous: Option<Instant>,
}

impl Drop for DeadlineScope {
    fn drop(&mut self) {
        DEADLINE.with(|d| d.set(self.previous));
    }
}

/// Bound the work this thread does until the scope is dropped. Every subprocess this module
/// runs ([`capture`], [`output`]) and every wait that consults [`remaining`] ends by `at`.
/// `None` lifts the bound. Threads do not inherit it; pass [`deadline`] on explicitly.
pub fn deadline_scope(at: Option<Instant>) -> DeadlineScope {
    DeadlineScope { previous: DEADLINE.with(|d| d.replace(at)) }
}

/// A scope that leaves at least `grace` from now, for recording what timed-out work did.
pub fn grace_scope(grace: Duration) -> DeadlineScope {
    let at = deadline().map(|d| d.max(Instant::now() + grace));
    deadline_scope(at)
}

/// This thread's deadline, if any.
pub fn deadline() -> Option<Instant> {
    DEADLINE.with(Cell::get)
}

/// Time left before this thread's deadline; `None` when there is none.
pub fn remaining() -> Option<Duration> {
    deadline().map(|d| d.saturating_duration_since(Instant::now()))
}

/// This thread's deadline has passed.
pub fn expired() -> bool {
    remaining().is_some_and(|r| r.is_zero())
}

/// `limit`, cut short by this thread's deadline.
pub fn bounded(limit: Duration) -> Duration {
    remaining().map_or(limit, |r| r.min(limit))
}

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

    fn into_bytes(self) -> Vec<u8> {
        self.bytes.into()
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
    /// Set once `terminate` reaped the child.
    status: Option<std::process::ExitStatus>,
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        Self { child, terminated: false, status: None }
    }

    /// Kill the child's process group, then reap the child. Callers observe exit with
    /// `exited`, which does not reap: until this `wait`, the child's PID (and so its group
    /// ID) cannot be reused, and the group signal cannot reach an unrelated process.
    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        #[cfg(unix)]
        {
            // SAFETY: the child was launched into its own process group and is not reaped yet.
            // Negative PID targets that owned group, including descendants that hold its pipes.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        self.status = self.child.wait().ok();
    }

    /// Reap a child that has exited, leaving whatever it started in its group running.
    fn reap(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        self.status = self.child.wait().ok();
    }
}

/// Whether the child has exited, leaving it unreaped (see `OwnedChild::terminate`).
#[cfg(unix)]
fn exited(child: &Child) -> io::Result<bool> {
    // SAFETY: waitid only writes the zeroed siginfo; WNOWAIT leaves the child waitable.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // With WNOHANG and nothing to report, the zeroed siginfo is left unchanged.
    Ok(info.si_signo != 0)
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
    let mut child = OwnedChild::new(spawn_piped(command)?);
    let piped = collect(&mut child, bounded(timeout), limit, |child, _| child.terminate())?;
    Ok(Captured {
        exit_code: if piped.timed_out { None } else { child.status.and_then(|s| s.code()) },
        timed_out: piped.timed_out,
        stdout_truncated: piped.stdout.truncated,
        stdout: piped.stdout.text(),
        stderr: piped.stderr.text(),
    })
}

/// Spawn in a new process group with piped output and empty input.
#[cfg(unix)]
fn spawn_piped(command: &mut Command) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// What a piped child wrote, and whether `timeout` passed first.
#[cfg(unix)]
struct Piped {
    timed_out: bool,
    stdout: Tail,
    stderr: Tail,
}

/// Drain a child's pipes until it exits or `timeout` elapses, then `finish` it (told whether
/// `timeout` passed) and keep what it wrote meanwhile.
#[cfg(unix)]
fn collect(
    child: &mut OwnedChild,
    timeout: Duration,
    limit: usize,
    finish: impl FnOnce(&mut OwnedChild, bool),
) -> io::Result<Piped> {
    use std::os::fd::AsRawFd;
    let start = Instant::now();
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
    let timed_out = loop {
        if out_open {
            out_open = drain(&mut stdout, &mut out)?;
        }
        if err_open {
            err_open = drain(&mut stderr, &mut err)?;
        }
        if exited(&child.child)? {
            break false;
        }
        let Some(remaining) = timeout
            .checked_sub(start.elapsed())
            .filter(|r| !r.is_zero())
        else {
            break true;
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
    finish(child, timed_out);
    // These reads never wait for EOF. A detached child cannot keep the server blocked.
    if out_open {
        drain(&mut stdout, &mut out)?;
    }
    if err_open {
        drain(&mut stderr, &mut err)?;
    }
    Ok(Piped { timed_out, stdout: out, stderr: err })
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

/// For [`output`]: pass the signal on, then let it end stack as it would have without the
/// handler. Stack installs no handlers of its own besides these, so that is the default action.
#[cfg(unix)]
extern "C" fn forward_and_end(signal: libc::c_int) {
    forward_signal(signal);
    // SAFETY: signal and raise are async-signal-safe. The raised signal is blocked while this
    // handler runs and is delivered, with the default action, once it returns.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

/// The signals `run_with_deadline` and `output` pass on to their command.
#[cfg(unix)]
const FORWARDED: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// Held while signals are forwarded to `FORWARD_TO`, which names one group at a time.
#[cfg(unix)]
static FORWARDING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The forwarded signals, as a set to block.
#[cfg(unix)]
fn forwarded_set() -> libc::sigset_t {
    // SAFETY: building a signal set in local memory.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in FORWARDED {
            libc::sigaddset(&mut set, signal);
        }
        set
    }
}

#[cfg(unix)]
fn os_check(rc: libc::c_int) -> io::Result<()> {
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Set this thread's signal mask, returning the previous one.
#[cfg(unix)]
fn set_mask(how: libc::c_int, set: &libc::sigset_t) -> io::Result<libc::sigset_t> {
    // SAFETY: plain pthread_sigmask call with valid pointers.
    unsafe {
        let mut old: libc::sigset_t = std::mem::zeroed();
        match libc::pthread_sigmask(how, set, &mut old) {
            0 => Ok(old),
            errno => Err(io::Error::from_raw_os_error(errno)),
        }
    }
}

/// Install the forwarding handler for each signal that is not ignored, returning the
/// dispositions to restore. On failure, whatever was installed is restored first.
#[cfg(unix)]
fn install_forwarding(handler: extern "C" fn(libc::c_int)) -> io::Result<Vec<(libc::c_int, libc::sigaction)>> {
    let mut previous = Vec::new();
    for signal in FORWARDED {
        // SAFETY: plain sigaction calls; the handler only reads an atomic and calls kill.
        let installed = unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            os_check(libc::sigaction(signal, std::ptr::null(), &mut old)).and_then(|()| {
                if old.sa_sigaction == libc::SIG_IGN {
                    return Ok(None);
                }
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = handler as *const () as libc::sighandler_t;
                libc::sigemptyset(&mut new.sa_mask);
                os_check(libc::sigaction(signal, &new, std::ptr::null_mut())).map(|()| Some(old))
            })
        };
        match installed {
            Ok(Some(old)) => previous.push((signal, old)),
            Ok(None) => {}
            Err(e) => {
                restore_dispositions(&previous);
                return Err(e);
            }
        }
    }
    Ok(previous)
}

#[cfg(unix)]
fn restore_dispositions(previous: &[(libc::c_int, libc::sigaction)]) {
    for (signal, old) in previous {
        // SAFETY: restores a disposition read by install_forwarding.
        unsafe {
            libc::sigaction(*signal, old, std::ptr::null_mut());
        }
    }
}

/// Run a command on the caller's stdout and stderr until it exits or `timeout` elapses, then
/// kill its whole process group. The command gets its own group so descendants are stopped
/// too; it therefore cannot read the terminal (stdin is empty). Interrupt, terminate and
/// hangup signals stack receives meanwhile are passed on to the group, unless ignored.
///
/// Those signals stay blocked from before the spawn until forwarding targets the new group,
/// and again during teardown, so none can end stack while the command would be left running;
/// one arriving in between is delivered (and forwarded) once unblocked. The command starts
/// with the caller's signal mask. Returns the exit code (128 + the signal for a command a
/// signal ended) and whether the deadline passed.
#[cfg(unix)]
pub fn run_with_deadline(command: &mut Command, timeout: Duration) -> io::Result<(Option<i32>, bool)> {
    use std::os::unix::process::CommandExt;
    let _forwarding = FORWARDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = forwarded_set();
    let caller_mask = set_mask(libc::SIG_BLOCK, &held)?;
    let result = run_held(command, timeout, &held, caller_mask);
    let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
    return result;

    fn run_held(
        command: &mut Command,
        timeout: Duration,
        held: &libc::sigset_t,
        caller_mask: libc::sigset_t,
    ) -> io::Result<(Option<i32>, bool)> {
        let previous = install_forwarding(forward_signal)?;
        command.process_group(0).stdin(Stdio::null());
        // SAFETY: sigprocmask is async-signal-safe, as pre_exec requires.
        unsafe {
            command.pre_exec(move || os_check(libc::sigprocmask(libc::SIG_SETMASK, &caller_mask, std::ptr::null_mut())));
        }
        let mut child = match command.spawn() {
            Ok(child) => OwnedChild::new(child),
            Err(e) => {
                restore_dispositions(&previous);
                return Err(e);
            }
        };
        FORWARD_TO.store(child.child.id() as i32, std::sync::atomic::Ordering::SeqCst);
        let waited = set_mask(libc::SIG_SETMASK, &caller_mask).and_then(|_| {
            let start = Instant::now();
            loop {
                if exited(&child.child)? {
                    break Ok(false);
                }
                if start.elapsed() >= timeout {
                    break Ok(true);
                }
                std::thread::sleep(EXIT_CHECK_INTERVAL);
            }
        });
        let _ = set_mask(libc::SIG_BLOCK, held);
        child.terminate();
        FORWARD_TO.store(0, std::sync::atomic::Ordering::SeqCst);
        restore_dispositions(&previous);
        let timed_out = waited?;
        // Like a shell: a command ended by a signal reports 128 + its number.
        let code = child.status.and_then(|status| {
            use std::os::unix::process::ExitStatusExt;
            status.code().or_else(|| status.signal().map(|s| 128 + s))
        });
        Ok((if timed_out { None } else { code }, timed_out))
    }
}

/// Most bytes [`output`] keeps of each stream under a deadline; provider output is far smaller.
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

/// `Command::output` with empty input, bounded by this thread's deadline. Without one it is
/// exactly that. Within one (on Unix) the command gets its own process group, which is killed
/// with every descendant still in it when the deadline passes; the result is then that of a
/// killed process, with the output written until then. One that finishes in time leaves its
/// descendants alone, as `Command::output` does. A command that cannot start before the
/// deadline is not started (`ErrorKind::TimedOut`). Interrupt, terminate and hangup signals stack
/// receives meanwhile are passed on to the group and then end stack, as they would have reached
/// the command in stack's own group; ignored signals stay ignored.
pub fn output(command: &mut Command) -> io::Result<Output> {
    command.stdin(Stdio::null());
    #[cfg(unix)]
    if let Some(remaining) = remaining() {
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the deadline passed before it could start"));
        }
        return output_within(command, remaining);
    }
    command.output()
}

#[cfg(unix)]
fn output_within(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    // One group at a time can receive forwarded signals. Another thread's command, should one
    // run at once, is still bounded and killed with its group; it just is not signalled first.
    let forwarding = FORWARDING.try_lock().ok();
    let held = forwarded_set();
    let caller_mask = set_mask(libc::SIG_BLOCK, &held)?;
    // Signals stay blocked from before the spawn until forwarding names the new group, so none
    // can end stack while the command would be left running; the command starts unblocked.
    let previous = match forwarding {
        Some(_) => install_forwarding(forward_and_end),
        None => Ok(Vec::new()),
    };
    let previous = match previous {
        Ok(previous) => previous,
        Err(e) => {
            let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
            return Err(e);
        }
    };
    // SAFETY: sigprocmask is async-signal-safe, as pre_exec requires.
    unsafe {
        command.pre_exec(move || os_check(libc::sigprocmask(libc::SIG_SETMASK, &caller_mask, std::ptr::null_mut())));
    }
    let spawned = spawn_piped(command);
    let mut child = match spawned {
        Ok(child) => OwnedChild::new(child),
        Err(e) => {
            restore_dispositions(&previous);
            let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
            return Err(e);
        }
    };
    if forwarding.is_some() {
        FORWARD_TO.store(child.child.id() as i32, std::sync::atomic::Ordering::SeqCst);
    }
    let piped = set_mask(libc::SIG_SETMASK, &caller_mask).and_then(|_| {
        collect(&mut child, timeout, OUTPUT_LIMIT, |child, timed_out| {
            // Stop forwarding before the group can be reaped and its ID reused.
            let _ = set_mask(libc::SIG_BLOCK, &held);
            if forwarding.is_some() {
                FORWARD_TO.store(0, std::sync::atomic::Ordering::SeqCst);
            }
            // A command that finished may have started something meant to outlive it, as a
            // supervisor client starts the supervisor; only a command cut short loses its group.
            if timed_out {
                child.terminate();
            } else {
                child.reap();
            }
        })
    });
    // `collect` may have failed before its teardown ran; then nothing it started is kept.
    let _ = set_mask(libc::SIG_BLOCK, &held);
    if forwarding.is_some() {
        FORWARD_TO.store(0, std::sync::atomic::Ordering::SeqCst);
    }
    child.terminate();
    restore_dispositions(&previous);
    let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
    drop(forwarding);
    let piped = piped?;
    Ok(Output {
        // A child whose status could not be read was killed at the latest by `terminate`.
        status: child.status.unwrap_or_else(|| std::process::ExitStatus::from_raw(libc::SIGKILL)),
        stdout: piped.stdout.into_bytes(),
        stderr: piped.stderr.into_bytes(),
    })
}

/// Start a command in a session of its own, with no input or output, and wait up to `wait` for
/// it to exit. A command still running then is left to finish on its own, never signalled: for
/// commands that may start a process outliving them which nothing here may stop. Returns its
/// exit status, or `None` if it was left running.
#[cfg(unix)]
pub fn run_detached(command: &mut Command, wait: Duration) -> io::Result<Option<std::process::ExitStatus>> {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid is async-signal-safe, as pre_exec requires.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if start.elapsed() >= wait {
            // Reaped in the background so it never lingers as a zombie of a long-lived server.
            std::thread::spawn(move || child.wait());
            return Ok(None);
        }
        std::thread::sleep(EXIT_CHECK_INTERVAL);
    }
}

#[cfg(not(unix))]
pub fn run_detached(command: &mut Command, wait: Duration) -> io::Result<Option<std::process::ExitStatus>> {
    let _ = (command, wait);
    Err(io::Error::new(io::ErrorKind::Unsupported, "detached commands currently require Unix"))
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

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    #[test]
    fn deadline_scopes_nest_and_restore_the_enclosing_deadline() {
        assert_eq!(deadline(), None);
        let at = Instant::now() + Duration::from_secs(60);
        let outer = deadline_scope(Some(at));
        assert_eq!(deadline(), Some(at));
        assert!(bounded(Duration::from_secs(600)) <= Duration::from_secs(60));
        {
            let _inner = deadline_scope(None);
            assert_eq!(remaining(), None);
            assert_eq!(bounded(Duration::from_secs(600)), Duration::from_secs(600));
        }
        assert_eq!(deadline(), Some(at));
        drop(outer);
        assert_eq!(deadline(), None);
        assert!(!expired());
    }

    #[test]
    fn a_grace_scope_extends_only_an_existing_deadline() {
        let _expired = deadline_scope(Some(Instant::now()));
        assert!(expired());
        {
            let _grace = grace_scope(Duration::from_secs(10));
            assert!(remaining().unwrap() > Duration::from_secs(9));
        }
        assert!(expired());
        let _none = deadline_scope(None);
        let _grace = grace_scope(Duration::from_secs(10));
        assert_eq!(deadline(), None);
    }

    #[test]
    fn output_under_a_deadline_kills_the_whole_group_and_keeps_what_was_written() {
        use std::os::unix::process::ExitStatusExt;
        let start = Instant::now();
        let _deadline = deadline_scope(Some(start + Duration::from_millis(300)));
        let out = output(Command::new("sh").args(["-c", "sleep 20 & echo $!; echo partial >&2; wait"])).unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert_eq!(out.status.signal(), Some(libc::SIGKILL));
        assert_eq!(String::from_utf8_lossy(&out.stderr), "partial\n");
        let descendant: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        assert!(!alive(descendant), "the deadline left {descendant} running");
    }

    #[test]
    fn output_that_finishes_in_time_leaves_what_it_started_running() {
        let _deadline = deadline_scope(Some(Instant::now() + Duration::from_secs(30)));
        let out = output(Command::new("sh").args(["-c", "sleep 20 >/dev/null 2>&1 & echo $!"])).unwrap();
        assert!(out.status.success());
        let started: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        // As a supervisor client leaves the supervisor it started.
        assert!(alive(started), "a command that finished lost {started}");
        // SAFETY: the test's own detached `sleep`.
        unsafe { libc::kill(started, libc::SIGKILL) };
    }

    #[test]
    fn output_is_not_started_once_the_deadline_passed() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let _deadline = deadline_scope(Some(Instant::now()));
        let err = output(Command::new("touch").arg(&marker)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(!marker.exists());
    }

    #[test]
    fn output_without_a_deadline_is_a_plain_command_output() {
        let out = output(Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"])).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
    }

    #[test]
    fn capture_is_cut_short_by_the_deadline() {
        let start = Instant::now();
        let _deadline = deadline_scope(Some(start + Duration::from_millis(200)));
        let out = capture(Command::new("sleep").arg("20"), Duration::from_secs(60), 1024).unwrap();
        assert!(out.timed_out && start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_detached_command_still_running_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ >{}; sleep 20", pid_file.display());
        let status = run_detached(Command::new("sh").args(["-c", &script]), Duration::from_millis(300)).unwrap();
        assert!(status.is_none());
        let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        assert!(alive(pid));
        // In a session of its own, so no group signal of stack's reaches it.
        // SAFETY: getsid only reads.
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let quick = run_detached(Command::new("sh").args(["-c", "exit 4"]), Duration::from_secs(10)).unwrap();
        assert_eq!(quick.and_then(|s| s.code()), Some(4));
    }

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

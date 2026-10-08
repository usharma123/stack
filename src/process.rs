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

// ---- redaction -----------------------------------------------------------------------------

/// Values that must not appear in captured output, each replaced by `[redacted:<KEY>]`.
///
/// Matching is on bytes, before output is bounded, so a value is caught wherever it falls in
/// the stream. Overlapping occurrences (of one value or of several) are replaced as one run
/// naming every key it covered. Deliberately neither `Debug` nor `Serialize` beyond key names.
#[derive(Clone, Default)]
pub struct Redactor {
    values: Vec<(Vec<u8>, String)>,
    longest: usize,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.values.iter().map(|(_, k)| k.as_str()).collect();
        f.debug_struct("Redactor").field("keys", &keys).finish()
    }
}

impl Redactor {
    /// Add `value` under `key`. Empty values match nothing and are ignored.
    pub fn add(&mut self, key: &str, value: &[u8]) {
        if value.is_empty() || self.values.iter().any(|(v, k)| v == value && k == key) {
            return;
        }
        self.longest = self.longest.max(value.len());
        self.values.push((value.to_vec(), key.to_string()));
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Every value replaced in a complete text (an error message, a whole stream).
    pub fn redact(&self, text: &str) -> String {
        let mut stream = self.stream();
        let mut out = stream.push(text.as_bytes());
        out.extend(stream.finish());
        String::from_utf8_lossy(&out).into_owned()
    }

    pub fn stream(&self) -> RedactStream<'_> {
        RedactStream { redactor: self, pending: Vec::new(), cover_end: 0, run: Vec::new(), offset: 0 }
    }
}

/// A [`Redactor`] applied to one stream that arrives in pieces. Holds back at most the longest
/// value minus one byte (plus any run of matched bytes still open), so a value split across
/// reads is still caught.
pub struct RedactStream<'a> {
    redactor: &'a Redactor,
    /// Bytes not yet decided, starting at absolute position `offset`.
    pending: Vec<u8>,
    /// Absolute end of the bytes covered by matches found so far.
    cover_end: usize,
    /// Keys of the covered run being skipped, in the order they matched.
    run: Vec<usize>,
    offset: usize,
}

impl RedactStream<'_> {
    /// Feed `bytes`; returns what can be released now.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        self.advance(false)
    }

    /// End of stream: release everything still held.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = self.advance(true);
        self.close_run(&mut out);
        out
    }

    fn close_run(&mut self, out: &mut Vec<u8>) {
        for key in self.run.drain(..) {
            out.extend_from_slice(format!("[redacted:{}]", self.redactor.values[key].1).as_bytes());
        }
    }

    fn advance(&mut self, end: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let longest = self.redactor.longest;
        let mut i = 0;
        while i < self.pending.len() {
            // Every match that starts here must be decidable: either the whole value fits in
            // what arrived, or the stream ended.
            if !end && self.pending.len() - i < longest {
                break;
            }
            let rest = &self.pending[i..];
            let position = self.offset + i;
            for (index, (value, _)) in self.redactor.values.iter().enumerate() {
                if rest.starts_with(value) {
                    self.cover_end = self.cover_end.max(position + value.len());
                    if !self.run.contains(&index) {
                        self.run.push(index);
                    }
                }
            }
            if position >= self.cover_end {
                self.close_run(&mut out);
                out.push(self.pending[i]);
            }
            i += 1;
        }
        self.pending.drain(..i);
        self.offset += i;
        out
    }
}

/// Where a captured stream goes: through the redactor if there is one, then into its tail.
struct Sink<'a> {
    stream: Option<RedactStream<'a>>,
    tail: Tail,
}

impl<'a> Sink<'a> {
    fn new(limit: usize, redactor: Option<&'a Redactor>) -> Self {
        Self { stream: redactor.filter(|r| !r.is_empty()).map(Redactor::stream), tail: Tail::new(limit) }
    }

    fn append(&mut self, bytes: &[u8]) {
        match self.stream.as_mut() {
            Some(stream) => {
                let released = stream.push(bytes);
                self.tail.append(&released);
            }
            None => self.tail.append(bytes),
        }
    }

    fn finish(mut self) -> Tail {
        if let Some(mut stream) = self.stream.take() {
            let released = stream.finish();
            self.tail.append(&released);
        }
        self.tail
    }
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
fn drain(pipe: &mut impl Read, tail: &mut Sink<'_>) -> io::Result<bool> {
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
    capture_redacted(command, timeout, limit, None)
}

/// [`capture`], with every value of `redactor` replaced in both streams before they are
/// bounded: `limit` applies to the redacted text, and a value split across reads or across
/// the start of the retained tail is still replaced. `None` (or an empty redactor) captures
/// exactly as [`capture`] does.
#[cfg(unix)]
pub fn capture_redacted(command: &mut Command, timeout: Duration, limit: usize, redactor: Option<&Redactor>) -> io::Result<Captured> {
    let mut child = OwnedChild::new(spawn_piped(command)?);
    let piped = collect(&mut child, bounded(timeout), limit, redactor, |child, _| child.terminate())?;
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
    redactor: Option<&Redactor>,
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
    let mut out = Sink::new(limit, redactor);
    let mut err = Sink::new(limit, redactor);
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
    Ok(Piped { timed_out, stdout: out.finish(), stderr: err.finish() })
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
        collect(&mut child, timeout, OUTPUT_LIMIT, None, |child, timed_out| {
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
pub fn capture(command: &mut Command, timeout: Duration, limit: usize) -> io::Result<Captured> {
    capture_redacted(command, timeout, limit, None)
}

#[cfg(not(unix))]
pub fn capture_redacted(_: &mut Command, _: Duration, _: usize, _: Option<&Redactor>) -> io::Result<Captured> {
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

    /// Whether `pid` ends within a second. A killed process finishes exiting after the signal is
    /// sent, and one that was orphaned then stays a zombie until its new parent reaps it. Neither
    /// runs again: on Linux `pid_alive` counts a zombie as gone; elsewhere this waits for the reap.
    fn ends_soon(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(1);
        while crate::state::pid_alive(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
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
        let descendant: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        assert!(ends_soon(descendant), "the deadline left {descendant} running");
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
        // `exec`, so the recorded PID is the whole command: dash would otherwise fork `sleep`
        // and leave it running once the shell is killed.
        let script = format!("echo $$ >{}; exec sleep 20", pid_file.display());
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
        assert!(ends_soon(pid), "descendant {pid} survived timeout");
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

    fn redactor(pairs: &[(&str, &str)]) -> Redactor {
        let mut r = Redactor::default();
        for (key, value) in pairs {
            r.add(key, value.as_bytes());
        }
        r
    }

    /// `text` fed to a stream in pieces of `size` bytes.
    fn streamed(r: &Redactor, text: &[u8], size: usize) -> Vec<u8> {
        let mut stream = r.stream();
        let mut out = Vec::new();
        for chunk in text.chunks(size.max(1)) {
            out.extend(stream.push(chunk));
        }
        out.extend(stream.finish());
        out
    }

    #[test]
    fn values_are_replaced_wherever_the_stream_is_split() {
        let r = redactor(&[("API_TOKEN", "s3cr3t-value-0042"), ("OTHER", "another-secret")]);
        let text = "a s3cr3t-value-0042 b s3cr3t-value-0042\nanother-secret!s3cr3t-value-004";
        let expected = "a [redacted:API_TOKEN] b [redacted:API_TOKEN]\n[redacted:OTHER]!s3cr3t-value-004";
        for size in 1..=text.len() {
            assert_eq!(String::from_utf8(streamed(&r, text.as_bytes(), size)).unwrap(), expected, "chunks of {size}");
        }
        assert_eq!(r.redact(text), expected);
    }

    #[test]
    fn overlapping_values_are_replaced_as_one_run_naming_each_key() {
        let r = redactor(&[("A", "abcdefgh"), ("B", "efghijkl")]);
        for size in 1..=12 {
            let out = String::from_utf8(streamed(&r, b"<abcdefghijkl>", size)).unwrap();
            assert_eq!(out, "<[redacted:A][redacted:B]>", "chunks of {size}");
        }
        // A value overlapping itself, and one value inside another.
        let r = redactor(&[("R", "aaaaaaaa"), ("IN", "xyz12345"), ("OUT", "--xyz12345--")]);
        assert_eq!(r.redact("aaaaaaaaaaa."), "[redacted:R].");
        assert_eq!(r.redact("(--xyz12345--)"), "([redacted:OUT][redacted:IN])");
        assert!(!r.redact("aaaaaaaaaaaaaaaaaaaaaaaa").contains("aaaaaaaa"));
    }

    #[test]
    fn multibyte_values_and_context_are_matched_on_bytes() {
        let r = redactor(&[("K", "pässwörd✓")]);
        let text = "ü→pässwörd✓←ü".as_bytes();
        for size in 1..=text.len() {
            assert_eq!(String::from_utf8(streamed(&r, text, size)).unwrap(), "ü→[redacted:K]←ü", "chunks of {size}");
        }
    }

    #[test]
    fn an_empty_redactor_changes_nothing() {
        let r = Redactor::default();
        assert!(r.is_empty());
        assert_eq!(r.redact("anything at all"), "anything at all");
        assert_eq!(format!("{:?}", redactor(&[("KEY", "value-never-shown")])), "Redactor { keys: [\"KEY\"] }");
    }

    #[test]
    fn capture_redacts_both_streams_before_bounding_them() {
        let r = redactor(&[("TOKEN", "sentinel-0123456789")]);
        // The value straddles the start of the retained tail and arrives in separate writes.
        let script = "printf 'sentinel-01'; sleep 0.05; printf '23456789'; head -c 5000 /dev/zero | tr '\\0' x; \
                      printf 'sentinel-0123456789'; printf 'sentinel-0123456789' >&2";
        let out = capture_redacted(Command::new("sh").args(["-c", script]), Duration::from_secs(5), 64, Some(&r)).unwrap();
        assert!(out.stdout_truncated);
        assert!(out.stdout.ends_with("[redacted:TOKEN]"), "{}", out.stdout);
        assert_eq!(out.stderr, "[redacted:TOKEN]");
        // Exactly at the tail boundary: the redacted marker, not a fragment of the value.
        let script = "head -c 100 /dev/zero | tr '\\0' x; printf 'sentinel-0123456789'";
        let out = capture_redacted(Command::new("sh").args(["-c", script]), Duration::from_secs(5), 19, Some(&r)).unwrap();
        assert!(!out.stdout.contains("sentinel"), "{}", out.stdout);
        assert!(out.stdout.ends_with("ted:TOKEN]"), "{}", out.stdout);
    }

    #[test]
    fn a_timed_out_capture_still_redacts_what_was_held_back() {
        let r = redactor(&[("TOKEN", "sentinel-0123456789")]);
        let out = capture_redacted(Command::new("sh").args(["-c", "printf 'x sentinel-0123456789'; sleep 20"]), Duration::from_millis(300), 1024, Some(&r)).unwrap();
        assert!(out.timed_out);
        assert_eq!(out.stdout, "x [redacted:TOKEN]");
        let out = capture_redacted(Command::new("sh").args(["-c", "printf 'x sentinel-01234'; sleep 20"]), Duration::from_millis(300), 1024, Some(&r)).unwrap();
        assert_eq!(out.stdout, "x sentinel-01234", "a partial value is released at the end");
    }
}

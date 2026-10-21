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

thread_local! {
    static CANCEL: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// Lifts this thread's cancellation flag when dropped.
#[must_use = "cancellation applies only while the scope is alive"]
pub struct CancelScope;

impl Drop for CancelScope {
    fn drop(&mut self) {
        CANCEL.with(|c| c.borrow_mut().take());
    }
}

/// Let another thread end the command this thread runs through [`output`]: once `flag` is set,
/// the command is ended as if its deadline had passed (its process group killed), and so is
/// every later one, until the scope is dropped.
pub fn cancel_scope(flag: std::sync::Arc<std::sync::atomic::AtomicBool>) -> CancelScope {
    CANCEL.with(|c| *c.borrow_mut() = Some(flag));
    CancelScope
}

fn cancelled() -> bool {
    CANCEL.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst)))
}

fn cancellable() -> bool {
    CANCEL.with(|c| c.borrow().is_some())
}

pub struct Captured {
    pub exit_code: Option<i32>,
    /// The signal that ended the command, when one did and it was not cut short by the deadline.
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    /// More than `limit` bytes were written; `stdout` holds only the tail.
    pub stdout_truncated: bool,
}

// ---- redaction -----------------------------------------------------------------------------

/// Prefixed to a captured stream that kept only its last `limit` bytes.
pub const TRUNCATED: &str = "…[truncated]…";

/// The marker for a value whose key cannot be shown in it.
pub const REDACTED: &str = "[redacted]";

/// What lossy decoding writes in place of bytes that are not UTF-8.
const REPLACEMENT: &str = "\u{FFFD}";

/// Longest key shown in a marker.
const MAX_LABEL: usize = 128;

/// Values that must not appear in captured output, each replaced by a marker:
/// `[redacted:<KEY>]`, or [`REDACTED`] when the key cannot be shown.
///
/// Matching is on bytes, before output is bounded, so a value is caught wherever it falls in
/// the stream. Overlapping occurrences (of one value or of several) are replaced as one run
/// naming every key it covered. Deliberately neither `Debug` nor `Serialize` beyond key names.
///
/// Replacing one value must never write another, or itself. Text stack inserts (markers, the
/// [`TRUNCATED`] notice, the replacement character of lossy decoding) is the only text in a
/// result the command did not write, and every value occurrence the command wrote is replaced,
/// so a value can only reappear across inserted text. Hence:
///
/// - a key is shown only if it is a variable name (`[A-Z_][A-Z0-9_]*`, at most 128 bytes) and
///   no value could be read across its marker; otherwise its marker is [`REDACTED`];
/// - a value that could still be read across a marker in use, the notice or the replacement
///   character is a [conflict](Self::conflicts). A redactor with conflicts fails closed: its
///   streams release nothing and [`capture_redacted`] refuses to run the command. Callers
///   refuse such grants before that.
#[derive(Clone, Default)]
pub struct Redactor {
    entries: Vec<Redacted>,
    longest: usize,
    /// Indexes of entries whose value could be read across inserted text.
    conflicts: Vec<usize>,
}

#[derive(Clone)]
struct Redacted {
    value: Vec<u8>,
    key: String,
    marker: String,
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.entries.iter().map(|e| e.key.as_str()).collect();
        f.debug_struct("Redactor").field("keys", &keys).finish()
    }
}

impl Redactor {
    /// Add `value` under `key`. Empty values match nothing and are ignored. Check
    /// [`conflicts`](Self::conflicts) afterwards: a value that cannot be replaced safely makes
    /// the redactor withhold everything.
    pub fn add(&mut self, key: &str, value: &[u8]) {
        if value.is_empty() || self.entries.iter().any(|e| e.value == value && e.key == key) {
            return;
        }
        self.longest = self.longest.max(value.len());
        self.entries.push(Redacted { value: value.to_vec(), key: key.to_string(), marker: String::new() });
        self.settle();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Keys whose value could be read across text stack inserts, in the order added. Empty
    /// for a redactor that is safe to use.
    pub fn conflicts(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = Vec::new();
        for &i in &self.conflicts {
            if !keys.contains(&self.entries[i].key.as_str()) {
                keys.push(&self.entries[i].key);
            }
        }
        keys
    }

    /// Choose every marker and find the conflicts, for the values added so far.
    fn settle(&mut self) {
        let values: Vec<&[u8]> = self.entries.iter().map(|e| e.value.as_slice()).collect();
        let markers: Vec<String> = self
            .entries
            .iter()
            .map(|e| {
                let labeled = format!("[redacted:{}]", e.key);
                if showable(&e.key) && !values.iter().any(|v| overlaps(v, labeled.as_bytes())) {
                    labeled
                } else {
                    REDACTED.to_string()
                }
            })
            .collect();
        let mut used: Vec<&str> = Vec::new();
        for marker in &markers {
            if !used.contains(&marker.as_str()) {
                used.push(marker);
            }
        }
        self.conflicts = values
            .iter()
            .enumerate()
            .filter(|(_, v)| {
                used.iter().any(|m| overlaps(v, m.as_bytes()))
                    || overlaps(v, REPLACEMENT.as_bytes())
                    || starts_inside(v, TRUNCATED.as_bytes())
            })
            .map(|(i, _)| i)
            .collect();
        for (entry, marker) in self.entries.iter_mut().zip(markers) {
            entry.marker = marker;
        }
    }

    /// Every value replaced in a complete text (an error message, a whole stream). Empty when
    /// the redactor has [conflicts](Self::conflicts).
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

/// A key shown in its marker: a variable name of reasonable length. Anything else (a
/// dependency name fnox reported, say) is not echoed into output.
fn showable(key: &str) -> bool {
    let mut bytes = key.bytes();
    key.len() <= MAX_LABEL
        && bytes.next().is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// The shorter of `a` and `b` is a prefix of the other.
fn agree(a: &[u8], b: &[u8]) -> bool {
    let n = a.len().min(b.len());
    a[..n] == b[..n]
}

/// `value` could be read starting inside `inserted`: within it, or running on past its end.
fn starts_inside(value: &[u8], inserted: &[u8]) -> bool {
    (0..inserted.len()).any(|s| agree(&inserted[s..], value))
}

/// `value` could be read across some byte of `inserted`, whatever text surrounds it: starting
/// inside it, or starting before it and reaching into (or past) it.
fn overlaps(value: &[u8], inserted: &[u8]) -> bool {
    starts_inside(value, inserted) || (1..value.len()).any(|j| value[j] == inserted[0] && agree(&value[j..], inserted))
}

/// A [`Redactor`] applied to one stream that arrives in pieces. Holds back at most the longest
/// value minus one byte (plus any run of matched bytes still open), so a value split across
/// reads is still caught. A redactor with conflicts releases nothing.
pub struct RedactStream<'a> {
    redactor: &'a Redactor,
    /// Bytes not yet decided, starting at absolute position `offset`.
    pending: Vec<u8>,
    /// Absolute end of the bytes covered by matches found so far.
    cover_end: usize,
    /// Entries of the covered run being skipped, in the order they matched.
    run: Vec<usize>,
    offset: usize,
}

impl RedactStream<'_> {
    /// Feed `bytes`; returns what can be released now.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        if !self.redactor.conflicts.is_empty() {
            return Vec::new();
        }
        self.pending.extend_from_slice(bytes);
        self.advance(false)
    }

    /// End of stream: release everything still held.
    pub fn finish(&mut self) -> Vec<u8> {
        if !self.redactor.conflicts.is_empty() {
            return Vec::new();
        }
        let mut out = self.advance(true);
        self.close_run(&mut out);
        out
    }

    /// One marker per distinct marker text of the run, in the order they matched.
    fn close_run(&mut self, out: &mut Vec<u8>) {
        let redactor = self.redactor;
        let mut written: Vec<&str> = Vec::new();
        for index in self.run.drain(..) {
            let marker = redactor.entries[index].marker.as_str();
            if !written.contains(&marker) {
                written.push(marker);
                out.extend_from_slice(marker.as_bytes());
            }
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
            for (index, entry) in self.redactor.entries.iter().enumerate() {
                if rest.starts_with(&entry.value) {
                    self.cover_end = self.cover_end.max(position + entry.value.len());
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
            format!("{TRUNCATED}{text}")
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

    /// Kill only the child, which shares stack's own process group, then reap it.
    fn terminate_leader(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
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
///
/// For stack's own provider commands. Interrupt, terminate and hangup signals stack receives
/// meanwhile are passed on to the command's group and then end stack, as for [`output`].
#[cfg(unix)]
pub fn capture(command: &mut Command, timeout: Duration, limit: usize) -> io::Result<Captured> {
    capture_with(command, timeout, limit, None, false)
}

/// [`capture`] for a user's command (`exec --json`, `run --json`, MCP), with every value of
/// `redactor` replaced in both streams before they are bounded: `limit` applies to the
/// redacted text, and a value split across reads or across the start of the retained tail is
/// still replaced. `None` (or an empty redactor) redacts nothing. A redactor with
/// [conflicts](Redactor::conflicts) is refused before anything runs: its output could not be
/// shown without a value.
///
/// Interrupt, terminate and hangup signals stack receives meanwhile are passed on to the
/// command's group, and stack waits for it (see [`CANCEL_GRACE`]) instead of ending at once and
/// leaving it running. Whatever is left in the group is killed once the command is done.
#[cfg(unix)]
pub fn capture_redacted(command: &mut Command, timeout: Duration, limit: usize, redactor: Option<&Redactor>) -> io::Result<Captured> {
    if redactor.is_some_and(|r| !r.conflicts.is_empty()) {
        return Err(io::Error::other("a granted value could be reproduced by its redaction marker, so output cannot be captured safely"));
    }
    capture_with(command, timeout, limit, redactor, true)
}

/// [`capture`] and [`capture_redacted`]: the command is named for forwarding (`passing` for a
/// user's command, which a signal reaches without ending stack), and its group is killed when it
/// is done, timed out, or stopped after a passed-on signal.
#[cfg(unix)]
fn capture_with(command: &mut Command, timeout: Duration, limit: usize, redactor: Option<&Redactor>, passing: bool) -> io::Result<Captured> {
    use std::os::unix::process::CommandExt;
    forwarder()?;
    let held = forwarded_set();
    let caller_mask = set_mask(libc::SIG_BLOCK, &held)?;
    // As in `output_within`: blocked in this thread from before the spawn to after the reaping,
    // so no signal can end stack while the command it should reach is not yet named.
    // SAFETY: sigprocmask is async-signal-safe, as pre_exec requires.
    unsafe {
        command.pre_exec(move || os_check(libc::sigprocmask(libc::SIG_SETMASK, &caller_mask, std::ptr::null_mut())));
    }
    let (mut child, passed) = match spawn_named(command, passing, false, spawn_piped) {
        Ok((child, passed)) => (OwnedChild::new(child), passing.then_some(passed)),
        Err(e) => {
            let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
            return Err(e);
        }
    };
    let mut retired = false;
    let piped = set_mask(libc::SIG_SETMASK, &caller_mask).and_then(|_| {
        collect(&mut child, bounded(timeout), limit, redactor, passed, |child, _| {
            let _ = set_mask(libc::SIG_BLOCK, &held);
            retire(child, true);
            retired = true;
        })
    });
    let _ = set_mask(libc::SIG_BLOCK, &held);
    if !retired {
        retire(&mut child, true);
    }
    let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
    let piped = piped?;
    use std::os::unix::process::ExitStatusExt;
    Ok(Captured {
        exit_code: if piped.timed_out { None } else { child.status.and_then(|s| s.code()) },
        signal: if piped.timed_out { None } else { child.status.and_then(|s| s.signal()) },
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
/// `timeout` passed) and keep what it wrote meanwhile. With `passed`, the child's group is
/// one signals are passed on to: once [`Passed::expired`], the wait ends too, and the child is
/// finished as a command cut short, though not reported as timed out.
#[cfg(unix)]
fn collect(
    child: &mut OwnedChild,
    timeout: Duration,
    limit: usize,
    redactor: Option<&Redactor>,
    mut passed: Option<Passed>,
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
    let mut stopped = false;
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
        if cancelled() {
            break true;
        }
        if passed.as_mut().is_some_and(Passed::expired) {
            stopped = true;
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
    finish(child, timed_out || stopped);
    // These reads never wait for EOF. A detached child cannot keep the server blocked.
    if out_open {
        drain(&mut stdout, &mut out)?;
    }
    if err_open {
        drain(&mut stderr, &mut err)?;
    }
    Ok(Piped { timed_out, stdout: out.finish(), stderr: err.finish() })
}

/// Commands running with signals passed on to them: those of [`output`] (several threads may
/// each run one) and of `run_with_deadline`. The handler, installed once and kept, only
/// writes the signal to a pipe with its arrival number; [`forwarder`] does the rest in an
/// ordinary thread, holding this lock. Commands are spawned and named here, and leave it and
/// are reaped, under the same lock, so a group is signalled only between its command's spawn
/// and its reaping: never a group ID that is not yet named, or one already free for reuse.
#[cfg(unix)]
struct Registry {
    commands: Vec<Named>,
}

/// One command's process group; `passing` for `run_with_deadline`'s, which a signal reaches
/// without ending stack. A signal is the command's when it arrived after `since` (the arrival
/// number as its spawn began) and, once it was unnamed, by `until` (the number then). An
/// unnamed command is never signalled, its group may be reused, but still decides what a
/// signal that arrived for it does; it is forgotten once every such signal was handled.
///
/// A `leader` command shares stack's own process group, as a terminal's foreground job does:
/// only its leader is signalled, and only with SIGTERM, which a terminal never sends (an
/// interrupt or hangup from the terminal already reached it). `forwarded` counts the signals
/// passed on to it.
#[cfg(unix)]
struct Named {
    group: i32,
    passing: bool,
    since: u64,
    until: Option<u64>,
    leader: bool,
    forwarded: u32,
}

#[cfg(unix)]
impl Named {
    fn received(&self, arrival: u64, signal: libc::c_int) -> bool {
        self.since < arrival && self.until.map_or(true, |until| arrival <= until) && (!self.leader || signal == libc::SIGTERM)
    }
}

/// How long a user's command may take to exit after stack passed it an interrupt, terminate or
/// hangup signal, before its whole process group is killed. A second such signal kills it at once.
pub const CANCEL_GRACE: Duration = Duration::from_secs(10);

/// Signals passed on to `passing` commands so far, so a waiting command looks itself up in the
/// registry only when one came.
#[cfg(unix)]
static PASSED_ON: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The first signal passed on to a `passing` command, 0 before one came.
#[cfg(unix)]
static TERMINATION: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// The first interrupt, terminate or hangup signal stack passed on to a user's command instead
/// of ending: once that command is done, stack should end too (exit 128 + the signal), as the
/// signal asked, whatever the command's own exit status, and start nothing more. `None` while
/// none came. Also set for a signal that came as the command was exiting, too late to reach it.
/// Every signal that arrived while a command ran is handled before its run returns (see
/// [`settle`]), so read once it returns, this already counts them.
#[cfg(unix)]
pub fn passed_on() -> Option<i32> {
    Some(TERMINATION.load(std::sync::atomic::Ordering::SeqCst)).filter(|s| *s != 0)
}

#[cfg(not(unix))]
pub fn passed_on() -> Option<i32> {
    None
}

/// Signals passed on to the command named with `group`, and whether to stop waiting for it.
/// Made by [`spawn_named`] as it names the command, under the registry's lock: every signal
/// passed on to the command after that, however soon, moves [`PASSED_ON`] past `seen`, so it is
/// noticed even when it was handled before the run began to watch.
#[cfg(unix)]
struct Passed {
    group: i32,
    seen: u64,
    forwarded: u32,
    first: Option<Instant>,
}

#[cfg(unix)]
impl Passed {
    /// Whether a signal was passed on to the command and it must now be killed: a second one
    /// came, or [`CANCEL_GRACE`] passed since the first.
    fn expired(&mut self) -> bool {
        let now = PASSED_ON.load(std::sync::atomic::Ordering::SeqCst);
        if now != self.seen {
            self.seen = now;
            let registry = registry();
            if let Some(named) = registry.commands.iter().find(|c| c.group == self.group && c.until.is_none()) {
                self.forwarded = named.forwarded;
            }
            if self.forwarded > 0 && self.first.is_none() {
                self.first = Some(Instant::now());
            }
        }
        self.forwarded >= 2 || self.first.is_some_and(|first| first.elapsed() >= CANCEL_GRACE)
    }

    /// Whether any signal was passed on to the command.
    fn signalled(&mut self) -> bool {
        self.expired();
        self.first.is_some()
    }
}

#[cfg(unix)]
static REGISTRY: std::sync::Mutex<Registry> = std::sync::Mutex::new(Registry { commands: Vec::new() });

#[cfg(unix)]
fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Signals reported so far: each report carries its number, taken as the handler's first act.
#[cfg(unix)]
static ARRIVALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Every report numbered up to this one was handled (passed on without ending stack: one that
/// ends stack leaves nothing to wait for).
#[cfg(unix)]
static HANDLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Notified each time [`HANDLED`] moves, for [`settle`].
#[cfg(unix)]
static SETTLED: (std::sync::Mutex<()>, std::sync::Condvar) = (std::sync::Mutex::new(()), std::sync::Condvar::new());

/// Held by a test to keep reports read meanwhile from being handled, as a busy machine may
/// delay the thread that handles them.
#[cfg(all(unix, test))]
static READER_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Where a command is in its admission (see [`spawn_named`]), for a test that delivers a
/// signal there: `Deciding` before it takes the registry's lock, `Admitted` holding it, its
/// boundary checked, before the spawn, and `Registered` once it is named and the lock released,
/// before its run watches it.
#[cfg(all(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Deciding,
    Admitted,
    Registered,
}

/// Run by a test at each [`Stage`], in the thread running the command.
#[cfg(all(unix, test))]
#[allow(clippy::type_complexity)]
static STAGE_HOOK: std::sync::Mutex<Option<Box<dyn FnMut(Stage) + Send>>> = std::sync::Mutex::new(None);

#[cfg(all(unix, test))]
fn stage(reached: Stage) {
    if let Some(hook) = STAGE_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut() {
        hook(reached);
    }
}

/// Times [`settle`] had to wait, for a test that holds the reader back.
#[cfg(all(unix, test))]
static SETTLE_WAITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Wait until every report numbered up to `arrival` was handled: passed on, with what stack
/// owes it recorded (see [`passed_on`]), or ending stack. Reports are handled in
/// [`forwarder`]'s thread, which may run only after the command a signal was for is done, or
/// after a signal that came between commands; what ran it must not judge it, nor start the
/// next, before then. A report counted is already being written (see [`report_signal`]), so
/// this ends. Never called holding the registry's lock, which handling takes.
#[cfg(unix)]
fn settle(arrival: u64) {
    let (lock, moved) = &SETTLED;
    let mut guard = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    #[cfg(test)]
    if HANDLED.load(std::sync::atomic::Ordering::SeqCst) < arrival {
        SETTLE_WAITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    while HANDLED.load(std::sync::atomic::Ordering::SeqCst) < arrival {
        guard = moved.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// The write end of the pipe the handler reports signals through, once [`forwarder`] runs.
#[cfg(unix)]
static WAKE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Bytes of one report: the signal, then its arrival number. Smaller than `PIPE_BUF`, so each
/// is written whole; the reader collects whole reports.
#[cfg(unix)]
const REPORT_SIZE: usize = 16;

/// Tell [`forwarder`] which signal came, and when. Only atomics, `write`, `signal` and
/// `raise`, which are async-signal-safe; errno is kept for the interrupted code. A report that
/// cannot be written (thousands queued) takes the default action at once, ending stack, so no
/// report is ever lost.
#[cfg(unix)]
extern "C" fn report_signal(signal: libc::c_int) {
    let arrival = ARRIVALS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let fd = WAKE.load(std::sync::atomic::Ordering::SeqCst);
    let mut record = [0u8; REPORT_SIZE];
    record[0] = signal as u8;
    record[8..].copy_from_slice(&arrival.to_ne_bytes());
    // SAFETY: errno is thread-local and saved around write. The raised signal is blocked while
    // this handler runs and is delivered, with the default action, once it returns.
    unsafe {
        let errno = *errno_location();
        let written = fd >= 0 && libc::write(fd, record.as_ptr().cast(), REPORT_SIZE) == REPORT_SIZE as isize;
        *errno_location() = errno;
        if !written {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }
}



#[cfg(target_os = "macos")]
unsafe fn errno_location() -> *mut libc::c_int {
    libc::__error()
}

#[cfg(all(unix, not(target_os = "macos")))]
unsafe fn errno_location() -> *mut libc::c_int {
    libc::__errno_location()
}

/// Start, once, the thread that handles reported signals, and install the handler for each
/// forwarded signal stack does not ignore (one ignored then stays ignored). A signal is passed
/// on to every command named before it arrived. When all of them are `run_with_deadline`'s,
/// stack goes on; otherwise, or when there were none, it takes the default action and ends
/// stack, as it would have without the handler (stack installs no handlers of its own
/// besides this one).
#[cfg(unix)]
fn forwarder() -> io::Result<()> {
    static STARTED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    STARTED
        .get_or_init(|| {
            let mut fds = [0; 2];
            // SAFETY: pipe writes two descriptors into the array.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            for fd in fds {
                // SAFETY: plain fcntl on descriptors this function owns.
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            }
            // A full pipe fails the handler's write rather than blocking it (see
            // `report_signal`).
            // SAFETY: as above.
            unsafe { libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK) };
            let read = fds[0];
            std::thread::Builder::new()
                .name("stack-signals".into())
                .spawn(move || {
                    let mut record = [0u8; REPORT_SIZE];
                    let mut filled = 0;
                    // Reports handled beyond the contiguous boundary: they may be read out of
                    // order, as a handler can be paused between taking its number and writing.
                    let mut ahead = std::collections::BTreeSet::new();
                    loop {
                        // SAFETY: reads into the unfilled rest of a local buffer.
                        let n = unsafe { libc::read(read, record[filled..].as_mut_ptr().cast(), REPORT_SIZE - filled) };
                        if n <= 0 {
                            continue;
                        }
                        filled += n as usize;
                        if filled < REPORT_SIZE {
                            continue;
                        }
                        filled = 0;
                        #[cfg(test)]
                        drop(READER_GATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                        let signal = libc::c_int::from(record[0]);
                        let arrival = u64::from_ne_bytes(record[8..].try_into().expect("eight bytes"));
                        let mut registry = registry();
                        let (targets, goes_on) = decide(&mut registry.commands, arrival, signal);
                        for (group, leader) in &targets {
                            // SAFETY: the group (or a leader's PID) is a running command's own:
                            // it cannot be reaped while this lock is held.
                            unsafe { libc::kill(if *leader { *group } else { -group }, signal) };
                        }
                        // Stack still owes the signal its end once the command is done, also
                        // when the command was unnamed (it was exiting) before this was read:
                        // what runs it must not take the command's own exit as the outcome.
                        if goes_on {
                            let _ = TERMINATION.compare_exchange(0, signal, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst);
                        }
                        if goes_on && !targets.is_empty() {
                            PASSED_ON.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        if goes_on {
                            ahead.insert(arrival);
                            let mut boundary = HANDLED.load(std::sync::atomic::Ordering::SeqCst);
                            while ahead.remove(&(boundary + 1)) {
                                boundary += 1;
                            }
                            HANDLED.store(boundary, std::sync::atomic::Ordering::SeqCst);
                            forget_settled(&mut registry);
                            let _waiting = SETTLED.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            SETTLED.1.notify_all();
                            continue;
                        }
                        // SAFETY: restore the default action, unblock it here and raise it in
                        // this thread, which ends stack; the lock is held until then.
                        unsafe {
                            libc::signal(signal, libc::SIG_DFL);
                            let mut set: libc::sigset_t = std::mem::zeroed();
                            libc::sigemptyset(&mut set);
                            libc::sigaddset(&mut set, signal);
                            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                            libc::raise(signal);
                        }
                        drop(registry);
                    }
                })
                .map_err(|e| e.to_string())?;
            WAKE.store(fds[1], std::sync::atomic::Ordering::SeqCst);
            install_forwarding(report_signal).map(|_| ()).map_err(|e| e.to_string())
        })
        .clone()
        .map_err(io::Error::other)
}

/// What `signal`, numbered `arrival`, does: the groups to pass it on to (its commands still
/// named, each with whether it is a `leader` signalled alone), and whether stack goes on, which
/// it does only when the signal had commands and all of them were `passing`. Each command it
/// is passed on to counts it.
#[cfg(unix)]
fn decide(commands: &mut [Named], arrival: u64, signal: libc::c_int) -> (Vec<(i32, bool)>, bool) {
    let mut targets = Vec::new();
    let (mut any, mut all_passing) = (false, true);
    for command in commands.iter_mut().filter(|c| c.received(arrival, signal)) {
        any = true;
        all_passing &= command.passing;
        if command.until.is_none() {
            command.forwarded += 1;
            targets.push((command.group, command.leader));
        }
    }
    (targets, any && all_passing)
}

/// Spawn `command` and name its process group (see [`Registry`]), under the registry's lock.
/// A signal arriving from the moment the spawn begins is the command's too. A `leader` command
/// stays in stack's own group; its PID names it. Nothing is spawned while a signal that
/// already arrived is still to be handled (see [`settle`]): one that ends stack does so first.
/// Returns the command with what watches it for the signals passed on to it, which counts
/// from its naming, not from when its run gets to watching.
#[cfg(unix)]
fn spawn_named(command: &mut Command, passing: bool, leader: bool, spawn: impl FnOnce(&mut Command) -> io::Result<Child>) -> io::Result<(Child, Passed)> {
    let (mut registry, since) = admitted();
    #[cfg(test)]
    stage(Stage::Admitted);
    let child = spawn(command)?;
    let group = child.id() as i32;
    registry.commands.push(Named { group, passing, since, until: None, leader, forwarded: 0 });
    let passed = Passed { group, seen: PASSED_ON.load(std::sync::atomic::Ordering::SeqCst), forwarded: 0, first: None };
    drop(registry);
    #[cfg(test)]
    stage(Stage::Registered);
    Ok((child, passed))
}

/// The registry's lock, with every signal that arrived so far handled, and that arrival
/// number: one decision, so a signal is either already handled (and one that ends stack did
/// so) or arrives after it, for the command spawned under this lock. Handling takes the lock,
/// and [`HANDLED`] moves only under it, so until it is released no report handled meanwhile
/// can change the decision. Settling waits without the lock, then decides again.
#[cfg(unix)]
fn admitted() -> (std::sync::MutexGuard<'static, Registry>, u64) {
    loop {
        #[cfg(test)]
        stage(Stage::Deciding);
        let registry = registry();
        let arrival = ARRIVALS.load(std::sync::atomic::Ordering::SeqCst);
        if HANDLED.load(std::sync::atomic::Ordering::SeqCst) >= arrival {
            return (registry, arrival);
        }
        drop(registry);
        settle(arrival);
    }
}

/// Unname `child`'s group and end it (`kill`: with its group, else only reap it), under the
/// registry's lock, so the group is never signalled once its ID may be reused. A signal that
/// arrived while it was named still counts it (see [`Named`]), and is handled before this
/// returns (see [`settle`]): what ran the command then sees it in [`passed_on`].
#[cfg(unix)]
fn retire(child: &mut OwnedChild, kill: bool) {
    let mut registry = registry();
    let group = child.child.id() as i32;
    let until = ARRIVALS.load(std::sync::atomic::Ordering::SeqCst);
    let mut leader = false;
    if let Some(named) = registry.commands.iter_mut().find(|c| c.group == group && c.until.is_none()) {
        named.until = Some(until);
        leader = named.leader;
    }
    match (kill, leader) {
        (true, false) => child.terminate(),
        (true, true) => child.terminate_leader(),
        (false, _) => child.reap(),
    }
    forget_settled(&mut registry);
    drop(registry);
    settle(until);
}

/// Forget unnamed commands every signal they received was handled for.
#[cfg(unix)]
fn forget_settled(registry: &mut Registry) {
    let handled = HANDLED.load(std::sync::atomic::Ordering::SeqCst);
    registry.commands.retain(|c| c.until.map_or(true, |until| until > handled));
}

/// The signals `run_with_deadline` and `output` pass on to their command.
#[cfg(unix)]
const FORWARDED: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];


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
/// hangup signals stack receives meanwhile are passed on to the group, unless ignored, and the
/// group is killed once [`CANCEL_GRACE`] passes or a second one comes.
///
/// Those signals stay blocked from before the spawn until forwarding targets the new group,
/// and again during teardown, so none can end stack while the command would be left running;
/// one arriving in between is delivered (and forwarded) once unblocked. The command starts
/// with the caller's signal mask. Returns the exit code (128 + the signal for a command a
/// signal ended) and whether the deadline passed.
#[cfg(unix)]
pub fn run_with_deadline(command: &mut Command, timeout: Duration) -> io::Result<(Option<i32>, bool)> {
    use std::os::unix::process::CommandExt;
    forwarder()?;
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
        command.process_group(0).stdin(Stdio::null());
        // SAFETY: sigprocmask is async-signal-safe, as pre_exec requires.
        unsafe {
            command.pre_exec(move || os_check(libc::sigprocmask(libc::SIG_SETMASK, &caller_mask, std::ptr::null_mut())));
        }
        let (child, mut passed) = spawn_named(command, true, false, Command::spawn)?;
        let mut child = OwnedChild::new(child);
        let waited = set_mask(libc::SIG_SETMASK, &caller_mask).and_then(|_| {
            let start = Instant::now();
            loop {
                if exited(&child.child)? {
                    break Ok(false);
                }
                if start.elapsed() >= timeout {
                    break Ok(true);
                }
                // Stopped after a passed-on signal: killed below, not reported as timed out.
                if passed.expired() {
                    break Ok(false);
                }
                std::thread::sleep(EXIT_CHECK_INTERVAL);
            }
        });
        let _ = set_mask(libc::SIG_BLOCK, held);
        retire(&mut child, true);
        let timed_out = waited?;
        // Like a shell: a command ended by a signal reports 128 + its number.
        let code = child.status.and_then(|status| {
            use std::os::unix::process::ExitStatusExt;
            status.code().or_else(|| status.signal().map(|s| 128 + s))
        });
        Ok((if timed_out { None } else { code }, timed_out))
    }
}

/// Run a command on stack's own stdin, stdout and stderr until it exits, with no deadline, and
/// return how it ended. Interrupt, terminate and hangup signals stack receives meanwhile are
/// passed on, and stack waits for the command instead of ending and leaving it running; once
/// [`CANCEL_GRACE`] passes after the first, or a second comes, it is killed.
///
/// With no terminal on stdin, stdout or stderr (an agent or CI runner), the command gets its
/// own process group: each signal reaches the whole group, and whatever is still in it is
/// killed once the command is done after a signal was passed on. A command that ends on its own
/// leaves what it started running, as a shell would. On a terminal the command stays in stack's
/// group, the terminal's foreground job, so it can read the terminal and gets the terminal's own
/// interrupts directly; a SIGTERM sent to stack alone is passed on to the command itself.
#[cfg(unix)]
pub fn run_attached(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    forwarder()?;
    // SAFETY: isatty only inspects the descriptor.
    let terminal = [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO].into_iter().any(|fd| unsafe { libc::isatty(fd) } == 1);
    let held = forwarded_set();
    let caller_mask = set_mask(libc::SIG_BLOCK, &held)?;
    if !terminal {
        command.process_group(0);
    }
    // SAFETY: sigprocmask is async-signal-safe, as pre_exec requires.
    unsafe {
        command.pre_exec(move || os_check(libc::sigprocmask(libc::SIG_SETMASK, &caller_mask, std::ptr::null_mut())));
    }
    let (mut child, mut passed) = match spawn_named(command, true, terminal, Command::spawn) {
        Ok((child, passed)) => (OwnedChild::new(child), passed),
        Err(e) => {
            let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
            return Err(e);
        }
    };
    let waited = set_mask(libc::SIG_SETMASK, &caller_mask).and_then(|_| loop {
        if exited(&child.child)? {
            break Ok(());
        }
        if passed.expired() {
            break Ok(());
        }
        std::thread::sleep(EXIT_CHECK_INTERVAL);
    });
    let _ = set_mask(libc::SIG_BLOCK, &held);
    let kill = waited.is_err() || passed.signalled();
    retire(&mut child, kill);
    let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
    waited?;
    // A child whose status could not be read was killed at the latest by `terminate`.
    Ok(child.status.unwrap_or_else(|| std::process::ExitStatus::from_raw(libc::SIGKILL)))
}

#[cfg(not(unix))]
pub fn run_attached(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    command.status()
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
    if cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled before it could start"));
    }
    #[cfg(unix)]
    if let Some(remaining) = remaining() {
        if remaining.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the deadline passed before it could start"));
        }
        return output_within(command, remaining);
    }
    // Only a command in its own group can be ended for a cancellation (see [`cancel_scope`]).
    #[cfg(unix)]
    if cancellable() {
        return output_within(command, Duration::from_secs(u32::MAX.into()));
    }
    command.output()
}

#[cfg(unix)]
fn output_within(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    forwarder()?;
    let held = forwarded_set();
    let caller_mask = set_mask(libc::SIG_BLOCK, &held)?;
    // This thread blocks those signals from before the spawn to after the reaping; the command
    // starts unblocked. Every command running at once is named (several threads may each run
    // one), so a signal reaches each before stack ends.
    // SAFETY: sigprocmask is async-signal-safe, as pre_exec requires.
    unsafe {
        command.pre_exec(move || os_check(libc::sigprocmask(libc::SIG_SETMASK, &caller_mask, std::ptr::null_mut())));
    }
    let mut child = match spawn_named(command, false, false, spawn_piped) {
        Ok((child, _)) => OwnedChild::new(child),
        Err(e) => {
            let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
            return Err(e);
        }
    };
    let mut retired = false;
    let piped = set_mask(libc::SIG_SETMASK, &caller_mask).and_then(|_| {
        collect(&mut child, timeout, OUTPUT_LIMIT, None, None, |child, timed_out| {
            let _ = set_mask(libc::SIG_BLOCK, &held);
            // A command that finished may have started something meant to outlive it, as a
            // supervisor client starts the supervisor; only a command cut short loses its group.
            retire(child, timed_out);
            retired = true;
        })
    });
    // `collect` may have failed before its teardown ran; then nothing it started is kept.
    let _ = set_mask(libc::SIG_BLOCK, &held);
    if !retired {
        retire(&mut child, true);
    }
    let _ = set_mask(libc::SIG_SETMASK, &caller_mask);
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
/// commands that may start a process outliving them which nothing here may stop. A cancellation
/// (see [`cancel_scope`]) also ends the wait. Returns its exit status, or `None` if it was left
/// running.
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
        if start.elapsed() >= wait || cancelled() {
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
    fn another_thread_can_end_output_with_its_group_and_nothing_starts_after() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let started = tmp.path().join("started");
        let flag = Arc::new(AtomicBool::new(false));
        let worker = {
            let flag = flag.clone();
            let started = started.clone();
            std::thread::spawn(move || {
                // No deadline: cancellation alone must still end the command's whole group.
                let _cancel = cancel_scope(flag);
                let script = "sleep 30 & echo $!; touch \"$1\"; wait";
                let out = output(Command::new("sh").args(["-c", script, "sh"]).arg(&started)).unwrap();
                let child: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
                (out.status.success(), child, output(&mut Command::new("true")).map(|_| ()).unwrap_err().kind())
            })
        };
        // Cancelled only once the command is running, however long a busy machine takes to get
        // there; the bound keeps a command that never starts from hanging the test.
        let waiting = Instant::now();
        while !started.exists() && !worker.is_finished() {
            assert!(waiting.elapsed() < Duration::from_secs(20), "the command never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = Instant::now();
        flag.store(true, Ordering::SeqCst);
        let (succeeded, child, later) = worker.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert!(!succeeded);
        assert!(ends_soon(child as u32), "the group's sleep survived");
        assert_eq!(later, io::ErrorKind::Interrupted);
    }

    #[test]
    fn commands_run_at_once_are_each_named_for_forwarding_and_released_once() {
        let _deadline = deadline_scope(Some(Instant::now() + Duration::from_secs(30)));
        let running: Vec<_> = (0..8)
            .map(|n| {
                let deadline = deadline();
                std::thread::spawn(move || {
                    let _deadline = deadline_scope(deadline);
                    output(Command::new("sh").args(["-c", &format!("sleep 0.{n}; exit {n}")])).unwrap().status.code()
                })
            })
            .collect();
        let codes: Vec<_> = running.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(codes, (0..8).map(Some).collect::<Vec<_>>());
        // Other tests may run commands meanwhile; once all are done nothing stays named.
        let until = Instant::now() + Duration::from_secs(20);
        while registry().commands.iter().any(|c| c.until.is_none()) {
            assert!(Instant::now() < until, "commands stayed named");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_signal_is_decided_by_the_commands_named_when_it_arrived() {
        let named = |group, passing, since, until| Named { group, passing, since, until, leader: false, forwarded: 0 };
        let term = libc::SIGTERM;
        // exec's command (passing) and a provider command, both named from arrival 0.
        let mut commands = vec![named(10, true, 0, None), named(20, false, 0, None)];
        assert_eq!(decide(&mut commands, 1, term), (vec![(10, false), (20, false)], false), "reaches both, ends stack");
        // The provider command is unnamed at arrival 1: a signal numbered 1 is still its,
        // so it still ends stack, but its group is no longer signalled.
        commands[1].until = Some(1);
        assert_eq!(decide(&mut commands, 1, term), (vec![(10, false)], false));
        // One that arrived after only reaches exec's command, and stack goes on.
        assert_eq!(decide(&mut commands, 2, term), (vec![(10, false)], true));
        // Each command counts what was passed on to it.
        assert_eq!((commands[0].forwarded, commands[1].forwarded), (3, 1));
        // A user's command unnamed as it exited, after the signal arrived for it: nothing is
        // signalled, but the signal was its, so stack goes on and owes the signal its end.
        assert_eq!(decide(&mut [named(50, true, 0, Some(3))], 3, term), (vec![], true));
        // A command named after a signal arrived is not its; with no command it ends stack. Such
        // a signal is handled before that command is spawned (see `admitted`).
        assert_eq!(decide(&mut [named(30, true, 5, None)], 5, term), (vec![], false));
        assert_eq!(decide(&mut [], 1, term), (vec![], false));
        // A terminal's foreground command is passed SIGTERM alone, as its own PID; the
        // terminal's interrupt already reached it, and still ends stack as it did.
        let mut attached = vec![Named { leader: true, ..named(40, true, 0, None) }];
        assert_eq!(decide(&mut attached, 1, term), (vec![(40, true)], true));
        assert_eq!(decide(&mut attached, 2, libc::SIGINT), (vec![], false));
        assert_eq!(decide(&mut attached, 3, libc::SIGHUP), (vec![], false));
    }

    /// Set, to a scratch directory, in the process [`run_alone`] starts.
    const ALONE: &str = "STACK_PROCESS_TEST_ALONE";

    /// Run ignored test `name` of this module in a process of its own, which it may send signals
    /// that would end it, and return how that process ended. Bounded, so one that hangs fails.
    fn run_alone(name: &str, scratch: &std::path::Path) -> Captured {
        let name = format!("{}::{name}", module_path!().split_once("::").unwrap().1);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([name.as_str(), "--exact", "--ignored", "--test-threads=1", "--nocapture"]).env(ALONE, scratch);
        let run = capture(&mut command, Duration::from_secs(60), 1 << 20).unwrap();
        assert!(!run.timed_out, "{name} did not finish within 60s\nstdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        run
    }

    /// Wait until stack's handler counted `n` signals.
    fn arrived(n: u64) {
        let until = Instant::now() + Duration::from_secs(10);
        while ARRIVALS.load(std::sync::atomic::Ordering::SeqCst) < n {
            assert!(Instant::now() < until, "no signal arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_signal_a_command_got_counts_once_its_run_returns_however_late_it_is_handled() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_alone("late_signal_for_a_command", dir.path());
        assert!(run.exit_code == Some(0) && run.stdout.contains("1 passed"), "stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
    }

    #[test]
    #[ignore = "run by a_signal_a_command_got_counts_once_its_run_returns_however_late_it_is_handled"]
    fn late_signal_for_a_command() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else { return };
        let go = dir.join("go");
        let path = std::ffi::CString::new(go.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid path.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        // The signal is read but not handled until this is dropped, as a busy machine may delay it.
        let gate = READER_GATE.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let fifo = go.clone();
        std::thread::spawn(move || {
            // A setup that handles SIGTERM: it sends stack (this process) one, then exits 0.
            let mut sh = Command::new("sh");
            sh.args(["-c", "kill -TERM $PPID && read go < \"$1\" && exit 0", "sh"]).arg(fifo);
            let out = capture_redacted(&mut sh, Duration::from_secs(30), 1024, None).map(|o| o.exit_code);
            tx.send((out.map_err(|e| e.to_string()), passed_on())).unwrap();
        });
        // The signal arrived while the command ran; only then may it exit.
        arrived(1);
        std::fs::write(&go, "\n").unwrap();
        if let Ok(early) = rx.recv_timeout(Duration::from_secs(2)) {
            panic!("the run returned before the signal it got was handled: {early:?}");
        }
        drop(gate);
        let (out, passed) = rx.recv_timeout(Duration::from_secs(10)).expect("the run returns once the signal is handled");
        assert_eq!(out, Ok(Some(0)));
        assert_eq!(passed, Some(libc::SIGTERM), "the signal is owed, though the command exited 0");
    }

    #[test]
    fn nothing_starts_while_a_signal_that_arrived_is_still_to_be_handled() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_alone("late_signal_between_commands", dir.path());
        // The signal came while no command ran, so it ended stack (this child) as it asked,
        // before the next command could start.
        assert_eq!((run.exit_code, run.signal), (None, Some(libc::SIGTERM)), "stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        assert!(!dir.path().join("started").exists(), "a command started before the signal was handled");
    }

    #[test]
    #[ignore = "run by nothing_starts_while_a_signal_that_arrived_is_still_to_be_handled"]
    fn late_signal_between_commands() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else { return };
        forwarder().unwrap();
        let gate = READER_GATE.lock().unwrap();
        // SAFETY: signals this process alone, which runs no other test.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        arrived(1);
        let started = dir.join("started");
        std::thread::spawn(move || capture_redacted(Command::new("touch").arg(started), Duration::from_secs(30), 1024, None));
        std::thread::sleep(Duration::from_secs(1));
        drop(gate);
        std::thread::sleep(Duration::from_secs(20));
        panic!("still running after the signal was handled");
    }

    /// Run `hook` in the thread running a command, the first time one reaches `stage`.
    fn at_stage(stage: Stage, hook: impl FnOnce() + Send + 'static) {
        let mut hook = Some(hook);
        *STAGE_HOOK.lock().unwrap() = Some(Box::new(move |reached| {
            if reached == stage {
                if let Some(hook) = hook.take() {
                    hook();
                }
            }
        }));
    }

    /// Send this process SIGTERM, and wait until stack's handler counted it as arrival `n`.
    fn terminate_self(n: u64) {
        // SAFETY: signals this process alone, which runs no other test.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        arrived(n);
    }

    /// Wait until `done`, bounded so a schedule that never gets there fails.
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Writes its PID to `$1` whole, then runs on: a command that would outlive stack.
    const STARTS: &str = r#"echo $$ > "$1.tmp" && mv "$1.tmp" "$1" && exec sleep 30"#;

    /// The PID a command running [`STARTS`] wrote to `file`.
    fn pid_in(file: &std::path::Path) -> Option<i32> {
        std::fs::read_to_string(file).ok()?.trim().parse().ok()
    }

    /// End a test's own command left running, so a failing schedule leaves nothing behind.
    fn reap_left(file: &std::path::Path) -> Option<i32> {
        let pid = pid_in(file).filter(|pid| alive(*pid))?;
        // SAFETY: the PID of the test's own `sleep`, just checked to be running.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        Some(pid)
    }

    #[test]
    fn a_signal_that_arrives_as_a_command_is_admitted_is_handled_before_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_alone("signal_during_admission", dir.path());
        let left = reap_left(&dir.path().join("started"));
        let shown = format!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        assert_eq!(left, None, "a command admitted after the signal arrived, but before it was handled, outlived stack\n{shown}");
        // With no command to pass it on to, the signal ended stack (this child) as it asked.
        assert_eq!((run.exit_code, run.signal), (None, Some(libc::SIGTERM)), "{shown}");
        assert!(!dir.path().join("started").exists(), "a command started after the signal that ended stack\n{shown}");
    }

    #[test]
    #[ignore = "run by a_signal_that_arrives_as_a_command_is_admitted_is_handled_before_it_starts"]
    fn signal_during_admission() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else { return };
        forwarder().unwrap();
        // The signal is read but not handled until this is dropped, as a busy machine may delay it.
        let gate = READER_GATE.lock().unwrap();
        // It arrives after any settling an earlier admission did, before this one's decision.
        at_stage(Stage::Deciding, || terminate_self(1));
        let waits = SETTLE_WAITS.load(std::sync::atomic::Ordering::SeqCst);
        let started = dir.join("started");
        let file = started.clone();
        std::thread::spawn(move || capture_redacted(Command::new("sh").args(["-c", STARTS, "sh"]).arg(file), Duration::from_secs(30), 1024, None));
        // The admission waits for the signal to be handled, or (wrongly) the command starts.
        wait_for("the admission to wait or its command to start", || started.exists() || SETTLE_WAITS.load(std::sync::atomic::Ordering::SeqCst) > waits);
        drop(gate);
        std::thread::sleep(Duration::from_secs(20));
        panic!("still running after the signal was handled");
    }

    #[test]
    fn a_signal_after_a_commands_admission_boundary_reaches_it() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_alone("signal_after_admission", dir.path());
        let started = dir.path().join("started");
        let pid = pid_in(&started);
        let ended = pid.is_some_and(|pid| ends_soon(pid as u32));
        let left = reap_left(&started);
        let shown = format!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        assert!(pid.is_some() && ended && left.is_none(), "the admitted command {pid:?} was not passed the signal\n{shown}");
        // A provider command's signal is passed on, then ends stack.
        assert_eq!((run.exit_code, run.signal), (None, Some(libc::SIGTERM)), "{shown}");
    }

    #[test]
    #[ignore = "run by a_signal_after_a_commands_admission_boundary_reaches_it"]
    fn signal_after_admission() {
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else { return };
        forwarder().unwrap();
        let gate = READER_GATE.lock().unwrap();
        // Holding the registry's lock, the boundary checked: the signal is the command's.
        at_stage(Stage::Admitted, || terminate_self(1));
        let started = dir.join("started");
        let file = started.clone();
        std::thread::spawn(move || capture(Command::new("sh").args(["-c", STARTS, "sh"]).arg(file), Duration::from_secs(30), 1024));
        wait_for("the command to start", || started.exists());
        drop(gate);
        std::thread::sleep(Duration::from_secs(20));
        panic!("still running after the signal was handled");
    }

    #[test]
    fn a_signal_for_a_running_command_is_handled_before_another_is_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_alone("signal_between_overlapping_admissions", dir.path());
        let left = reap_left(&dir.path().join("first"));
        let shown = format!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        assert_eq!(left, None, "the running command outlived stack\n{shown}");
        assert!(run.exit_code == Some(0) && run.stdout.contains("1 passed"), "{shown}");
    }

    #[test]
    #[ignore = "run by a_signal_for_a_running_command_is_handled_before_another_is_admitted"]
    fn signal_between_overlapping_admissions() {
        use std::sync::atomic::Ordering::SeqCst;
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else { return };
        let (first, second) = (dir.join("first"), dir.join("second"));
        // A user's command running throughout: the signal is its, so stack goes on.
        let file = first.clone();
        let running = std::thread::spawn(move || capture_redacted(Command::new("sh").args(["-c", STARTS, "sh"]).arg(file), Duration::from_secs(30), 1024, None).unwrap());
        wait_for("the first command to start", || first.exists());
        let gate = READER_GATE.lock().unwrap();
        at_stage(Stage::Deciding, || terminate_self(1));
        let waits = SETTLE_WAITS.load(SeqCst);
        let file = second.clone();
        let admitted = std::thread::spawn(move || capture_redacted(Command::new("sh").args(["-c", r#"echo $$ > "$1""#, "sh"]).arg(file), Duration::from_secs(30), 1024, None).unwrap());
        wait_for("the admission to wait or its command to start", || second.exists() || SETTLE_WAITS.load(SeqCst) > waits);
        assert!(!second.exists(), "the second command started before the signal that arrived first was handled");
        assert_eq!((HANDLED.load(SeqCst), passed_on()), (0, None));
        // What the admission saw once it went ahead.
        let (tx, rx) = std::sync::mpsc::channel();
        at_stage(Stage::Admitted, move || tx.send((HANDLED.load(SeqCst), passed_on())).unwrap());
        drop(gate);
        let running = running.join().unwrap();
        let admitted = admitted.join().unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok((1, Some(libc::SIGTERM))), "admitted before the signal was handled");
        assert_eq!(running.signal, Some(libc::SIGTERM), "the running command was passed the signal");
        assert_eq!((admitted.exit_code, admitted.signal), (Some(0), None), "the command admitted after it was not");
        assert_eq!(passed_on(), Some(libc::SIGTERM));
        // Both retired once done, and forgotten: every signal they received was handled.
        assert!(registry().commands.is_empty());
    }

    /// Starts a background `sleep`, writes its PID to `$1` whole and waits for it, both ignoring
    /// SIGTERM: a user's command that keeps working after a cancellation.
    const IGNORES: &str = r#"trap '' TERM; sleep 60 & echo $! > "$1.tmp" && mv "$1.tmp" "$1"; wait"#;

    /// An unrelated process, in a group of its own, that no signal of stack's may reach; killed
    /// once dropped.
    struct Sentinel(std::process::Child);

    impl Sentinel {
        fn start() -> Self {
            use std::os::unix::process::CommandExt;
            Self(Command::new("sleep").arg("60").process_group(0).spawn().unwrap())
        }

        fn alive(&mut self) -> bool {
            self.0.try_wait().unwrap().is_none()
        }
    }

    impl Drop for Sentinel {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The runs that watch a user's command for the signals passed on to it.
    #[derive(Clone, Copy)]
    enum Watched {
        Captured,
        Deadline,
        Attached,
    }

    /// Run [`watched_after_a_signal`] for `how` in a process of its own (`name`); nothing it
    /// started may be left.
    fn signal_before_watching(name: &str) {
        let dir = tempfile::tempdir().unwrap();
        let run = run_alone(name, dir.path());
        let left = reap_left(&dir.path().join("started"));
        let shown = format!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
        assert_eq!(left, None, "the command's group outlived its run\n{shown}");
        assert!(run.exit_code == Some(0) && run.stdout.contains("1 passed"), "{shown}");
    }

    /// A user's command is passed SIGTERM, and that is fully handled, once it is named but before
    /// its run watches it. It ignores the signal. With no second signal, its group must still be
    /// killed once [`CANCEL_GRACE`] passes: not at the run's own deadline, and not never.
    fn watched_after_a_signal(how: Watched) {
        use std::os::unix::process::ExitStatusExt;
        use std::sync::atomic::Ordering::SeqCst;
        let Some(dir) = std::env::var_os(ALONE).map(std::path::PathBuf::from) else { return };
        forwarder().unwrap();
        let mut sentinel = Sentinel::start();
        let started = dir.join("started");
        let file = started.clone();
        at_stage(Stage::Registered, move || {
            wait_for("the command to start", || file.exists());
            terminate_self(1);
            wait_for("the signal to be passed on", || HANDLED.load(SeqCst) >= 1);
        });
        let mut command = Command::new("sh");
        command.args(["-c", IGNORES, "sh"]).arg(&started);
        let start = Instant::now();
        let (signal, timed_out) = match how {
            Watched::Captured => {
                let out = capture_redacted(&mut command, Duration::from_secs(40), 1024, None).unwrap();
                (out.signal, out.timed_out)
            }
            Watched::Deadline => {
                let (code, timed_out) = run_with_deadline(&mut command, Duration::from_secs(40)).unwrap();
                (code.map(|c| c - 128), timed_out)
            }
            Watched::Attached => (run_attached(&mut command).unwrap().signal(), false),
        };
        let took = start.elapsed();
        assert!(!timed_out && took < Duration::from_secs(30), "the grace never started: ended after {took:?}, timed out {timed_out}");
        assert!(took >= CANCEL_GRACE, "killed after {took:?}, before the grace passed");
        assert_eq!(signal, Some(libc::SIGKILL), "the command was killed with its group");
        assert_eq!((ARRIVALS.load(SeqCst), passed_on()), (1, Some(libc::SIGTERM)), "one signal, owed");
        let child = pid_in(&started).unwrap();
        assert!(ends_soon(child as u32), "the group's sleep {child} survived");
        assert!(sentinel.alive(), "an unrelated process was signalled");
    }

    #[test]
    fn a_signal_handled_before_a_capture_watches_its_command_still_starts_the_grace() {
        signal_before_watching("signal_before_watching_a_capture");
    }

    #[test]
    #[ignore = "run by a_signal_handled_before_a_capture_watches_its_command_still_starts_the_grace"]
    fn signal_before_watching_a_capture() {
        watched_after_a_signal(Watched::Captured);
    }

    #[test]
    fn a_signal_handled_before_a_deadline_run_watches_its_command_still_starts_the_grace() {
        signal_before_watching("signal_before_watching_a_deadline_run");
    }

    #[test]
    #[ignore = "run by a_signal_handled_before_a_deadline_run_watches_its_command_still_starts_the_grace"]
    fn signal_before_watching_a_deadline_run() {
        watched_after_a_signal(Watched::Deadline);
    }

    #[test]
    fn a_signal_handled_before_an_attached_run_watches_its_command_still_starts_the_grace() {
        signal_before_watching("signal_before_watching_an_attached_run");
    }

    #[test]
    #[ignore = "run by a_signal_handled_before_an_attached_run_watches_its_command_still_starts_the_grace"]
    fn signal_before_watching_an_attached_run() {
        watched_after_a_signal(Watched::Attached);
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

    /// `text` streamed in pieces of `size` through `r` into a tail of `limit`, as a captured
    /// stream is.
    fn sanitized(r: &Redactor, text: &[u8], size: usize, limit: usize) -> String {
        let mut sink = Sink::new(limit, Some(r));
        for chunk in text.chunks(size.max(1)) {
            sink.append(chunk);
        }
        sink.finish().text()
    }

    #[test]
    fn a_marker_never_spells_out_the_value_it_replaces() {
        // The marker word itself: no marker can be written without it, so nothing is released.
        let r = redactor(&[("TOKEN", "redacted")]);
        assert_eq!(r.conflicts(), ["TOKEN"]);
        assert_eq!(r.redact("redacted"), "");
        assert_eq!(r.redact("anything else"), "");
        // A value equal to its key: the key is not shown.
        let r = redactor(&[("SECRETKEY", "SECRETKEY")]);
        assert!(r.conflicts().is_empty());
        assert_eq!(r.redact("x SECRETKEY y"), "x [redacted] y");
        // Values the labeled marker (or text around it) would spell out: the key is hidden.
        for value in ["[redacted:K]x", "redacted:K", "d:K]tail", "x[redacted:K"] {
            let r = redactor(&[("K", value)]);
            assert!(r.conflicts().is_empty(), "{value}");
            assert_eq!(r.redact(&format!("<{value}>")), "<[redacted]>", "{value}");
        }
        // Values any marker would spell out, with whatever the command writes around it.
        for value in ["]-starts-with-a-close", "ends-with-an-open[", "ends-in-[reda", "redacted"] {
            assert_eq!(redactor(&[("K", value)]).conflicts(), ["K"], "{value}");
        }
    }

    #[test]
    fn a_key_naming_another_value_is_not_shown() {
        // OTHER's value is DEPLOY_KEY's name.
        let r = redactor(&[("DEPLOY_KEY", "deploy-value-123"), ("OTHER", "DEPLOY_KEY")]);
        assert!(r.conflicts().is_empty());
        assert_eq!(r.redact("deploy-value-123 DEPLOY_KEY"), "[redacted] [redacted:OTHER]");
        // A dependency fnox reports under a name that contains a granted value.
        let r = redactor(&[("DEPLOY_KEY", "LEAKVALUE9"), ("DEP_LEAKVALUE9_X", "dependency-value-9")]);
        assert_eq!(r.redact("LEAKVALUE9 dependency-value-9"), "[redacted:DEPLOY_KEY] [redacted]");
        // Names that are not variable names are never echoed into output.
        let long = "K".repeat(129);
        for key in ["dep key", "lower_case", "NEW\nLINE", "Ä", "", long.as_str()] {
            assert_eq!(redactor(&[(key, "some-long-value")]).redact("<some-long-value>"), "<[redacted]>", "{key:?}");
        }
        assert_eq!(redactor(&[(&"K".repeat(128), "some-long-value")]).redact("some-long-value"), format!("[redacted:{}]", "K".repeat(128)));
    }

    #[test]
    fn replacing_one_value_never_writes_another_across_the_marker() {
        // B's value would be read across A's labeled marker and the text after it.
        let r = redactor(&[("A", "value-of-a-1"), ("B", "d:A]tail-xyz")]);
        assert!(r.conflicts().is_empty());
        for size in 1..=20 {
            assert_eq!(String::from_utf8(streamed(&r, b"value-of-a-1tail-xyz", size)).unwrap(), "[redacted]tail-xyz", "chunks of {size}");
        }
        // Identical values under two keys, and overlapping ones whose markers are both hidden.
        let r = redactor(&[("A", "same-value-1"), ("B", "same-value-1")]);
        assert_eq!(r.redact("<same-value-1>"), "<[redacted:A][redacted:B]>");
        let r = redactor(&[("dep a", "abcdefgh"), ("dep b", "efghijkl")]);
        assert_eq!(r.redact("<abcdefghijkl>"), "<[redacted]>");
        // The restriction is narrow: brackets, the word, and ellipses elsewhere are fine.
        for value in ["notredacted123", "[redacted]", "a]b[c-long", "pass[word]x", "tail…ends…", "[]{}()<>"] {
            assert!(redactor(&[("K", value)]).conflicts().is_empty(), "{value}");
        }
    }

    #[test]
    fn the_truncation_notice_and_lossy_decoding_never_spell_out_a_value() {
        for value in ["truncated]…rest", "…[truncated]…x", "[truncated]", "…starts-with-an-ellipsis", "pass\u{FFFD}word"] {
            assert_eq!(redactor(&[("K", value)]).conflicts(), ["K"], "{value}");
        }
        // What those values guard against: inserted text completing what the command wrote.
        let r = redactor(&[("K", "value-long-enough")]);
        assert_eq!(sanitized(&r, b"xxxxtruncated]\xe2\x80\xa6rest", 4, 17), "…[truncated]…truncated]…rest");
        assert_eq!(sanitized(&r, b"pass\xffword", 3, 1024), "pass\u{FFFD}word");
    }

    #[test]
    fn no_value_survives_any_split_or_bound() {
        // Value sets around markers, keys, the notice and invalid bytes; inputs are random
        // sequences of values, their pieces, and the text stack inserts.
        let sets: &[&[(&str, &str)]] = &[
            &[("A", "value-of-a-1"), ("B", "d:A]tail-xyz")],
            &[("SECRETKEY", "SECRETKEY"), ("OTHER", "SECRETKEY-2")],
            &[("DEPLOY_KEY", "deploy-value-123"), ("OTHER", "DEPLOY_KEY")],
            &[("A", "same-value-1"), ("B", "same-value-1"), ("C", "value-1same")],
            &[("R", "aaaaaaaa"), ("IN", "xyz12345"), ("OUT", "--xyz12345--")],
            &[("dep a", "abcdefgh"), ("dep b", "efghijkl"), ("K", "hijk[]{}")],
            &[("K", "pässwörd✓"), ("L", "mid…ellipsis…")],
        ];
        let mut seed: u64 = 0x5eed;
        let mut next = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        let mut checked = 0;
        for set in sets {
            let r = redactor(set);
            assert!(r.conflicts().is_empty(), "{set:?}");
            let mut pieces: Vec<Vec<u8>> = vec![b"x".to_vec(), b"\xff".to_vec(), b"\xe2".to_vec(), TRUNCATED.into(), REDACTED.into(), b"]".to_vec(), b"[".to_vec()];
            for (key, value) in *set {
                let marker = format!("[redacted:{key}]");
                for text in [value.as_bytes(), marker.as_bytes()] {
                    pieces.push(text.to_vec());
                    pieces.push(text[..text.len() / 2].to_vec());
                    pieces.push(text[text.len() / 2..].to_vec());
                }
            }
            for _ in 0..400 {
                let input: Vec<u8> = (0..1 + next(6)).flat_map(|_| pieces[next(pieces.len())].clone()).collect();
                for (size, limit) in [(1, 1 << 20), (3, 7), (5, 13), (64, 24), (2, 40), (input.len(), 1 << 20)] {
                    let out = sanitized(&r, &input, size, limit);
                    for (_, value) in *set {
                        assert!(!out.contains(value), "{set:?} {:?} chunks of {size}, limit {limit}: {out:?}", String::from_utf8_lossy(&input));
                    }
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, sets.len() * 400 * 6);
    }

    #[test]
    fn a_redactor_with_conflicts_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        let r = redactor(&[("SAFE", "safe-value-1"), ("TOKEN", "redacted")]);
        assert_eq!(r.conflicts(), ["TOKEN"]);
        let e = capture_redacted(Command::new("touch").arg(&ran), Duration::from_secs(5), 1024, Some(&r)).err().unwrap();
        assert!(!e.to_string().contains("safe-value-1") && !e.to_string().contains("redacted\""), "{e}");
        assert!(!ran.exists(), "the command ran");
    }

    #[test]
    fn the_bound_applies_to_the_sanitized_stream() {
        let r = redactor(&[("A", "value-of-a-1"), ("B", "d:A]tail-xyz")]);
        // Each value is shorter than its replacement: 1000 of them still fit the bound.
        let script = "i=0; while [ $i -lt 1000 ]; do printf 'value-of-a-1tail-xyz'; printf 'value-of-a-1tail-xyz' >&2; i=$((i+1)); done";
        let out = capture_redacted(Command::new("sh").args(["-c", script]), Duration::from_secs(10), 100, Some(&r)).unwrap();
        assert!(out.stdout_truncated);
        for stream in [&out.stdout, &out.stderr] {
            assert_eq!(stream.len(), TRUNCATED.len() + 100, "{stream}");
            assert!(stream.ends_with("[redacted]tail-xyz") && !stream.contains("d:A]tail-xyz") && !stream.contains("value-of-a-1"), "{stream}");
        }
        // Every cut of the tail, including inside a marker.
        for limit in 1..=40 {
            let out = capture_redacted(Command::new("sh").args(["-c", "printf 'xx value-of-a-1tail-xyz' | tee /dev/stderr"]), Duration::from_secs(5), limit, Some(&r)).unwrap();
            for stream in [&out.stdout, &out.stderr] {
                let body = stream.strip_prefix(TRUNCATED).unwrap_or(stream);
                assert!(body.len() <= limit && !stream.contains("d:A]tail-xyz") && !stream.contains("value-of-a-1"), "{limit}: {stream}");
            }
        }
        // A timed-out capture releases what it held back the same way.
        let out = capture_redacted(Command::new("sh").args(["-c", "printf 'value-of-a-1tail-xyz'; sleep 20"]), Duration::from_millis(300), 1024, Some(&r)).unwrap();
        assert!(out.timed_out);
        assert_eq!(out.stdout, "[redacted]tail-xyz");
    }
}

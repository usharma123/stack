//! The MCP server loop: JSON-RPC over stdio, with calls running concurrently.
//!
//! Stdin is read on its own thread, so cheap requests (`ping`, `tools/list`, ...) are answered
//! at once, also while a long `tools/call` runs. Each `tools/call` runs on a worker thread.
//! Calls on the same checkout run one at a time, in the order they arrived; calls on different
//! checkouts run concurrently. `notifications/cancelled` stops an in-flight call through its
//! [`Cancel`], which ends it exactly as its deadline would (subprocesses killed with their
//! process groups, launched services recorded), and no response is sent for it. A call that
//! carries `_meta.progressToken` gets `notifications/progress` for each lifecycle step and a
//! heartbeat while it runs. Every message is written whole, one per line, under one lock.
//!
//! When a call names no `dir`, its checkout is the client's single MCP root: the server asks
//! for `roots/list` once the client is initialized (and again on
//! `notifications/roots/list_changed`) and holds calls that need the answer until it arrives,
//! or [`ROOTS_WAIT`] passes. It never falls back to its own working directory.

use super::{call, needs_roots, negotiate, project_key, tools, INSTRUCTIONS};
use crate::process::Cancel;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long calls that need the client's roots wait for its `roots/list` answer.
const ROOTS_WAIT: Duration = Duration::from_secs(5);

/// Time between heartbeat progress notifications of a call that asked for progress.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// The only protocol revision that allows JSON-RPC batches.
const BATCH_VERSION: &str = "2025-03-26";

/// The agent guide, served as the `stack://guide` resource.
const GUIDE: &str = include_str!("../../docs/user/agents.md");
const GUIDE_URI: &str = "stack://guide";

/// MCP log levels, least severe first.
const LEVELS: [&str; 8] = ["debug", "info", "notice", "warning", "error", "critical", "alert", "emergency"];

/// The level `notifications/message` starts at, until the client sets one: `warning`.
const DEFAULT_LOG_LEVEL: usize = 3;

/// Runs one `tools/call`: its params and the client's roots (`None` when it offers none).
pub(super) type Caller = Arc<dyn Fn(&Value, Option<&[PathBuf]>) -> Value + Send + Sync>;

/// What the loop waits for.
enum Event {
    Line(std::io::Result<String>),
    /// Input ended.
    End,
    /// A call was answered after a signal was passed on to a command (see below).
    Stop,
}

/// Serve until `input` ends, then let calls in flight finish and answer them.
pub fn serve(input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> std::io::Result<()> {
    serve_with(input, output, Arc::new(call))
}

pub(super) fn serve_with(input: impl BufRead + Send + 'static, output: impl Write + Send + 'static, caller: Caller) -> std::io::Result<()> {
    let (events, received) = mpsc::channel();
    let lines = events.clone();
    std::thread::spawn(move || {
        for line in input.lines() {
            let end = line.is_err();
            if lines.send(Event::Line(line)).is_err() || end {
                return;
            }
        }
        let _ = lines.send(Event::End);
    });
    let mut server = Server::new(Arc::new(Out(Mutex::new(Box::new(output)))), caller, events);
    let result = loop {
        // A signal that would have ended the server was passed on to the command a call ran
        // instead: nothing more starts, and the server ends as the signal asked once the calls
        // in flight are answered.
        if crate::process::passed_on().is_some() {
            server.deferred.clear();
            break Ok(());
        }
        let next = match server.roots_deadline() {
            Some(at) => received.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => received.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Event::Line(Ok(line))) => server.line(&line),
            Ok(Event::Line(Err(e))) => break Err(e),
            Ok(Event::Stop) => {}
            Err(RecvTimeoutError::Timeout) => server.roots_failed("the client did not answer roots/list in time"),
            Ok(Event::End) | Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
    };
    server.finish();
    result
}

/// The output stream; one message at a time.
struct Out(Mutex<Box<dyn Write + Send>>);

impl Out {
    fn send(&self, message: &Value) {
        let mut out = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A client that went away cannot be told; the calls still finish.
        let _ = super::send(&mut *out, message.clone());
    }
}

/// Where a request's response goes: straight out, or into the batch it arrived in.
#[derive(Clone)]
enum Reply {
    Direct(Arc<Out>),
    Batch(Arc<Batch>),
}

impl Reply {
    fn ok(&self, id: &Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn error(&self, id: &Value, code: i64, message: impl Into<String>) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } }));
    }

    fn send(&self, response: Value) {
        match self {
            Reply::Direct(out) => out.send(&response),
            Reply::Batch(batch) => batch.add(Some(response)),
        }
    }

    /// No response will come (a cancelled request).
    fn skip(&self) {
        if let Reply::Batch(batch) = self {
            batch.add(None);
        }
    }

    /// A response is still owed.
    fn expect(&self) {
        if let Reply::Batch(batch) = self {
            batch.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0 += 1;
        }
    }
}

/// The responses of one batch, sent together once the last owed one is in.
struct Batch {
    out: Arc<Out>,
    /// Responses still owed, and those collected.
    state: Mutex<(usize, Vec<Value>)>,
}

impl Batch {
    fn add(&self, response: Option<Value>) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.1.extend(response);
        state.0 -= 1;
        if state.0 == 0 && !state.1.is_empty() {
            self.out.send(&Value::Array(std::mem::take(&mut state.1)));
        }
    }
}

enum Roots {
    /// The client offers none.
    Unsupported,
    /// Asked for (or about to be); calls that need them wait. `since` starts [`ROOTS_WAIT`].
    Pending { since: Instant, asked: Option<String> },
    Known(Vec<PathBuf>),
}

struct Server {
    out: Arc<Out>,
    caller: Caller,
    events: mpsc::Sender<Event>,
    /// The protocol revision `initialize` settled on.
    version: &'static str,
    client_roots: bool,
    roots: Roots,
    requests_sent: u64,
    /// Calls waiting for the client's roots, in arrival order.
    deferred: VecDeque<(Value, Reply)>,
    /// The [`Cancel`] of each call in flight, by its request id.
    inflight: Arc<Mutex<HashMap<String, Cancel>>>,
    queues: Arc<Queues>,
    workers: Vec<JoinHandle<()>>,
    /// The least severe of [`LEVELS`] sent as `notifications/message`.
    log_level: usize,
}

impl Server {
    fn new(out: Arc<Out>, caller: Caller, events: mpsc::Sender<Event>) -> Self {
        Server {
            out,
            caller,
            events,
            version: super::PROTOCOL_VERSIONS[0],
            client_roots: false,
            roots: Roots::Unsupported,
            requests_sent: 0,
            deferred: VecDeque::new(),
            inflight: Arc::default(),
            queues: Arc::default(),
            workers: Vec::new(),
            log_level: DEFAULT_LOG_LEVEL,
        }
    }

    fn line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let direct = Reply::Direct(self.out.clone());
        match serde_json::from_str::<Value>(line) {
            Err(e) => direct.error(&Value::Null, -32700, format!("parse error: {e}")),
            Ok(Value::Array(items)) => self.batch(items),
            Ok(message) => self.message(message, direct),
        }
    }

    fn batch(&mut self, items: Vec<Value>) {
        let direct = Reply::Direct(self.out.clone());
        if self.version != BATCH_VERSION {
            return direct.error(&Value::Null, -32600, format!("JSON-RPC batches are not supported in protocol version {}; send one message per line", self.version));
        }
        if items.is_empty() {
            return direct.error(&Value::Null, -32600, "invalid request: empty batch");
        }
        // One owed response holds the batch open until every item is dispatched.
        let batch = Reply::Batch(Arc::new(Batch { out: self.out.clone(), state: Mutex::new((1, Vec::new())) }));
        for item in items {
            self.message(item, batch.clone());
        }
        batch.skip();
    }

    fn message(&mut self, message: Value, reply: Reply) {
        let id = message.get("id").cloned();
        match (message.get("method").and_then(Value::as_str).map(String::from), id) {
            (None, Some(id)) if message.get("result").is_some() || message.get("error").is_some() => self.response(&id, &message),
            (Some(method), Some(id)) => {
                reply.expect();
                self.request(&method, id, message, reply);
            }
            (Some(method), None) => self.notification(&method, &message["params"]),
            (None, id) => {
                reply.expect();
                reply.error(&id.unwrap_or(Value::Null), -32600, "invalid request: no method");
            }
        }
    }

    fn request(&mut self, method: &str, id: Value, message: Value, reply: Reply) {
        let params = &message["params"];
        match method {
            "initialize" => {
                self.version = negotiate(params["protocolVersion"].as_str());
                self.client_roots = params["capabilities"]["roots"].is_object();
                if self.client_roots {
                    self.roots = Roots::Pending { since: Instant::now(), asked: None };
                }
                reply.ok(&id, json!({
                    "protocolVersion": self.version,
                    "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false }, "logging": {} },
                    "serverInfo": { "name": "stack", "title": "Stack", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }));
            }
            "ping" => reply.ok(&id, json!({})),
            "tools/list" => reply.ok(&id, json!({ "tools": tools() })),
            "resources/list" => reply.ok(&id, json!({ "resources": [{
                "uri": GUIDE_URI, "name": "guide", "title": "Stack agent guide", "mimeType": "text/markdown",
                "description": "How a coding agent uses Stack: the tools, the loop, reading results, timeouts and arguments",
                "size": GUIDE.len(),
            }] })),
            "resources/templates/list" => reply.ok(&id, json!({ "resourceTemplates": [] })),
            "resources/read" => match params["uri"].as_str() {
                Some(GUIDE_URI) => reply.ok(&id, json!({ "contents": [{ "uri": GUIDE_URI, "mimeType": "text/markdown", "text": GUIDE }] })),
                uri => reply.send(json!({ "jsonrpc": "2.0", "id": id, "error": {
                    "code": -32002, "message": format!("resource not found: {}", uri.unwrap_or("(no uri)")), "data": { "uri": params["uri"] } } })),
            },
            "logging/setLevel" => match LEVELS.iter().position(|l| Some(*l) == params["level"].as_str()) {
                Some(level) => {
                    self.log_level = level;
                    reply.ok(&id, json!({}));
                }
                None => reply.error(&id, -32602, format!("invalid params: level must be one of {}", LEVELS.join(", "))),
            },
            "tools/call" => {
                // Calls after a held one are held too, so calls on one checkout keep their order.
                if matches!(self.roots, Roots::Pending { .. }) && (needs_roots(params) || !self.deferred.is_empty()) {
                    self.ask_roots_once();
                    self.deferred.push_back((message, reply));
                } else {
                    self.spawn_call(message, reply);
                }
            }
            _ => reply.error(&id, -32601, format!("method not found: {method}")),
        }
    }

    fn notification(&mut self, method: &str, params: &Value) {
        match method {
            "notifications/initialized" => {
                if self.client_roots {
                    self.ask_roots_once();
                }
            }
            "notifications/roots/list_changed" if self.client_roots => {
                // Calls arriving from now on wait for the new list, not the old one.
                self.roots = Roots::Pending { since: Instant::now(), asked: None };
                self.ask_roots_once();
            }
            "notifications/cancelled" => self.cancel(&params["requestId"]),
            _ => {}
        }
    }

    /// A response to a request this server sent: only `roots/list` is ever asked.
    fn response(&mut self, id: &Value, message: &Value) {
        let Roots::Pending { asked: Some(asked), .. } = &self.roots else { return };
        if id.as_str() != Some(asked.as_str()) {
            return;
        }
        if let Some(error) = message.get("error") {
            return self.roots_failed(&format!("the client answered roots/list with an error: {error}"));
        }
        let roots = message["result"]["roots"].as_array().cloned().unwrap_or_default();
        // A root that is not a local directory names no checkout.
        let paths: Vec<PathBuf> = roots.iter().filter_map(|r| r["uri"].as_str().and_then(file_uri_path)).collect();
        if paths.len() < roots.len() {
            self.log(3, json!({ "message": "ignored client roots that are not file:// URIs", "roots": roots }));
        }
        self.roots = Roots::Known(paths);
        self.run_deferred();
    }

    /// Ask the client for its roots, unless already asked.
    fn ask_roots_once(&mut self) {
        let Roots::Pending { asked, .. } = &mut self.roots else { return };
        if asked.is_some() {
            return;
        }
        self.requests_sent += 1;
        let id = format!("stack-roots-{}", self.requests_sent);
        *asked = Some(id.clone());
        self.out.send(&json!({ "jsonrpc": "2.0", "id": id, "method": "roots/list" }));
    }

    /// When held calls stop waiting for roots.
    fn roots_deadline(&self) -> Option<Instant> {
        match &self.roots {
            Roots::Pending { since, .. } if !self.deferred.is_empty() => Some(*since + ROOTS_WAIT),
            _ => None,
        }
    }

    /// Carry on without roots (`why` says why, in a warning to the client): calls that name no
    /// `dir` then fail with a usage error.
    fn roots_failed(&mut self, why: &str) {
        if matches!(self.roots, Roots::Pending { .. }) {
            self.log(3, json!({ "message": format!("{why}; calls must pass `dir`") }));
            self.roots = Roots::Known(Vec::new());
        }
        self.run_deferred();
    }

    fn run_deferred(&mut self) {
        while let Some((message, reply)) = self.deferred.pop_front() {
            self.spawn_call(message, reply);
        }
    }

    /// `notifications/message` at `level` (an index into [`LEVELS`]), if the client wants it.
    fn log(&self, level: usize, data: Value) {
        if level >= self.log_level {
            self.out.send(&json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": { "level": LEVELS[level], "logger": "stack", "data": data } }));
        }
    }

    fn roots_snapshot(&self) -> Option<Vec<PathBuf>> {
        match &self.roots {
            Roots::Known(roots) => Some(roots.clone()),
            Roots::Unsupported | Roots::Pending { .. } => None,
        }
    }

    /// Stop the call `request` names: one in flight ends as at its deadline, one still held
    /// for roots is dropped. Neither is answered.
    fn cancel(&mut self, request: &Value) {
        if request.is_null() {
            return;
        }
        if let Some(cancel) = self.inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&request.to_string()) {
            cancel.cancel();
            return;
        }
        if let Some(at) = self.deferred.iter().position(|(m, _)| m["id"] == *request) {
            if let Some((_, reply)) = self.deferred.remove(at) {
                reply.skip();
            }
        }
    }

    fn spawn_call(&mut self, message: Value, reply: Reply) {
        let id = message["id"].clone();
        let key = id.to_string();
        let cancel = Cancel::new();
        self.inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key.clone(), cancel.clone());
        let roots = self.roots_snapshot();
        let params = message["params"].clone();
        // Taken here, on the reading thread, so calls on one checkout keep their arrival order.
        let ticket = project_key(&params, roots.as_deref()).map(|project| self.queues.take(project));
        let (out, caller, inflight, queues, events) = (self.out.clone(), self.caller.clone(), self.inflight.clone(), self.queues.clone(), self.events.clone());
        self.workers.retain(|w| !w.is_finished());
        self.workers.push(std::thread::spawn(move || {
            let turn = Turn { queues: &queues, ticket };
            let outcome = turn.wait(&cancel).then(|| {
                let token = &params["_meta"]["progressToken"];
                let progress = (token.is_string() || token.is_i64() || token.is_u64())
                    .then(|| Progress::start(out.clone(), token.clone(), params["name"].as_str().unwrap_or_default()));
                let outcome = {
                    let _cancel = crate::process::cancellation_scope(Some(cancel.clone()));
                    let _progress = crate::progress::scope(progress.as_ref().map(Progress::sink));
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| caller(&params, roots.as_deref())))
                };
                // The heartbeat stops before the response: no progress follows it.
                drop(progress);
                outcome
            });
            drop(turn);
            inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&key);
            match outcome {
                // Cancelled: the client expects no response.
                _ if cancel.is_cancelled() => reply.skip(),
                Some(Ok(result)) => reply.ok(&id, result),
                Some(Err(_)) => reply.error(&id, -32603, "internal error: the call panicked"),
                None => reply.skip(),
            }
            if crate::process::passed_on().is_some() {
                let _ = events.send(Event::Stop);
            }
        }));
    }

    /// Calls still held for roots run without them; then every call in flight is answered.
    fn finish(&mut self) {
        if matches!(self.roots, Roots::Pending { .. }) {
            self.roots_failed("the client closed the connection before answering roots/list");
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Per-checkout lines of calls: each is served in the order its ticket was taken.
#[derive(Default)]
struct Queues {
    lines: Mutex<HashMap<PathBuf, Line>>,
    turn: Condvar,
}

#[derive(Default)]
struct Line {
    next: u64,
    serving: u64,
    /// Tickets given up before their turn.
    abandoned: BTreeSet<u64>,
}

struct Ticket {
    project: PathBuf,
    number: u64,
}

impl Queues {
    fn take(&self, project: PathBuf) -> Ticket {
        let mut lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let line = lines.entry(project.clone()).or_default();
        line.next += 1;
        Ticket { project, number: line.next - 1 }
    }

    /// Wait for the ticket's turn; `false` if cancelled first.
    fn wait(&self, ticket: &Ticket, cancel: &Cancel) -> bool {
        let mut lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if lines.get(&ticket.project).is_some_and(|l| l.serving == ticket.number) {
                return true;
            }
            if cancel.is_cancelled() {
                return false;
            }
            lines = self.turn.wait_timeout(lines, Duration::from_millis(50)).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
    }

    /// The ticket is done with, served or not: the next one in line may go.
    fn leave(&self, ticket: &Ticket) {
        let mut lines = self.lines.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(line) = lines.get_mut(&ticket.project) else { return };
        if line.serving == ticket.number {
            line.serving += 1;
            while line.abandoned.remove(&line.serving) {
                line.serving += 1;
            }
        } else {
            line.abandoned.insert(ticket.number);
        }
        if line.serving == line.next {
            lines.remove(&ticket.project);
        }
        self.turn.notify_all();
    }
}

/// A call's place in its checkout's line, given up when dropped (also on a panic).
struct Turn<'a> {
    queues: &'a Queues,
    ticket: Option<Ticket>,
}

impl Turn<'_> {
    fn wait(&self, cancel: &Cancel) -> bool {
        self.ticket.as_ref().map_or(!cancel.is_cancelled(), |t| self.queues.wait(t, cancel))
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = &self.ticket {
            self.queues.leave(ticket);
        }
    }
}

/// `notifications/progress` for one call: each lifecycle step, and a heartbeat until dropped.
struct Progress {
    shared: Arc<ProgressState>,
    stop: Option<mpsc::Sender<()>>,
    heartbeat: Option<JoinHandle<()>>,
}

struct ProgressState {
    out: Arc<Out>,
    token: Value,
    tool: String,
    started: Instant,
    /// Notifications sent (each one's `progress`), and the last step reported.
    state: Mutex<(u64, String)>,
}

impl ProgressState {
    fn notify(&self, step: Option<String>) {
        // Held while sending, so `progress` only ever increases on the wire.
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.0 += 1;
        let message = match step {
            Some(step) => {
                state.1 = step.clone();
                format!("{}: {step}", self.tool)
            }
            None => format!("{}: still working ({}s; last: {})", self.tool, self.started.elapsed().as_secs(), state.1),
        };
        self.out.send(&json!({ "jsonrpc": "2.0", "method": "notifications/progress",
            "params": { "progressToken": self.token, "progress": state.0, "message": message } }));
    }
}

impl Progress {
    fn start(out: Arc<Out>, token: Value, tool: &str) -> Self {
        let shared = Arc::new(ProgressState { out, token, tool: tool.to_string(), started: Instant::now(), state: Mutex::new((0, String::new())) });
        shared.notify(Some("started".into()));
        let (stop, stopped) = mpsc::channel::<()>();
        let beat = shared.clone();
        let heartbeat = std::thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(HEARTBEAT) {
                beat.notify(None);
            }
        });
        Progress { shared, stop: Some(stop), heartbeat: Some(heartbeat) }
    }

    fn sink(&self) -> crate::progress::Sink {
        let shared = self.shared.clone();
        Arc::new(move |step: &str, status: &str| shared.notify(Some(format!("{step} {status}"))))
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
    }
}

/// `file:///path` (or `file://localhost/path`) as a path; anything else is not a local root.
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(hi), Some(lo)) => {
                decoded.push((hi * 16 + lo) as u8);
                i += 3;
            }
            (b, _, _) => {
                decoded.push(b);
                i += 1;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8_lossy(&decoded).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Input fed line by line from a test, ending when the sender is dropped.
    struct Feed {
        lines: mpsc::Receiver<String>,
        pending: Vec<u8>,
    }

    impl Read for Feed {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.is_empty() {
                match self.lines.recv() {
                    Ok(line) => self.pending = format!("{line}\n").into_bytes(),
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.pending.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            Ok(n)
        }
    }

    /// Output delivered to the test message by message.
    struct Sink {
        buffer: Vec<u8>,
        messages: mpsc::Sender<Value>,
    }

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                let _ = self.messages.send(serde_json::from_slice(&line).expect("one JSON message per line"));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub(super) struct Session {
        input: Option<mpsc::Sender<String>>,
        output: mpsc::Receiver<Value>,
        server: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    }

    impl Session {
        pub(super) fn start(caller: Caller) -> Self {
            let (input, lines) = mpsc::channel();
            let (messages, output) = mpsc::channel();
            let feed = std::io::BufReader::new(Feed { lines, pending: Vec::new() });
            let server = std::thread::spawn(move || serve_with(feed, Sink { buffer: Vec::new(), messages }, caller));
            Session { input: Some(input), output, server: Some(server) }
        }

        pub(super) fn send(&self, message: Value) {
            self.input.as_ref().unwrap().send(message.to_string()).unwrap();
        }

        pub(super) fn recv(&self) -> Value {
            self.output.recv_timeout(Duration::from_secs(20)).expect("the server answers")
        }

        /// Messages up to and including the response to `id`.
        pub(super) fn until(&self, id: Value) -> Vec<Value> {
            let mut seen = Vec::new();
            loop {
                let message = self.recv();
                let done = message["id"] == id && message.get("method").is_none();
                seen.push(message);
                if done {
                    return seen;
                }
            }
        }

        /// Close input and collect what the server still writes before it exits.
        pub(super) fn close(mut self) -> Vec<Value> {
            drop(self.input.take());
            self.server.take().unwrap().join().unwrap().unwrap();
            self.output.try_iter().collect()
        }
    }

    fn tool_call(id: u64, name: &str, arguments: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } })
    }

    fn initialize(version: &str, capabilities: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": version, "capabilities": capabilities, "clientInfo": { "name": "test", "version": "0" } } })
    }

    /// A caller that sleeps for `ms` (beside the arguments), or until cancelled, recording when it ran.
    fn sleeper(log: Arc<Mutex<Vec<(String, Instant, Instant)>>>) -> Caller {
        Arc::new(move |params: &Value, _: Option<&[PathBuf]>| {
            let start = Instant::now();
            // Outside `arguments`, which are checked against the tool's schema.
            let ms = params["ms"].as_u64().unwrap_or(0);
            while start.elapsed() < Duration::from_millis(ms) && !crate::process::cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let dir = params["arguments"]["dir"].as_str().unwrap_or_default().to_string();
            log.lock().unwrap().push((dir, start, Instant::now()));
            json!({ "slept": ms })
        })
    }

    #[test]
    fn initialize_offers_the_requested_revision_or_the_newest() {
        let session = Session::start(Arc::new(call));
        for (asked, offered) in [("2025-11-25", "2025-11-25"), ("2025-06-18", "2025-06-18"), ("2024-11-05", "2024-11-05"), ("2099-01-01", "2025-11-25")] {
            session.send(initialize(asked, json!({})));
            let result = &session.recv()["result"];
            assert_eq!(result["protocolVersion"], offered, "{result}");
            assert!(result["capabilities"]["tools"].is_object() && result["capabilities"]["resources"].is_object() && result["capabilities"]["logging"].is_object());
            assert_eq!(result["serverInfo"]["name"], "stack");
        }
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        session.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "nope" }));
        let reply = session.recv();
        assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (json!(1), json!(-32601)), "the notification got no reply: {reply}");
        session.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "logging/setLevel", "params": { "level": "debug" } }));
        assert_eq!(session.recv()["result"], json!({}));
        session.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "logging/setLevel", "params": { "level": "loud" } }));
        assert_eq!(session.recv()["error"]["code"], -32602);
        session.send(json!({ "jsonrpc": "2.0", "id": 4 }));
        assert_eq!(session.recv()["error"]["code"], -32600);
        session.send(json!(5));
        assert_eq!(session.recv()["error"]["code"], -32600);
        session.input.as_ref().unwrap().send("{not json".into()).unwrap();
        let reply = session.recv();
        assert_eq!((reply["id"].clone(), reply["error"]["code"].clone()), (Value::Null, json!(-32700)));
        assert!(session.close().is_empty());
    }

    #[test]
    fn the_guide_is_a_resource() {
        let session = Session::start(Arc::new(call));
        session.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }));
        let listed = session.recv();
        assert_eq!(listed["result"]["resources"][0]["uri"], GUIDE_URI, "{listed}");
        session.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": { "uri": GUIDE_URI } }));
        let text = session.recv()["result"]["contents"][0]["text"].as_str().unwrap().to_string();
        assert!(text.starts_with("# Coding agents"), "{text}");
        assert!(text.contains("stack://guide"), "the guide names itself");
        session.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": { "uri": "stack://nope" } }));
        assert_eq!(session.recv()["error"]["code"], -32002);
        session.close();
    }

    #[test]
    fn batches_are_answered_together_only_in_the_revision_that_has_them() {
        let session = Session::start(Arc::new(call));
        session.send(initialize("2025-06-18", json!({})));
        session.recv();
        session.send(json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]));
        assert_eq!(session.recv()["error"]["code"], -32600);
        session.send(initialize("2025-03-26", json!({})));
        session.recv();
        session.send(json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": 2, "method": "tools/list" },
            { "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "stack_down", "arguments": {} } },
        ]));
        let batch = session.recv();
        let ids: BTreeSet<u64> = batch.as_array().unwrap().iter().map(|r| r["id"].as_u64().unwrap()).collect();
        assert_eq!(ids, [1, 2, 3].into());
        session.send(json!([]));
        assert_eq!(session.recv()["error"]["code"], -32600);
        session.close();
    }

    #[test]
    fn a_client_whose_roots_fail_is_warned_that_calls_must_pass_dir() {
        let session = Session::start(Arc::new(call));
        session.send(initialize("2025-06-18", json!({ "roots": {} })));
        session.recv();
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let ask = session.recv();
        session.send(json!({ "jsonrpc": "2.0", "id": ask["id"], "error": { "code": -32601, "message": "no roots here" } }));
        session.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }));
        let seen = session.until(json!(1));
        let warning = seen.iter().find(|m| m["method"] == "notifications/message").expect("a warning about roots");
        assert_eq!(warning["params"]["level"], "warning");
        assert!(warning["params"]["data"]["message"].as_str().unwrap().contains("calls must pass `dir`"), "{warning}");
        session.close();
    }

    #[test]
    fn ping_is_answered_while_a_call_runs() {
        let log = Arc::default();
        let session = Session::start(sleeper(Arc::clone(&log)));
        session.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "stack_status", "arguments": { "dir": "/" }, "ms": 1500 } }));
        session.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }));
        let first = session.recv();
        assert_eq!((first["id"].clone(), first["result"].clone()), (json!(2), json!({})), "ping waited for the call: {first}");
        assert_eq!(session.recv()["result"]["slept"], 1500);
        session.close();
    }

    #[test]
    fn calls_on_one_checkout_take_turns_and_others_run_alongside() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (a.path().canonicalize().unwrap(), b.path().canonicalize().unwrap());
        let log: Arc<Mutex<Vec<(String, Instant, Instant)>>> = Arc::default();
        let session = Session::start(sleeper(Arc::clone(&log)));
        for (id, dir) in [(1, &a), (2, &a), (3, &b)] {
            session.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "stack_status", "arguments": { "dir": dir }, "ms": 400 } }));
        }
        let answered: BTreeSet<u64> = session.close().iter().map(|m| m["id"].as_u64().unwrap()).collect();
        assert_eq!(answered, [1, 2, 3].into());
        let log = log.lock().unwrap();
        let runs = |dir: &PathBuf| log.iter().filter(|(d, _, _)| d == dir.to_str().unwrap()).collect::<Vec<_>>();
        let (in_a, in_b) = (runs(&a), runs(&b));
        assert_eq!((in_a.len(), in_b.len()), (2, 1));
        assert!(in_a[1].1 >= in_a[0].2, "two calls on one checkout overlapped");
        assert!(in_b[0].1 < in_a[0].2, "a call on another checkout waited for the first");
    }

    #[test]
    fn a_cancelled_call_stops_its_command_and_gets_no_response() {
        // The caller runs a command that would sleep for a minute, through the same deadline
        // machinery the lifecycle uses.
        let caller: Caller = Arc::new(|_: &Value, _: Option<&[PathBuf]>| {
            let start = Instant::now();
            let ran = crate::process::output(std::process::Command::new("sleep").arg("60"));
            json!({ "elapsed_ms": start.elapsed().as_millis() as u64, "success": ran.is_ok_and(|o| o.status.success()) })
        });
        let session = Session::start(caller);
        session.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "stack_status", "arguments": { "dir": "/" }, "_meta": { "progressToken": "p1" } } }));
        let started = session.recv();
        assert_eq!(started["method"], "notifications/progress", "{started}");
        let cancelled_at = Instant::now();
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 1, "reason": "test" } }));
        session.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }));
        assert_eq!(session.until(json!(2)).last().unwrap()["result"], json!({}));
        let rest = session.close();
        assert!(cancelled_at.elapsed() < Duration::from_secs(10), "the command was not stopped");
        assert!(rest.iter().all(|m| m["id"] != 1), "a cancelled call was answered: {rest:?}");
    }

    #[test]
    fn a_call_cancelled_while_waiting_its_turn_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let log: Arc<Mutex<Vec<(String, Instant, Instant)>>> = Arc::default();
        let session = Session::start(sleeper(Arc::clone(&log)));
        for (id, ms) in [(1, 600), (2, 0), (3, 0)] {
            session.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "stack_status", "arguments": { "dir": dir.path() }, "ms": ms } }));
        }
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 2 } }));
        let answered: Vec<u64> = session.close().iter().map(|m| m["id"].as_u64().unwrap()).collect();
        assert_eq!(answered, [1, 3], "the call after the cancelled one still got its turn");
        assert_eq!(log.lock().unwrap().len(), 2);
    }

    #[test]
    fn progress_reports_each_step_before_the_response() {
        let caller: Caller = Arc::new(|_: &Value, _: Option<&[PathBuf]>| {
            crate::progress::notify(crate::progress::Event { step: "gc", phase: crate::progress::Phase::Ok, code: None, detail: None });
            crate::progress::started("install");
            crate::progress::notify(crate::progress::Event { step: "install", phase: crate::progress::Phase::Ok, code: None, detail: None });
            json!({ "done": true })
        });
        let session = Session::start(caller);
        session.send(json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "stack_up", "arguments": { "dir": "/" }, "_meta": { "progressToken": 42 } } }));
        let seen = session.until(json!(7));
        let progress: Vec<&Value> = seen.iter().filter(|m| m["method"] == "notifications/progress").collect();
        let messages: Vec<&str> = progress.iter().map(|m| m["params"]["message"].as_str().unwrap()).collect();
        assert_eq!(messages, ["stack_up: started", "stack_up: gc ok", "stack_up: install started", "stack_up: install ok"]);
        assert!(progress.iter().all(|m| m["params"]["progressToken"] == 42));
        let counts: Vec<u64> = progress.iter().map(|m| m["params"]["progress"].as_u64().unwrap()).collect();
        assert!(counts.windows(2).all(|w| w[0] < w[1]), "{counts:?}");
        assert_eq!(seen.last().unwrap()["result"]["done"], true);
        // Without a token, nothing but the response.
        session.send(tool_call(8, "stack_up", json!({ "dir": "/" })));
        assert_eq!(session.until(json!(8)).len(), 1);
        session.close();
    }

    #[test]
    fn a_call_without_dir_uses_the_clients_only_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let session = Session::start(Arc::new(call));
        session.send(initialize("2025-06-18", json!({ "roots": { "listChanged": true } })));
        session.recv();
        // Held until the roots are known.
        session.send(tool_call(1, "stack_status", json!({})));
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let ask = session.recv();
        assert_eq!(ask["method"], "roots/list", "{ask}");
        let uri = format!("file://{}", root.to_str().unwrap().replace(' ', "%20"));
        session.send(json!({ "jsonrpc": "2.0", "id": ask["id"], "result": { "roots": [{ "uri": uri, "name": "checkout" }] } }));
        let reply = session.recv();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["structuredContent"]["project"], json!(root), "{reply}");
        // Two roots: no guess.
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed" }));
        let ask = session.recv();
        assert_eq!(ask["method"], "roots/list");
        session.send(tool_call(2, "stack_status", json!({})));
        session.send(json!({ "jsonrpc": "2.0", "id": ask["id"], "result": { "roots": [{ "uri": "file:///a" }, { "uri": "file:///b" }] } }));
        let error = &session.recv()["result"]["structuredContent"]["error"];
        assert_eq!(error["code"], "usage");
        assert!(error["hint"].as_str().unwrap().contains("absolute path"), "{error}");
        session.close();
    }

    #[test]
    fn a_client_that_never_answers_roots_gets_a_usage_error_not_the_cwd() {
        let session = Session::start(Arc::new(call));
        session.send(initialize("2025-06-18", json!({ "roots": {} })));
        session.recv();
        session.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        assert_eq!(session.recv()["method"], "roots/list");
        let asked = Instant::now();
        session.send(tool_call(1, "stack_down", json!({})));
        let reply = session.until(json!(1)).pop().unwrap();
        assert!(asked.elapsed() >= ROOTS_WAIT - Duration::from_millis(500), "answered before the wait for roots ended: {reply}");
        assert_eq!(reply["result"]["structuredContent"]["error"]["code"], "usage", "{reply}");
        session.close();
    }

    #[test]
    fn without_roots_a_call_must_name_its_checkout() {
        let session = Session::start(Arc::new(call));
        session.send(initialize("2025-06-18", json!({})));
        session.recv();
        session.send(tool_call(1, "stack_down", json!({})));
        let error = session.recv()["result"]["structuredContent"]["error"].clone();
        assert_eq!(error["code"], "usage");
        assert!(error["hint"].as_str().unwrap().contains("pass `dir` as the absolute path"), "{error}");
        session.send(tool_call(2, "stack_down", json!({ "dir": "relative/checkout" })));
        let error = session.recv()["result"]["structuredContent"]["error"].clone();
        assert!(error["message"].as_str().unwrap().contains("must be an absolute path"), "{error}");
        session.close();
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(file_uri_path("file:///home/a%20b/c"), Some(PathBuf::from("/home/a b/c")));
        assert_eq!(file_uri_path("file://localhost/x"), Some(PathBuf::from("/x")));
        assert_eq!(file_uri_path("https://example.com/x"), None);
        assert_eq!(file_uri_path("file://host/x"), None);
    }
}

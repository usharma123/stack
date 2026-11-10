//! The MCP server loop: JSON-RPC over stdio.
//!
//! Stdin is read on its own thread, so the loop can wait for an answer from the client with a
//! bound. When a call names no `dir`, its checkout is the client's single MCP root: the server
//! asks for `roots/list` once the client is initialized (and again on
//! `notifications/roots/list_changed`) and holds calls that need the answer until it arrives,
//! or [`ROOTS_WAIT`] passes. It never falls back to its own working directory.

use super::{call, needs_roots, negotiate, tools, INSTRUCTIONS};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long calls that need the client's roots wait for its `roots/list` answer.
const ROOTS_WAIT: Duration = Duration::from_secs(5);

/// Runs one `tools/call`: its params and the client's roots (`None` when it offers none).
pub(super) type Caller = Arc<dyn Fn(&Value, Option<&[PathBuf]>) -> Value + Send + Sync>;

/// Serve until `input` ends, then answer the calls still held.
pub fn serve(input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> std::io::Result<()> {
    serve_with(input, output, Arc::new(call))
}

pub(super) fn serve_with(input: impl BufRead + Send + 'static, output: impl Write + Send + 'static, caller: Caller) -> std::io::Result<()> {
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            let end = line.is_err();
            if lines.send(line).is_err() || end {
                break;
            }
        }
    });
    let mut server = Server::new(Arc::new(Out(Mutex::new(Box::new(output)))), caller);
    let result = loop {
        let next = match server.roots_deadline() {
            Some(at) => received.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => received.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Ok(line)) => server.line(&line),
            Ok(Err(e)) => break Err(e),
            Err(RecvTimeoutError::Timeout) => server.roots_failed(),
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
        }
        // A signal that would have ended the server was passed on to the command a call ran
        // instead; now that it was answered, the server ends as the signal asked.
        if crate::process::passed_on().is_some() {
            server.deferred.clear();
            break Ok(());
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

    fn ok(&self, id: &Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn error(&self, id: &Value, code: i64, message: impl Into<String>) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } }));
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
    client_roots: bool,
    roots: Roots,
    requests_sent: u64,
    /// Calls waiting for the client's roots, in arrival order.
    deferred: VecDeque<Value>,
}

impl Server {
    fn new(out: Arc<Out>, caller: Caller) -> Self {
        Server { out, caller, client_roots: false, roots: Roots::Unsupported, requests_sent: 0, deferred: VecDeque::new() }
    }

    fn line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        match serde_json::from_str::<Value>(line) {
            Err(e) => self.out.error(&Value::Null, -32700, e.to_string()),
            Ok(message) => self.message(message),
        }
    }

    fn message(&mut self, message: Value) {
        let id = message.get("id").cloned();
        match (message.get("method").and_then(Value::as_str).map(String::from), id) {
            (None, Some(id)) if message.get("result").is_some() || message.get("error").is_some() => self.response(&id, &message),
            (method, Some(id)) => self.request(method.as_deref().unwrap_or_default(), id, message),
            (Some(method), None) => self.notification(&method),
            // Neither a request nor a notification: nothing to answer.
            (None, None) => {}
        }
    }

    fn request(&mut self, method: &str, id: Value, message: Value) {
        let params = &message["params"];
        match method {
            "initialize" => {
                self.client_roots = params["capabilities"]["roots"].is_object();
                if self.client_roots {
                    self.roots = Roots::Pending { since: Instant::now(), asked: None };
                }
                self.out.ok(&id, json!({
                    "protocolVersion": negotiate(params["protocolVersion"].as_str()),
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "stack", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }));
            }
            "ping" => self.out.ok(&id, json!({})),
            "tools/list" => self.out.ok(&id, json!({ "tools": tools() })),
            "tools/call" => {
                // Calls after a held one are held too, so calls keep their order.
                if matches!(self.roots, Roots::Pending { .. }) && (needs_roots(params) || !self.deferred.is_empty()) {
                    self.ask_roots_once();
                    self.deferred.push_back(message);
                } else {
                    self.run_call(&message);
                }
            }
            _ => self.out.error(&id, -32601, format!("method not found: {method}")),
        }
    }

    fn notification(&mut self, method: &str) {
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
            _ => {}
        }
    }

    /// A response to a request this server sent: only `roots/list` is ever asked.
    fn response(&mut self, id: &Value, message: &Value) {
        let Roots::Pending { asked: Some(asked), .. } = &self.roots else { return };
        if id.as_str() != Some(asked.as_str()) {
            return;
        }
        if message.get("error").is_some() {
            return self.roots_failed();
        }
        let roots = message["result"]["roots"].as_array().cloned().unwrap_or_default();
        // A root that is not a local directory names no checkout.
        let paths: Vec<PathBuf> = roots.iter().filter_map(|r| r["uri"].as_str().and_then(file_uri_path)).collect();
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

    /// Carry on without roots (the client did not answer `roots/list` in time, answered it with
    /// an error, or closed the connection): calls that name no `dir` then fail with a usage error.
    fn roots_failed(&mut self) {
        if matches!(self.roots, Roots::Pending { .. }) {
            self.roots = Roots::Known(Vec::new());
        }
        self.run_deferred();
    }

    fn run_deferred(&mut self) {
        while let Some(message) = self.deferred.pop_front() {
            self.run_call(&message);
        }
    }

    fn roots_snapshot(&self) -> Option<Vec<PathBuf>> {
        match &self.roots {
            Roots::Known(roots) => Some(roots.clone()),
            Roots::Unsupported | Roots::Pending { .. } => None,
        }
    }

    fn run_call(&mut self, message: &Value) {
        let roots = self.roots_snapshot();
        let result = (self.caller)(&message["params"], roots.as_deref());
        self.out.ok(&message["id"], result);
    }

    /// Calls still held for roots run without them.
    fn finish(&mut self) {
        if matches!(self.roots, Roots::Pending { .. }) {
            self.roots_failed();
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
        let reply = session.recv();
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

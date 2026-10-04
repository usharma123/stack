//! Network trust tests use loopback fixtures and dummy credentials only.
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tempfile::TempDir;

#[derive(Clone, Debug)]
struct Request {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
}
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Response {
    fn new(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }
    fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }
}
struct Server {
    origin: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&Request) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let done = stop.clone();
        let seen = requests.clone();
        let handle = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Accepted sockets inherit non-blocking mode on BSD and macOS.
                        stream.set_nonblocking(false).unwrap();
                        let request = read_request(&mut stream);
                        let response = handler(&request);
                        seen.lock().unwrap().push(request.clone());
                        let mut text = format!(
                            "HTTP/1.1 {} Fixture\r\nConnection: close\r\nContent-Length: {}\r\n",
                            response.status,
                            response.body.len()
                        );
                        for (key, value) in response.headers {
                            text.push_str(&format!("{key}: {value}\r\n"));
                        }
                        text.push_str("\r\n");
                        if request.method != "HEAD" {
                            text.push_str(&response.body);
                        }
                        stream.write_all(text.as_bytes()).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        });
        Self {
            origin,
            requests,
            stop,
            handle: Some(handle),
        }
    }
    fn seen(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 65536);
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap().split_whitespace();
    let method = first.next().unwrap().to_string();
    let path = first.next().unwrap().to_string();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| {
            line.split_once(':')
                .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
        })
        .collect();
    let length: usize = headers
        .get("content-length")
        .map(|n| n.parse().unwrap())
        .unwrap_or(0);
    assert!(length < 1024 * 1024);
    stream.read_exact(&mut vec![0; length]).unwrap();
    Request {
        method,
        path,
        headers,
    }
}
fn command(dir: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
    command
        .args(["-C", dir.path().to_str().unwrap(), "--json"])
        .env("STACK_CACHE_DIR", dir.path().join("cache"))
        .env("STACK_STATE_DIR", dir.path().join("state"))
        .env("STACK_OCI_USERNAME", "review-user")
        .env("STACK_OCI_PASSWORD", "dummy-review-password")
        .env_remove("STACK_OCI_AUTH_REALMS")
        .env_remove("STACK_OCI_PLAIN_HTTP");
    command
}
fn compile(dir: &TempDir, registry: &Server, approved: Option<&str>) -> Value {
    std::fs::write(
        dir.path().join("stack.toml"),
        format!(
            "[[use]]\nbundle='oci:{}/bundle:1'\n",
            registry.origin.trim_start_matches("http://")
        ),
    )
    .unwrap();
    let mut command = command(dir);
    if let Some(origin) = approved {
        command.env("STACK_OCI_AUTH_REALMS", origin);
    }
    let output = command.arg("compile").output().unwrap();
    assert!(!output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn registry_cannot_send_credentials_to_an_unapproved_token_origin() {
    let token = Server::new(|_| Response::new(200, "{\"token\":\"fixture-token\"}"));
    let realm = format!("{}/token", token.origin);
    let registry = Server::new(move |_| {
        Response::new(401, "").header("WWW-Authenticate", &format!("Bearer realm=\"{realm}\""))
    });
    let result = compile(&tempfile::tempdir().unwrap(), &registry, None);
    assert_eq!(result["error"]["code"], "oci_auth_untrusted");
    assert!(
        token.seen().is_empty(),
        "unapproved token server was contacted"
    );
}

#[test]
fn approved_token_origin_receives_credentials_and_token_is_scoped_to_the_registry() {
    let token = Server::new(|_| Response::new(200, "{\"token\":\"fixture-token\"}"));
    let realm = format!("{}/token", token.origin);
    let registry = Server::new(move |request| {
        if request.headers.contains_key("authorization") {
            Response::new(404, "")
        } else {
            Response::new(401, "").header("WWW-Authenticate", &format!("Bearer realm=\"{realm}\""))
        }
    });
    let result = compile(
        &tempfile::tempdir().unwrap(),
        &registry,
        Some(&token.origin),
    );
    assert_eq!(result["error"]["code"], "oci_not_found");
    assert!(token.seen()[0].headers["authorization"].starts_with("Basic "));
    assert_eq!(
        registry.seen()[1].headers["authorization"],
        "Bearer fixture-token"
    );
}

#[test]
fn approved_token_service_cannot_redirect_credentials_to_another_server() {
    let victim = Server::new(|_| Response::new(200, "{\"token\":\"leaked\"}"));
    let location = format!("{}/victim", victim.origin);
    let token = Server::new(move |_| Response::new(302, "").header("Location", &location));
    let realm = format!("{}/token", token.origin);
    let registry = Server::new(move |_| {
        Response::new(401, "").header("WWW-Authenticate", &format!("Bearer realm=\"{realm}\""))
    });
    let result = compile(
        &tempfile::tempdir().unwrap(),
        &registry,
        Some(&token.origin),
    );
    assert_eq!(result["error"]["code"], "oci_auth_untrusted");
    assert!(victim.seen().is_empty());
}

#[test]
fn absolute_upload_locations_do_not_receive_registry_authorization() {
    let upload = Server::new(|_| Response::new(201, ""));
    let location = format!("{}/upload", upload.origin);
    let registry = Server::new(move |request| {
        if !request.headers.contains_key("authorization") {
            return Response::new(401, "").header("WWW-Authenticate", "Basic realm=\"registry\"");
        }
        if request.method == "HEAD" {
            Response::new(404, "")
        } else if request.method == "POST" {
            Response::new(202, "").header("Location", &location)
        } else {
            Response::new(201, "")
        }
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bundle.toml"), "[bundle]\nname='test'\n").unwrap();
    let target = format!(
        "oci:{}/bundle:1",
        registry.origin.trim_start_matches("http://")
    );
    let output = command(&dir)
        .args(["publish", dir.path().to_str().unwrap(), &target])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let requests = upload.seen();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request.method, "PUT");
        assert!(!request.headers.contains_key("authorization"));
        assert!(request.path.contains("digest=sha256"));
    }
}

/// Each case runs in its own process so the opt-in never leaks between tests.
fn compile_with_plain_http(
    registry: &str,
    plain_http: Option<&str>,
    approved: Option<&str>,
) -> Value {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("stack.toml"),
        format!("[[use]]\nbundle='oci:{registry}/bundle:1'\n"),
    )
    .unwrap();
    let mut command = command(&dir);
    if let Some(value) = plain_http {
        command.env("STACK_OCI_PLAIN_HTTP", value);
    }
    if let Some(origin) = approved {
        command.env("STACK_OCI_AUTH_REALMS", origin);
    }
    let output = command.arg("compile").output().unwrap();
    assert!(!output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

const NOT_OPTED_IN: [Option<&str>; 5] = [None, Some(""), Some("0"), Some("false"), Some("yes")];

#[test]
fn only_an_explicit_opt_in_uses_plain_http_for_remote_registries() {
    // `.invalid` never resolves, so the error reveals the scheme without any network traffic.
    for value in NOT_OPTED_IN {
        let result = compile_with_plain_http("registry.invalid:5000", value, None);
        let message = result["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("https://registry.invalid:5000/"),
            "{value:?}: {message}"
        );
    }
    let result = compile_with_plain_http("registry.invalid:5000", Some("1"), None);
    let message = result["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("http://registry.invalid:5000/"),
        "{message}"
    );
}

#[test]
fn redirects_and_token_realms_to_remote_plain_http_need_the_explicit_opt_in() {
    let redirect = Server::new(|_| {
        Response::new(307, "").header("Location", "http://registry.invalid/v2/bundle/manifests/1")
    });
    let realm = Server::new(|_| {
        Response::new(401, "").header(
            "WWW-Authenticate",
            "Bearer realm=\"http://auth.invalid/token\"",
        )
    });
    let registries = [
        redirect.origin.trim_start_matches("http://").to_string(),
        realm.origin.trim_start_matches("http://").to_string(),
    ];
    for registry in &registries {
        for value in NOT_OPTED_IN {
            let result = compile_with_plain_http(registry, value, Some("http://auth.invalid"));
            assert_eq!(
                result["error"]["code"], "oci_auth_untrusted",
                "{registry} {value:?}: {result}"
            );
        }
        let result = compile_with_plain_http(registry, Some("1"), Some("http://auth.invalid"));
        assert_eq!(
            result["error"]["code"], "oci_unreachable",
            "{registry}: {result}"
        );
    }
}

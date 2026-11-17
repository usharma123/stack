use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

/// What a scripted server does once a client connected.
#[derive(Clone)]
enum Step {
    /// Read one Postgres startup packet (length-prefixed, no type byte).
    ReadStartup,
    /// Read one Postgres message (type byte, length, body).
    ReadMessage,
    /// Read one Redis command (`*n` and its bulk strings).
    ReadCommand,
    Write(Vec<u8>),
    /// Write each byte, pausing between them.
    Trickle(Vec<u8>, Duration),
    Sleep(Duration),
}

/// A server on a free loopback port that plays `script` to the first client, then closes.
fn scripted(script: Vec<Step>) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let exact = |c: &mut TcpStream, n: usize| {
            let mut b = vec![0u8; n];
            c.read_exact(&mut b).map(|_| b)
        };
        for step in script {
            let ok = match step {
                Step::ReadStartup => exact(&mut c, 4)
                    .and_then(|len| exact(&mut c, u32::from_be_bytes(len.try_into().unwrap()) as usize - 4))
                    .is_ok(),
                Step::ReadMessage => exact(&mut c, 5)
                    .and_then(|h| exact(&mut c, u32::from_be_bytes(h[1..5].try_into().unwrap()) as usize - 4))
                    .is_ok(),
                Step::ReadCommand => {
                    let line = |c: &mut TcpStream| {
                        let mut l = Vec::new();
                        while !l.ends_with(b"\r\n") {
                            l.extend(exact(c, 1)?);
                        }
                        Ok::<_, std::io::Error>(String::from_utf8_lossy(&l[1..l.len() - 2]).into_owned())
                    };
                    match line(&mut c).map(|n| n.parse::<usize>().unwrap()) {
                        Ok(n) => (0..n).all(|_| line(&mut c).is_ok_and(|size| exact(&mut c, size.parse::<usize>().unwrap() + 2).is_ok())),
                        Err(_) => false,
                    }
                }
                Step::Write(bytes) => c.write_all(&bytes).is_ok(),
                Step::Trickle(bytes, pause) => bytes.iter().all(|b| {
                    std::thread::sleep(pause);
                    c.write_all(&[*b]).is_ok()
                }),
                Step::Sleep(d) => {
                    std::thread::sleep(d);
                    true
                }
            };
            if !ok {
                break;
            }
        }
    });
    (addr, handle)
}

fn msg(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![kind];
    m.extend(((body.len() + 4) as u32).to_be_bytes());
    m.extend(body);
    m
}

fn auth(code: u32) -> Vec<u8> {
    msg(b'R', &code.to_be_bytes())
}

fn ready() -> Vec<u8> {
    msg(b'Z', b"I")
}

fn row(values: &[Option<&[u8]>]) -> Vec<u8> {
    let mut body = (values.len() as i16).to_be_bytes().to_vec();
    for v in values {
        match v {
            Some(v) => {
                body.extend((v.len() as i32).to_be_bytes());
                body.extend(*v);
            }
            None => body.extend((-1i32).to_be_bytes()),
        }
    }
    msg(b'D', &body)
}

fn error(message: &str) -> Vec<u8> {
    error_coded("28000", message)
}

fn error_coded(code: &str, message: &str) -> Vec<u8> {
    msg(b'E', format!("SFATAL\0C{code}\0M{message}\0\0").as_bytes())
}

/// A server that authenticates without a password and answers the query with `rows`.
/// A RowDescription of `columns` text columns, as a server sends it.
fn description(columns: i16) -> Vec<u8> {
    let mut body = columns.to_be_bytes().to_vec();
    for _ in 0..columns {
        body.extend(b"current_setting\0");
        body.extend(0u32.to_be_bytes()); // table
        body.extend(0i16.to_be_bytes()); // column
        body.extend(25u32.to_be_bytes()); // text
        body.extend((-1i16).to_be_bytes()); // length
        body.extend((-1i32).to_be_bytes()); // modifier
        body.extend(0i16.to_be_bytes()); // text format
    }
    msg(b'T', &body)
}

fn complete() -> Vec<u8> {
    msg(b'C', b"SELECT 1\0")
}

/// A server that authenticates and then answers the query with exactly `answer`.
fn replying(answer: Vec<u8>) -> Vec<Step> {
    let mut connected = auth(0);
    connected.extend(ready());
    vec![Step::ReadStartup, Step::Write(connected), Step::ReadMessage, Step::Write(answer), Step::ReadMessage]
}

fn answering(rows: Vec<Vec<u8>>) -> Vec<Step> {
    let mut answer = description(1);
    for r in rows {
        answer.extend(r);
    }
    answer.extend(msg(b'C', b"SELECT 1\0"));
    answer.extend(ready());
    let mut connected = auth(0);
    connected.extend(msg(b'S', b"server_encoding\0UTF8\0"));
    connected.extend(msg(b'K', &[0, 0, 0, 1, 0, 0, 0, 2]));
    connected.extend(ready());
    vec![Step::ReadStartup, Step::Write(connected), Step::ReadMessage, Step::Write(answer), Step::ReadMessage]
}

fn pg_url(addr: SocketAddr) -> String {
    format!("postgresql://postgres@{addr}/postgres")
}

const QUERY: &str = "select current_setting('data_directory')";

fn ask_pg(script: Vec<Step>) -> Asked {
    let (addr, server) = scripted(script);
    let asked = postgres_seeing(&pg_url(addr), &[], QUERY);
    server.join().unwrap();
    asked
}

fn ask_redis(script: Vec<Step>) -> Asked {
    let (addr, server) = scripted(script);
    let asked = redis_dir_seeing(&format!("redis://{addr}"), &[]);
    server.join().unwrap();
    asked
}

fn failed(asked: &Asked, says: &str) {
    match asked {
        Asked::Failed(why) => assert!(why.contains(says), "{why:?} does not say {says:?}"),
        other => panic!("expected a failure saying {says:?}, got {other:?}"),
    }
}

/// A deadline of `ms` for what runs while the scope lives (as a caller's would bound it).
fn within(ms: u64) -> crate::process::DeadlineScope {
    crate::process::deadline_scope(Some(Instant::now() + Duration::from_millis(ms)))
}

#[test]
fn only_urls_the_client_would_connect_to_exactly_are_read() {
    let target = postgres_target("postgresql://postgres@127.0.0.1:5432/postgres").unwrap();
    assert_eq!(target, PostgresTarget { addr: "127.0.0.1:5432".parse().unwrap(), user: "postgres".into(), database: "postgres".into() });
    let target = postgres_target("postgres://us%40er@[::1]:6000/my%20db").unwrap();
    assert_eq!((target.addr, target.user.as_str(), target.database.as_str()), ("[::1]:6000".parse().unwrap(), "us@er", "my db"));
    for url in [
        "postgresql://postgres@localhost:5432/postgres",       // a host name: resolution is libpq's
        "postgresql://postgres:secret@127.0.0.1:5432/postgres", // a password
        "postgresql://postgres@127.0.0.1:5432/postgres?sslmode=require",
        "postgresql://postgres@127.0.0.1:5432/postgres#x",
        "postgresql://127.0.0.1:5432/postgres",       // no user: PGUSER or the OS user
        "postgresql://postgres@127.0.0.1:5432/",      // no database: PGDATABASE or the user
        "postgresql://postgres@127.0.0.1:5432",       // no database either
        "postgresql://postgres@127.0.0.1/postgres",   // no port: PGPORT or 5432
        "postgresql://postgres@127.0.0.1:0/postgres",
        "postgresql://postgres@127.0.0.1:1,127.0.0.1:2/postgres",
        "postgresql://postgres@%2Ftmp/postgres",      // a socket directory
        "postgresql://postgres@127.0.0.1:5432/post%zzgres",
        "host=127.0.0.1 port=5432 user=postgres",     // a keyword string
        "postgresql://unverified.stack.invalid:5432/postgres",
        "postgresql://postgres@::1:5432/postgres",    // IPv6 without brackets: psql refuses it
        "",
    ] {
        assert_eq!(postgres_target(url), None, "{url}");
        assert!(matches!(postgres_seeing(url, &[], QUERY), Asked::Fallback(_)), "{url}");
    }
    assert_eq!(redis_target("redis://127.0.0.1:6379"), Some("127.0.0.1:6379".parse().unwrap()));
    assert_eq!(redis_target("redis://127.0.0.1:6379/"), Some("127.0.0.1:6379".parse().unwrap()));
    for url in [
        "redis://localhost:6379",
        "redis://:secret@127.0.0.1:6379",
        "redis://user:secret@127.0.0.1:6379",
        "redis://127.0.0.1:6379/2", // redis-cli selects the database first
        "redis://127.0.0.1:6379?x=1",
        "redis://127.0.0.1",
        "redis://::1:6379",
        "rediss://127.0.0.1:6379",
        "unix:///tmp/redis.sock",
    ] {
        assert_eq!(redis_target(url), None, "{url}");
        assert!(matches!(redis_dir_seeing(url, &[]), Asked::Fallback(_)), "{url}");
    }
}

#[test]
fn client_variables_that_could_change_the_connection_leave_the_question_to_the_client() {
    let url = "postgresql://postgres@127.0.0.1:1/postgres";
    for var in ["PGHOSTADDR", "PGSSLMODE", "PGPASSWORD", "PGPASSFILE", "PGOPTIONS", "PGSERVICE", "PGGSSENCMODE", "PGAPPNAME"] {
        assert_eq!(postgres_seeing(url, &[var.to_string()], QUERY), Asked::Fallback(format!("{var} is set")));
    }
    // The URL wins over these four, as for libpq; nothing listens on port 1, so it fails.
    let overridden: Vec<String> = ["PGHOST", "PGPORT", "PGUSER", "PGDATABASE", "PGCONNECT_TIMEOUT", "PATH"].map(String::from).to_vec();
    assert!(matches!(postgres_seeing(url, &overridden, QUERY), Asked::Failed(_)));
    assert_eq!(redis_dir_seeing("redis://127.0.0.1:1", &["REDISCLI_AUTH".into()]), Asked::Fallback("REDISCLI_AUTH is set".into()));
    // What the provider environment adds counts as much as what Stack inherited.
    let env = IndexMap::from([("PGSSLMODE".to_string(), "require".to_string())]);
    assert_eq!(postgres(url, &env, QUERY), Asked::Fallback("PGSSLMODE is set".into()));
}

#[test]
fn postgres_answers_are_printed_as_psql_prints_them() {
    assert_eq!(ask_pg(answering(vec![row(&[Some(b"/data/pg")])])), Asked::Answer("/data/pg\n".into()));
    // Several rows, one per line, NULL as nothing, as `psql -At`.
    let rows = vec![row(&[Some(b"a")]), row(&[None]), row(&[Some(b"b")])];
    assert_eq!(ask_pg(answering(rows)), Asked::Answer("a\n\nb\n".into()));
    assert_eq!(ask_pg(answering(Vec::new())), Asked::Answer(String::new()));
}

#[test]
fn any_other_refusal_at_startup_is_left_to_psql_and_never_passes() {
    // No such role or database, or a pg_hba.conf line that wants the encryption psql tries.
    for (code, message) in [("28000", "no pg_hba.conf entry for host \"127.0.0.1\", no encryption"), ("3D000", "database \"x\" does not exist")] {
        let asked = ask_pg(vec![Step::ReadStartup, Step::Write(error_coded(code, message))]);
        assert_eq!(asked, Asked::Fallback(format!("the server refused a plain connection: {message}")));
    }
}

#[test]
fn an_authentication_request_is_left_to_psql_and_never_passes() {
    for (code, name) in [(3, "cleartext"), (5, "md5"), (10, "SASL"), (7, "GSS"), (2, "Kerberos")] {
        let mut request = auth(code);
        if code == 5 {
            request = msg(b'R', &[0, 0, 0, 5, 1, 2, 3, 4]);
        }
        let asked = ask_pg(vec![Step::ReadStartup, Step::Write(request)]);
        assert_eq!(asked, Asked::Fallback(format!("the server asks for authentication (method {code})")), "{name}");
    }
}

#[test]
fn every_postgres_failure_fails_closed() {
    let connected = || {
        let mut c = auth(0);
        c.extend(ready());
        c
    };
    // Refused at startup while starting up or shutting down, as psql would be, or an error
    // for the query.
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(error_coded("57P03", "the database system is starting up"))]), "starting up");
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(connected()), Step::ReadMessage, Step::Write([error("no"), ready()].concat())]), "answered an error: no");
    // Something that is not Postgres.
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(b"HTTP/1.1 400 Bad Request\r\n\r\n".to_vec())]), "message");
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(b"-ERR unknown command\r\n".to_vec())]), "message");
    // Ready before authenticating, parameters before authenticating, two AuthenticationOks.
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(ready())]), "unexpected message ('Z')");
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(msg(b'S', b"a\0b\0"))]), "unexpected message ('S')");
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write([auth(0), auth(0)].concat())]), "malformed authentication");
    // An unknown message in the answer, a malformed or non-UTF-8 row.
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(connected()), Step::ReadMessage, Step::Write(msg(b'W', b"\0\0\0"))]), "unexpected message ('W')");
    failed(&ask_pg(replying([description(1), msg(b'D', &[0, 1, 0, 0, 0, 9, b'x'])].concat())), "malformed row");
    failed(&ask_pg(replying([description(1), msg(b'D', &[0, 1, 0, 0, 0, 1, b'x', b'y'])].concat())), "malformed row");
    // A partial reply: the connection ends inside a message, or before the answer is complete.
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(auth(0)[..6].to_vec())]), "closed the connection");
    let mut cut = replying([description(1), row(&[Some(b"/data/pg")])].concat());
    cut.pop(); // closes instead of waiting for the client to finish
    failed(&ask_pg(cut), "closed the connection");
    // No answer at all.
    failed(&ask_pg(vec![Step::ReadStartup]), "closed the connection");
    // Oversized: one message beyond what a probe reads, or more messages than an answer has.
    let mut huge = vec![b'N'];
    huge.extend((10u32 * 1024 * 1024).to_be_bytes());
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(huge)]), "message of");
    let big = row(&[Some(&[b'x'; MESSAGE_LIMIT - 64])]);
    failed(&ask_pg(answering(vec![big.clone(), big])), "more than");
    let notices: Vec<u8> = (0..MESSAGE_COUNT_LIMIT + 1).flat_map(|_| notice_msg()).collect();
    failed(&ask_pg(vec![Step::ReadStartup, Step::Write(notices)]), "more than 256 messages");
    // Slow: nothing within the deadline, or bytes trickling past it.
    {
        let _deadline = within(300);
        failed(&ask_pg(vec![Step::ReadStartup, Step::Sleep(Duration::from_millis(800))]), "no complete answer");
    }
    {
        let _deadline = within(300);
        failed(&ask_pg(vec![Step::ReadStartup, Step::Trickle(connected(), Duration::from_millis(60))]), "no complete answer");
    }
    // Nothing listening.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    failed(&postgres_seeing(&pg_url(closed), &[], QUERY), "cannot connect");
}

#[test]
fn redis_answers_are_printed_as_redis_cli_prints_them() {
    let reply = b"*2\r\n$3\r\ndir\r\n$9\r\n/data/rds\r\n".to_vec();
    assert_eq!(ask_redis(vec![Step::ReadCommand, Step::Write(reply)]), Asked::Answer("dir\n/data/rds\n".into()));
    assert_eq!(ask_redis(vec![Step::ReadCommand, Step::Write(b"*0\r\n".to_vec())]), Asked::Answer(String::new()));
}

#[test]
fn a_redis_that_wants_credentials_is_left_to_redis_cli() {
    for reply in ["-NOAUTH Authentication required.\r\n", "-WRONGPASS invalid username-password pair\r\n", "-NOPERM no\r\n"] {
        assert!(matches!(ask_redis(vec![Step::ReadCommand, Step::Write(reply.as_bytes().to_vec())]), Asked::Fallback(_)), "{reply}");
    }
}

#[test]
fn every_redis_failure_fails_closed() {
    let ask = |reply: &[u8]| ask_redis(vec![Step::ReadCommand, Step::Write(reply.to_vec())]);
    failed(&ask(b"-ERR unknown command 'CONFIG'\r\n"), "answered an error: ERR unknown command");
    failed(&ask(b"+OK\r\n"), "unexpected reply");
    failed(&ask(b":1\r\n"), "unexpected reply");
    failed(&ask(b"HTTP/1.1 400 Bad Request\r\n\r\n"), "unexpected reply");
    failed(&ask(b"*2\r\n:1\r\n:2\r\n"), "unexpected reply");
    failed(&ask(b"*2\r\n$3\r\ndir\r\n$-1\r\n"), "unexpected reply");
    failed(&ask(b"*x\r\n"), "malformed reply");
    failed(&ask(b"*2\r\n$3\r\ndirXY"), "malformed reply");
    failed(&ask(b"*2\r\n$3\r\ndir\r\n$3\r\n\xff\xfe\xfd\r\n"), "not UTF-8");
    // Signed lengths are not RESP.
    failed(&ask(b"*+2\r\n$3\r\ndir\r\n$3\r\n/dd\r\n"), "malformed reply");
    failed(&ask(b"*2\r\n$+3\r\ndir\r\n$3\r\n/dd\r\n"), "unexpected reply");
    failed(&ask(&[b'*'; 600]), "too long");
    // Partial: the connection ends inside the reply, or before any.
    failed(&ask(b"*2\r\n$3\r\ndir\r\n$9\r\n/data"), "closed the connection");
    failed(&ask_redis(vec![Step::ReadCommand]), "closed the connection");
    // Oversized: too many elements, or an element larger than a probe reads.
    failed(&ask(b"*100\r\n"), "answered 100 elements");
    failed(&ask(format!("*1\r\n${}\r\n", 1024 * 1024).as_bytes()), "element of");
    // Slow.
    let _deadline = within(300);
    failed(&ask_redis(vec![Step::ReadCommand, Step::Trickle(b"*2\r\n$3\r\ndir\r\n".to_vec(), Duration::from_millis(60))]), "no complete answer");
}

// ---- real servers -------------------------------------------------------------------------

/// A directory holding `bins`, from PATH or from mise's installs of `tool` (newest name last).
fn find_bin_dir(tool: &str, bins: &[&str]) -> Option<PathBuf> {
    let has = |dir: &Path| bins.iter().all(|b| dir.join(b).is_file());
    if let Some(dir) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).find(|d| has(d))) {
        return Some(dir);
    }
    let data = std::env::var_os("MISE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/mise")))?;
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(data.join("installs").join(tool)).ok()?.filter_map(|e| Some(e.ok()?.path().join("bin"))).filter(|d| has(d)).collect();
    dirs.sort();
    dirs.pop()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A child process stopped when dropped: `SIGINT` (Postgres's fast shutdown, which releases its
/// shared memory; killing it would leak a System V segment), then `SIGKILL` after 10 seconds.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: signals only the child this test started, which has not been reaped yet.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGINT) };
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(port: u16) {
    let until = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < until, "nothing listens on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_real_postgres_answers_its_data_directory_as_psql_does() {
    let Some(bin) = find_bin_dir("postgres", &["initdb", "postgres", "psql"]) else {
        eprintln!("skipping: no postgres, initdb and psql on PATH or in mise's installs");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let out = std::process::Command::new(bin.join("initdb"))
        .args(["-D", data.to_str().unwrap(), "-U", "postgres", "--auth=trust", "--no-sync"])
        .env("LC_ALL", "C.UTF-8")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let port = free_port();
    let _server = Child(
        std::process::Command::new(bin.join("postgres"))
            .args(["-D", data.to_str().unwrap(), "-p", &port.to_string(), "-h", "127.0.0.1", "-k", ""])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(port);
    let url = format!("postgresql://postgres@127.0.0.1:{port}/postgres");
    let asked = {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            // Accepting connections comes a moment before it accepts sessions.
            match postgres_seeing(&url, &[], QUERY) {
                Asked::Failed(why) if why.contains("starting up") && Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
                other => break other,
            }
        }
    };
    let psql = std::process::Command::new(bin.join("psql")).args([url.as_str(), "-Atc", QUERY]).env("PGCONNECT_TIMEOUT", "3").output().unwrap();
    assert!(psql.status.success(), "{}", String::from_utf8_lossy(&psql.stderr));
    assert_eq!(asked, Asked::Answer(String::from_utf8(psql.stdout).unwrap()));
    match &asked {
        Asked::Answer(text) => assert_eq!(std::fs::canonicalize(text.trim()).unwrap(), std::fs::canonicalize(&data).unwrap()),
        other => panic!("{other:?}"),
    }
    assert_eq!(postgres_seeing(&url, &[], "show server_encoding"), Asked::Answer("UTF8\n".into()));
    // A role or database that does not exist is left to psql, which fails too.
    let refused = |url: String, says: &str| match postgres_seeing(&url, &[], QUERY) {
        Asked::Fallback(why) => assert!(why.contains(says), "{why}"),
        other => panic!("{other:?}"),
    };
    refused(format!("postgresql://nobody@127.0.0.1:{port}/postgres"), "nobody");
    refused(format!("postgresql://postgres@127.0.0.1:{port}/nothing"), "nothing");
    // A query error fails.
    failed(&postgres_seeing(&url, &[], "select current_setting('no_such_setting')"), "answered an error");
}

#[test]
fn a_real_postgres_that_wants_a_password_is_left_to_psql() {
    let Some(bin) = find_bin_dir("postgres", &["initdb", "postgres"]) else {
        eprintln!("skipping: no postgres and initdb on PATH or in mise's installs");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let pw = dir.path().join("pw");
    std::fs::write(&pw, "secret\n").unwrap();
    let out = std::process::Command::new(bin.join("initdb"))
        .args(["-D", data.to_str().unwrap(), "-U", "postgres", "--auth=scram-sha-256", "--no-sync", "--pwfile", pw.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let port = free_port();
    let _server = Child(
        std::process::Command::new(bin.join("postgres"))
            .args(["-D", data.to_str().unwrap(), "-p", &port.to_string(), "-h", "127.0.0.1", "-k", ""])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(port);
    let url = format!("postgresql://postgres@127.0.0.1:{port}/postgres");
    let until = Instant::now() + Duration::from_secs(20);
    let asked = loop {
        match postgres_seeing(&url, &[], QUERY) {
            Asked::Failed(why) if why.contains("starting up") && Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            other => break other,
        }
    };
    assert_eq!(asked, Asked::Fallback("the server asks for authentication (method 10)".into()));
}

#[test]
fn a_real_redis_answers_its_directory_as_redis_cli_does() {
    let Some(bin) = find_bin_dir("redis", &["redis-server", "redis-cli"]) else {
        eprintln!("skipping: no redis-server and redis-cli on PATH or in mise's installs");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    let server = |args: &[&str]| {
        Child(
            std::process::Command::new(bin.join("redis-server"))
                .args(["--bind", "127.0.0.1", "--port", &port.to_string(), "--dir", dir.path().to_str().unwrap(), "--save", ""])
                .args(args)
                .env("LC_ALL", "C")
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let url = format!("redis://127.0.0.1:{port}");
    {
        let _server = server(&[]);
        wait_for(port);
        let asked = redis_dir_seeing(&url, &[]);
        let cli = std::process::Command::new(bin.join("redis-cli")).args(["-u", url.as_str(), "--no-auth-warning", "config", "get", "dir"]).output().unwrap();
        assert!(cli.status.success());
        assert_eq!(asked, Asked::Answer(String::from_utf8(cli.stdout).unwrap()));
        match &asked {
            Asked::Answer(text) => {
                let reported = text.lines().last().unwrap();
                assert_eq!(std::fs::canonicalize(reported).unwrap(), std::fs::canonicalize(dir.path()).unwrap());
            }
            other => panic!("{other:?}"),
        }
    }
    // With a password set, the question is left to redis-cli.
    let port_free = || TcpStream::connect(("127.0.0.1", port)).is_err();
    let until = Instant::now() + Duration::from_secs(10);
    while !port_free() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _server = server(&["--requirepass", "secret"]);
    wait_for(port);
    assert_eq!(redis_dir_seeing(&url, &[]), Asked::Fallback("the server asks for authentication (NOAUTH)".into()));
}

#[test]
fn a_result_psql_would_reject_fails() {
    let dir = || row(&[Some(b"/data/this-checkout")]);
    // A row with no RowDescription before it (psql: "server sent data ("D" message) without
    // prior row description ("T" message)").
    failed(&ask_pg(replying([dir(), complete(), ready()].concat())), "row before");
    failed(&ask_pg(replying([dir(), ready()].concat())), "row before");
    // Two descriptions, or one of another shape.
    failed(&ask_pg(replying([description(1), description(1), dir(), complete(), ready()].concat())), "second row description");
    failed(&ask_pg(replying([description(2), row(&[Some(b"/d"), Some(b"x")]), complete(), ready()].concat())), "2 columns");
    failed(&ask_pg(replying([msg(b'T', &[0, 1, b'x']), dir(), complete(), ready()].concat())), "malformed row description");
    // A row of another width than described.
    failed(&ask_pg(replying([description(1), row(&[Some(b"/d"), Some(b"x")]), complete(), ready()].concat())), "malformed row");
    // Ready before the command completed, or completion before any description.
    failed(&ask_pg(replying([description(1), dir(), ready()].concat())), "before the command completed");
    failed(&ask_pg(replying([complete(), ready()].concat())), "before a row description");
    // Anything but ready after completion, and a malformed ReadyForQuery.
    failed(&ask_pg(replying([description(1), dir(), complete(), dir(), ready()].concat())), "after the command completed");
    failed(&ask_pg(replying([description(1), dir(), complete(), msg(b'Z', b"")].concat())), "malformed ReadyForQuery");
    failed(&ask_pg(replying([description(1), dir(), complete(), msg(b'Z', b"X")].concat())), "malformed ReadyForQuery");
    failed(&ask_pg(replying([description(1), dir(), msg(b'C', b"SELECT 1"), ready()].concat())), "malformed CommandComplete");
    failed(&ask_pg(replying([description(1), dir(), complete(), msg(b'I', b""), ready()].concat())), "unexpected message ('I')");
}

#[test]
fn the_startup_asks_for_no_client_encoding_and_text_that_is_not_utf8_goes_to_psql() {
    // What the server receives: no `client_encoding`, so it sends what psql would print.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut len = [0u8; 4];
        c.read_exact(&mut len).unwrap();
        let mut startup = vec![0u8; u32::from_be_bytes(len) as usize - 4];
        c.read_exact(&mut startup).unwrap();
        startup
    });
    let _ = postgres_seeing(&pg_url(addr), &[], QUERY);
    let startup = server.join().unwrap();
    let text = String::from_utf8_lossy(&startup);
    assert!(text.contains("user\0postgres\0database\0postgres\0"), "{text:?}");
    assert!(!text.contains("client_encoding"), "{text:?}");
    // A LATIN1 path's bytes are psql's to print.
    let asked = ask_pg(answering(vec![row(&[Some(b"/data/pg\xe9")])]));
    assert!(matches!(asked, Asked::Fallback(ref why) if why.contains("not UTF-8")), "{asked:?}");
}

#[test]
fn a_real_latin1_cluster_under_a_non_ascii_path_answers_as_psql_prints_it() {
    let Some(bin) = find_bin_dir("postgres", &["initdb", "postgres", "psql"]) else {
        eprintln!("skipping: no postgres, initdb and psql on PATH or in mise's installs");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("pg\u{e9}").join("data");
    std::fs::create_dir_all(data.parent().unwrap()).unwrap();
    let out = std::process::Command::new(bin.join("initdb"))
        .args(["-D", data.to_str().unwrap(), "-U", "postgres", "--auth=trust", "--no-sync", "-E", "LATIN1", "--locale=C"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let port = free_port();
    let _server = Child(
        std::process::Command::new(bin.join("postgres"))
            .args(["-D", data.to_str().unwrap(), "-p", &port.to_string(), "-h", "127.0.0.1", "-k", ""])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(port);
    let url = format!("postgresql://postgres@127.0.0.1:{port}/postgres");
    let until = Instant::now() + Duration::from_secs(20);
    let asked = loop {
        match postgres_seeing(&url, &[], QUERY) {
            Asked::Failed(why) if why.contains("starting up") && Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            other => break other,
        }
    };
    let psql = std::process::Command::new(bin.join("psql")).args(["-X", url.as_str(), "-Atc", QUERY]).output().unwrap();
    assert!(psql.status.success(), "{}", String::from_utf8_lossy(&psql.stderr));
    assert_eq!(asked, Asked::Answer(String::from_utf8(psql.stdout).unwrap()));
}

/// A NoticeResponse as a server sends one.
fn notice_msg() -> Vec<u8> {
    msg(b'N', b"SNOTICE\0VNOTICE\0C00000\0Mhello\0\0")
}

#[test]
fn asynchronous_messages_must_be_well_formed_wherever_they_come() {
    let dir = || row(&[Some(b"/data/this-checkout")]);
    // Well formed, before and inside the answer: passed over.
    let notification = msg(b'A', b"\0\0\0\x07chan\0payload\0");
    let asked = ask_pg(replying([notice_msg(), description(1), notification.clone(), dir(), msg(b'S', b"TimeZone\0UTC\0"), complete(), ready()].concat()));
    assert_eq!(asked, Asked::Answer("/data/this-checkout\n".into()));
    // Malformed: empty, unterminated, missing required fields, trailing bytes.
    for body in [&b""[..], b"\0", b"Mhi\0\0", b"SNOTICE\0C00000\0Mhi", b"SNOTICE\0C00000\0Mhi\0\0x"] {
        failed(&ask_pg(replying([description(1), msg(b'N', body), dir(), complete(), ready()].concat())), "malformed NoticeResponse");
        let mut connecting = vec![Step::ReadStartup, Step::Write([msg(b'N', body), auth(0)].concat())];
        connecting.push(Step::Sleep(Duration::from_millis(1)));
        failed(&ask_pg(connecting), "malformed NoticeResponse");
    }
    for body in [&b""[..], b"\0\0\0\x07", b"\0\0\0\x07chan\0", b"\0\0\0\x07\0payload\0", b"\0\0\0\x07chan\0payload\0x\0"] {
        failed(&ask_pg(replying([description(1), msg(b'A', body), dir(), complete(), ready()].concat())), "malformed NotificationResponse");
    }
    for body in [&b""[..], b"\0UTC\0", b"TimeZone\0", b"TimeZone\0UTC"] {
        failed(&ask_pg(replying([description(1), msg(b'S', body), dir(), complete(), ready()].concat())), "unexpected message ('S')");
    }
}

#[test]
fn resp_lengths_must_be_written_as_redis_writes_them() {
    let ask = |reply: &[u8]| ask_redis(vec![Step::ReadCommand, Step::Write(reply.to_vec())]);
    failed(&ask(b"*02\r\n$3\r\ndir\r\n$3\r\n/dd\r\n"), "malformed reply");
    failed(&ask(b"*2\r\n$03\r\ndir\r\n$3\r\n/dd\r\n"), "unexpected reply");
    failed(&ask(b"*00\r\n"), "malformed reply");
    assert_eq!(ask(b"*2\r\n$3\r\ndir\r\n$0\r\n\r\n"), Asked::Answer("dir\n\n".into()));
}

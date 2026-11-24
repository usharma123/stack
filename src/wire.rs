//! The Postgres and Redis identity questions, asked over the wire instead of through `psql` and
//! `redis-cli`.
//!
//! Each asks exactly what the client program is asked (`psql <url> -Atc <query>`, `redis-cli -u
//! <url> --no-auth-warning config get dir`) and returns the text that program prints on
//! success, so the caller compares it exactly as before. Only a connection this module can
//! make exactly as the client would is made here: an IP address and port written in the URL
//! (no host name, socket directory, password, query parameters or several hosts), no client
//! variable that could change where the client connects or how (`PG*` other than the four the
//! URL overrides and `PGCONNECT_TIMEOUT`; any `REDISCLI_*`), and no authentication. Anything
//! else, and a server that asks for a password or another authentication method, is
//! [`Asked::Fallback`]: the caller runs the client program as before. Never "verified".
//!
//! A Postgres that refuses the connection at startup is left to psql too (it may have wanted
//! the SSL or GSS encryption psql tries first), unless it is starting up or shutting down,
//! which psql would be told as well. Everything else that goes wrong once a conversation
//! started fails: no answer, an error reply, a malformed or partial reply, a message or reply larger than a client probe
//! would read, or no complete answer within the deadline (5 seconds, or the caller's earlier
//! one; connecting gets at most 3, as `PGCONNECT_TIMEOUT=3` gives psql).

use indexmap::IndexMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// What asking over the wire found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// What the client program would have printed.
    Answer(String),
    /// The question cannot be asked here exactly as the client would; run the client.
    Fallback(String),
    /// The question was asked and got no acceptable answer.
    Failed(String),
}

/// The whole conversation, at most (the client probe's limit).
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// Connecting, at most (what `PGCONNECT_TIMEOUT=3` gives psql).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Bytes of one message (Postgres) or one reply element (Redis); the client probes keep 16 KiB
/// of output.
const MESSAGE_LIMIT: usize = 16 * 1024;
/// Bytes read in one conversation, at most.
const READ_LIMIT: usize = 64 * 1024;
/// Messages read in one conversation, at most.
const MESSAGE_COUNT_LIMIT: usize = 256;

/// The variables a client run with `env` (the provider's environment) over Stack's own would
/// see, by name.
fn client_vars(env: &IndexMap<String, String>) -> Vec<String> {
    let mut names: Vec<String> = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect();
    names.extend(env.keys().cloned());
    names
}

/// `psql <url> -Atc <query>` over the wire: each row's fields joined by `|`, one row per line.
pub fn postgres(url: &str, env: &IndexMap<String, String>, query: &str) -> Asked {
    postgres_seeing(url, &client_vars(env), query)
}

/// [`postgres`] for a client that would see the variables named `vars`.
fn postgres_seeing(url: &str, vars: &[String], query: &str) -> Asked {
    // The URL names host, port, user and database, which win over PGHOST, PGPORT, PGUSER and
    // PGDATABASE; every other libpq variable could change the connection (PGHOSTADDR,
    // PGSSLMODE, PGPASSWORD, PGOPTIONS, PGSERVICE, ...) and is left to psql.
    let allowed = ["PGHOST", "PGPORT", "PGUSER", "PGDATABASE", "PGCONNECT_TIMEOUT"];
    if let Some(var) = vars.iter().find(|k| k.starts_with("PG") && !allowed.contains(&k.as_str())) {
        return Asked::Fallback(format!("{var} is set"));
    }
    let Some(target) = postgres_target(url) else { return Asked::Fallback("the URL is not one Stack reads itself".into()) };
    let mut conn = match Conn::open(target.addr) {
        Ok(conn) => conn,
        Err(why) => return Asked::Failed(format!("cannot connect to {}: {why}", target.addr)),
    };
    match postgres_conversation(&mut conn, &target, query) {
        Ok(answer) => answer,
        Err(why) => Asked::Failed(format!("postgres at {}: {why}", target.addr)),
    }
}

/// `redis-cli -u <url> --no-auth-warning config get dir` over the wire: the reply's elements,
/// one per line.
pub fn redis_dir(url: &str, env: &IndexMap<String, String>) -> Asked {
    redis_dir_seeing(url, &client_vars(env))
}

/// [`redis_dir`] for a client that would see the variables named `vars`.
fn redis_dir_seeing(url: &str, vars: &[String]) -> Asked {
    // REDISCLI_AUTH gives redis-cli a password; the others are left to it too.
    if let Some(var) = vars.iter().find(|k| k.starts_with("REDISCLI_")) {
        return Asked::Fallback(format!("{var} is set"));
    }
    let Some(addr) = redis_target(url) else { return Asked::Fallback("the URL is not one Stack reads itself".into()) };
    let mut conn = match Conn::open(addr) {
        Ok(conn) => conn,
        Err(why) => return Asked::Failed(format!("cannot connect to {addr}: {why}")),
    };
    match redis_conversation(&mut conn) {
        Ok(answer) => answer,
        Err(why) => Asked::Failed(format!("redis at {addr}: {why}")),
    }
}

// ---- targets -------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct PostgresTarget {
    addr: SocketAddr,
    user: String,
    database: String,
}

/// `postgres[ql]://<user>@<ip>:<port>/<database>` and nothing more: no password, query,
/// fragment, several hosts or host name; user and database present and percent-decoded.
fn postgres_target(url: &str) -> Option<PostgresTarget> {
    let rest = url.strip_prefix("postgresql://").or_else(|| url.strip_prefix("postgres://"))?;
    if rest.contains(['?', '#']) {
        return None;
    }
    let (authority, database) = rest.split_once('/')?;
    let (user, hostport) = authority.rsplit_once('@')?;
    if user.contains(':') {
        return None;
    }
    let (user, database) = (decode(user)?, decode(database)?);
    if user.is_empty() || database.is_empty() || user.contains('\0') || database.contains('\0') {
        return None;
    }
    Some(PostgresTarget { addr: socket_addr(hostport)?, user, database })
}

/// `redis://<ip>:<port>` with nothing after but an optional `/`.
fn redis_target(url: &str) -> Option<SocketAddr> {
    let rest = url.strip_prefix("redis://")?;
    let hostport = rest.strip_suffix('/').unwrap_or(rest);
    if hostport.contains(['@', '/', '?', '#']) {
        return None;
    }
    socket_addr(hostport)
}

/// `<ipv4>:<port>` or `[<ipv6>]:<port>`, with an explicit nonzero port. An IPv6 address must be
/// bracketed, as libpq and redis-cli require.
fn socket_addr(hostport: &str) -> Option<SocketAddr> {
    let (host, port) = hostport.rsplit_once(':')?;
    let host = match host.strip_prefix('[') {
        Some(bracketed) => bracketed.strip_suffix(']').filter(|h| h.contains(':'))?,
        None if host.contains(':') => return None,
        None => host,
    };
    if host.contains(',') || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ip: IpAddr = host.parse().ok()?;
    let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
    Some(SocketAddr::new(ip, port))
}

/// Percent-decoding of a URL component, as UTF-8.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---- connection ----------------------------------------------------------------------------

/// A connection bounded by one deadline and a read budget.
struct Conn {
    stream: TcpStream,
    deadline: Instant,
    read: usize,
}

impl Conn {
    fn open(addr: SocketAddr) -> Result<Self, String> {
        let total = crate::process::bounded(TIMEOUT);
        let deadline = Instant::now() + total;
        if total.is_zero() {
            return Err("no time is left".into());
        }
        let stream = TcpStream::connect_timeout(&addr, total.min(CONNECT_TIMEOUT)).map_err(|e| e.to_string())?;
        let _ = stream.set_nodelay(true);
        Ok(Self { stream, deadline, read: 0 })
    }

    fn left(&self) -> Result<Duration, String> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("no complete answer within {}s", TIMEOUT.as_secs()));
        }
        Ok(left)
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        let left = self.left()?;
        self.stream.set_write_timeout(Some(left)).map_err(|e| e.to_string())?;
        self.stream.write_all(bytes).map_err(|e| format!("cannot send: {e}"))
    }

    fn fill(&mut self, buf: &mut [u8]) -> Result<(), String> {
        self.read += buf.len();
        if self.read > READ_LIMIT {
            return Err(format!("answered more than {READ_LIMIT} bytes"));
        }
        let mut at = 0;
        while at < buf.len() {
            let left = self.left()?;
            self.stream.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
            match self.stream.read(&mut buf[at..]) {
                Ok(0) => return Err("closed the connection before a complete answer".into()),
                Ok(n) => at += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    return Err(format!("no complete answer within {}s", TIMEOUT.as_secs()))
                }
                Err(e) => return Err(format!("cannot read: {e}")),
            }
        }
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, String> {
        let mut b = [0u8; 1];
        self.fill(&mut b)?;
        Ok(b[0])
    }
}

// ---- Postgres (protocol 3.0) ----------------------------------------------------------------

fn postgres_conversation(conn: &mut Conn, target: &PostgresTarget, query: &str) -> Result<Asked, String> {
    let mut body = Vec::new();
    body.extend(196_608u32.to_be_bytes()); // 3.0
    // No `client_encoding`: like psql on a pipe, the text comes in the server's encoding, so the
    // bytes are the ones psql would print.
    for (key, value) in [("user", target.user.as_str()), ("database", target.database.as_str()), ("application_name", "stack")] {
        body.extend(key.as_bytes());
        body.push(0);
        body.extend(value.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut startup = ((body.len() + 4) as u32).to_be_bytes().to_vec();
    startup.extend(body);
    conn.send(&startup)?;

    let mut count = 0;
    let mut next = |conn: &mut Conn| -> Result<(u8, Vec<u8>), String> {
        count += 1;
        if count > MESSAGE_COUNT_LIMIT {
            return Err(format!("sent more than {MESSAGE_COUNT_LIMIT} messages"));
        }
        let kind = conn.byte()?;
        let mut len = [0u8; 4];
        conn.fill(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        if !(4..=MESSAGE_LIMIT + 4).contains(&len) {
            return Err(format!("sent a message of {len} bytes"));
        }
        let mut body = vec![0u8; len - 4];
        conn.fill(&mut body)?;
        Ok((kind, body))
    };

    let mut authenticated = false;
    loop {
        let (kind, body) = next(conn)?;
        match kind {
            b'R' => {
                let code = be_i32(&body, 0).ok_or("sent a malformed authentication request")?;
                if code != 0 {
                    // AuthenticationOk only: anything else needs credentials or a mechanism
                    // this module does not have; psql may.
                    return Ok(Asked::Fallback(format!("the server asks for authentication (method {code})")));
                }
                if body.len() != 4 || authenticated {
                    return Err("sent a malformed authentication message".into());
                }
                authenticated = true;
            }
            // Starting up, shutting down or recovering: psql is refused the same way. Any
            // other refusal (no such role or database, a pg_hba.conf line that wants SSL or
            // GSS encryption, which psql would try first) is psql's to answer.
            b'E' if field(&body, b'C').as_deref() == Some("57P03") => return Err(format!("refused the connection: {}", error_text(&body))),
            b'E' => return Ok(Asked::Fallback(format!("the server refused a plain connection: {}", error_text(&body)))),
            b'N' => notice(&body)?,
            b'S' if authenticated && parameter_status(&body) => {}
            b'K' if authenticated && body.len() == 8 => {}
            b'Z' if authenticated => {
                ready_status(&body)?;
                break;
            }
            other => return Err(format!("sent an unexpected message ({}) while connecting", printable(other))),
        }
    }

    let mut q = vec![b'Q'];
    q.extend(((query.len() + 5) as u32).to_be_bytes());
    q.extend(query.as_bytes());
    q.push(0);
    conn.send(&q)?;
    // The answer as psql accepts it: one RowDescription of one column, rows of that width, the
    // command's completion, then ReadyForQuery. Notices, parameter changes and notifications may
    // come between; anything else, or anything out of that order, fails.
    let mut described = false;
    let mut completed = false;
    let mut printed = Vec::new();
    loop {
        let (kind, body) = next(conn)?;
        match kind {
            b'N' => notice(&body)?,
            b'A' => notification(&body)?,
            b'S' if parameter_status(&body) => {}
            b'T' if completed => return Err("sent a row description after the command completed".into()),
            b'T' if described => return Err("sent a second row description".into()),
            b'T' => {
                let columns = row_description(&body).ok_or("sent a malformed row description")?;
                if columns != 1 {
                    return Err(format!("described {columns} columns for a one-column query"));
                }
                described = true;
            }
            b'D' if completed => return Err("sent a row after the command completed".into()),
            b'D' if !described => return Err("sent a row before a row description".into()),
            b'D' => {
                if be_i16(&body, 0) != Some(1) {
                    return Err("sent a malformed row".into());
                }
                let len = be_i32(&body, 2).ok_or("sent a malformed row")?;
                let value: &[u8] = match len {
                    -1 if body.len() == 6 => &[],
                    _ => {
                        let len = usize::try_from(len).map_err(|_| "sent a malformed row")?;
                        body.get(6..).filter(|v| v.len() == len).ok_or("sent a malformed row")?
                    }
                };
                printed.extend(value);
                printed.push(b'\n');
                if printed.len() > MESSAGE_LIMIT {
                    return Err(format!("answered more than {MESSAGE_LIMIT} bytes"));
                }
            }
            b'C' if completed => return Err("completed the command twice".into()),
            b'C' if !described => return Err("completed the command before a row description".into()),
            b'C' => {
                // One tag, NUL-terminated.
                if body.last() != Some(&0) || body.iter().filter(|b| **b == 0).count() != 1 {
                    return Err("sent a malformed CommandComplete".into());
                }
                completed = true;
            }
            b'E' => return Err(format!("answered an error: {}", error_text(&body))),
            b'Z' => {
                ready_status(&body)?;
                if !completed {
                    return Err("was ready before the command completed".into());
                }
                break;
            }
            other => return Err(format!("sent an unexpected message ({}) in its answer", printable(other))),
        }
    }
    let _ = conn.send(&[b'X', 0, 0, 0, 4]);
    // In the server's encoding, as psql prints it: what is not UTF-8 is psql's to print.
    match String::from_utf8(printed) {
        Ok(printed) => Ok(Asked::Answer(printed)),
        Err(_) => Ok(Asked::Fallback("the server answered text that is not UTF-8".into())),
    }
}

/// ReadyForQuery's one status byte: idle, in a transaction, or in a failed one.
fn ready_status(body: &[u8]) -> Result<(), String> {
    match body {
        [b'I' | b'T' | b'E'] => Ok(()),
        _ => Err("sent a malformed ReadyForQuery".into()),
    }
}

/// ParameterStatus: a name (not empty) and a value, each NUL-terminated.
fn parameter_status(body: &[u8]) -> bool {
    body.first().is_some_and(|b| *b != 0) && body.last() == Some(&0) && body.iter().filter(|b| **b == 0).count() == 2
}

/// NoticeResponse: fields of a type byte and a NUL-terminated value, then a NUL; severity,
/// code and message, which every server sends, among them.
fn notice(body: &[u8]) -> Result<(), String> {
    let malformed = || "sent a malformed NoticeResponse".to_string();
    let mut types = Vec::new();
    let mut at = 0;
    loop {
        let kind = *body.get(at).ok_or_else(malformed)?;
        if kind == 0 {
            break;
        }
        let end = body.get(at + 1..).and_then(|rest| rest.iter().position(|b| *b == 0)).ok_or_else(malformed)?;
        types.push(kind);
        at += 1 + end + 1;
    }
    if at + 1 != body.len() || !(types.contains(&b'S') && types.contains(&b'C') && types.contains(&b'M')) {
        return Err(malformed());
    }
    Ok(())
}

/// NotificationResponse: a process id, then a channel and a payload, each NUL-terminated.
fn notification(body: &[u8]) -> Result<(), String> {
    let rest = body.get(4..).filter(|r| !r.is_empty() && r.last() == Some(&0) && r[0] != 0);
    match rest {
        Some(rest) if rest.iter().filter(|b| **b == 0).count() == 2 => Ok(()),
        _ => Err("sent a malformed NotificationResponse".into()),
    }
}

/// The number of columns a RowDescription describes, when it is well formed: for each, a
/// NUL-terminated name and 18 bytes, and nothing after.
fn row_description(body: &[u8]) -> Option<i16> {
    let columns = be_i16(body, 0).filter(|n| *n >= 0)?;
    let mut at = 2;
    for _ in 0..columns {
        let name = body.get(at..)?.iter().position(|b| *b == 0)?;
        at += name + 1 + 18;
        if at > body.len() {
            return None;
        }
    }
    (at == body.len()).then_some(columns)
}

fn be_i32(body: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(body.get(at..at + 4)?.try_into().ok()?))
}

fn be_i16(body: &[u8], at: usize) -> Option<i16> {
    Some(i16::from_be_bytes(body.get(at..at + 2)?.try_into().ok()?))
}

/// The message (`M`) of an ErrorResponse, shortened.
fn error_text(body: &[u8]) -> String {
    let message = field(body, b'M').unwrap_or_else(|| "no message".into());
    message.chars().take(200).collect()
}

/// One field of an ErrorResponse or NoticeResponse.
fn field(body: &[u8], code: u8) -> Option<String> {
    body.split(|b| *b == 0).find_map(|f| f.strip_prefix(&[code])).map(|v| String::from_utf8_lossy(v).into_owned())
}

fn printable(byte: u8) -> String {
    if byte.is_ascii_graphic() {
        format!("'{}'", byte as char)
    } else {
        format!("0x{byte:02x}")
    }
}

// ---- Redis (RESP2) --------------------------------------------------------------------------

/// Elements a `CONFIG GET` reply may have; one parameter has two.
const REDIS_ELEMENTS: usize = 16;

fn redis_conversation(conn: &mut Conn) -> Result<Asked, String> {
    conn.send(b"*3\r\n$6\r\nCONFIG\r\n$3\r\nGET\r\n$3\r\ndir\r\n")?;
    let line = redis_line(conn)?;
    let (kind, rest) = (line.as_bytes()[0], line.get(1..).unwrap_or_default());
    match kind {
        b'-' => {
            // A server that wants credentials: redis-cli has none either (no password in the
            // URL, no REDISCLI_AUTH), but it is the one to say so.
            let code = rest.split_whitespace().next().unwrap_or_default();
            if matches!(code, "NOAUTH" | "WRONGPASS" | "NOPERM") {
                return Ok(Asked::Fallback(format!("the server asks for authentication ({code})")));
            }
            Err(format!("answered an error: {}", rest.chars().take(200).collect::<String>()))
        }
        b'*' => {
            let n = digits(rest).ok_or("sent a malformed reply")?;
            if n > REDIS_ELEMENTS {
                return Err(format!("answered {n} elements"));
            }
            let mut printed = String::new();
            for _ in 0..n {
                let header = redis_line(conn)?;
                let len = header.strip_prefix('$').and_then(digits).ok_or("sent an unexpected reply")?;
                if len > MESSAGE_LIMIT {
                    return Err(format!("answered an element of {len} bytes"));
                }
                let mut value = vec![0u8; len + 2];
                conn.fill(&mut value)?;
                if !value.ends_with(b"\r\n") {
                    return Err("sent a malformed reply".into());
                }
                value.truncate(len);
                printed.push_str(&String::from_utf8(value).map_err(|_| "answered text that is not UTF-8")?);
                printed.push('\n');
            }
            Ok(Asked::Answer(printed))
        }
        _ => Err("sent an unexpected reply".into()),
    }
}

/// A RESP length: decimal digits only, as Redis writes them (no sign, no leading zero).
fn digits(text: &str) -> Option<usize> {
    let canonical = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && (text == "0" || !text.starts_with('0'));
    canonical.then(|| text.parse().ok()).flatten()
}

/// One `\r\n`-terminated header line, without it.
fn redis_line(conn: &mut Conn) -> Result<String, String> {
    let mut line = Vec::new();
    loop {
        let b = conn.byte()?;
        if b == b'\n' && line.last() == Some(&b'\r') {
            line.pop();
            break;
        }
        line.push(b);
        if line.len() > 512 {
            return Err("sent a reply line that is too long".into());
        }
    }
    if line.is_empty() {
        return Err("sent a malformed reply".into());
    }
    String::from_utf8(line).map_err(|_| "sent a malformed reply".into())
}

#[cfg(test)]
mod tests;

//! Stand-ins for the preset servers Stack's supervisor runs (`postgres`, `initdb`,
//! `redis-server`) and the clients Stack verifies them with (`psql`, `redis-cli`), for tests
//! that put them first on PATH. Each server listens on its port until stopped and records its
//! data directory in `$REVIEW_FIXTURE/<server>-<port>` while it runs; each server, asked over
//! the wire, and each client answer with that directory, as a real server reports the
//! `-D`/`--dir` it was started with.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A server listening on `port` until SIGINT or SIGTERM, recording `dir` while it runs. It
/// answers the identity question Stack asks over the wire as a real server would: a Postgres
/// startup without authentication and `select current_setting('data_directory')` (or `show
/// server_encoding`), or Redis `CONFIG GET dir`. Anything else gets an error, or the connection
/// is closed.
const LISTENER: &str = r#"import os, signal, socket, struct, sys, threading
kind, port, data = sys.argv[1], int(sys.argv[2]), sys.argv[3]
record = os.path.join(os.environ["REVIEW_FIXTURE"], "%s-%d" % (kind, port))
def stop(*_):
    if os.path.exists(os.path.join(os.environ["REVIEW_FIXTURE"], "ignore-signals")):
        return
    try: os.remove(record)
    except FileNotFoundError: pass
    os._exit(0)
signal.signal(signal.SIGINT, stop)
signal.signal(signal.SIGTERM, stop)
def exact(c, n):
    b = b""
    while len(b) < n:
        chunk = c.recv(n - len(b))
        if not chunk: raise EOFError
        b += chunk
    return b
def msg(kind, body): return kind + struct.pack("!I", len(body) + 4) + body
def postgres(c):
    n = struct.unpack("!I", exact(c, 4))[0]; exact(c, n - 4)
    c.sendall(msg(b"R", struct.pack("!I", 0)) + msg(b"S", b"server_encoding\0UTF8\0") + msg(b"Z", b"I"))
    while True:
        k = exact(c, 1); n = struct.unpack("!I", exact(c, 4))[0]; body = exact(c, n - 4)
        if k == b"X": return
        if k != b"Q": return
        value = b"UTF8" if b"server_encoding" in body else data.encode()
        c.sendall(msg(b"T", struct.pack("!H", 1) + b"v\0" + struct.pack("!IHIhih", 0, 0, 25, -1, -1, 0))
                  + msg(b"D", struct.pack("!HI", 1, len(value)) + value) + msg(b"C", b"SELECT 1\0") + msg(b"Z", b"I"))
def line(c):
    b = b""
    while not b.endswith(b"\r\n"): b += exact(c, 1)
    return b[:-2]
def redis(c):
    while True:
        n = int(line(c)[1:]); args = []
        for _ in range(n):
            size = int(line(c)[1:]); args.append(exact(c, size + 2)[:-2])
        if [a.upper() for a in args[:2]] == [b"CONFIG", b"GET"] and args[2:] == [b"dir"]:
            c.sendall(b"*2\r\n$3\r\ndir\r\n$%d\r\n%s\r\n" % (len(data.encode()), data.encode()))
        elif args[:1] and args[0].upper() == b"PING":
            c.sendall(b"+PONG\r\n")
        else:
            c.sendall(b"-ERR unknown command\r\n")
def serve(c):
    c.settimeout(10)
    try: (postgres if kind == "postgres" else redis)(c)
    except Exception: pass
    finally: c.close()
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen()
open(record, "w").write(data)
while True:
    c, _ = s.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
"#;

fn script(bin: &Path, name: &str, body: &str) {
    let path = bin.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Write the stand-ins into `bin`. `postgres` reports `FAKE_PG_VERSION` (default 17.11) and
/// `redis-server` `FAKE_REDIS_VERSION` (default 8.2.1).
pub fn install(bin: &Path) {
    fs::write(bin.join("listener.py"), LISTENER).unwrap();
    script(bin, "initdb", "#!/bin/sh\n# initdb -D <dir> -U postgres --auth=trust\necho \"LC_ALL=$LC_ALL ${FAKE_PG_VERSION:-17.11} $*\" >>\"$REVIEW_FIXTURE/initdb.log\"\nmkdir -p \"$2\" && chmod 700 \"$2\" && echo 17 >\"$2/PG_VERSION\"\n");
    script(
        bin,
        "postgres",
        r#"#!/bin/sh
case "$1" in --version) echo "postgres (PostgreSQL) ${FAKE_PG_VERSION:-17.11}"; exit 0 ;; esac
while test $# -gt 0; do case "$1" in -D) dir=$2; shift 2 ;; -p) port=$2; shift 2 ;; *) shift ;; esac; done
printf 'postgres "-D" "%s"\n' "$dir" >"$dir/postmaster.opts"
echo "${FAKE_PG_VERSION:-17.11}" >"$REVIEW_FIXTURE/postgres-$port.version"
exec "${REVIEW_PYTHON:-python3}" "$(dirname "$0")/listener.py" postgres "$port" "$dir"
"#,
    );
    script(
        bin,
        "redis-server",
        r#"#!/bin/sh
case "$1" in --version) echo "Redis server v=${FAKE_REDIS_VERSION:-8.2.1} sha=00000000:0"; exit 0 ;; esac
while test $# -gt 0; do case "$1" in --dir) dir=$2; shift 2 ;; --port) port=$2; shift 2 ;; *) shift ;; esac; done
exec "${REVIEW_PYTHON:-python3}" "$(dirname "$0")/listener.py" redis "$port" "$dir"
"#,
    );
    // `psql <url> -Atc <query>`: the data directory of the server on the URL's port, or its
    // encoding when asked.
    script(
        bin,
        "psql",
        r#"#!/bin/sh
for a in "$@"; do case "$a" in *://*) url=$a ;; esac; done
port=${url##*:}; port=${port%%/*}
record="$REVIEW_FIXTURE/postgres-$port"
test -f "$record" || { echo "psql: error: connection to server on port $port failed: Connection refused" >&2; exit 2; }
case "$*" in *server_encoding*) echo UTF8 ;; *) cat "$record"; echo ;; esac
"#,
    );
    // `redis-cli -u <url> --no-auth-warning config get dir`.
    script(
        bin,
        "redis-cli",
        r#"#!/bin/sh
port=${2##*:}; port=${port%%/*}
record="$REVIEW_FIXTURE/redis-$port"
test -f "$record" || { echo "Could not connect to Redis at 127.0.0.1:$port: Connection refused" >&2; exit 1; }
echo dir; cat "$record"; echo
"#,
    );
}

/// A release of Postgres installed the way mise lays installs out
/// (`<root>/installs/postgres/<version>/bin`), running the stand-ins of [`install`] (which must
/// be in `bin`) as that version. Returns its bin directory.
pub fn postgres_release(root: &Path, bin: &Path, version: &str) -> std::path::PathBuf {
    let dir = root.join("installs/postgres").join(version).join("bin");
    fs::create_dir_all(&dir).unwrap();
    for tool in ["postgres", "initdb"] {
        script(&dir, tool, &format!("#!/bin/sh\nFAKE_PG_VERSION={version} exec \"{}\" \"$@\"\n", bin.join(tool).display()));
    }
    dir
}

/// `nats-server`: its client port and its monitoring port, which answers `GET /healthz`.
pub fn install_nats(bin: &Path) {
    script(
        bin,
        "nats-server",
        r#"#!/bin/sh
case "$1" in --version) echo "nats-server: v2.10.1"; exit 0 ;; esac
while test $# -gt 0; do case "$1" in --port) port=$2; shift 2 ;; --http_port) monitor=$2; shift 2 ;; --store_dir) dir=$2; shift 2 ;; *) shift ;; esac; done
exec "${REVIEW_PYTHON:-python3}" -c '
import http.server, os, signal, socket, sys, threading
port, monitor = int(sys.argv[1]), int(sys.argv[2])
signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", port)); s.listen()
threading.Thread(target=lambda: [s.accept()[0].close() for _ in iter(int, 1)], daemon=True).start()
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", monitor), H).serve_forever()
' "$port" "$monitor"
"#,
    );
}

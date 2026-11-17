//! Stand-ins for the preset servers Stack's supervisor runs (`postgres`, `initdb`,
//! `redis-server`) and the clients Stack verifies them with (`psql`, `redis-cli`), for tests
//! that put them first on PATH. Each server listens on its port until stopped and records its
//! data directory in `$REVIEW_FIXTURE/<server>-<port>` while it runs; each client answers with
//! that directory, as a real server reports the `-D`/`--dir` it was started with.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A server listening on `port` until SIGINT or SIGTERM, recording `dir` while it runs.
const LISTENER: &str = r#"import os, signal, socket, sys
kind, port, data = sys.argv[1], int(sys.argv[2]), sys.argv[3]
record = os.path.join(os.environ["REVIEW_FIXTURE"], "%s-%d" % (kind, port))
def stop(*_):
    try: os.remove(record)
    except FileNotFoundError: pass
    os._exit(0)
signal.signal(signal.SIGINT, stop)
signal.signal(signal.SIGTERM, stop)
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port)); s.listen()
open(record, "w").write(data)
while True:
    c, _ = s.accept(); c.close()
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

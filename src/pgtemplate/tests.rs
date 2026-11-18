use super::*;
use std::cell::Cell;
use std::net::{TcpListener, TcpStream};

thread_local! {
    /// Treat every install as settled (fake installs are made seconds before they are keyed).
    pub(super) static INSTALLS_SETTLED: Cell<bool> = const { Cell::new(false) };
    /// Make the identity edit fail as a full disk would, where it would first write.
    pub(super) static EDIT_WRITE_FAILS: Cell<bool> = const { Cell::new(false) };
    /// Run once at one of three points: 0, just before the next look at a template directory's
    /// identity (`generation`'s first, when a start begins); 1, between `generation`'s first
    /// look and its read of the record; 2, between that read and its second look.
    static BETWEEN_READS: std::cell::RefCell<Option<(u8, ReadHook)>> = std::cell::RefCell::new(None);
    /// Run on a fresh copy before the identity edit.
    static BEFORE_EDIT: std::cell::RefCell<Option<CopyHook>> = std::cell::RefCell::new(None);
}

type CopyHook = Box<dyn FnOnce(&Path)>;
type ReadHook = Box<dyn FnOnce()>;

pub(super) fn between_reads(point: u8) {
    let hook = BETWEEN_READS.with(|h| {
        let mut slot = h.borrow_mut();
        if slot.as_ref().is_some_and(|(at, _)| *at == point) { slot.take().map(|(_, hook)| hook) } else { None }
    });
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn before_edit(data: &Path) {
    if let Some(hook) = BEFORE_EDIT.with(|h| h.borrow_mut().take()) {
        hook(data);
    }
}

struct Settled;

impl Settled {
    fn new() -> Self {
        INSTALLS_SETTLED.with(|s| s.set(true));
        Self
    }
}

impl Drop for Settled {
    fn drop(&mut self) {
        INSTALLS_SETTLED.with(|s| s.set(false));
    }
}

// ---- real Postgres -------------------------------------------------------------------------

fn has_tools(dir: &Path) -> bool {
    ["initdb", "postgres", "psql", "pg_controldata", "pg_checksums", "pg_waldump"].iter().all(|b| dir.join(b).is_file())
}

/// One Postgres `bin` directory per installed major (the newest release of each): from
/// `STACK_TEST_PG_BIN` alone when set, else mise's installs, else the first on PATH. Empty
/// skips the real-Postgres tests, which say so.
fn pg_bins() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("STACK_TEST_PG_BIN").map(PathBuf::from).filter(|d| has_tools(d)) {
        return vec![dir.canonicalize().unwrap()];
    }
    let mut by_major: std::collections::BTreeMap<u32, (Vec<u32>, PathBuf)> = Default::default();
    let data = std::env::var_os("MISE_DATA_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/mise")));
    if let Some(entries) = data.and_then(|d| fs::read_dir(d.join("installs").join("postgres")).ok()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let parts: Option<Vec<u32>> = name.split('.').map(|p| p.parse().ok()).collect();
            let Some(parts) = parts.filter(|p| p.len() >= 2) else { continue };
            let bin = entry.path().join("bin");
            if !has_tools(&bin) {
                continue;
            }
            let slot = by_major.entry(parts[0]).or_insert_with(|| (Vec::new(), bin.clone()));
            if parts > slot.0 {
                *slot = (parts, bin);
            }
        }
    }
    let mut bins: Vec<PathBuf> = by_major.into_values().map(|(_, b)| b.canonicalize().unwrap()).collect();
    if bins.is_empty() {
        bins.extend(std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).find(|d| has_tools(d))).map(|d| d.canonicalize().unwrap()));
    }
    bins
}

macro_rules! pg_or_skip {
    () => {{
        let bins = pg_bins();
        if bins.is_empty() {
            eprintln!("skipping: no Postgres with initdb, psql, pg_controldata, pg_checksums and pg_waldump in mise's installs or on PATH");
            return;
        }
        bins
    }};
}

/// The environment the template code sees: `bin` as PATH, the rest this process's (which
/// `initdb` inherits).
fn env_for(bin: &Path) -> impl Fn(&str) -> Option<OsString> + '_ {
    move |name: &str| if name == "PATH" { Some(bin.as_os_str().to_os_string()) } else { std::env::var_os(name) }
}

fn major(bin: &Path) -> u32 {
    let out = std::process::Command::new(bin.join("postgres")).arg("--version").output().unwrap();
    crate::supervisor::parse_major(&String::from_utf8_lossy(&out.stdout)).unwrap().parse().unwrap()
}

fn control(bin: &Path, data: &Path, field: &str) -> String {
    let out = std::process::Command::new(bin.join("pg_controldata")).arg(data).env("LC_ALL", "C").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.strip_prefix(field).map(|v| v.trim_start_matches(':').trim().to_string())).unwrap_or_else(|| panic!("no {field} in {text}"))
}

fn tool(bin: &Path, name: &str, args: &[&str]) -> std::process::Output {
    std::process::Command::new(bin.join(name)).args(args).env("LC_ALL", "C").output().unwrap()
}

/// A server this test started, stopped with `SIGINT` when dropped (a killed Postgres leaks its
/// System V shared memory segment).
struct Server(std::process::Child);

impl Server {
    fn start(bin: &Path, data: &Path) -> (Self, u16) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let child = std::process::Command::new(bin.join("postgres"))
            .args(["-D", data.to_str().unwrap(), "-p", &port.to_string(), "-h", "127.0.0.1", "-k", ""])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let server = Self(child);
        let until = Instant::now() + Duration::from_secs(30);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < until, "postgres in {} did not listen", data.display());
            std::thread::sleep(Duration::from_millis(20));
        }
        (server, port)
    }

    fn stop(mut self) {
        self.halt(libc::SIGINT);
    }

    fn halt(&mut self, signal: i32) {
        // SAFETY: signals only the child this test started, not yet reaped.
        unsafe { libc::kill(self.0.id() as i32, signal) };
        let until = Instant::now() + Duration::from_secs(15);
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

impl Drop for Server {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            self.halt(libc::SIGINT);
        }
    }
}

fn psql(bin: &Path, port: u16, sql: &str) -> String {
    let url = format!("postgresql://postgres@127.0.0.1:{port}/postgres");
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let out = std::process::Command::new(bin.join("psql")).args(["-X", &url, "-Atc", sql]).output().unwrap();
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!((err.contains("starting up") || err.contains("recovery")) && Instant::now() < until, "{sql}: {err}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Every entry under `dir`, relative, with its kind, mode and (for files) size.
fn listing(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let meta = fs::symlink_metadata(dir.join(&rel)).unwrap();
        out.push(format!("{} {:o} {}", rel.display(), meta.mode(), if meta.is_dir() { 0 } else { meta.len() }));
        if meta.is_dir() {
            for e in fs::read_dir(dir.join(&rel)).unwrap() {
                pending.push(rel.join(e.unwrap().file_name()));
            }
        }
    }
    out.sort();
    out
}

fn recipe_for(bin: &Path) -> Recipe {
    Recipe::new(&bin.join("initdb"), &env_for(bin)).unwrap()
}

fn marker(bin: &Path) -> String {
    format!("postgres:{}", major(bin))
}

fn make(root: &Path, bin: &Path, target: &Path) -> Result<String, Failure> {
    initialise(root, &recipe_for(bin), &bin.join("initdb"), &marker(bin), target, &env_for(bin))
}

fn dirs_in(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root).unwrap().flatten().filter(|e| e.file_type().unwrap().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn first_segment(data: &Path) -> String {
    let mut names: Vec<String> = fs::read_dir(data.join("pg_wal")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.len() == 24).collect();
    names.sort();
    names.remove(0)
}

#[test]
fn a_clone_is_a_fresh_cluster_of_its_own_on_every_installed_major() {
    for bin in pg_or_skip!() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("templates");
        let (first, second, fresh) = (tmp.path().join("first"), tmp.path().join("second"), tmp.path().join("fresh"));
        let note = make(&root, &bin, &first).unwrap();
        assert!(note.contains("built now"), "{}: {note}", bin.display());
        let note = make(&root, &bin, &second).unwrap();
        assert!(!note.contains("built now") && note.contains("initialised from the template"), "{note}");
        let template = root.join(recipe_for(&bin).key).join("data");

        // What initdb itself writes, beside it.
        run_initdb(&bin.join("initdb"), &fresh).unwrap();
        write_marker(&fresh, &marker(&bin)).unwrap();
        assert_eq!(listing(&second), listing(&fresh), "{}: same entries, modes and sizes as initdb's", bin.display());
        for file in ["postgresql.conf", "postgresql.auto.conf", "pg_hba.conf", "pg_ident.conf", "PG_VERSION", crate::supervisor::VERSION_FILE] {
            assert_eq!(fs::read(second.join(file)).unwrap(), fs::read(fresh.join(file)).unwrap(), "{file}");
        }
        assert_eq!(fs::metadata(&second).unwrap().mode() & 0o777, 0o700);
        for run_file in ["postmaster.pid", "postmaster.opts"] {
            assert!(!second.join(run_file).exists());
        }

        // Its own identity, and Postgres' own tools read the edited files.
        let id = |d: &Path| control(&bin, d, "Database system identifier");
        let ids = [id(&template), id(&first), id(&second), id(&fresh)];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b, "{ids:?}");
            }
        }
        assert!(note.contains(&ids[2]), "the log names the new identifier: {note}");
        assert_eq!(control(&bin, &second, "Database cluster state"), "shut down");
        let segment = second.join("pg_wal").join(first_segment(&second));
        let dump = tool(&bin, "pg_waldump", &["-n", "1", segment.to_str().unwrap()]);
        assert!(dump.status.success() && String::from_utf8_lossy(&dump.stdout).contains("CHECKPOINT_SHUTDOWN"), "{}", String::from_utf8_lossy(&dump.stderr));
        if control(&bin, &second, "Data page checksum version") == "0" {
            let enable = tool(&bin, "pg_checksums", &["--enable", "-D", second.to_str().unwrap()]);
            assert!(enable.status.success(), "{}", String::from_utf8_lossy(&enable.stderr));
        }
        let check = tool(&bin, "pg_checksums", &["--check", "-D", second.to_str().unwrap()]);
        assert!(check.status.success() && String::from_utf8_lossy(&check.stdout).contains("Bad checksums:  0"), "{}{}", String::from_utf8_lossy(&check.stdout), String::from_utf8_lossy(&check.stderr));

        // It starts, names its own directory, takes writes and keeps them across a restart.
        let (server, port) = Server::start(&bin, &second);
        assert_eq!(fs::canonicalize(psql(&bin, port, "select current_setting('data_directory')")).unwrap(), fs::canonicalize(&second).unwrap());
        psql(&bin, port, "create table t (x int); insert into t values (1), (2)");
        assert_eq!(psql(&bin, port, "select system_identifier::text from pg_control_system()"), ids[2]);
        server.stop();
        let (server, port) = Server::start(&bin, &second);
        assert_eq!(psql(&bin, port, "select count(*) from t"), "2");
        server.stop();
        let record = read_any_record(template.parent().unwrap()).unwrap();
        assert_eq!(tree_digest(&template).unwrap().0, record.digest, "the template was never touched");
        assert!(record.full_flush, "macOS and Linux flush a template fully before publishing it");
    }
}

/// The SysV shared memory id a postmaster recorded in its `postmaster.pid` (line 7).
fn shm_id(data: &Path) -> Option<String> {
    let pid = fs::read_to_string(data.join("postmaster.pid")).ok()?;
    pid.lines().nth(6)?.split_whitespace().nth(1).map(str::to_string)
}

fn shm_attached(id: &str) -> Option<u32> {
    let out = std::process::Command::new("ipcs").args(["-m", "-o"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let cols: Vec<&str> = l.split_whitespace().collect();
        (cols.len() > 6 && cols[0] == "m" && cols[1] == id).then(|| cols[6].parse().ok()).flatten()
    })
}

#[test]
fn a_clone_recovers_from_a_crash_through_its_edited_wal() {
    for bin in pg_or_skip!() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("templates");
        let data = tmp.path().join("d");
        make(&root, &bin, &data).unwrap();
        let (mut server, port) = Server::start(&bin, &data);
        // No checkpoint before the crash: redo starts in the first segment, the edited one.
        psql(&bin, port, "create table t (x int); insert into t select generate_series(1, 20000)");
        let id = shm_id(&data);
        // The one deliberate kill: this test's own postmaster.
        server.halt(libc::SIGKILL);
        if let Some(id) = &id {
            let until = Instant::now() + Duration::from_secs(20);
            while shm_attached(id).is_some_and(|n| n > 0) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
            // Its segment, which the kill leaked; only this test's.
            let _ = std::process::Command::new("ipcrm").args(["-m", id]).status();
        }
        let (server, port) = Server::start(&bin, &data);
        assert_eq!(psql(&bin, port, "select count(*) from t"), "20000", "{}", bin.display());
        let id_now = control(&bin, &data, "Database system identifier");
        assert_eq!(psql(&bin, port, "select system_identifier::text from pg_control_system()"), id_now);
        server.stop();
    }
}

#[test]
fn the_identity_edit_changes_exactly_its_fields_on_every_installed_major() {
    for bin in pg_or_skip!() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("d");
        run_initdb(&bin.join("initdb"), &data).unwrap();
        let read = |p: &Path| fs::read(p).unwrap();
        let control_path = data.join("global/pg_control");
        let before = read(&control_path);
        let layout = layout(&before[..CONTROL_SAFE]).unwrap();
        distinguish_with(&data, 0x0102_0304_0506_0708, [0xAB; NONCE_LEN]).unwrap();
        let after = read(&control_path);
        assert_eq!(before.len(), after.len());
        for (i, (a, b)) in before.iter().zip(&after).enumerate() {
            let allowed = i < 8 || (layout.nonce..layout.nonce + NONCE_LEN).contains(&i) || (layout.crc..layout.crc + 4).contains(&i);
            assert!(allowed || a == b, "{}: byte {i} changed", bin.display());
        }
        assert!(after[layout.nonce..layout.nonce + NONCE_LEN].iter().all(|b| *b == 0xAB));
        assert_eq!(control(&bin, &data, "Mock authentication nonce"), "ab".repeat(NONCE_LEN));
        assert_eq!(control(&bin, &data, "Database system identifier"), 0x0102_0304_0506_0708u64.to_string());
        assert_eq!(control(&bin, &data, "Database cluster state"), "shut down", "pg_controldata accepts its checksum");
    }
}

#[test]
fn concurrent_first_starts_build_one_template() {
    let bin = pg_or_skip!().remove(0);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("templates");
    let notes: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let (root, bin, target) = (&root, &bin, tmp.path().join(format!("d{i}")));
                scope.spawn(move || make(root, bin, &target).unwrap())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(notes.iter().filter(|n| n.contains("built now")).count(), 1, "{notes:?}");
    assert_eq!(dirs_in(&root), vec![recipe_for(&bin).key], "one template, no build left behind");
    let ids: std::collections::BTreeSet<String> = (0..4).map(|i| control(&bin, &tmp.path().join(format!("d{i}")), "Database system identifier")).collect();
    assert_eq!(ids.len(), 4);
}

#[test]
fn a_build_that_died_or_another_boots_unflushed_template_is_never_used() {
    let bin = pg_or_skip!().remove(0);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("templates");
    let key = recipe_for(&bin).key;
    // A build that died before its rename, and a template directory without its record (a
    // removal that stopped half way): neither is cloned.
    fs::create_dir_all(root.join(format!(".build-{key}-1-1")).join("data")).unwrap();
    fs::create_dir_all(root.join(&key).join("data")).unwrap();
    fs::write(root.join(&key).join("data").join("PG_VERSION"), "junk").unwrap();
    let note = make(&root, &bin, &tmp.path().join("d")).unwrap();
    assert!(note.contains("built now"), "{note}");
    assert_eq!(dirs_in(&root), vec![key.clone()]);
    // A record of another recipe is not this template either.
    let record_path = root.join(&key).join(RECORD);
    let edit = |change: &dyn Fn(&mut Record)| {
        let mut record = read_any_record(&root.join(&key)).unwrap();
        change(&mut record);
        fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    };
    edit(&|r| r.recipe.push_str("env LC_ALL=xx\n"));
    assert!(make(&root, &bin, &tmp.path().join("e")).unwrap().contains("built now"));
    // Not fully flushed: usable in the boot it was built in, rebuilt in another.
    edit(&|r| {
        r.full_flush = false;
        r.boot = crate::procinfo::boot_id();
    });
    assert!(!make(&root, &bin, &tmp.path().join("f")).unwrap().contains("built now"));
    edit(&|r| {
        r.full_flush = false;
        r.boot = Some("an earlier boot".into());
    });
    assert!(make(&root, &bin, &tmp.path().join("g")).unwrap().contains("built now"));
}

#[test]
fn a_changed_template_is_removed_and_a_failed_clone_keeps_it() {
    let bin = pg_or_skip!().remove(0);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("templates");
    make(&root, &bin, &tmp.path().join("a")).unwrap();
    let dir = root.join(recipe_for(&bin).key);

    // A clone that cannot be made (no parent here) falls back to initdb and leaves the template.
    match make(&root, &bin, &tmp.path().join("missing/parent/d")) {
        Err(Failure::Fallback(why)) => assert!(why.contains("cannot clone"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(dir.join(RECORD).exists());
    assert!(!make(&root, &bin, &tmp.path().join("b")).unwrap().contains("built now"));

    // Anything written into the template since it was built: removed, and nothing is made.
    let conf = dir.join("data").join("postgresql.conf");
    let mut f = fs::OpenOptions::new().append(true).open(&conf).unwrap();
    f.write_all(b"\nfsync = off\n").unwrap();
    drop(f);
    let target = tmp.path().join("c");
    match make(&root, &bin, &target) {
        Err(Failure::Fallback(why)) => assert!(why.contains("changed since it was built; it was removed"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!target.exists() && !dir.exists());
    assert!(make(&root, &bin, &target).unwrap().contains("built now"));
}

#[test]
fn whole_second_times_wait_and_a_held_build_lock_times_out_to_initdb() {
    let bin = pg_or_skip!().remove(0);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("templates");
    let recipe = recipe_for(&bin);
    // Someone else is building (holds the lock) and no template exists: wait, then initdb.
    fs::create_dir_all(&root).unwrap();
    let building = crate::state::FileLock::try_acquire(&root.join(format!("{}.lock", recipe.key))).unwrap().unwrap();
    let started = Instant::now();
    match initialise_waiting(&root, &recipe, &bin.join("initdb"), &marker(&bin), &tmp.path().join("a"), &env_for(&bin), (Duration::from_millis(300), RETIRE_WAIT)) {
        Err(Failure::Fallback(why)) => assert!(why.contains("held") && why.contains("300 ms"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() >= Duration::from_millis(300));
    drop(building);

    make(&root, &bin, &tmp.path().join("b")).unwrap();
    let dir = root.join(&recipe.key);
    let mut record = read_any_record(&dir).unwrap();
    record.coarse = true;
    record.newest_ms = now_ms(SystemTime::now());
    fs::write(dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
    match make(&root, &bin, &tmp.path().join("c")) {
        Err(Failure::Fallback(why)) => assert!(why.contains("whole-second"), "{why}"),
        other => panic!("{other:?}"),
    }
    record.newest_ms = now_ms(SystemTime::now() - SETTLE - Duration::from_secs(1));
    fs::write(dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(make(&root, &bin, &tmp.path().join("c")).unwrap().contains("initialised from the template"));
}

// ---- without a server ----------------------------------------------------------------------

/// A minimal `pg_control` of `layout` (system identifier `id`, its checksum at the layout's
/// offset, distinct filler elsewhere so a stray write shows) and one WAL segment naming `wal_id`.
fn synthetic(data: &Path, layout: &Layout, id: u64, wal_id: u64) {
    fs::create_dir_all(data.join("global")).unwrap();
    fs::create_dir_all(data.join("pg_wal/archive_status")).unwrap();
    let mut control: Vec<u8> = (0..8192u32).map(|i| if i < CONTROL_SAFE as u32 { (i % 251) as u8 } else { 0 }).collect();
    control[..8].copy_from_slice(&id.to_le_bytes());
    control[CONTROL_VERSION_AT..CONTROL_VERSION_AT + 4].copy_from_slice(&layout.control_version.to_le_bytes());
    let crc = crc32c(&control[..layout.crc]);
    control[layout.crc..layout.crc + 4].copy_from_slice(&crc.to_le_bytes());
    fs::write(data.join("global/pg_control"), &control).unwrap();
    let mut wal = vec![0u8; 8192];
    wal[..2].copy_from_slice(&layout.wal_magic.to_le_bytes());
    wal[2..4].copy_from_slice(&XLP_LONG_HEADER.to_le_bytes());
    wal[24..32].copy_from_slice(&wal_id.to_le_bytes());
    fs::write(data.join("pg_wal/000000010000000000000001"), &wal).unwrap();
}

#[test]
fn the_identity_edit_writes_exactly_the_verified_fields_or_nothing() {
    assert_eq!(crc32c(b"123456789"), 0xE306_9283, "CRC-32C check value");
    let tmp = tempfile::tempdir().unwrap();
    for layout in LAYOUTS {
        let data = tmp.path().join(format!("v{}", layout.control_version));
        synthetic(&data, layout, 42, 42);
        let before = fs::read(data.join("global/pg_control")).unwrap();
        assert_eq!(distinguish_with(&data, 7, [0xAB; NONCE_LEN]).unwrap(), 7);
        let after = fs::read(data.join("global/pg_control")).unwrap();
        for (i, (a, b)) in before.iter().zip(&after).enumerate() {
            let field = i < 8 || (layout.nonce..layout.nonce + NONCE_LEN).contains(&i) || (layout.crc..layout.crc + 4).contains(&i);
            assert!(field || a == b, "{}: byte {i} changed", layout.control_version);
        }
        assert_eq!(after[..8], 7u64.to_le_bytes());
        assert_eq!(after[layout.nonce..layout.nonce + NONCE_LEN], [0xAB; NONCE_LEN]);
        // On 18 the bytes between the nonce and the checksum are padding: untouched.
        assert_eq!(after[layout.nonce + NONCE_LEN..layout.crc], before[layout.nonce + NONCE_LEN..layout.crc]);
        assert_eq!(after[layout.crc..layout.crc + 4], crc32c(&after[..layout.crc]).to_le_bytes());
        let wal = fs::read(data.join("pg_wal/000000010000000000000001")).unwrap();
        assert_eq!(wal[24..32], 7u64.to_le_bytes());
    }

    let refused = |name: &str, prepare: &dyn Fn(&Path)| {
        let data = tmp.path().join(name);
        synthetic(&data, &LAYOUTS[0], 42, 42);
        prepare(&data);
        let files = || (fs::read(data.join("global/pg_control")).unwrap(), fs::read(data.join("pg_wal/000000010000000000000001")).unwrap());
        let before = files();
        assert!(distinguish_with(&data, 7, [0xAB; NONCE_LEN]).is_err(), "{name}");
        assert_eq!(before, files(), "{name}: nothing written");
    };
    // A version whose layout is not verified (16, say), even with a checksum that would match.
    refused("unknown", &|d| synthetic(d, &Layout { major: 16, control_version: 1600, wal_magic: 0xD116, nonce: 256, crc: 288 }, 42, 42));
    // The checksum not where the layout keeps it.
    refused("crc", &|d| {
        let mut c = fs::read(d.join("global/pg_control")).unwrap();
        c[100] ^= 1;
        fs::write(d.join("global/pg_control"), c).unwrap();
    });
    // A WAL segment of another system, or of another release's page format.
    refused("other system", &|d| synthetic(d, &LAYOUTS[0], 42, 9));
    refused("other magic", &|d| {
        let p = d.join("pg_wal/000000010000000000000001");
        let mut w = fs::read(&p).unwrap();
        w[..2].copy_from_slice(&0xD118u16.to_le_bytes());
        fs::write(p, w).unwrap();
    });
}

/// A fake install: `bin/initdb` (running `initdb_script`) and `bin/postgres`, `lib` with a
/// plpgsql module, `share` with `postgres.bki` and `extension`.
fn fake_install(at: &Path, initdb_script: &str) -> PathBuf {
    let bin = at.join("bin");
    for dir in ["bin", "lib", "share/extension", "share/timezonesets"] {
        fs::create_dir_all(at.join(dir)).unwrap();
    }
    for (name, script) in [("initdb", initdb_script), ("postgres", "#!/bin/sh\nexit 0\n")] {
        fs::write(bin.join(name), script).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(at.join("lib/plpgsql.so"), "module").unwrap();
    fs::write(at.join("share/postgres.bki"), "bki").unwrap();
    fs::write(at.join("share/extension/plpgsql.control"), "control").unwrap();
    bin.canonicalize().unwrap()
}

fn path_env(bin: &Path) -> impl Fn(&str) -> Option<OsString> + '_ {
    move |name: &str| (name == "PATH").then(|| bin.as_os_str().to_os_string())
}

#[test]
fn a_template_is_keyed_by_what_initdb_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let bin = fake_install(&install, "#!/bin/sh\nexit 0\n");
    let key = || Recipe::new(&bin.join("initdb"), &path_env(&bin)).map(|r| r.key);
    let vars = |extra: &'static [(&'static str, &'static str)]| {
        let bin = bin.clone();
        move |name: &str| match name {
            "PATH" => Some(bin.as_os_str().to_os_string()),
            _ => extra.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(v)),
        }
    };
    let with = |extra| Recipe::new(&bin.join("initdb"), &vars(extra)).unwrap().key;
    let base = key().unwrap();
    assert_eq!(base, key().unwrap(), "the same inputs, the same template");
    assert_ne!(base, with(&[("LC_ALL", "C")]), "locale");
    assert_ne!(base, with(&[("TZ", "UTC")]), "time zone");
    assert_ne!(base, with(&[("LANG", "en_US.UTF-8")]), "LANG");
    assert!(!Recipe::new(&bin.join("initdb"), &path_env(&bin)).unwrap().settled(), "an install changed in the last seconds keys nothing");
    let mut seen = vec![base];
    let mut changed = |what: &str| {
        let now = key().unwrap();
        assert!(!seen.contains(&now), "{what} changed nothing");
        seen.push(now);
    };
    // Initdb's inputs one level down, edited in place.
    fs::write(install.join("share/extension/plpgsql.control"), "edited").unwrap();
    changed("an extension script edited in place");
    fs::write(install.join("share/timezonesets/Default"), "x").unwrap();
    changed("a timezone set added");
    fs::write(install.join("lib/plpgsql.so"), "rebuilt module").unwrap();
    changed("the plpgsql module rewritten");
    // A link child: its text and what it leads to.
    let outside = tmp.path().join("outside.sql");
    fs::write(&outside, "one").unwrap();
    std::os::unix::fs::symlink(&outside, install.join("share/extension/linked.sql")).unwrap();
    changed("a linked script added");
    fs::write(&outside, "two, longer").unwrap();
    changed("a link's target rewritten outside the install");
    let other_dir = tmp.path().join("linked-dir");
    fs::create_dir_all(&other_dir).unwrap();
    std::os::unix::fs::symlink(&other_dir, install.join("share/extension/dir")).unwrap();
    changed("a linked directory added");
    fs::write(other_dir.join("f"), "x").unwrap();
    changed("an entry added in a linked directory");
    fs::remove_file(bin.join("postgres")).unwrap();
    fs::write(bin.join("postgres"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(bin.join("postgres"), fs::Permissions::from_mode(0o755)).unwrap();
    changed("the server replaced");

    // No key: a dangling link, a loop, no postgres.bki, no plpgsql module.
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), install.join("share/extension/dangling")).unwrap();
    assert!(key().unwrap_err().contains("leads nowhere"));
    fs::remove_file(install.join("share/extension/dangling")).unwrap();
    std::os::unix::fs::symlink(install.join("share/extension/loop"), install.join("share/extension/loop")).unwrap();
    assert!(key().is_err(), "a loop");
    fs::remove_file(install.join("share/extension/loop")).unwrap();
    key().unwrap();
    fs::rename(install.join("lib/plpgsql.so"), install.join("lib/other.so")).unwrap();
    assert!(key().unwrap_err().contains("plpgsql"));
    fs::rename(install.join("lib/other.so"), install.join("lib/plpgsql.so")).unwrap();
    // The share directory may be share/postgresql; with postgres.bki in neither, no key.
    fs::create_dir_all(install.join("share/postgresql/extension")).unwrap();
    fs::rename(install.join("share/postgres.bki"), install.join("share/postgresql/postgres.bki")).unwrap();
    key().unwrap();
    fs::remove_file(install.join("share/postgresql/postgres.bki")).unwrap();
    assert!(key().unwrap_err().contains("postgres.bki"));

    // The postgres on PATH must be the one initdb runs; a relative PATH entry finds nothing.
    fs::write(install.join("share/postgres.bki"), "bki").unwrap();
    let other = fake_install(&tmp.path().join("other"), "#!/bin/sh\nexit 0\n");
    let path = std::env::join_paths([other.clone(), bin.clone()]).unwrap();
    let split = move |name: &str| (name == "PATH").then(|| path.clone());
    assert!(Recipe::new(&bin.join("initdb"), &split).unwrap_err().contains("not the one beside"));
    let relative = std::env::join_paths([PathBuf::from("rel"), bin.clone()]).unwrap();
    assert!(find(&relative, "initdb").is_none());
    assert_eq!(find(bin.as_os_str(), "initdb"), Some(bin.join("initdb")));
}

/// A fake `initdb` that copies `fixture` to the directory it is given, failing for a target
/// under `templates/` when `fail_in_templates`, and everywhere when `fail_everywhere`.
fn copying_initdb(fixture: &Path, fail_in_templates: bool, fail_everywhere: bool) -> String {
    let mut script = String::from("#!/bin/sh\n");
    if fail_everywhere {
        script.push_str("echo 'initdb: no space' >&2; exit 1\n");
    }
    if fail_in_templates {
        script.push_str("case \"$2\" in */templates/*) echo 'initdb: no space left on device' >&2; exit 1;; esac\n");
    }
    script.push_str(&format!("cp -R '{}' \"$2\"\n", fixture.display()));
    script
}

fn fixture(at: &Path, layout: &Layout, conf: &str) {
    synthetic(at, layout, 42, 42);
    fs::write(at.join("postgresql.conf"), conf).unwrap();
    fs::write(at.join("PG_VERSION"), "17\n").unwrap();
}

const DEFAULT_CONF: &str = "max_connections = 100\t\t\t# (change requires restart)\nshared_buffers = 128MB\t\t\t# min 128kB\n";

#[test]
fn only_a_build_like_any_other_is_kept_and_a_failed_one_retries_at_the_data_directory() {
    let _settled = Settled::new();
    let tmp = tempfile::tempdir().unwrap();
    let run = |name: &str, conf: &str, layout: &Layout, fail_in_templates: bool, fail_everywhere: bool| {
        let base = tmp.path().join(name);
        fixture(&base.join("fixture"), layout, conf);
        let bin = fake_install(&base.join("install"), &copying_initdb(&base.join("fixture"), fail_in_templates, fail_everywhere));
        let root = base.join("templates");
        fs::create_dir_all(base.join("staging")).unwrap();
        let target = base.join("staging/data");
        let made = super::make(Some(&root), "postgres:17", &target, &path_env(&bin));
        (made, root, target)
    };

    // Built during a shortage: used for this data directory, not kept.
    let (made, root, target) = run("short", "max_connections = 25\nshared_buffers = 128MB\n", &LAYOUTS[0], false, false);
    let notes = made.unwrap();
    assert!(notes[0].contains("not kept as a template") && notes[0].contains("max_connections = 25"), "{notes:?}");
    assert_eq!(fs::read_to_string(target.join("postgresql.conf")).unwrap(), "max_connections = 25\nshared_buffers = 128MB\n");
    assert_eq!(fs::read_to_string(target.join(crate::supervisor::VERSION_FILE)).unwrap(), "postgres:17");
    assert!(dirs_in(&root).is_empty(), "nothing kept, nothing left: {:?}", dirs_in(&root));

    // A build whose identity cannot be edited is never used: see
    // `a_build_the_identity_edit_refuses_is_neither_the_data_directory_nor_a_template`.

    // An unconstrained build of a verified layout is kept, and cloned from next time.
    let (made, root, _) = run("kept", DEFAULT_CONF, &LAYOUTS[1], false, false);
    assert!(made.unwrap()[0].contains("built now"));
    assert_eq!(dirs_in(&root).len(), 1);

    // The template's initdb fails where the template is: initdb runs once at the data directory.
    let (made, root, target) = run("full", DEFAULT_CONF, &LAYOUTS[0], true, false);
    let notes = made.unwrap();
    assert!(notes[0].contains("the template's initdb failed") && notes[0].contains("running initdb for this data directory"), "{notes:?}");
    assert!(target.join("global/pg_control").exists() && target.join(crate::supervisor::VERSION_FILE).exists());
    assert!(dirs_in(&root).is_empty(), "the failed build was discarded: {:?}", dirs_in(&root));

    // Both fail: both reported.
    let (made, _, _) = run("broken", DEFAULT_CONF, &LAYOUTS[0], false, true);
    let e = made.unwrap_err();
    assert!(e.contains("the template's initdb failed") && e.contains("the data directory could not be made either"), "{e}");

    // No usable template directory at all (a file in its place): initdb at the data directory.
    let base = tmp.path().join("nodir");
    fixture(&base.join("fixture"), &LAYOUTS[0], DEFAULT_CONF);
    let bin = fake_install(&base.join("install"), &copying_initdb(&base.join("fixture"), false, false));
    fs::write(base.join("templates"), "").unwrap();
    fs::create_dir_all(base.join("staging")).unwrap();
    let notes = super::make(Some(&base.join("templates")), "postgres:17", &base.join("staging/data"), &path_env(&bin)).unwrap();
    assert!(notes[0].contains("running initdb for this data directory"), "{notes:?}");
    assert!(base.join("staging/data/global/pg_control").exists());
}

/// A fake `initdb` that copies `bad` to a target under `templates/` (a template build) and
/// `good` anywhere else (the data directory).
fn two_fixture_initdb(good: &Path, bad: &Path) -> String {
    format!("#!/bin/sh\ncase \"$2\" in */templates/*) cp -R '{}' \"$2\";; *) cp -R '{}' \"$2\";; esac\n", bad.display(), good.display())
}

/// Damage the cluster at `at` as `how` says: the `pg_control` checksum (byte 292 of an 18
/// layout), its version, or the first WAL segment's page magic.
fn damage(at: &Path, how: &str) {
    let edit = |file: &Path, offset: usize, bytes: &[u8]| {
        let mut content = fs::read(file).unwrap();
        content[offset..offset + bytes.len()].copy_from_slice(bytes);
        fs::write(file, content).unwrap();
    };
    let control = at.join("global/pg_control");
    match how {
        "checksum" => {
            let byte = fs::read(&control).unwrap()[LAYOUTS[1].crc];
            edit(&control, LAYOUTS[1].crc, &[!byte]);
        }
        "version" => edit(&control, CONTROL_VERSION_AT, &0x9999u32.to_le_bytes()),
        "wal magic" => edit(&at.join("pg_wal/000000010000000000000001"), 0, &[0, 0]),
        other => panic!("{other}"),
    }
}

#[test]
fn a_build_the_identity_edit_refuses_is_neither_the_data_directory_nor_a_template() {
    let _settled = Settled::new();
    let tmp = tempfile::tempdir().unwrap();
    for how in ["checksum", "version", "wal magic"] {
        let base = tmp.path().join(how.replace(' ', "-"));
        let (good, bad) = (base.join("good"), base.join("bad"));
        fixture(&good, &LAYOUTS[1], DEFAULT_CONF);
        fixture(&bad, &LAYOUTS[1], DEFAULT_CONF);
        damage(&bad, how);
        let bin = fake_install(&base.join("install"), &two_fixture_initdb(&good, &bad));
        let root = base.join("templates");
        fs::create_dir_all(base.join("staging")).unwrap();
        let target = base.join("staging/data");
        let notes = super::make(Some(&root), "postgres:18", &target, &path_env(&bin)).unwrap();
        assert!(notes[0].contains("running initdb for this data directory") && notes[0].contains("discarded"), "{how}: {notes:?}");
        // The data directory is what initdb wrote there, untouched: not the build, not a clone.
        for file in ["global/pg_control", "pg_wal/000000010000000000000001", "postgresql.conf"] {
            assert_eq!(fs::read(target.join(file)).unwrap(), fs::read(good.join(file)).unwrap(), "{how}: {file}");
        }
        assert_eq!(fs::read_to_string(target.join(crate::supervisor::VERSION_FILE)).unwrap(), "postgres:18");
        assert!(dirs_in(&root).is_empty(), "{how}: nothing published, no build left: {:?}", dirs_in(&root));
        // And the next start tries again, with the same result.
        let again = base.join("staging/again");
        assert!(super::make(Some(&root), "postgres:18", &again, &path_env(&bin)).unwrap()[0].contains("discarded"));
        assert!(dirs_in(&root).is_empty());
    }
}

#[test]
fn a_published_template_whose_copy_the_edit_refuses_is_removed() {
    let _settled = Settled::new();
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good");
    fixture(&good, &LAYOUTS[1], DEFAULT_CONF);
    let bin = fake_install(&tmp.path().join("install"), &copying_initdb(&good, false, false));
    let root = tmp.path().join("templates");
    fs::create_dir_all(tmp.path().join("staging")).unwrap();
    assert!(super::make(Some(&root), "postgres:18", &tmp.path().join("staging/a"), &path_env(&bin)).unwrap()[0].contains("built now"));
    // As a template an earlier Stack published with a WAL segment the edit refuses: its record
    // matches what is there, so only the edit of its copy can tell.
    let key = dirs_in(&root).remove(0);
    let dir = root.join(&key);
    damage(&dir.join("data"), "wal magic");
    let mut record = read_any_record(&dir).unwrap();
    record.digest = tree_digest(&dir.join("data")).unwrap().0;
    fs::write(dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();

    let target = tmp.path().join("staging/b");
    let notes = super::make(Some(&root), "postgres:18", &target, &path_env(&bin)).unwrap();
    assert!(notes[0].contains("cannot give the copy") && notes[0].contains("not a WAL segment") && notes[0].contains("it was removed"), "{notes:?}");
    assert!(dirs_in(&root).is_empty(), "{:?}", dirs_in(&root));
    for file in ["global/pg_control", "pg_wal/000000010000000000000001"] {
        assert_eq!(fs::read(target.join(file)).unwrap(), fs::read(good.join(file)).unwrap(), "{file}: initdb's, untouched");
    }
    // The next start builds a good one again.
    assert!(super::make(Some(&root), "postgres:18", &tmp.path().join("staging/c"), &path_env(&bin)).unwrap()[0].contains("built now"));
}

/// A fake install whose `initdb` copies a good 18 fixture, the template built from it, and a
/// way to make that template one an earlier Stack published with a WAL segment the edit
/// refuses (its record matching what is there).
struct Published {
    tmp: tempfile::TempDir,
    good: PathBuf,
    bin: PathBuf,
    root: PathBuf,
    dir: PathBuf,
}

impl Published {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good");
        fixture(&good, &LAYOUTS[1], DEFAULT_CONF);
        let bin = fake_install(&tmp.path().join("install"), &format!("#!/bin/sh\n# Fails at a data directory named fail-*.\ncase \"$2\" in */staging/fail-*) echo 'initdb: no space left on device' >&2; exit 1;; esac\n{}", copying_initdb(&good, false, false).trim_start_matches("#!/bin/sh\n")));
        let root = super::root(&tmp.path().join("state"));
        fs::create_dir_all(tmp.path().join("staging")).unwrap();
        let published = Self { dir: PathBuf::new(), good, bin, root, tmp };
        assert!(published.make("first").unwrap()[0].contains("built now"));
        let dir = published.root.join(dirs_in(&published.root).remove(0));
        Self { dir, ..published }
    }

    fn make(&self, name: &str) -> Result<Vec<String>, String> {
        super::make(Some(&self.root), "postgres:18", &self.tmp.path().join("staging").join(name), &path_env(&self.bin))
    }

    fn initialise(&self, name: &str, retire_wait: Duration) -> Result<String, Failure> {
        let initdb = self.bin.join("initdb");
        let recipe = Recipe::new(&initdb, &path_env(&self.bin)).unwrap();
        initialise_waiting(&self.root, &recipe, &initdb, "postgres:18", &self.tmp.path().join("staging").join(name), &path_env(&self.bin), (BUILD_WAIT, retire_wait))
    }

    fn damage_wal(&self) {
        damage(&self.dir.join("data"), "wal magic");
        let mut record = read_any_record(&self.dir).unwrap();
        record.digest = tree_digest(&self.dir.join("data")).unwrap().0;
        fs::write(self.dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
    }

    fn key(&self) -> String {
        self.dir.file_name().unwrap().to_string_lossy().into_owned()
    }

    fn record(&self) -> Record {
        read_any_record(&self.dir).unwrap()
    }

    /// Generation B in A's place: a new directory (new inodes) with its own matching record.
    fn replace(&self) -> PathBuf {
        let aside = self.tmp.path().join(format!("aside-{}", dirs_in(self.tmp.path()).len()));
        fs::rename(&self.dir, &aside).unwrap();
        clone_tree(&aside, &self.dir).unwrap();
        let mut record = self.record();
        record.digest = tree_digest(&self.dir.join("data")).unwrap().0;
        fs::write(self.dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
        aside
    }

    fn lock(&self) -> crate::state::FileLock {
        crate::state::FileLock::try_acquire(&self.lock_path()).unwrap().unwrap()
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(format!("{}.lock", self.dir.file_name().unwrap().to_string_lossy()))
    }
}

#[test]
fn a_bad_template_whose_lock_is_held_is_left_in_place_and_the_log_says_so() {
    let _settled = Settled::new();
    let p = Published::new();
    p.damage_wal();
    let held = p.lock();
    let started = Instant::now();
    match p.initialise("b", Duration::from_millis(100)) {
        Err(Failure::Fallback(why)) => {
            assert!(why.contains("cannot give the copy") && why.contains("its lock could not be taken within the wait, so nothing was checked or removed"), "{why}");
            assert!(!why.contains("it was removed"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(4), "the injected wait, not the default");
    assert!(p.dir.join(RECORD).exists(), "still there, as the log says");
    assert!(!p.tmp.path().join("staging/b").exists(), "and not used");
    // Through `make`, the data directory is initdb's, untouched.
    let notes = p.make("c").unwrap();
    assert!(notes[0].contains("could not be taken within the wait"), "{notes:?}");
    assert_eq!(fs::read(p.tmp.path().join("staging/c/pg_wal/000000010000000000000001")).unwrap(), fs::read(p.good.join("pg_wal/000000010000000000000001")).unwrap());
    drop(held);
    // The first start that can take the lock removes it.
    match p.initialise("d", Duration::from_millis(100)) {
        Err(Failure::Fallback(why)) => assert!(why.ends_with("it was removed"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(!p.dir.exists());
}

#[test]
fn retire_reports_what_became_of_the_template() {
    let _settled = Settled::new();
    let p = Published::new();
    let key = p.key();
    let wait = Duration::from_millis(50);
    let record_a = p.record();
    // Another template in its place (a rebuild): left alone, said so.
    let aside = p.replace();
    let record_b = p.record();
    assert_ne!(record_a, record_b);
    assert_eq!(retire(&p.root, &key, &p.dir, &record_a, false, None, wait), Retired::Replaced);
    assert!(p.dir.join(RECORD).exists());
    // A record that cannot be read is not a replacement.
    fs::set_permissions(p.dir.join(RECORD), fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = retire(&p.root, &key, &p.dir, &record_b, false, None, wait);
    fs::set_permissions(p.dir.join(RECORD), fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(&unreadable, Retired::Failed(e) if e.contains("cannot read its record")), "{unreadable:?}");
    // A lock that cannot be opened is not contention.
    fs::remove_file(p.lock_path()).unwrap();
    fs::create_dir(p.lock_path()).unwrap();
    let unlockable = retire(&p.root, &key, &p.dir, &record_b, false, None, wait);
    fs::remove_dir(p.lock_path()).unwrap();
    assert!(matches!(&unlockable, Retired::Failed(e) if e.contains("cannot lock")), "{unlockable:?}");
    assert!(unlockable.says().starts_with("it was left in place: cannot lock"), "{}", unlockable.says());
    // A record missing from a directory still there, or one that does not parse: not a
    // replacement either.
    let bytes = fs::read(p.dir.join(RECORD)).unwrap();
    fs::remove_file(p.dir.join(RECORD)).unwrap();
    let missing = retire(&p.root, &key, &p.dir, &record_b, false, None, wait);
    fs::write(p.dir.join(RECORD), b"{").unwrap();
    let garbled = retire(&p.root, &key, &p.dir, &record_b, false, None, wait);
    fs::write(p.dir.join(RECORD), &bytes).unwrap();
    assert!(matches!(&missing, Retired::Failed(e) if e.contains("cannot read its record")), "{missing:?}");
    assert!(matches!(&garbled, Retired::Failed(e) if e.contains("cannot read its record")), "{garbled:?}");
    // Held past the wait.
    let held = p.lock();
    assert_eq!(retire(&p.root, &key, &p.dir, &record_b, false, None, wait), Retired::LockHeld);
    // A template that passes the check is kept.
    assert_eq!(retire(&p.root, &key, &p.dir, &record_b, true, Some(held), wait), Retired::Kept);
    // Otherwise the generation copied goes, and then is gone.
    assert_eq!(retire(&p.root, &key, &p.dir, &record_b, false, None, wait), Retired::Removed);
    assert!(!p.dir.exists());
    assert_eq!(retire(&p.root, &key, &p.dir, &record_b, false, None, wait), Retired::Replaced, "already gone");
    assert!(aside.join(RECORD).exists(), "A, set aside by the test, untouched");
}

/// Restores a directory's mode when dropped.
struct Mode(PathBuf);

impl Mode {
    fn set(path: &Path, mode: u32) -> Self {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        Self(path.to_path_buf())
    }
}

impl Drop for Mode {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

#[test]
fn a_template_that_cannot_be_moved_aside_is_said_to_be_left_and_gc_does_not_list_it() {
    let _settled = Settled::new();
    let p = Published::new();
    p.damage_wal();
    let read_only = Mode::set(&p.root, 0o500);
    let notes = p.make("b").unwrap();
    assert!(notes[0].contains("it was left in place: cannot move it aside") && !notes[0].contains("it was removed"), "{notes:?}");
    assert_eq!(fs::read(p.tmp.path().join("staging/b/pg_wal/000000010000000000000001")).unwrap(), fs::read(p.good.join("pg_wal/000000010000000000000001")).unwrap(), "initdb's");
    assert!(p.dir.join(RECORD).exists(), "still there, as the log says");
    // `stack gc` judges it unusable but cannot move it either: not listed as removed.
    let mut record = p.record();
    record.format = "stack-pg-template-2".into();
    fs::write(p.dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(collect(&p.tmp.path().join("state"), true).len(), 1, "a dry run lists it");
    assert!(collect(&p.tmp.path().join("state"), false).is_empty());
    assert!(p.dir.join(RECORD).exists());
    drop(read_only);
    assert_eq!(collect(&p.tmp.path().join("state"), false).len(), 1);
    assert!(!p.dir.exists());
}

#[test]
fn a_record_is_read_between_two_looks_at_its_directory_and_kept_only_if_they_agree() {
    let calls = std::cell::RefCell::new(Vec::new());
    let look = |ids: &'static [u8]| {
        let calls = &calls;
        move || {
            let n = calls.borrow().iter().filter(|c| **c == "look").count();
            calls.borrow_mut().push("look");
            ids.get(n).copied()
        }
    };
    let read = |r: Option<&'static str>| {
        let calls = &calls;
        move || {
            calls.borrow_mut().push("read");
            r
        }
    };
    assert_eq!(unmoved(look(&[7, 7]), read(Some("a"))), Some("a"));
    assert_eq!(*calls.borrow(), ["look", "read", "look"], "the directory is looked at before the read and after it");
    calls.borrow_mut().clear();
    assert_eq!(unmoved(look(&[7, 8]), read(Some("a"))), None, "replaced while it was read");
    assert_eq!(*calls.borrow(), ["look", "read", "look"]);
    calls.borrow_mut().clear();
    assert_eq!(unmoved(look(&[7, 7]), read(None)), None, "no record");
    assert_eq!(*calls.borrow(), ["look", "read"], "and the directory is not looked at again");
    calls.borrow_mut().clear();
    assert_eq!(unmoved(look(&[]), read(Some("a"))), None, "no directory");
    assert_eq!(*calls.borrow(), ["look"], "and nothing is read");
    calls.borrow_mut().clear();
    assert_eq!(unmoved(look(&[7]), read(Some("a"))), None, "gone after the read");
    assert_eq!(*calls.borrow(), ["look", "read", "look"]);
}

#[test]
fn a_template_replaced_while_its_record_is_read_is_read_again_under_the_lock() {
    let _settled = Settled::new();
    // B is published at each of generation's three points: just before its first look at the
    // directory (so a record read any earlier would be A's), after that look and before it
    // reads the record, and after it read A's record and before it looks again.
    for point in [0, 1, 2] {
        let p = Published::new();
        let record_a = p.record();
        let replaced = std::rc::Rc::new(std::cell::RefCell::new(None));
        let slot = replaced.clone();
        let publish = Published { tmp: tempfile::tempdir().unwrap(), good: p.good.clone(), bin: p.bin.clone(), root: p.root.clone(), dir: p.dir.clone() };
        BETWEEN_READS.with(|h| *h.borrow_mut() = Some((point, Box::new(move || *slot.borrow_mut() = Some(publish.replace())))));
        let note = p.initialise("b", Duration::from_millis(50)).unwrap();
        assert!(replaced.borrow().is_some(), "point {point}: the hook ran");
        assert!(note.contains("initialised from the template") && !note.contains("built now"), "point {point}: {note}");
        let record_b = p.record();
        assert_ne!(record_a, record_b, "point {point}: B, not A, is the template");
        assert_eq!(tree_digest(&p.dir.join("data")).unwrap().0, record_b.digest, "point {point}: and B is kept, unchanged");
    }
}

#[test]
fn the_retirement_outcome_is_reported_when_initdb_fails_too() {
    let _settled = Settled::new();
    let p = Published::new();
    p.damage_wal();
    fs::remove_file(p.lock_path()).unwrap();
    fs::create_dir(p.lock_path()).unwrap();
    let e = p.make("fail-a").unwrap_err();
    fs::remove_dir(p.lock_path()).unwrap();
    assert!(e.contains("cannot give the copy") && e.contains("it was left in place: cannot lock") && e.contains("the data directory could not be made: initdb failed"), "{e}");
}

#[test]
fn a_copy_damaged_after_the_clone_keeps_a_good_template_and_a_bad_template_goes() {
    let _settled = Settled::new();
    let p = Published::new();
    BEFORE_EDIT.with(|h| *h.borrow_mut() = Some(Box::new(|data: &Path| fs::remove_file(data.join("global/pg_control")).unwrap())));
    let notes = p.make("b").unwrap();
    assert!(notes[0].contains("cannot give the copy") && notes[0].contains("passes the check, so it was kept"), "{notes:?}");
    assert!(p.tmp.path().join("staging/b/global/pg_control").exists(), "initdb's");
    assert!(p.make("c").unwrap()[0].contains("initialised from the template"), "the next start clones it");
    // The same failure from the template itself: it goes.
    p.damage_wal();
    let notes = p.make("d").unwrap();
    assert!(notes[0].contains("not a WAL segment") && notes[0].contains("it was removed"), "{notes:?}");
    assert!(!p.dir.exists());
}

#[test]
fn a_failure_writing_the_copy_keeps_a_good_template() {
    let _settled = Settled::new();
    let p = Published::new();
    EDIT_WRITE_FAILS.with(|f| f.set(true));
    let result = p.make("b");
    EDIT_WRITE_FAILS.with(|f| f.set(false));
    let notes = result.unwrap();
    assert!(notes[0].contains("No space left on device") && notes[0].contains("passes the check, so it was kept"), "{notes:?}");
    assert!(p.dir.join(RECORD).exists());
    assert!(p.make("c").unwrap()[0].contains("initialised from the template"));
}

#[test]
fn a_template_of_an_earlier_format_is_not_cloned() {
    let _settled = Settled::new();
    let p = Published::new();
    let mut record = read_any_record(&p.dir).unwrap();
    record.format = "stack-pg-template-2".into();
    fs::write(p.dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(p.make("b").unwrap()[0].contains("built now"), "rebuilt, not cloned");
    assert_eq!(read_any_record(&p.dir).unwrap().format, FORMAT);
}

#[test]
fn a_release_without_a_verified_layout_builds_no_template() {
    let _settled = Settled::new();
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good");
    fixture(&good, &Layout { major: 16, control_version: 1300, wal_magic: 0xD113, nonce: 256, crc: 288 }, DEFAULT_CONF);
    // An initdb that fails in a template build: not run there at all.
    let bin = fake_install(&tmp.path().join("install"), &copying_initdb(&good, true, false));
    let root = tmp.path().join("templates");
    fs::create_dir_all(tmp.path().join("staging")).unwrap();
    let target = tmp.path().join("staging/data");
    let notes = super::make(Some(&root), "postgres:16", &target, &path_env(&bin)).unwrap();
    assert!(notes[0].contains("running initdb for this data directory: no template for Postgres 16"), "{notes:?}");
    assert_eq!(fs::read(target.join("global/pg_control")).unwrap(), fs::read(good.join("global/pg_control")).unwrap());
    assert!(!root.exists(), "nothing made under templates");
}

#[test]
fn every_share_directory_holding_postgres_bki_is_keyed() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("install");
    let bin = fake_install(&install, "#!/bin/sh\nexit 0\n");
    // Both `share` and `share/postgresql` hold postgres.bki, the second a link to a directory
    // outside the install.
    let outside = tmp.path().join("pgshare");
    fs::create_dir_all(outside.join("extension")).unwrap();
    fs::write(outside.join("postgres.bki"), "bki").unwrap();
    fs::write(outside.join("extension/plpgsql.control"), "control").unwrap();
    std::os::unix::fs::symlink(&outside, install.join("share/postgresql")).unwrap();
    let key = || Recipe::new(&bin.join("initdb"), &path_env(&bin)).unwrap().key;
    let mut seen = vec![key()];
    let mut changed = |what: &str| {
        let now = key();
        assert!(!seen.contains(&now), "{what} changed nothing");
        seen.push(now);
    };
    let rewrite = |path: &Path, text: &str| {
        let mut f = fs::OpenOptions::new().write(true).truncate(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    };
    rewrite(&install.join("share/postgresql/postgres.bki"), "bki edited in place");
    changed("share/postgresql/postgres.bki edited in place");
    rewrite(&install.join("share/postgres.bki"), "bki edited in place");
    changed("share/postgres.bki edited in place");
    rewrite(&install.join("share/postgresql/extension/plpgsql.control"), "edited");
    changed("an extension script under share/postgresql edited in place");
    fs::create_dir_all(outside.join("tsearch_data")).unwrap();
    changed("share/postgresql/tsearch_data added");
    fs::write(outside.join("tsearch_data/english.stop"), "a").unwrap();
    changed("a dictionary added under share/postgresql/tsearch_data");
    fs::create_dir_all(outside.join("timezonesets")).unwrap();
    changed("share/postgresql/timezonesets added");
    fs::write(outside.join("timezonesets/Default"), "x").unwrap();
    changed("a timezone set added under share/postgresql/timezonesets");
}

#[test]
fn every_c_library_and_musl_loader_is_keyed_including_multiarch_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, usr) = (tmp.path().join("lib"), tmp.path().join("usr-lib"));
    for dir in ["lib/x86_64-linux-gnu", "lib/x86_64-linux-musl", "usr-lib/aarch64-linux-gnu"] {
        fs::create_dir_all(tmp.path().join(dir)).unwrap();
    }
    let files = ["lib/ld-musl-x86_64.so.1", "lib/x86_64-linux-gnu/libc.so.6", "lib/x86_64-linux-musl/ld-musl-x86_64.so.1", "lib/x86_64-linux-musl/libc.musl-x86_64.so.1", "usr-lib/aarch64-linux-gnu/libc.so.6", "lib/x86_64-linux-gnu/libm.so.6"];
    for f in files {
        fs::write(tmp.path().join(f), f).unwrap();
    }
    let print = || {
        let mut print = Fingerprint::default();
        let found = c_libraries(&mut print, &[&lib, &usr, &tmp.path().join("absent")]).unwrap();
        (found, print.text)
    };
    let (found, before) = print();
    assert_eq!(found, 5, "every C library and loader, and nothing else: {before}");
    // A loader in a multiarch directory replaced (a new file, as a package upgrade writes).
    let loader = tmp.path().join("lib/x86_64-linux-musl/ld-musl-x86_64.so.1");
    fs::remove_file(&loader).unwrap();
    fs::write(&loader, "new loader").unwrap();
    let (_, after) = print();
    assert_ne!(before, after);
    // A multiarch directory that can be searched but not listed may hold a C library: no key.
    let hidden = tmp.path().join("lib/x86_64-linux-gnu");
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o311)).unwrap();
    let mut listing = Fingerprint::default();
    let refused = c_libraries(&mut listing, &[&lib]);
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(refused.as_ref().is_err_and(|e| e.contains("cannot list") && e.contains("x86_64-linux-gnu")), "{refused:?}");
    // A multiarch link whose target cannot be reached (an unsearchable parent) is no key either.
    let base = tmp.path().join("linked");
    fs::create_dir_all(tmp.path().join("locked/real")).unwrap();
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("libc.so.6"), "visible").unwrap();
    std::os::unix::fs::symlink(tmp.path().join("locked/real"), base.join("x86_64-linux-gnu")).unwrap();
    let locked = Mode::set(&tmp.path().join("locked"), 0o600);
    let mut probe = Fingerprint::default();
    let refused = c_libraries(&mut probe, &[&base]);
    drop(locked);
    assert!(refused.as_ref().is_err_and(|e| e.contains("cannot read") && e.contains("x86_64-linux-gnu")), "{refused:?}");
    // A dangling loader link there is no key, as anywhere else.
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), tmp.path().join("lib/x86_64-linux-gnu/ld-musl-x86_64.so.1")).unwrap();
    let mut print = Fingerprint::default();
    assert!(c_libraries(&mut print, &[&lib]).unwrap_err().contains("leads nowhere"));
}

#[test]
fn existing_postgres_data_is_never_touched() {
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("postgres");
    fs::write(&script, "#!/bin/sh\necho 'postgres (PostgreSQL) 17.12'\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let p = crate::supervisor::Preset { name: "postgres", server: Box::leak(script.to_string_lossy().into_owned().into_boxed_str()), stop_signal: libc::SIGINT, keeps_data: true };
    let root = tmp.path().join("templates");
    let data = tmp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("PG_VERSION"), "17").unwrap();
    let e = crate::supervisor::init_data(&p, &data, None, Some(&root)).unwrap_err();
    assert!(e.contains("not initialized completely"), "{e}");
    fs::write(data.join(crate::supervisor::VERSION_FILE), "postgres:16").unwrap();
    assert!(crate::supervisor::init_data(&p, &data, None, Some(&root)).unwrap_err().contains("incompatible postgres data"));
    fs::write(data.join(crate::supervisor::VERSION_FILE), "postgres:17").unwrap();
    crate::supervisor::init_data(&p, &data, None, Some(&root)).unwrap();
    assert!(!root.exists(), "no template is made or read for existing data");
    assert_eq!(listing(&data).len(), 3);
    assert_eq!(fs::read_to_string(data.join("PG_VERSION")).unwrap(), "17");
}

fn fake_template(root: &Path, name: &str, install: &Path, snapshot: Option<String>, change: &dyn Fn(&mut Record)) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("data")).unwrap();
    let mut record = Record {
        format: FORMAT.into(),
        recipe: name.into(),
        digest: String::new(),
        coarse: false,
        newest_ms: 0,
        install: install.to_path_buf(),
        install_snapshot: snapshot.unwrap_or_else(|| install_snapshot(install).unwrap().0),
        built_at: 0,
        full_flush: true,
        boot: None,
    };
    change(&mut record);
    fs::write(dir.join(RECORD), serde_json::to_vec(&record).unwrap()).unwrap();
    dir
}

#[test]
fn gc_removes_only_templates_nothing_can_use_and_judges_each_under_its_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let root = root(&state);
    let install = fake_install(&tmp.path().join("install"), "#!/bin/sh\nexit 0\n").parent().unwrap().to_path_buf();
    let keep = |_: &mut Record| {};
    let kept = fake_template(&root, "aaaa", &install, None, &keep);
    fake_template(&root, "bbbb", &install, Some("another snapshot".into()), &keep);
    fake_template(&root, "cccc", &tmp.path().join("gone"), Some("x".into()), &keep);
    let idle = fake_template(&root, "dddd", &install, None, &keep);
    let used = fs::File::create(idle.join(USED)).unwrap();
    used.set_modified(SystemTime::now() - UNUSED - Duration::from_secs(60)).unwrap();
    fs::create_dir_all(root.join("eeee").join("data")).unwrap();
    fs::write(root.join("eeee").join(RECORD), "{").unwrap();
    fs::create_dir_all(root.join(".build-ffff-1-1/data")).unwrap();
    fake_template(&root, "kkkk", &install, None, &|r| r.format = "stack-pg-template-2".into());
    fs::create_dir_all(root.join(".build-gggg-1-1/data")).unwrap();
    fake_template(&root, "hhhh", &install, None, &|r| {
        r.full_flush = false;
        r.boot = Some("an earlier boot".into());
    });
    fake_template(&root, "iiii", &install, None, &|r| {
        r.full_flush = false;
        r.boot = crate::procinfo::boot_id();
    });
    // An unusable template whose recipe is locked (being rebuilt), and a build in progress.
    fake_template(&root, "jjjj", &install, Some("old".into()), &keep);
    let _rebuilding = crate::state::FileLock::try_acquire(&root.join("jjjj.lock")).unwrap().unwrap();
    let _building = crate::state::FileLock::try_acquire(&root.join("gggg.lock")).unwrap().unwrap();

    let names = |found: &[Unused]| {
        let mut n: Vec<String> = found.iter().map(|u| u.dir.file_name().unwrap().to_string_lossy().into_owned()).collect();
        n.sort();
        n
    };
    let expected = vec![".build-ffff-1-1", "bbbb", "cccc", "dddd", "eeee", "hhhh", "kkkk"];
    let planned = collect(&state, true);
    assert_eq!(names(&planned), expected, "a dry run lists what a pass would remove, not what it would skip");
    assert_eq!(dirs_in(&root).len(), 11, "a dry run removes nothing");
    let removed = collect(&state, false);
    assert_eq!(names(&removed), expected);
    assert!(removed.iter().any(|u| u.reason.contains("changed")) && removed.iter().any(|u| u.reason.contains("gone")) && removed.iter().any(|u| u.reason.contains("days")) && removed.iter().any(|u| u.reason.contains("restarted")) && removed.iter().any(|u| u.reason.contains("another format")));
    assert_eq!(dirs_in(&root), vec![".build-gggg-1-1", "aaaa", "iiii", "jjjj"]);
    assert!(kept.join(RECORD).exists());
}

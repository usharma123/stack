//! `stack __preset postgres` with a template directory, against real Postgres (each installed
//! major in mise's installs; skipped when there is none, unless `STACK_REQUIRE_PGTEMPLATE` is
//! set): a new data directory is cloned from a template, and one whose template directory cannot
//! be used is initialised by `initdb`, the reason in the service's log either way.
//!
//! mise's installs are found where mise keeps them: a nonempty `MISE_DATA_DIR`, else a nonempty
//! `XDG_DATA_HOME`'s `mise`, else `~/.local/share/mise`. With `STACK_REQUIRE_PGTEMPLATE` set,
//! every selected Postgres must have its own `psql` beside it (an executable reporting that
//! release) before any cluster starts; without it, a release lacking one is still tested, its
//! servers' identifiers and data not queried, and says so.
//!
//! Which way a clone was made is checked, not only that one was:
//! - `STACK_PGTEMPLATE_CLONE` names the path the clone must take: `clonefile` (macOS APFS),
//!   `reflink` (Linux `FICLONE`, Btrfs or XFS) or `copy` (any other Linux filesystem). With
//!   `reflink`, a filesystem that refuses `FICLONE` fails the test as unsupported before anything
//!   runs, never as a pass of another path; on Linux each clone's untouched files must share
//!   their extents with the template (`reflink`) or not (`copy`), as `FIEMAP` reports them.
//! - `STACK_PGTEMPLATE_DIR` puts the templates and data directories on the filesystem under test.
//! - `STACK_TEST_BINARY` runs a packaged binary in place of this build's.
//!
//! Each clone must be its own cluster: a system identifier unlike the template's and the other
//! clone's, which the running server reports, and data written in one never seen in another.

use std::ffi::OsString;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// mise's data directory, as mise finds it from the variables `var` gives: a nonempty
/// `MISE_DATA_DIR`, else a nonempty `XDG_DATA_HOME`'s `mise`, else `~/.local/share/mise`.
fn mise_data_dir(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    set("MISE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| set("XDG_DATA_HOME").map(|d| PathBuf::from(d).join("mise")))
        .or_else(|| set("HOME").map(|h| PathBuf::from(h).join(".local/share/mise")))
}

#[test]
fn mise_data_dir_is_found_as_mise_finds_it() {
    let from = |vars: &[(&str, &str)]| {
        let vars: Vec<(String, OsString)> = vars.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        mise_data_dir(|name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    };
    assert_eq!(from(&[("MISE_DATA_DIR", "/m"), ("XDG_DATA_HOME", "/x"), ("HOME", "/h")]), Some(PathBuf::from("/m")));
    assert_eq!(from(&[("MISE_DATA_DIR", ""), ("XDG_DATA_HOME", "/x"), ("HOME", "/h")]), Some(PathBuf::from("/x/mise")));
    assert_eq!(from(&[("MISE_DATA_DIR", ""), ("XDG_DATA_HOME", ""), ("HOME", "/h")]), Some(PathBuf::from("/h/.local/share/mise")));
    assert_eq!(from(&[("HOME", "")]), None);
}

/// The newest release's `bin` of each installed major under mise's data directory `data`.
fn pg_bins(data: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(data.join("installs/postgres")) else { return Vec::new() };
    let mut by_major: std::collections::BTreeMap<u32, (Vec<u32>, String, PathBuf)> = Default::default();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let parts: Option<Vec<u32>> = name.split('.').map(|p| p.parse().ok()).collect();
        let Some(parts) = parts.filter(|p| p.len() >= 2) else { continue };
        let bin = entry.path().join("bin");
        if !(bin.join("initdb").is_file() && bin.join("postgres").is_file()) {
            continue;
        }
        let slot = by_major.entry(parts[0]).or_insert_with(|| (Vec::new(), String::new(), PathBuf::new()));
        if parts > slot.0 {
            *slot = (parts, name, bin);
        }
    }
    by_major.into_values().map(|(_, version, bin)| (version, bin)).collect()
}

/// Why `bin` holds no `psql` of `version` (an executable file whose `--version` reports that
/// release), or `None` when it does.
fn psql_mismatch(bin: &Path, version: &str) -> Option<String> {
    let psql = bin.join("psql");
    match std::fs::metadata(&psql) {
        Err(e) => return Some(format!("{}: {e}", psql.display())),
        Ok(meta) if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 => return Some(format!("{} is not an executable file", psql.display())),
        Ok(_) => {}
    }
    let out = match Command::new(&psql).arg("--version").env_clear().env("LC_ALL", "C.UTF-8").stdin(Stdio::null()).output() {
        Ok(out) => out,
        Err(e) => return Some(format!("{} --version: {e}", psql.display())),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // `psql (PostgreSQL) 17.2`
    let reported = text.split_whitespace().nth(2);
    (!(out.status.success() && reported == Some(version)))
        .then(|| format!("{} --version reported {:?} ({}), not {version}", psql.display(), text.trim(), out.status))
}

fn stack_bin() -> PathBuf {
    std::env::var_os("STACK_TEST_BINARY").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_stack")))
}

/// Run the preset until Postgres accepts queries, call `ready` with its port, stop it with `SIGINT` (it is
/// the postmaster after its `exec`), and return what it logged.
fn run_preset(bin: &Path, version: &str, data: &Path, templates: &Path, log: &Path, ready: impl FnOnce(u16)) -> String {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(stack_bin())
        .args(["__preset", "postgres", data.to_str().unwrap(), &port.to_string(), version, &format!("templates={}", templates.display())])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LC_ALL", "C.UTF-8")
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    // The postmaster binds its socket before startup finishes. A TCP handshake can succeed
    // while a SQL connection still receives "the database system is starting up".
    loop {
        let bytes = std::fs::read(log).unwrap_or_default();
        let output = String::from_utf8_lossy(&bytes);
        // Ended (and so reaped): nothing to signal.
        if let Some(status) = child.try_wait().unwrap() {
            panic!("the preset {version} exited ({status}) before it was ready on {port}: {output}");
        }
        if output.contains("database system is ready to accept connections") {
            break;
        }
        if Instant::now() > until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the preset {version} was not ready on {port} within 30s, stopped: {output}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Stopped whatever `ready` asserts.
    struct Stop(std::process::Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            // SAFETY: signals only the child this test started, not yet reaped.
            unsafe { libc::kill(self.0.id() as i32, libc::SIGINT) };
            let _ = self.0.wait();
        }
    }
    let stop = Stop(child);
    ready(port);
    drop(stop);
    std::fs::read_to_string(log).unwrap()
}

/// `psql` against the server on `port` with `sql`; its trimmed output.
fn psql(bin: &Path, port: u16, sql: &str) -> String {
    let out = Command::new(bin.join("psql"))
        .args(["-X", "-q", "-At", "-v", "ON_ERROR_STOP=1", "-h", "127.0.0.1", "-p", &port.to_string(), "-U", "postgres", "-d", "postgres", "-c", sql])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "psql {sql}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// How the log says the data directory was cloned (`clonefile`, `reflink` or `copy`) and the
/// system identifier it was given; `None` when it was not cloned.
fn cloned(log: &str) -> Option<(String, u64)> {
    let line = log.lines().find(|l| l.contains("initialised from the template in "))?;
    let inner = &line[line.find(" (")? + 2..];
    let how = inner.split(", ").find(|p| ["clonefile", "reflink", "copy"].contains(p))?.to_string();
    let id = line.rsplit("system identifier ").next()?.trim().parse().ok()?;
    Some((how, id))
}

/// The system identifier in the first 8 bytes of `data`'s `pg_control` (little-endian, the only
/// layout Stack edits).
fn control_system_identifier(data: &Path) -> u64 {
    let bytes = std::fs::read(data.join("global/pg_control")).unwrap();
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

/// The one published template's cluster under `templates`.
fn template_data(templates: &Path) -> PathBuf {
    let published: Vec<PathBuf> = std::fs::read_dir(templates)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("template.json").is_file() && p.join("data").is_dir())
        .collect();
    assert_eq!(published.len(), 1, "expected one published template: {published:?}");
    published[0].join("data")
}

#[cfg(target_os = "linux")]
/// The largest regular file under `dir`: a catalog file of `template1`, which neither the server
/// nor Stack's identity edit writes.
fn largest_file(dir: &Path) -> PathBuf {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let meta = entry.metadata().unwrap();
        if meta.is_file() && best.as_ref().is_none_or(|(size, _)| meta.len() > *size) {
            best = Some((meta.len(), entry.path()));
        }
    }
    best.expect("a file").1
}

/// Whether `FICLONE` works between two files in `dir`: `Err` says why not.
#[cfg(target_os = "linux")]
fn ficlone_supported(dir: &Path) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let source = dir.join(".ficlone-probe-source");
    let dest = dir.join(".ficlone-probe-dest");
    std::fs::write(&source, vec![7u8; 64 * 1024]).map_err(|e| e.to_string())?;
    let result = (|| {
        let src = std::fs::File::open(&source).map_err(|e| e.to_string())?;
        let dst = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
        // SAFETY: two valid open descriptors; FICLONE shares the source's extents or fails.
        if unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&source);
    let _ = std::fs::remove_file(&dest);
    result
}

#[cfg(not(target_os = "linux"))]
fn ficlone_supported(_: &Path) -> Result<(), String> {
    Err("FICLONE is Linux's".into())
}

/// Whether any extent of `path` is shared with another file, as `FIEMAP` reports it; an error
/// where the filesystem has no extent map to report (tmpfs).
#[cfg(target_os = "linux")]
fn shares_extents(path: &Path) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Extent {
        _logical: u64,
        _physical: u64,
        _length: u64,
        _reserved64: [u64; 2],
        flags: u32,
        _reserved: [u32; 3],
    }
    #[repr(C)]
    struct Fiemap {
        _start: u64,
        length: u64,
        flags: u32,
        mapped: u32,
        count: u32,
        _reserved: u32,
        extents: [Extent; 64],
    }
    /// `_IOWR('f', 11, struct fiemap)`, the 32-byte header.
    const FS_IOC_FIEMAP: u32 = 0xC020_660B;
    const FIEMAP_FLAG_SYNC: u32 = 0x1;
    const FIEMAP_EXTENT_SHARED: u32 = 0x2000;
    let file = std::fs::File::open(path).unwrap();
    // SAFETY: all-zero is a valid value of these plain integer structs.
    let mut map: Fiemap = unsafe { std::mem::zeroed() };
    map.length = u64::MAX;
    map.flags = FIEMAP_FLAG_SYNC;
    map.count = 64;
    // SAFETY: a valid descriptor and a fiemap header followed by room for `count` extents.
    if unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP as _, &mut map) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(map.extents[..(map.mapped as usize).min(64)].iter().any(|e| e.flags & FIEMAP_EXTENT_SHARED != 0))
}

#[test]
fn the_preset_clones_a_template_and_falls_back_to_initdb_when_it_cannot() {
    let expected = std::env::var("STACK_PGTEMPLATE_CLONE").ok().filter(|v| !v.is_empty());
    if let Some(expected) = &expected {
        assert!(["clonefile", "reflink", "copy"].contains(&expected.as_str()), "STACK_PGTEMPLATE_CLONE={expected} names no clone path");
    }
    let base = std::env::var_os("STACK_PGTEMPLATE_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if expected.as_deref() == Some("reflink") {
        if let Err(why) = ficlone_supported(&base) {
            panic!("UNSUPPORTED FILESYSTEM, not a pass: {} refuses FICLONE ({why}), so the reflink path cannot run there", base.display());
        }
    }
    let required = std::env::var_os("STACK_REQUIRE_PGTEMPLATE").is_some();
    let data = mise_data_dir(|name| std::env::var_os(name));
    let bins = data.as_deref().map(pg_bins).unwrap_or_default();
    if bins.is_empty() {
        assert!(!required, "STACK_REQUIRE_PGTEMPLATE is set, but mise's installs (under {data:?}) hold no Postgres");
        eprintln!("skipping: no Postgres in mise's installs (under {data:?})");
        return;
    }
    // Every selected release's psql, checked before any cluster starts.
    let bins: Vec<(String, PathBuf, Option<String>)> = bins.into_iter().map(|(version, bin)| { let why = psql_mismatch(&bin, &version); (version, bin, why) }).collect();
    let missing: Vec<String> = bins.iter().filter_map(|(version, _, why)| why.as_ref().map(|why| format!("{version}: {why}"))).collect();
    assert!(
        !required || missing.is_empty(),
        "STACK_REQUIRE_PGTEMPLATE is set, but not every selected Postgres has its psql, so the servers' identifiers and data cannot be queried:\n{}",
        missing.join("\n")
    );
    for line in &missing {
        eprintln!("{line}; the servers of this release will not be queried");
    }
    for (version, bin, why) in bins {
        let tmp = tempfile::tempdir_in(&base).unwrap();
        let templates = tmp.path().join("templates");
        let has_psql = why.is_none();
        let mut first_id = None;
        let log = run_preset(&bin, &version, &tmp.path().join("a/postgres"), &templates, &tmp.path().join("a.log"), |port| {
            if has_psql {
                first_id = Some(psql(&bin, port, "select system_identifier from pg_control_system()"));
                psql(&bin, port, "create table written_in_a (v int); insert into written_in_a values (1)");
            }
        });
        assert!(log.contains("initialised from the template") && log.contains("built now"), "{version}: {log}");
        let (how_a, id_a) = cloned(&log).unwrap_or_else(|| panic!("{version}: no clone in {log}"));
        let mut second_id = None;
        let mut seen_in_b = None;
        let log = run_preset(&bin, &version, &tmp.path().join("b/postgres"), &templates, &tmp.path().join("b.log"), |port| {
            if has_psql {
                second_id = Some(psql(&bin, port, "select system_identifier from pg_control_system()"));
                seen_in_b = Some(psql(&bin, port, "select count(*) from pg_class where relname = 'written_in_a'"));
            }
        });
        assert!(log.contains("initialised from the template") && !log.contains("built now"), "{version}: {log}");
        let (how_b, id_b) = cloned(&log).unwrap_or_else(|| panic!("{version}: no clone in {log}"));

        // The path taken, the same for both clones, and the one required.
        assert_eq!(how_a, how_b, "{version}: two clones of one template took different paths");
        eprintln!("{version}: template clone path {how_a} under {}", base.display());
        if let Some(expected) = &expected {
            assert_eq!(&how_a, expected, "{version}: the clone took the {how_a} path, not {expected}");
        }
        if cfg!(target_os = "macos") {
            assert_eq!(how_a, "clonefile", "{version}: macOS clones only with clonefile");
        }

        // Each clone its own cluster, as the running server says.
        let template = template_data(&templates);
        let template_id = control_system_identifier(&template);
        assert!(id_a != template_id && id_b != template_id && id_a != id_b, "{version}: identifiers template {template_id}, a {id_a}, b {id_b}");
        assert_eq!(control_system_identifier(&tmp.path().join("a/postgres")), id_a, "{version}");
        assert_eq!(control_system_identifier(&tmp.path().join("b/postgres")), id_b, "{version}");
        if has_psql {
            assert_eq!(first_id, Some(id_a.to_string()), "{version}: the server on a reports another identifier");
            assert_eq!(second_id, Some(id_b.to_string()), "{version}: the server on b reports another identifier");
            assert_eq!(seen_in_b.as_deref(), Some("0"), "{version}: what a wrote is in b, cloned from the template after it");
        } else {
            eprintln!("{version}: no psql beside postgres; the running servers' identifiers and data were not queried");
        }

        // On Linux, what the filesystem did: shared extents for a reflink, none for a copy.
        #[cfg(target_os = "linux")]
        {
            let untouched = largest_file(&template.join("base/1"));
            let relative = untouched.strip_prefix(&template).unwrap();
            for clone in ["a/postgres", "b/postgres"] {
                let file = tmp.path().join(clone).join(relative);
                assert_eq!(std::fs::read(&file).unwrap(), std::fs::read(&untouched).unwrap(), "{version}: {} differs from the template", file.display());
                match (how_a.as_str(), shares_extents(&file)) {
                    ("reflink", Ok(shared)) => assert!(shared, "{version}: {} shares no extent, yet the clone says reflink", file.display()),
                    ("reflink", Err(e)) => panic!("{version}: the extents of reflinked {} cannot be read: {e}", file.display()),
                    (_, Ok(shared)) => assert!(!shared, "{version}: {} shares an extent, yet the clone says {how_a}", file.display()),
                    // No extent map (tmpfs): nothing can be shared there.
                    (_, Err(e)) if e.raw_os_error() == Some(libc::EOPNOTSUPP) => {}
                    (_, Err(e)) => panic!("{version}: the extents of {} cannot be read: {e}", file.display()),
                }
            }
        }

        // A template directory that cannot be used (a file in its place): initdb, and why.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let log = run_preset(&bin, &version, &tmp.path().join("c/postgres"), &blocked, &tmp.path().join("c.log"), |_| {});
        assert!(log.contains("running initdb for this data directory: cannot create"), "{version}: {log}");
        assert!(cloned(&log).is_none(), "{version}: {log}");
        for d in ["a", "b", "c"] {
            assert_eq!(std::fs::read_to_string(tmp.path().join(d).join("postgres/.mise-daemon-version")).unwrap(), format!("postgres:{}", version.split('.').next().unwrap()));
        }
    }
}

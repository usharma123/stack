//! `stack __preset postgres` with a template directory, against real Postgres (each installed
//! major in mise's installs; skipped when there is none): a new data directory is cloned from a
//! template, and one whose template directory cannot be used is initialised by `initdb`, the
//! reason in the service's log either way.

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The newest release's `bin` of each installed major.
fn pg_bins() -> Vec<(String, PathBuf)> {
    let data = std::env::var_os("MISE_DATA_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/mise")));
    let Some(entries) = data.and_then(|d| std::fs::read_dir(d.join("installs/postgres")).ok()) else { return Vec::new() };
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

/// Run the preset until it listens, stop it with `SIGINT` (it is the postmaster after its
/// `exec`), and return what it logged.
fn run_preset(bin: &Path, version: &str, data: &Path, templates: &Path, log: &Path) -> String {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_stack"))
        .args(["__preset", "postgres", data.to_str().unwrap(), &port.to_string(), version, &format!("templates={}", templates.display())])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LC_ALL", "C.UTF-8")
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        if child.try_wait().unwrap().is_some() || Instant::now() > until {
            panic!("the preset did not listen: {}", std::fs::read_to_string(log).unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: signals only the child this test started, not yet reaped.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    child.wait().unwrap();
    std::fs::read_to_string(log).unwrap()
}

#[test]
fn the_preset_clones_a_template_and_falls_back_to_initdb_when_it_cannot() {
    let bins = pg_bins();
    if bins.is_empty() {
        eprintln!("skipping: no Postgres in mise's installs");
        return;
    }
    for (version, bin) in bins {
        let tmp = tempfile::tempdir().unwrap();
        let templates = tmp.path().join("templates");
        let log = run_preset(&bin, &version, &tmp.path().join("a/postgres"), &templates, &tmp.path().join("a.log"));
        assert!(log.contains("initialised from the template") && log.contains("built now"), "{version}: {log}");
        let log = run_preset(&bin, &version, &tmp.path().join("b/postgres"), &templates, &tmp.path().join("b.log"));
        assert!(log.contains("initialised from the template") && !log.contains("built now"), "{version}: {log}");
        // A template directory that cannot be used (a file in its place): initdb, and why.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let log = run_preset(&bin, &version, &tmp.path().join("c/postgres"), &blocked, &tmp.path().join("c.log"));
        assert!(log.contains("running initdb for this data directory: cannot create"), "{version}: {log}");
        for d in ["a", "b", "c"] {
            assert_eq!(std::fs::read_to_string(tmp.path().join(d).join("postgres/.mise-daemon-version")).unwrap(), format!("postgres:{}", version.split('.').next().unwrap()));
        }
    }
}

//! Shared OCI cache installation across independent projects and processes.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Serves one bundle. Blob responses are held until `barrier` blob requests are in flight, so
/// concurrent pulls provably overlap instead of relying on timing.
struct Registry {
    host: String,
    blob_requests: Arc<(Mutex<usize>, Condvar)>,
}

impl Registry {
    fn new(manifest: Vec<u8>, blob: Vec<u8>, barrier: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let blob_requests = Arc::new((Mutex::new(0usize), Condvar::new()));
        let seen = blob_requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let (manifest, blob, seen) = (manifest.clone(), blob.clone(), seen.clone());
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let path = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_string();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap() == 0 || header == "\r\n" {
                            break;
                        }
                    }
                    let (status, body) = if path.contains("/manifests/") {
                        (200, manifest)
                    } else if path.contains("/blobs/") {
                        let (count, cvar) = &*seen;
                        let mut n = count.lock().unwrap();
                        *n += 1;
                        cvar.notify_all();
                        let (n, wait) = cvar
                            .wait_timeout_while(n, Duration::from_secs(20), |n| *n < barrier)
                            .unwrap();
                        drop(n);
                        if wait.timed_out() {
                            (500, b"concurrent pulls did not overlap".to_vec())
                        } else {
                            (200, blob)
                        }
                    } else {
                        (404, Vec::new())
                    };
                    let head = format!(
                        "HTTP/1.1 {status} Fixture\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                });
            }
        });
        Self {
            host,
            blob_requests,
        }
    }

    fn blob_requests(&self) -> usize {
        *self.blob_requests.0.lock().unwrap()
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", stack::hash::sha256_hex(bytes))
}

/// A bundle artifact for `blob`, returning (manifest, manifest digest).
fn manifest_for(blob: &[u8]) -> (Vec<u8>, String) {
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "layers": [{ "mediaType": stack::oci::LAYER_TYPE, "digest": sha256(blob), "size": blob.len() }],
    }))
    .unwrap();
    let digest = sha256(&manifest);
    (manifest, digest)
}

fn bundle() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    std::fs::write(dir.path().join("bundle.toml"), "[bundle]\nname='shared'\n").unwrap();
    std::fs::write(dir.path().join("bin/tool"), "#!/bin/sh\necho shared\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("bin/tool"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    dir
}

fn project(machine: &Path, registry: &Registry, digest: &str) -> (TempDir, Command) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("stack.toml"),
        format!("[[use]]\nbundle='oci:{}/shared@{digest}'\n", registry.host),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
    command
        .args(["-C", dir.path().to_str().unwrap(), "--json", "compile"])
        .env("STACK_CACHE_DIR", machine.join("cache"))
        .env("STACK_STATE_DIR", machine.join("state"))
        .env_remove("STACK_OCI_PLAIN_HTTP")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (dir, command)
}

fn locked_hash(project: &TempDir) -> String {
    let lock: toml::Value =
        toml::from_str(&std::fs::read_to_string(project.path().join("stack.lock")).unwrap())
            .unwrap();
    lock["bundle"][0]["content_hash"]
        .as_str()
        .unwrap()
        .to_string()
}

fn cache_entries(machine: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(machine.join("cache/bundles"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn separate_projects_cold_fetch_one_digest_into_a_shared_cache_concurrently() {
    let source = bundle();
    let blob = stack::oci::archive(source.path()).unwrap();
    let expected = stack::hash::hash_dir(source.path()).unwrap();
    let (manifest, digest) = manifest_for(&blob);
    let registry = Registry::new(manifest, blob, 2);
    let machine = tempfile::tempdir().unwrap();

    let (a, mut compile_a) = project(machine.path(), &registry, &digest);
    let (b, mut compile_b) = project(machine.path(), &registry, &digest);
    let (child_a, child_b) = (compile_a.spawn().unwrap(), compile_b.spawn().unwrap());
    let (out_a, out_b) = (
        child_a.wait_with_output().unwrap(),
        child_b.wait_with_output().unwrap(),
    );
    assert_ok(&out_a);
    assert_ok(&out_b);
    assert_eq!(registry.blob_requests(), 2, "both pulls were cold");
    assert_eq!(locked_hash(&a), expected);
    assert_eq!(locked_hash(&b), expected);

    let hex = digest.trim_start_matches("sha256:");
    let published = format!("oci-{hex}");
    assert_eq!(
        cache_entries(machine.path()),
        [format!(".oci-{hex}.lock"), published.clone()],
        "staging directories were left behind"
    );
    let tree = machine.path().join("cache/bundles").join(&published);
    assert_eq!(stack::hash::hash_dir(&tree).unwrap(), expected);

    // Warm cache: a third project installs nothing and downloads nothing.
    let (c, mut compile_c) = project(machine.path(), &registry, &digest);
    assert_ok(&compile_c.output().unwrap());
    assert_eq!(registry.blob_requests(), 2);
    assert_eq!(locked_hash(&c), expected);
}

#[test]
fn a_failed_install_leaves_neither_a_destination_nor_staging() {
    let source = bundle();
    let full = stack::oci::archive(source.path()).unwrap();
    // Digest-valid but truncated: extraction itself fails.
    let blob = full[..full.len() / 2].to_vec();
    let (manifest, digest) = manifest_for(&blob);
    let registry = Registry::new(manifest, blob, 1);
    let machine = tempfile::tempdir().unwrap();
    let (_project, mut compile) = project(machine.path(), &registry, &digest);
    let output = compile.output().unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "bundle_invalid", "{result}");
    let hex = digest.trim_start_matches("sha256:");
    assert_eq!(cache_entries(machine.path()), [format!(".oci-{hex}.lock")]);
}

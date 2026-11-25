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
        [format!(".oci-{hex}.evidence.json"), format!(".oci-{hex}.lock"), published.clone()],
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

/// A gzip-compressed tar holding `bundle.toml` and whatever `extra` adds.
fn layer(extra: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let manifest = b"[bundle]\nname='shared'\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    tar.append_data(&mut header, "bundle.toml", &manifest[..]).unwrap();
    extra(&mut tar);
    let raw = tar.into_inner().unwrap();
    let mut gz = flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::default());
    gz.write_all(&raw).unwrap();
    gz.finish().unwrap()
}

#[test]
fn layers_with_links_or_excluded_directories_never_reach_the_cache() {
    let link = layer(|tar| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "bin", "/usr/bin").unwrap();
    });
    let hidden = layer(|tar| {
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, ".stack/bin/tool", &b"x"[..]).unwrap();
    });
    let folded = layer(|tar| {
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, ".GIT/config", &b"x"[..]).unwrap();
    });
    for (blob, reason) in [(link, "unsupported layer entry type"), (hidden, "inside .stack"), (folded, "inside .GIT")] {
        let (manifest, digest) = manifest_for(&blob);
        let registry = Registry::new(manifest, blob, 1);
        let machine = tempfile::tempdir().unwrap();
        let (_project, mut compile) = project(machine.path(), &registry, &digest);
        let output = compile.output().unwrap();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["error"]["code"], "bundle_invalid", "{result}");
        assert!(result["error"]["message"].as_str().unwrap().contains(reason), "{result}");
        let hex = digest.trim_start_matches("sha256:");
        assert_eq!(cache_entries(machine.path()), [format!(".oci-{hex}.lock")]);
    }
}

#[cfg(unix)]
#[test]
fn a_link_added_to_a_cached_bundle_is_refused_when_it_is_read() {
    let source = bundle();
    let blob = stack::oci::archive(source.path()).unwrap();
    let (manifest, digest) = manifest_for(&blob);
    let registry = Registry::new(manifest, blob, 1);
    let machine = tempfile::tempdir().unwrap();
    let (_a, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());

    let tree = machine
        .path()
        .join("cache/bundles")
        .join(format!("oci-{}", digest.trim_start_matches("sha256:")));
    std::os::unix::fs::symlink("/etc/hosts", tree.join("bin/hosts")).unwrap();
    let (_b, mut compile) = project(machine.path(), &registry, &digest);
    let output = compile.output().unwrap();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "bundle_invalid", "{result}");
    assert!(result["error"]["message"].as_str().unwrap().contains("bin/hosts is a symbolic link"), "{result}");
}

/// The cache directory and its evidence file for `digest`.
fn cached(machine: &Path, digest: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let hex = digest.trim_start_matches("sha256:");
    let bundles = machine.join("cache/bundles");
    (bundles.join(format!("oci-{hex}")), bundles.join(format!(".oci-{hex}.evidence.json")))
}

fn error_of(output: &Output) -> Value {
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    result["error"].clone()
}

/// A cached tree is reused without a lock vouching for it only with the evidence this Stack
/// recorded when it installed it from that manifest. Without that evidence (a cache an earlier
/// Stack filled) the registry is asked again and the tree reused only if it is the layer's; a
/// tree changed since is refused and left for a person to remove. With stack.lock pinning the
/// digest and content hash, no registry is needed.
#[test]
fn an_unlocked_cached_bundle_is_reused_only_with_evidence_tying_it_to_its_manifest() {
    let source = bundle();
    let blob = stack::oci::archive(source.path()).unwrap();
    let expected = stack::hash::hash_dir(source.path()).unwrap();
    let (manifest, digest) = manifest_for(&blob);
    let registry = Registry::new(manifest, blob, 1);
    let machine = tempfile::tempdir().unwrap();
    let (tree, evidence) = cached(machine.path(), &digest);

    let (locked, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());
    assert_eq!(registry.blob_requests(), 1);
    let recorded: Value = serde_json::from_slice(&std::fs::read(&evidence).unwrap()).unwrap();
    assert_eq!((recorded["manifest"].as_str(), recorded["content_hash"].as_str()), (Some(digest.as_str()), Some(expected.as_str())));

    // Another project, no lock: the evidence vouches, nothing is downloaded.
    let (_b, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());
    assert_eq!(registry.blob_requests(), 1);

    // No evidence (as an earlier Stack left its cache): checked against the registry once,
    // then recorded.
    std::fs::remove_file(&evidence).unwrap();
    let (_c, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());
    assert_eq!(registry.blob_requests(), 2);
    assert!(evidence.exists());
    let (_d, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());
    assert_eq!(registry.blob_requests(), 2);

    // The project whose stack.lock pins it needs no evidence and no registry.
    std::fs::remove_file(&evidence).unwrap();
    let mut again = Command::new(env!("CARGO_BIN_EXE_stack"));
    again
        .args(["-C", locked.path().to_str().unwrap(), "--json", "compile", "--locked"])
        .env("STACK_CACHE_DIR", machine.path().join("cache"))
        .env("STACK_STATE_DIR", machine.path().join("state"))
        .env_remove("STACK_OCI_PLAIN_HTTP");
    assert_ok(&again.output().unwrap());
    assert_eq!(registry.blob_requests(), 2);

    // Changed since it was installed (dummy content): the evidence no longer matches, the
    // registry's layer does not either, and the tree is refused, not replaced.
    std::fs::write(tree.join("bin/tool"), "#!/bin/sh\necho changed\n").unwrap();
    let (_e, mut compile) = project(machine.path(), &registry, &digest);
    let output = compile.output().unwrap();
    let error = error_of(&output);
    assert_eq!(error["code"], "content_hash_mismatch", "{error}");
    assert!(error["message"].as_str().unwrap().contains("is not what"), "{error}");
    assert_eq!(registry.blob_requests(), 3);
    assert_eq!(std::fs::read_to_string(tree.join("bin/tool")).unwrap(), "#!/bin/sh\necho changed\n");
    // The locked project refuses it by its lock, offline.
    let output = again.output().unwrap();
    assert_eq!(error_of(&output)["code"], "content_hash_mismatch");
    assert_eq!(registry.blob_requests(), 3);
}

/// A cached tree holding a directory bundles cannot contain, however it is spelled, was filled
/// some other way: refused, whatever vouches for the rest.
#[test]
fn a_cached_bundle_holding_an_excluded_directory_is_refused() {
    let source = bundle();
    let blob = stack::oci::archive(source.path()).unwrap();
    let (manifest, digest) = manifest_for(&blob);
    let registry = Registry::new(manifest, blob, 1);
    let machine = tempfile::tempdir().unwrap();
    let (_a, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());
    let (tree, _) = cached(machine.path(), &digest);
    std::fs::create_dir_all(tree.join("bin/.Stack")).unwrap();
    std::fs::write(tree.join("bin/.Stack/dummy"), "x").unwrap();
    let (_b, mut compile) = project(machine.path(), &registry, &digest);
    let error = error_of(&compile.output().unwrap());
    assert_eq!(error["code"], "bundle_invalid", "{error}");
    assert!(error["message"].as_str().unwrap().contains("bin/.Stack"), "{error}");
}

/// Digests from a reference, from stack.lock and from a manifest are checked before any of
/// them names a path or a request.
#[test]
fn only_digests_name_cache_paths_or_registry_requests() {
    let source = bundle();
    let blob = stack::oci::archive(source.path()).unwrap();
    // A manifest naming a layer by something that is not a digest: refused before the blob is
    // asked for.
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "layers": [{ "mediaType": stack::oci::LAYER_TYPE, "digest": "sha256:../../../dummy", "size": blob.len() }],
    }))
    .unwrap();
    let digest = sha256(&manifest);
    let registry = Registry::new(manifest, blob.clone(), 1);
    let machine = tempfile::tempdir().unwrap();
    let (_a, mut compile) = project(machine.path(), &registry, &digest);
    let error = error_of(&compile.output().unwrap());
    assert_eq!(error["code"], "bundle_invalid", "{error}");
    assert!(error["message"].as_str().unwrap().contains("not a sha256 digest"), "{error}");
    assert_eq!(registry.blob_requests(), 0);
    assert!(!machine.path().join("cache/bundles").exists() || cache_entries(machine.path()).iter().all(|e| !e.contains("dummy")));

    // A reference's digest, and the one stack.lock records.
    let (manifest, digest) = manifest_for(&blob);
    let registry = Registry::new(manifest, blob, 1);
    let (bad, mut compile) = project(machine.path(), &registry, "sha256:../../dummy");
    let error = error_of(&compile.output().unwrap());
    assert_eq!(error["code"], "source_invalid", "{error}");
    drop(bad);
    let (pinned, mut compile) = project(machine.path(), &registry, &digest);
    assert_ok(&compile.output().unwrap());
    let lock = std::fs::read_to_string(pinned.path().join("stack.lock")).unwrap();
    std::fs::write(pinned.path().join("stack.lock"), lock.replace(&format!("digest = \"{digest}\""), "digest = \"sha256:../../../dummy\"")).unwrap();
    let before = registry.blob_requests();
    let error = error_of(&compile.output().unwrap());
    assert_eq!(error["code"], "lock_invalid", "{error}");
    assert_eq!(registry.blob_requests(), before);
}

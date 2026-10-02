//! OCI registries as a bundle transport: `oci:<registry>/<repository>:<tag>` or `@sha256:<digest>`.
//!
//! A bundle is one deterministic tar.gz layer in an OCI artifact manifest. Projects pin the
//! manifest digest in stack.lock, the same way git sources pin a commit.
//!
//! Credentials, when a registry needs them: `STACK_OCI_USERNAME` and `STACK_OCI_PASSWORD`.
//! Plain HTTP is used for `localhost`/`127.0.0.1`, or anywhere with `STACK_OCI_PLAIN_HTTP=1`.

use crate::error::{io_error, Result, StackError};
use crate::hash::{is_executable, list_files, sha256_hex};
use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::path::Path;

pub const ARTIFACT_TYPE: &str = "application/vnd.stack.bundle.v1";
pub const LAYER_TYPE: &str = "application/vnd.stack.bundle.layer.v1.tar+gzip";
const MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const EMPTY_TYPE: &str = "application/vnd.oci.empty.v1+json";
const MAX_BLOB: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub registry: String,
    pub repository: String,
    /// A tag, or a `sha256:` digest.
    pub reference: String,
}

impl Reference {
    pub fn parse(s: &str) -> Result<Self> {
        let invalid = || {
            StackError::new("source_invalid", format!("invalid OCI reference '{s}'"))
                .hint("use oci:<registry>/<repository>:<tag> or oci:<registry>/<repository>@sha256:<digest>")
        };
        let (registry, rest) = s.split_once('/').ok_or_else(invalid)?;
        if !(registry.contains('.') || registry.contains(':') || registry == "localhost") {
            return Err(invalid());
        }
        let (repository, reference) = if let Some((repo, digest)) = rest.split_once('@') {
            (repo, digest)
        } else {
            match rest.rsplit_once(':') {
                Some((repo, tag)) if !tag.contains('/') => (repo, tag),
                _ => {
                    return Err(StackError::new("ref_required", format!("'{s}' has no tag or digest"))
                        .hint("pin a tag or digest, e.g. oci:ghcr.io/acme/pybase:1.0.0"))
                }
            }
        };
        if repository.is_empty() || reference.is_empty() {
            return Err(invalid());
        }
        Ok(Self { registry: registry.into(), repository: repository.into(), reference: reference.into() })
    }

    fn base(&self) -> String {
        let plain = self.registry.starts_with("localhost")
            || self.registry.starts_with("127.0.0.1")
            || std::env::var_os("STACK_OCI_PLAIN_HTTP").is_some();
        format!("{}://{}/v2/{}", if plain { "http" } else { "https" }, self.registry, self.repository)
    }
}

pub struct Client {
    agent: ureq::Agent,
    token: RefCell<Option<String>>,
}

impl Default for Client {
    fn default() -> Self {
        Self { agent: ureq::AgentBuilder::new().redirects(5).build(), token: RefCell::new(None) }
    }
}

fn creds() -> Option<(String, String)> {
    Some((std::env::var("STACK_OCI_USERNAME").ok()?, std::env::var("STACK_OCI_PASSWORD").ok()?))
}

fn basic(user: &str, pass: &str) -> String {
    use std::fmt::Write;
    // Minimal base64 so we don't pull in a crate for one header.
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{user}:{pass}").into_bytes();
    let mut out = String::from("Basic ");
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                let _ = out.write_char('=');
            }
        }
    }
    out
}

fn oci_err(context: &str, e: ureq::Error) -> StackError {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let code_name = if code == 404 { "oci_not_found" } else if code == 401 || code == 403 { "oci_unauthorized" } else { "oci_failed" };
            let mut err = StackError::new(code_name, format!("{context}: HTTP {code} {}", body.chars().take(200).collect::<String>()));
            if code == 401 || code == 403 {
                err = err.hint("set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for this registry");
            }
            err
        }
        other => StackError::new("oci_unreachable", format!("{context}: {other}")),
    }
}

impl Client {
    /// Send a request, answering one auth challenge (Bearer token or Basic) if the registry asks.
    #[allow(clippy::result_large_err)] // ureq's error type; boxing it buys nothing on this path
    fn send(&self, method: &str, url: &str, accept: Option<&str>, body: Option<(&[u8], &str)>) -> std::result::Result<ureq::Response, ureq::Error> {
        let build = || {
            let mut req = self.agent.request(method, url);
            if let Some(a) = accept {
                req = req.set("Accept", a);
            }
            if let Some(t) = self.token.borrow().as_ref() {
                req = req.set("Authorization", t);
            }
            req
        };
        let call = |req: ureq::Request| match body {
            Some((bytes, ct)) => req.set("Content-Type", ct).send_bytes(bytes),
            None => req.call(),
        };
        match call(build()) {
            Err(ureq::Error::Status(401, resp)) => {
                let challenge = resp.header("WWW-Authenticate").unwrap_or_default().to_string();
                self.authenticate(&challenge).map_err(|_| ureq::Error::Status(401, resp))?;
                call(build())
            }
            other => other,
        }
    }

    fn authenticate(&self, challenge: &str) -> std::result::Result<(), ()> {
        if let Some(params) = challenge.strip_prefix("Bearer ") {
            let field = |k: &str| {
                params.split(',').find_map(|p| {
                    let (key, v) = p.trim().split_once('=')?;
                    (key == k).then(|| v.trim_matches('"').to_string())
                })
            };
            let realm = field("realm").ok_or(())?;
            let mut req = self.agent.get(&realm);
            for k in ["service", "scope"] {
                if let Some(v) = field(k) {
                    req = req.query(k, &v);
                }
            }
            if let Some((u, p)) = creds() {
                req = req.set("Authorization", &basic(&u, &p));
            }
            let text = req.call().map_err(|_| ())?.into_string().map_err(|_| ())?;
            let body: Value = serde_json::from_str(&text).map_err(|_| ())?;
            let token = body.get("token").or_else(|| body.get("access_token")).and_then(Value::as_str).ok_or(())?;
            *self.token.borrow_mut() = Some(format!("Bearer {token}"));
            Ok(())
        } else if challenge.starts_with("Basic") {
            let (u, p) = creds().ok_or(())?;
            *self.token.borrow_mut() = Some(basic(&u, &p));
            Ok(())
        } else {
            Err(())
        }
    }

    fn read_limited(resp: ureq::Response) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        resp.into_reader().take(MAX_BLOB + 1).read_to_end(&mut buf).map_err(|e| io_error("registry response", e))?;
        if buf.len() as u64 > MAX_BLOB {
            return Err(StackError::new("oci_failed", "blob exceeds 256 MiB"));
        }
        Ok(buf)
    }

    /// Resolve a tag (or pass through a digest) to the manifest digest.
    pub fn resolve(&self, r: &Reference) -> Result<String> {
        if r.reference.starts_with("sha256:") {
            return Ok(r.reference.clone());
        }
        let url = format!("{}/manifests/{}", r.base(), r.reference);
        let resp = self.send("GET", &url, Some(MANIFEST_TYPE), None).map_err(|e| oci_err(&format!("resolve {}", r.reference), e))?;
        let bytes = Self::read_limited(resp)?;
        Ok(format!("sha256:{}", sha256_hex(&bytes)))
    }

    /// Download the bundle at `digest` and extract it into `dest` (atomically).
    pub fn pull(&self, r: &Reference, digest: &str, dest: &Path) -> Result<()> {
        if dest.exists() {
            return Ok(());
        }
        let url = format!("{}/manifests/{digest}", r.base());
        let resp = self.send("GET", &url, Some(MANIFEST_TYPE), None).map_err(|e| oci_err("fetch manifest", e))?;
        let bytes = Self::read_limited(resp)?;
        verify_digest(&bytes, digest, "manifest")?;
        let manifest: Value = serde_json::from_slice(&bytes)
            .map_err(|e| StackError::new("oci_failed", format!("manifest is not JSON: {e}")))?;
        let layer = manifest["layers"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["mediaType"] == LAYER_TYPE))
            .ok_or_else(|| StackError::new("bundle_invalid", format!("{digest} is not a stack bundle (no {LAYER_TYPE} layer)")))?;
        let layer_digest = layer["digest"].as_str().unwrap_or_default();

        let url = format!("{}/blobs/{layer_digest}", r.base());
        let resp = self.send("GET", &url, None, None).map_err(|e| oci_err("fetch layer", e))?;
        let blob = Self::read_limited(resp)?;
        verify_digest(&blob, layer_digest, "layer")?;

        let tmp = dest.with_extension("tmp");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| io_error(tmp.display(), e))?;
        // `unpack` refuses entries that escape the destination (absolute paths, `..`).
        tar::Archive::new(GzDecoder::new(blob.as_slice()))
            .unpack(&tmp)
            .map_err(|e| StackError::new("bundle_invalid", format!("cannot extract layer: {e}")))?;
        fs::rename(&tmp, dest).map_err(|e| io_error(dest.display(), e))
    }

    /// Publish `dir` as a bundle artifact tagged `r.reference`. Returns the manifest digest.
    pub fn push(&self, dir: &Path, r: &Reference, title: &str, version: Option<&str>) -> Result<String> {
        if r.reference.starts_with("sha256:") {
            return Err(StackError::new("source_invalid", "publish needs a tag, not a digest"));
        }
        let layer = archive(dir)?;
        let config = b"{}".to_vec();
        let layer_digest = self.upload(r, &layer)?;
        let config_digest = self.upload(r, &config)?;

        let mut annotations = json!({ "org.opencontainers.image.title": title });
        if let Some(v) = version {
            annotations["org.opencontainers.image.version"] = json!(v);
        }
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": MANIFEST_TYPE,
            "artifactType": ARTIFACT_TYPE,
            "config": { "mediaType": EMPTY_TYPE, "digest": config_digest, "size": config.len() },
            "layers": [{ "mediaType": LAYER_TYPE, "digest": layer_digest, "size": layer.len() }],
            "annotations": annotations,
        });
        let bytes = serde_json::to_vec(&manifest).expect("manifest serializes");
        let url = format!("{}/manifests/{}", r.base(), r.reference);
        self.send("PUT", &url, None, Some((&bytes, MANIFEST_TYPE))).map_err(|e| oci_err("push manifest", e))?;
        Ok(format!("sha256:{}", sha256_hex(&bytes)))
    }

    fn upload(&self, r: &Reference, blob: &[u8]) -> Result<String> {
        let digest = format!("sha256:{}", sha256_hex(blob));
        let head = format!("{}/blobs/{digest}", r.base());
        if self.send("HEAD", &head, None, None).is_ok() {
            return Ok(digest);
        }
        let start = format!("{}/blobs/uploads/", r.base());
        let resp = self.send("POST", &start, None, Some((&[], "application/octet-stream"))).map_err(|e| oci_err("start upload", e))?;
        let location = resp.header("Location").ok_or_else(|| StackError::new("oci_failed", "registry gave no upload location"))?;
        let location = if location.starts_with('/') {
            let (scheme_host, _) = r.base().split_once("/v2/").map(|(a, b)| (a.to_string(), b.to_string())).expect("base has /v2/");
            format!("{scheme_host}{location}")
        } else {
            location.to_string()
        };
        let sep = if location.contains('?') { '&' } else { '?' };
        let put = format!("{location}{sep}digest={digest}");
        self.send("PUT", &put, None, Some((blob, "application/octet-stream"))).map_err(|e| oci_err("upload blob", e))?;
        Ok(digest)
    }
}

fn verify_digest(bytes: &[u8], expected: &str, what: &str) -> Result<()> {
    let actual = format!("sha256:{}", sha256_hex(bytes));
    if actual != expected {
        return Err(StackError::new("content_hash_mismatch", format!("{what} digest {actual} != {expected}")));
    }
    Ok(())
}

/// Deterministic tar.gz: sorted paths, zeroed times and owners, only the executable bit kept.
pub fn archive(dir: &Path) -> Result<Vec<u8>> {
    let gz = GzBuilder::new().mtime(0).write(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);
    for rel in list_files(dir)? {
        let path = dir.join(&rel);
        let data = fs::read(&path).map_err(|e| io_error(path.display(), e))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(if is_executable(&path) { 0o755 } else { 0o644 });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, &rel, data.as_slice()).map_err(|e| io_error(rel.display(), e))?;
    }
    let gz = tar.into_inner().map_err(|e| io_error("archive", e))?;
    gz.finish().map_err(|e| io_error("archive", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_references() {
        let r = Reference::parse("ghcr.io/acme/pybase:1.0.0").unwrap();
        assert_eq!((r.registry.as_str(), r.repository.as_str(), r.reference.as_str()), ("ghcr.io", "acme/pybase", "1.0.0"));
        let r = Reference::parse("localhost:5000/pybase@sha256:abc").unwrap();
        assert_eq!((r.registry.as_str(), r.reference.as_str()), ("localhost:5000", "sha256:abc"));
        assert_eq!(Reference::parse("ghcr.io/acme/pybase").unwrap_err().code, "ref_required");
        assert_eq!(Reference::parse("pybase:1").unwrap_err().code, "source_invalid");
    }

    #[test]
    fn basic_auth_encodes() {
        assert_eq!(basic("user", "pass"), "Basic dXNlcjpwYXNz");
        assert_eq!(basic("a", "b"), "Basic YTpi");
    }

    #[test]
    fn archive_is_deterministic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::write(dir.path().join("bundle.toml"), "[bundle]\nname='x'\n").unwrap();
        fs::write(dir.path().join("bin/tool"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path().join("bin/tool"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let a = archive(dir.path()).unwrap();
        assert_eq!(a, archive(dir.path()).unwrap());

        let out = tempfile::tempdir().unwrap();
        tar::Archive::new(GzDecoder::new(a.as_slice())).unpack(out.path()).unwrap();
        assert_eq!(crate::hash::hash_dir(out.path()).unwrap(), crate::hash::hash_dir(dir.path()).unwrap());
    }
}

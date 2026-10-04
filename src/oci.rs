//! OCI registries as a bundle transport: `oci:<registry>/<repository>:<tag>` or `@sha256:<digest>`.
//!
//! A bundle is one deterministic tar.gz layer in an OCI artifact manifest. Projects pin the
//! manifest digest in stack.lock, the same way git sources pin a commit.
//!
//! Credentials, when a registry needs them: `STACK_OCI_USERNAME` and `STACK_OCI_PASSWORD`.
//! Plain HTTP is used for loopback registries (`localhost`, `127.0.0.0/8`, `::1`). Elsewhere it
//! requires exactly `STACK_OCI_PLAIN_HTTP=1`; any other value, including `0`, `false` or empty,
//! keeps HTTPS. An HTTPS registry can never be downgraded to HTTP.

use crate::error::{io_error, Result, StackError};
use crate::hash::{is_executable, list_files, sha256_hex};
use crate::state::FileLock;
use flate2::{Compression, GzBuilder};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::path::Path;
use url::Url;

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
                    return Err(StackError::new(
                        "ref_required",
                        format!("'{s}' has no tag or digest"),
                    )
                    .hint("pin a tag or digest, e.g. oci:ghcr.io/acme/pybase:1.0.0"))
                }
            }
        };
        if repository.is_empty() || reference.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            registry: registry.into(),
            repository: repository.into(),
            reference: reference.into(),
        })
    }

    fn base(&self) -> String {
        self.base_with(plain_http_opt_in())
    }

    fn base_with(&self, plain_http: bool) -> String {
        let plain = plain_http
            || Url::parse(&format!("http://{}", self.registry)).is_ok_and(|url| loopback(&url));
        format!(
            "{}://{}/v2/{}",
            if plain { "http" } else { "https" },
            self.registry,
            self.repository
        )
    }
}

pub struct Client {
    agent: ureq::Agent,
    token: RefCell<Option<(String, String)>>,
    credential_origin: RefCell<Option<String>>,
}

impl Default for Client {
    fn default() -> Self {
        Self {
            // Redirects are handled explicitly so they cannot carry auth to another origin.
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(std::time::Duration::from_secs(30))
                .build(),
            token: RefCell::new(None),
            credential_origin: RefCell::new(None),
        }
    }
}

fn creds() -> Option<(String, String)> {
    Some((
        std::env::var("STACK_OCI_USERNAME").ok()?,
        std::env::var("STACK_OCI_PASSWORD").ok()?,
    ))
}

fn basic(user: &str, pass: &str) -> String {
    use std::fmt::Write;
    // Minimal base64 so we don't pull in a crate for one header.
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{user}:{pass}").into_bytes();
    let mut out = String::from("Basic ");
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
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
            let code_name = if code == 404 {
                "oci_not_found"
            } else if code == 401 || code == 403 {
                "oci_unauthorized"
            } else {
                "oci_failed"
            };
            let mut err = StackError::new(
                code_name,
                format!(
                    "{context}: HTTP {code} {}",
                    body.chars().take(200).collect::<String>()
                ),
            );
            if code == 401 || code == 403 {
                err = err.hint("set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for this registry");
            }
            err
        }
        other => StackError::new("oci_unreachable", format!("{context}: {other}")),
    }
}

impl Client {
    /// Authorization belongs to the challenged registry origin, never to upload locations.
    fn send(
        &self,
        method: &str,
        url: &str,
        accept: Option<&str>,
        body: Option<(&[u8], &str)>,
    ) -> Result<ureq::Response> {
        let mut target = parse_url(url)?;
        let origin = target.origin().ascii_serialization();
        let registry = self
            .credential_origin
            .borrow_mut()
            .get_or_insert(origin)
            .clone();
        let mut redirects = 0;
        let mut challenged = false;
        loop {
            validate_transport(&target, &registry)?;
            let target_origin = target.origin().ascii_serialization();
            let mut req = self.agent.request(method, target.as_str());
            if let Some(a) = accept {
                req = req.set("Accept", a);
            }
            if let Some((origin, token)) = self.token.borrow().as_ref() {
                if origin == &target_origin {
                    req = req.set("Authorization", token);
                }
            }
            let response = match body {
                Some((bytes, ct)) => req.set("Content-Type", ct).send_bytes(bytes),
                None => req.call(),
            };
            let resp = match response {
                Err(ureq::Error::Status(401, resp)) if !challenged => {
                    let challenge = resp.header("WWW-Authenticate").unwrap_or_default();
                    self.authenticate(challenge, &target, &registry)?;
                    challenged = true;
                    continue;
                }
                Err(e) => return Err(oci_err(&format!("{method} {}", target.as_str()), e)),
                Ok(resp) => resp,
            };
            if [301, 302, 303, 307, 308].contains(&resp.status()) {
                if redirects == 5 {
                    return Err(StackError::new("oci_failed", "too many registry redirects"));
                }
                let location = resp
                    .header("Location")
                    .ok_or_else(|| StackError::new("oci_failed", "redirect has no location"))?;
                target = target
                    .join(location)
                    .map_err(|_| StackError::new("oci_failed", "invalid redirect location"))?;
                redirects += 1;
                challenged = false;
                continue;
            }
            return Ok(resp);
        }
    }

    fn authenticate(&self, challenge: &str, challenged: &Url, registry: &str) -> Result<()> {
        let target_origin = challenged.origin().ascii_serialization();
        if target_origin != registry {
            return Err(StackError::new(
                "oci_auth_untrusted",
                "refusing authentication at a redirected origin",
            ));
        }
        if let Some(params) = challenge.strip_prefix("Bearer ") {
            let field = |k: &str| {
                params.split(',').find_map(|p| {
                    let (key, v) = p.trim().split_once('=')?;
                    (key == k).then(|| v.trim_matches('"').to_string())
                })
            };
            let realm = field("realm")
                .ok_or_else(|| StackError::new("oci_failed", "Bearer challenge has no realm"))?;
            let realm = parse_url(&realm)?;
            validate_transport(&realm, registry)?;
            let trusted = trusted_realm(
                &realm,
                registry,
                &std::env::var("STACK_OCI_AUTH_REALMS").unwrap_or_default(),
            );
            if !trusted {
                return Err(StackError::new("oci_auth_untrusted", format!("token origin {} is not approved", realm.origin().ascii_serialization()))
                    .hint("approve the registry's token service origin in STACK_OCI_AUTH_REALMS (comma-separated origins)"));
            }
            let mut req = self.agent.get(realm.as_str());
            for k in ["service", "scope"] {
                if let Some(v) = field(k) {
                    req = req.query(k, &v);
                }
            }
            if let Some((u, p)) = creds() {
                req = req.set("Authorization", &basic(&u, &p));
            }
            // Auth requests never follow redirects, even to another approved origin.
            let resp = req.call().map_err(|e| oci_err("authenticate", e))?;
            if resp.status() != 200 {
                return Err(StackError::new(
                    "oci_auth_untrusted",
                    "token service redirected authentication",
                ));
            }
            let bytes = Self::read_limited(resp)?;
            let body: Value = serde_json::from_slice(&bytes)
                .map_err(|_| StackError::new("oci_failed", "invalid token response"))?;
            let token = body
                .get("token")
                .or_else(|| body.get("access_token"))
                .and_then(Value::as_str)
                .ok_or_else(|| StackError::new("oci_failed", "token response contains no token"))?;
            *self.token.borrow_mut() = Some((target_origin, format!("Bearer {token}")));
            Ok(())
        } else if challenge.starts_with("Basic") {
            let (u, p) = creds().ok_or_else(|| {
                StackError::new("oci_unauthorized", "registry requires credentials")
                    .hint("set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for this registry")
            })?;
            *self.token.borrow_mut() = Some((target_origin, basic(&u, &p)));
            Ok(())
        } else {
            Err(StackError::new(
                "oci_unauthorized",
                "unsupported registry authentication challenge",
            ))
        }
    }

    fn read_limited(resp: ureq::Response) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        resp.into_reader()
            .take(MAX_BLOB + 1)
            .read_to_end(&mut buf)
            .map_err(|e| io_error("registry response", e))?;
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
        let resp = self.send("GET", &url, Some(MANIFEST_TYPE), None)?;
        let bytes = Self::read_limited(resp)?;
        Ok(format!("sha256:{}", sha256_hex(&bytes)))
    }

    /// Download the bundle at `digest` and install it at `dest`, which may be shared by other
    /// projects and processes. See [`install`].
    pub fn pull(&self, r: &Reference, digest: &str, dest: &Path) -> Result<()> {
        if dest.exists() {
            return Ok(());
        }
        let url = format!("{}/manifests/{digest}", r.base());
        let resp = self.send("GET", &url, Some(MANIFEST_TYPE), None)?;
        let bytes = Self::read_limited(resp)?;
        verify_digest(&bytes, digest, "manifest")?;
        let manifest: Value = serde_json::from_slice(&bytes)
            .map_err(|e| StackError::new("oci_failed", format!("manifest is not JSON: {e}")))?;
        let layer = manifest["layers"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["mediaType"] == LAYER_TYPE))
            .ok_or_else(|| {
                StackError::new(
                    "bundle_invalid",
                    format!("{digest} is not a stack bundle (no {LAYER_TYPE} layer)"),
                )
            })?;
        let layer_digest = layer["digest"].as_str().unwrap_or_default();

        let url = format!("{}/blobs/{layer_digest}", r.base());
        let resp = self.send("GET", &url, None, None)?;
        let blob = Self::read_limited(resp)?;
        verify_digest(&blob, layer_digest, "layer")?;
        install(&blob, dest)
    }

    /// Publish `dir` as a bundle artifact tagged `r.reference`. Returns the manifest digest.
    pub fn push(
        &self,
        dir: &Path,
        r: &Reference,
        title: &str,
        version: Option<&str>,
    ) -> Result<String> {
        if r.reference.starts_with("sha256:") {
            return Err(StackError::new(
                "source_invalid",
                "publish needs a tag, not a digest",
            ));
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
        self.send("PUT", &url, None, Some((&bytes, MANIFEST_TYPE)))?;
        Ok(format!("sha256:{}", sha256_hex(&bytes)))
    }

    fn upload(&self, r: &Reference, blob: &[u8]) -> Result<String> {
        let digest = format!("sha256:{}", sha256_hex(blob));
        let head = format!("{}/blobs/{digest}", r.base());
        if self.send("HEAD", &head, None, None).is_ok() {
            return Ok(digest);
        }
        let start = format!("{}/blobs/uploads/", r.base());
        let resp = self.send(
            "POST",
            &start,
            None,
            Some((&[], "application/octet-stream")),
        )?;
        let location = resp
            .header("Location")
            .ok_or_else(|| StackError::new("oci_failed", "registry gave no upload location"))?;
        let start_url = parse_url(&start)?;
        let mut put = start_url
            .join(location)
            .map_err(|_| StackError::new("oci_failed", "invalid upload location"))?;
        validate_transport(&put, &start_url.origin().ascii_serialization())?;
        put.query_pairs_mut().append_pair("digest", &digest);
        self.send(
            "PUT",
            put.as_str(),
            None,
            Some((blob, "application/octet-stream")),
        )?;
        Ok(digest)
    }
}

/// Extract a verified layer into the shared cache at `dest`, exactly once.
///
/// Installers of one destination serialize on a sibling lock file, recheck `dest` under it, and
/// extract into their own uniquely named staging directory. Only a complete extraction is renamed
/// into place; every failure removes the staging directory. A competing installer that already
/// published `dest` counts as success. Staging directories left by a crashed installer are
/// removed by the next one, which is safe because staging only exists while the lock is held.
pub fn install(blob: &[u8], dest: &Path) -> Result<()> {
    let (parent, name) = match (dest.parent(), dest.file_name()) {
        (Some(parent), Some(name)) => (parent, name.to_string_lossy()),
        _ => {
            return Err(StackError::new(
                "io",
                format!("invalid cache path {}", dest.display()),
            ))
        }
    };
    fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    let _lock = FileLock::acquire(&parent.join(format!(".{name}.lock")))?;
    if dest.exists() {
        return Ok(());
    }
    let prefix = format!(".{name}.staging-");
    for entry in fs::read_dir(parent)
        .map_err(|e| io_error(parent.display(), e))?
        .flatten()
    {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            remove_tree(&entry.path());
        }
    }
    let staging = Staging(
        tempfile::Builder::new()
            .prefix(&prefix)
            .tempdir_in(parent)
            .map_err(|e| io_error(parent.display(), e))?
            .keep(),
    );
    extract(blob, &staging.0)?;
    // Once renamed, nothing is left at the staging path for the guard to remove.
    fs::rename(&staging.0, dest).map_err(|e| io_error(dest.display(), e))
}

/// A staging directory, removed with everything in it when dropped (under the install lock).
struct Staging(std::path::PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}

/// Best-effort recursive removal, first granting the owner access to every directory so that
/// one without write or search permission cannot keep its contents in place.
fn remove_tree(path: &Path) {
    fn grant(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let Ok(meta) = fs::symlink_metadata(path) else {
                return;
            };
            if !meta.is_dir() {
                return;
            }
            let mode = meta.permissions().mode() | 0o700;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
            for entry in fs::read_dir(path).into_iter().flatten().flatten() {
                grant(&entry.path());
            }
        }
    }
    grant(path);
    let _ = fs::remove_dir_all(path);
}

fn extract(blob: &[u8], into: &Path) -> Result<()> {
    // `unpack` refuses entries that escape the destination (absolute paths, `..`).
    tar::Archive::new(flate2::read::GzDecoder::new(blob))
        .unpack(into)
        .map_err(|e| StackError::new("bundle_invalid", format!("cannot extract layer: {e}")))
}

fn parse_url(value: &str) -> Result<Url> {
    let url =
        Url::parse(value).map_err(|_| StackError::new("source_invalid", "invalid registry URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(StackError::new(
            "source_invalid",
            "registry URL must be HTTP(S) without userinfo or a fragment",
        ));
    }
    Ok(url)
}

fn loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

/// The development opt-in for plain HTTP to non-loopback hosts: exactly `1`.
fn plain_http_opt_in() -> bool {
    parse_plain_http(std::env::var_os("STACK_OCI_PLAIN_HTTP").as_deref())
}

fn parse_plain_http(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

fn validate_transport(target: &Url, registry: &str) -> Result<()> {
    validate_transport_with(target, registry, plain_http_opt_in())
}

fn validate_transport_with(target: &Url, registry: &str, plain_http: bool) -> Result<()> {
    parse_url(target.as_str())?;
    if target.scheme() == "http"
        && (registry.starts_with("https:") || (!loopback(target) && !plain_http))
    {
        return Err(StackError::new(
            "oci_auth_untrusted",
            "refusing an insecure registry, token, or upload URL",
        ));
    }
    Ok(())
}

fn trusted_realm(realm: &Url, registry: &str, approved: &str) -> bool {
    let origin = realm.origin().ascii_serialization();
    origin == registry || approved.split(',').any(|v| v.trim() == origin)
}

fn verify_digest(bytes: &[u8], expected: &str, what: &str) -> Result<()> {
    let actual = format!("sha256:{}", sha256_hex(bytes));
    if actual != expected {
        return Err(StackError::new(
            "content_hash_mismatch",
            format!("{what} digest {actual} != {expected}"),
        ));
    }
    Ok(())
}

/// Deterministic tar.gz: sorted paths, zeroed times and owners, only the executable bit kept.
pub fn archive(dir: &Path) -> Result<Vec<u8>> {
    let gz = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
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
        tar.append_data(&mut header, &rel, data.as_slice())
            .map_err(|e| io_error(rel.display(), e))?;
    }
    let gz = tar.into_inner().map_err(|e| io_error("archive", e))?;
    gz.finish().map_err(|e| io_error("archive", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;

    #[test]
    fn parses_references() {
        let r = Reference::parse("ghcr.io/acme/pybase:1.0.0").unwrap();
        assert_eq!(
            (
                r.registry.as_str(),
                r.repository.as_str(),
                r.reference.as_str()
            ),
            ("ghcr.io", "acme/pybase", "1.0.0")
        );
        let r = Reference::parse("localhost:5000/pybase@sha256:abc").unwrap();
        assert_eq!(
            (r.registry.as_str(), r.reference.as_str()),
            ("localhost:5000", "sha256:abc")
        );
        assert_eq!(
            Reference::parse("ghcr.io/acme/pybase").unwrap_err().code,
            "ref_required"
        );
        assert_eq!(
            Reference::parse("pybase:1").unwrap_err().code,
            "source_invalid"
        );
    }

    #[test]
    fn transport_checks_reject_downgrades_and_prefix_lookalikes() {
        assert!(!loopback(
            &Url::parse("http://localhost.attacker.example").unwrap()
        ));
        assert!(!loopback(
            &Url::parse("http://127.0.0.1.attacker.example").unwrap()
        ));
        assert!(loopback(&Url::parse("http://127.0.0.1:5000").unwrap()));
        assert!(validate_transport(
            &Url::parse("http://127.0.0.1/token").unwrap(),
            "https://registry.example"
        )
        .is_err());
        assert!(parse_url("https://user:secret@registry.example").is_err());
        assert!(parse_url("file:///tmp/token").is_err());
    }

    #[test]
    fn plain_http_requires_exactly_one() {
        use std::ffi::OsStr;
        assert!(parse_plain_http(Some(OsStr::new("1"))));
        for value in ["", "0", "false", "no", "true", "yes", " 1", "01"] {
            assert!(!parse_plain_http(Some(OsStr::new(value))), "{value:?}");
        }
        assert!(!parse_plain_http(None));
    }

    #[test]
    fn plain_http_opt_in_governs_references_and_every_transport_check() {
        let remote = Reference::parse("registry.example/acme/b:1").unwrap();
        assert_eq!(
            remote.base_with(false),
            "https://registry.example/v2/acme/b"
        );
        assert_eq!(remote.base_with(true), "http://registry.example/v2/acme/b");
        for registry in ["localhost:5000", "127.0.0.1:5000", "[::1]:5000"] {
            let local = Reference::parse(&format!("{registry}/b:1")).unwrap();
            assert_eq!(local.base_with(false), format!("http://{registry}/v2/b"));
        }
        for lookalike in ["localhost.attacker.example", "127.0.0.1.attacker.example"] {
            let r = Reference::parse(&format!("{lookalike}/b:1")).unwrap();
            assert!(r.base_with(false).starts_with("https://"), "{lookalike}");
        }

        let plain_registry = "http://127.0.0.1:5000";
        let remote_http = Url::parse("http://registry.example/token").unwrap();
        assert_eq!(
            validate_transport_with(&remote_http, plain_registry, false)
                .unwrap_err()
                .code,
            "oci_auth_untrusted"
        );
        assert!(validate_transport_with(&remote_http, plain_registry, true).is_ok());
        let local_http = Url::parse("http://localhost:5000/upload").unwrap();
        assert!(validate_transport_with(&local_http, plain_registry, false).is_ok());
        let lookalike = Url::parse("http://localhost.attacker.example/token").unwrap();
        assert!(validate_transport_with(&lookalike, plain_registry, false).is_err());
        // The opt-in never permits downgrading an HTTPS registry.
        for target in [&remote_http, &local_http] {
            assert!(validate_transport_with(target, "https://registry.example", true).is_err());
        }
    }

    #[test]
    fn basic_auth_encodes() {
        assert_eq!(basic("user", "pass"), "Basic dXNlcjpwYXNz");
        assert_eq!(basic("a", "b"), "Basic YTpi");
    }

    fn tar_gz(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        build(&mut tar);
        let raw = tar.into_inner().unwrap();
        let mut gz = GzBuilder::new().write(Vec::new(), Compression::best());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        gz.finish().unwrap()
    }

    fn files(sizes: &[usize]) -> Vec<u8> {
        tar_gz(|tar| {
            for (i, size) in sizes.iter().enumerate() {
                let mut header = tar::Header::new_gnu();
                header.set_size(*size as u64);
                header.set_mode(0o644);
                header.set_entry_type(tar::EntryType::Regular);
                tar.append_data(&mut header, format!("f{i}"), vec![0u8; *size].as_slice())
                    .unwrap();
            }
        })
    }

    fn bundle_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::write(dir.path().join("bundle.toml"), "[bundle]\nname='x'\n").unwrap();
        fs::write(dir.path().join("bin/tool"), "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.path().join("bin/tool"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn install_publishes_only_complete_trees_and_cleans_up_on_failure() {
        let source = bundle_dir();
        let blob = archive(source.path()).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let dest = cache.path().join("oci-x");
        // Residue of a crashed installer, including a directory without owner access; staging
        // only exists while the lock is held.
        let crashed = cache.path().join(".oci-x.staging-crashed");
        fs::create_dir_all(crashed.join("z")).unwrap();
        fs::write(crashed.join("z/partial"), "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(crashed.join("z"), fs::Permissions::from_mode(0o000)).unwrap();
        }
        let leftovers = || {
            let mut names: Vec<String> = fs::read_dir(cache.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        let truncated = &blob[..blob.len() / 2];
        assert_eq!(
            install(truncated, &dest).unwrap_err().code,
            "bundle_invalid"
        );
        assert_eq!(leftovers(), [".oci-x.lock"], "failure left residue");

        install(&blob, &dest).unwrap();
        assert_eq!(leftovers(), [".oci-x.lock", "oci-x"]);
        let hash = crate::hash::hash_dir(&dest).unwrap();
        assert_eq!(hash, crate::hash::hash_dir(source.path()).unwrap());
        assert!(is_executable(&dest.join("bin/tool")));

        // A published tree is never replaced or written into by a later installer.
        install(&files(&[10]), &dest).unwrap();
        assert_eq!(crate::hash::hash_dir(&dest).unwrap(), hash);
        assert_eq!(leftovers(), [".oci-x.lock", "oci-x"]);
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
            fs::set_permissions(
                dir.path().join("bin/tool"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let a = archive(dir.path()).unwrap();
        assert_eq!(a, archive(dir.path()).unwrap());

        let out = tempfile::tempdir().unwrap();
        tar::Archive::new(GzDecoder::new(a.as_slice()))
            .unpack(out.path())
            .unwrap();
        assert_eq!(
            crate::hash::hash_dir(out.path()).unwrap(),
            crate::hash::hash_dir(dir.path()).unwrap()
        );
    }
}

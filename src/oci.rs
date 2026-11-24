//! OCI registries as a bundle transport: `oci:<registry>/<repository>:<tag>` or `@sha256:<digest>`.
//!
//! A bundle is one deterministic tar.gz layer in an OCI artifact manifest. Projects pin the
//! manifest digest in stack.lock, the same way git sources pin a commit.
//!
//! Credentials, when a registry needs them: `STACK_OCI_USERNAME` and `STACK_OCI_PASSWORD`, bound
//! to the one registry origin named by `STACK_OCI_REGISTRY` (scheme, host and effective port,
//! e.g. `https://ghcr.io`). They are offered only when that exact origin challenges, and only to
//! it or to a token service it names that is its own origin or approved in
//! `STACK_OCI_AUTH_REALMS`. A reference, redirect or challenge can never widen that scope: every
//! other registry is reached anonymously. Credentials set without a valid binding fail closed.
//! Plain HTTP is used for loopback registries (`localhost`, `127.0.0.0/8`, `::1`). Elsewhere it
//! requires exactly `STACK_OCI_PLAIN_HTTP=1`; any other value, including `0`, `false` or empty,
//! keeps HTTPS. An HTTPS registry can never be downgraded to HTTP.

use crate::error::{io_error, Result, StackError};
use crate::hash::{is_executable, list_files, sha256_hex};
use crate::state::FileLock;
use flate2::{Compression, GzBuilder};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::fs;
use std::io::{self, Read};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;
use url::Url;

pub const ARTIFACT_TYPE: &str = "application/vnd.stack.bundle.v1";
pub const LAYER_TYPE: &str = "application/vnd.stack.bundle.layer.v1.tar+gzip";
const MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const EMPTY_TYPE: &str = "application/vnd.oci.empty.v1+json";
const MAX_BLOB: u64 = 256 * 1024 * 1024;
/// Longest one registry request may take, host lookup and body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Most registry host lookups running at once in this process, abandoned ones included.
const MAX_LOOKUPS: usize = 4;
static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// How far one bundle layer may expand. Digests bound what is downloaded, not what a
/// compressed archive expands to.
#[derive(Debug, Clone, Copy)]
pub struct ExtractLimits {
    /// Total size of all files.
    pub bytes: u64,
    /// Files and directories.
    pub entries: u64,
}

/// Policy: 256 MiB of files and 10,000 entries per bundle.
pub const EXTRACT_LIMITS: ExtractLimits = ExtractLimits {
    bytes: 256 * 1024 * 1024,
    entries: 10_000,
};

/// Tar headers, long names and padding allowed per entry beyond file contents.
const ENTRY_OVERHEAD: u64 = 8 * 1024;

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
        Self::with_resolver(Resolver {
            lookup: Arc::new(|netloc| netloc.to_socket_addrs().map(Iterator::collect)),
            running: &LOOKUPS,
            max: MAX_LOOKUPS,
        })
    }
}

impl Client {
    fn with_resolver(resolver: Resolver) -> Self {
        Self {
            // Redirects are handled explicitly so they cannot carry auth to another origin.
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(REQUEST_TIMEOUT)
                .resolver(resolver)
                .build(),
            token: RefCell::new(None),
            credential_origin: RefCell::new(None),
        }
    }
}

type Lookup = dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync;

/// Host lookups that end with the request making them.
///
/// ureq resolves a host before its request timeout applies, and the system resolver cannot be
/// interrupted. So each lookup runs on a thread of its own while the request waits only as long
/// as it may: REQUEST_TIMEOUT, cut short by the caller's deadline. A lookup that outlives its
/// request finishes unread; no request is ever made with what it finds. At most `max` lookups
/// run at once, abandoned ones included; beyond that a lookup fails rather than add a thread.
struct Resolver {
    lookup: Arc<Lookup>,
    running: &'static AtomicUsize,
    max: usize,
}

impl ureq::Resolver for Resolver {
    // ureq calls this on the requesting thread, whose deadline it therefore sees.
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        self.resolve_within(netloc, crate::process::bounded(REQUEST_TIMEOUT))
    }
}

impl Resolver {
    fn resolve_within(&self, netloc: &str, within: Duration) -> io::Result<Vec<SocketAddr>> {
        if let Ok(addr) = netloc.parse::<SocketAddr>() {
            return Ok(vec![addr]);
        }
        let timed_out = || {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("looking up {netloc} did not finish in time"),
            )
        };
        if within.is_zero() {
            return Err(timed_out());
        }
        if self.running.fetch_add(1, Ordering::SeqCst) >= self.max {
            self.running.fetch_sub(1, Ordering::SeqCst);
            return Err(io::Error::other(format!(
                "{} earlier registry host lookups have not finished",
                self.max
            )));
        }
        let slot = LookupSlot(self.running);
        let (lookup, host) = (self.lookup.clone(), netloc.to_owned());
        let (found, result) = mpsc::sync_channel(1);
        // Should the thread not start, the closure is dropped with the slot in it.
        std::thread::Builder::new()
            .name("registry-lookup".into())
            .spawn(move || {
                let addrs = lookup(&host);
                // Freed before the answer is sent, so a caller that receives it can look up again.
                drop(slot);
                let _ = found.send(addrs);
            })?;
        match result.recv_timeout(within) {
            Ok(addrs) => addrs,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(timed_out()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(io::Error::other(format!("looking up {netloc} failed")))
            }
        }
    }
}

/// One running lookup, counted until dropped.
struct LookupSlot(&'static AtomicUsize);

impl Drop for LookupSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The operator's registry credentials and the origin they are bound to, as configured in the
/// environment. Read when a registry challenges, never from a manifest or a server response.
#[derive(Debug, Default, Clone)]
struct CredentialEnv {
    username: Option<String>,
    password: Option<String>,
    /// `STACK_OCI_REGISTRY`, as written.
    registry: Option<String>,
}

/// What a challenged registry origin may be given.
#[derive(Debug, PartialEq)]
enum Grant {
    /// No credentials are configured: requests stay anonymous.
    Anonymous,
    /// The origin is the configured one.
    Credentials(String, String),
    /// Credentials are configured for another origin (named here); requests stay anonymous.
    Elsewhere(String),
}

impl CredentialEnv {
    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self {
            username: var("STACK_OCI_USERNAME"),
            password: var("STACK_OCI_PASSWORD"),
            registry: var("STACK_OCI_REGISTRY"),
        }
    }

    /// Whether `origin` (already serialized by [`Url::origin`]) may receive the credentials.
    /// Credentials that are incomplete, unbound or bound to something that is not an origin
    /// are an error rather than sent anywhere.
    fn grant(&self, origin: &str) -> Result<Grant> {
        let unbound = |message: String| {
            StackError::new("oci_credentials_unbound", message).hint(format!(
                "set STACK_OCI_REGISTRY to the registry origin the credentials belong to (this request was for {origin}), or unset STACK_OCI_USERNAME and STACK_OCI_PASSWORD"
            ))
        };
        let (username, password) = match (&self.username, &self.password) {
            (None, None) => return Ok(Grant::Anonymous),
            (Some(u), Some(p)) => (u, p),
            (Some(_), None) => return Err(unbound("STACK_OCI_USERNAME is set without STACK_OCI_PASSWORD; Stack sent no credentials".into())),
            (None, Some(_)) => return Err(unbound("STACK_OCI_PASSWORD is set without STACK_OCI_USERNAME; Stack sent no credentials".into())),
        };
        let configured = match self.registry.as_deref().map(str::trim) {
            None | Some("") => {
                return Err(unbound(format!(
                    "registry {origin} asked for authentication, but STACK_OCI_USERNAME and STACK_OCI_PASSWORD are not bound to a registry origin; Stack sent no credentials"
                )))
            }
            // Not echoed: a malformed value may hold userinfo.
            Some(value) => registry_origin(value).ok_or_else(|| {
                unbound(
                    "STACK_OCI_REGISTRY is not a registry origin (scheme://host[:port], no path, query or userinfo); Stack sent no credentials".into(),
                )
            })?,
        };
        Ok(if configured == origin {
            Grant::Credentials(username.clone(), password.clone())
        } else {
            Grant::Elsewhere(configured)
        })
    }
}

/// `value` as an origin, if it is exactly one: HTTP(S), a host, an optional port and nothing
/// else (a lone trailing `/` is allowed). Default ports are dropped and the host lower-cased, so
/// `https://GHCR.io:443` and `https://ghcr.io` are the same origin.
fn registry_origin(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let bare = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        // `Url` accepts `https:host`; an origin is written with `//`.
        && value.split_once("://").is_some();
    bare.then(|| url.origin().ascii_serialization())
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

/// `req` bounded by REQUEST_TIMEOUT and by the caller's deadline, if any (as within `stack up`):
/// host lookup, connection, and reading the response body all end by then.
fn bounded(req: ureq::Request) -> Result<ureq::Request> {
    if crate::process::expired() {
        return Err(StackError::new("timed_out", format!("the deadline passed before a registry {} request", req.method())));
    }
    Ok(req.timeout(crate::process::bounded(REQUEST_TIMEOUT)))
}

/// `registry` is the origin whose credentials would apply, named in the hint for 401 and 403.
fn oci_err(context: &str, registry: &str, e: ureq::Error) -> StackError {
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
                err = err.hint(format!(
                    "if you trust {registry}, set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for it with STACK_OCI_REGISTRY={registry}; credentials are sent only to that origin"
                ));
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
            let mut req = bounded(self.agent.request(method, target.as_str()))?;
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
                Err(e) => return Err(oci_err(&format!("{method} {}", target.as_str()), &registry, e)),
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

    /// Answer `challenge` from `challenged`, which must be the request's own registry origin.
    /// Credentials go only to the origin the operator bound them to (see [`CredentialEnv`]),
    /// and through it only to its own or an approved token service.
    fn authenticate(&self, challenge: &str, challenged: &Url, registry: &str) -> Result<()> {
        self.authenticate_with(challenge, challenged, registry, &CredentialEnv::from_env())
    }

    fn authenticate_with(&self, challenge: &str, challenged: &Url, registry: &str, credentials: &CredentialEnv) -> Result<()> {
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
            )?;
            if !trusted {
                return Err(StackError::new("oci_auth_untrusted", format!("token origin {} is not approved", realm.origin().ascii_serialization()))
                    .hint("approve the registry's token service origin in STACK_OCI_AUTH_REALMS (comma-separated origins)"));
            }
            let grant = credentials.grant(registry)?;
            let mut req = bounded(self.agent.get(realm.as_str()))?;
            for k in ["service", "scope"] {
                if let Some(v) = field(k) {
                    req = req.query(k, &v);
                }
            }
            if let Grant::Credentials(u, p) = &grant {
                req = req.set("Authorization", &basic(u, p));
            }
            // Auth requests never follow redirects, even to another approved origin.
            let resp = req.call().map_err(|e| oci_err("authenticate", registry, e))?;
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
            match credentials.grant(registry)? {
                Grant::Credentials(u, p) => {
                    *self.token.borrow_mut() = Some((target_origin, basic(&u, &p)));
                    Ok(())
                }
                Grant::Anonymous => Err(StackError::new("oci_unauthorized", format!("registry {registry} requires credentials"))
                    .hint(format!("if you trust {registry}, set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for it with STACK_OCI_REGISTRY={registry}"))),
                Grant::Elsewhere(configured) => Err(StackError::new(
                    "oci_unauthorized",
                    format!("registry {registry} requires credentials; STACK_OCI_USERNAME and STACK_OCI_PASSWORD are bound to {configured}, so Stack sent none"),
                )
                .hint(format!("credentials are sent only to STACK_OCI_REGISTRY; if you trust {registry}, use credentials for it with STACK_OCI_REGISTRY={registry}"))),
            }
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
        install(&blob, dest, EXTRACT_LIMITS)
    }

    /// Publish `dir` as a bundle artifact tagged `r.reference`. Returns the manifest digest.
    pub fn push(
        &self,
        dir: &Path,
        r: &Reference,
        title: &str,
        version: Option<&str>,
        force: bool,
    ) -> Result<String> {
        if r.reference.starts_with("sha256:") {
            return Err(StackError::new(
                "source_invalid",
                "publish needs a tag, not a digest",
            ));
        }
        let layer = archive(dir)?;
        let config = b"{}".to_vec();
        let manifest = Self::manifest(&layer, &config, title, version);
        let digest = format!("sha256:{}", sha256_hex(&manifest));
        // Consumers that resolved this tag earlier pinned its old digest; silently moving it
        // makes `compile --update` change their bundle. Republishing identical content is fine.
        if !force {
            match self.resolve(r) {
                Ok(existing) if existing != digest => {
                    return Err(StackError::new(
                        "tag_exists",
                        format!("{}:{} already points to {existing}", r.repository, r.reference),
                    )
                    .hint("publish a new tag, or pass --force to move this one"));
                }
                Ok(_) => {}
                Err(e) if e.code == "oci_not_found" => {}
                Err(e) => return Err(e),
            }
        }
        self.upload(r, &layer)?;
        self.upload(r, &config)?;
        let url = format!("{}/manifests/{}", r.base(), r.reference);
        self.send("PUT", &url, None, Some((&manifest, MANIFEST_TYPE)))?;
        Ok(digest)
    }

    fn manifest(layer: &[u8], config: &[u8], title: &str, version: Option<&str>) -> Vec<u8> {
        let mut annotations = json!({ "org.opencontainers.image.title": title });
        if let Some(v) = version {
            annotations["org.opencontainers.image.version"] = json!(v);
        }
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": MANIFEST_TYPE,
            "artifactType": ARTIFACT_TYPE,
            "config": { "mediaType": EMPTY_TYPE, "digest": format!("sha256:{}", sha256_hex(config)), "size": config.len() },
            "layers": [{ "mediaType": LAYER_TYPE, "digest": format!("sha256:{}", sha256_hex(layer)), "size": layer.len() }],
            "annotations": annotations,
        });
        serde_json::to_vec(&manifest).expect("manifest serializes")
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
pub fn install(blob: &[u8], dest: &Path, limits: ExtractLimits) -> Result<()> {
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
    extract(blob, &staging.0, limits)?;
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

/// Unpack only regular files and directories, within `limits`, refusing any entry that would
/// land outside `into`, inside a directory the content hash excludes, or under a non-UTF-8 name. The whole gzip stream is then read through the same budget: its length
/// and CRC must check out, anything after the tar end marker must be zero padding, and nothing
/// may follow the single gzip member.
fn extract(blob: &[u8], into: &Path, limits: ExtractLimits) -> Result<()> {
    let into = into
        .canonicalize()
        .map_err(|e| io_error(into.display(), e))?;
    let exceeded = Rc::new(Cell::new(false));
    let stream = Budget {
        inner: flate2::bufread::GzDecoder::new(blob),
        left: limits.bytes.saturating_add(
            limits
                .entries
                .saturating_add(1)
                .saturating_mul(ENTRY_OVERHEAD),
        ),
        exceeded: exceeded.clone(),
    };
    let too_large = |what: String| {
        StackError::new("bundle_too_large", format!("bundle layer exceeds {what}"))
            .hint("bundles are limited to 256 MiB of files and 10,000 entries")
    };
    let invalid = |e: std::io::Error| {
        if exceeded.get() {
            too_large(format!("{} bytes when expanded", limits.bytes))
        } else {
            StackError::new("bundle_invalid", format!("cannot extract layer: {e}"))
        }
    };
    let malformed = |what: &str| StackError::new("bundle_invalid", format!("layer {what}"));
    let mut archive = tar::Archive::new(stream);
    let (mut bytes, mut entries) = (0u64, 0u64);
    for entry in archive.entries().map_err(invalid)? {
        let entry = entry.map_err(invalid)?;
        entries += 1;
        if entries > limits.entries {
            return Err(too_large(format!("{} entries", limits.entries)));
        }
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        if !kind.is_file() && !kind.is_dir() {
            return Err(StackError::new(
                "bundle_invalid",
                format!("unsupported layer entry type {kind:?}; bundles contain only files"),
            ));
        }
        bytes = bytes
            .checked_add(entry.size())
            .filter(|b| *b <= limits.bytes)
            .ok_or_else(|| too_large(format!("{} bytes when expanded", limits.bytes)))?;
        let path = entry.path().map_err(invalid)?;
        let mut relative = std::path::PathBuf::new();
        for part in path.components() {
            match part {
                // Never hashed, so never let into the shared cache (`archive` never writes them).
                std::path::Component::Normal(part) if crate::hash::excluded(part) => {
                    return Err(malformed(&format!(
                        "entry {} is inside {}, which bundles cannot contain",
                        path.display(),
                        part.to_string_lossy()
                    )))
                }
                std::path::Component::Normal(part) if part.to_str().is_none() => {
                    return Err(malformed(&format!("entry {} is not valid UTF-8", path.display())))
                }
                std::path::Component::Normal(part) => relative.push(part),
                std::path::Component::ParentDir => {
                    return Err(malformed(&format!(
                        "entry {} escapes the bundle",
                        path.display()
                    )))
                }
                _ => {}
            }
        }
        if kind.is_dir() {
            // Created with default permissions, not the archived mode: a directory without
            // owner access could not be filled, hashed or cleaned up.
            fs::create_dir_all(into.join(&relative)).map_err(invalid)?;
            continue;
        }
        unpack_in(entry, &into, &invalid)?;
    }
    // Tar stops at its end marker; the gzip trailer behind it has not been checked yet.
    let mut stream = archive.into_inner();
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream.read(&mut chunk).map_err(invalid)?;
        if n == 0 {
            break;
        }
        if chunk[..n].iter().any(|b| *b != 0) {
            return Err(malformed("has data after the end of the archive"));
        }
    }
    if !stream.inner.into_inner().is_empty() {
        return Err(malformed("has data after the gzip stream"));
    }
    Ok(())
}

fn unpack_in<R: Read>(
    mut entry: tar::Entry<'_, R>,
    into: &Path,
    invalid: &impl Fn(std::io::Error) -> StackError,
) -> Result<()> {
    // `unpack_in` strips leading `/` and refuses symlinked parents outside `into`; it skips
    // entries containing `..`, which `extract` has already rejected rather than dropped.
    if entry.unpack_in(into).map_err(invalid)? {
        Ok(())
    } else {
        Err(StackError::new(
            "bundle_invalid",
            format!(
                "layer entry {} escapes the bundle",
                String::from_utf8_lossy(&entry.path_bytes())
            ),
        ))
    }
}

/// Fails, rather than ending early, once more than `left` decompressed bytes are read.
struct Budget<R> {
    inner: R,
    left: u64,
    exceeded: Rc<Cell<bool>>,
}

impl<R: Read> Read for Budget<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        match self.left.checked_sub(n as u64) {
            Some(left) => {
                self.left = left;
                Ok(n)
            }
            None => {
                self.exceeded.set(true);
                Err(std::io::Error::other("layer exceeds its expansion budget"))
            }
        }
    }
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

/// Whether a token service at `realm` may be asked: the registry's own origin, or one listed in
/// `approved` (comma-separated origins, normalized like `STACK_OCI_REGISTRY`). A listed value that
/// is not an origin is an error rather than silently ignored or loosely matched.
fn trusted_realm(realm: &Url, registry: &str, approved: &str) -> Result<bool> {
    let origin = realm.origin().ascii_serialization();
    if origin == registry {
        return Ok(true);
    }
    for value in approved.split(',').map(str::trim).filter(|v| !v.is_empty()) {
        let listed = registry_origin(value).ok_or_else(|| {
            StackError::new("oci_auth_untrusted", format!("STACK_OCI_AUTH_REALMS entry '{value}' is not an origin"))
                .hint("list token service origins as scheme://host[:port], comma-separated, e.g. https://auth.docker.io")
        })?;
        if listed == origin {
            return Ok(true);
        }
    }
    Ok(false)
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

    fn env(user: Option<&str>, pass: Option<&str>, registry: Option<&str>) -> CredentialEnv {
        CredentialEnv {
            username: user.map(String::from),
            password: pass.map(String::from),
            registry: registry.map(String::from),
        }
    }

    #[test]
    fn registry_origins_include_scheme_and_effective_port_and_nothing_else() {
        for (value, origin) in [
            ("https://ghcr.io", "https://ghcr.io"),
            ("https://ghcr.io/", "https://ghcr.io"),
            ("https://GHCR.io:443", "https://ghcr.io"),
            ("http://localhost:5000", "http://localhost:5000"),
            ("http://127.0.0.1:80/", "http://127.0.0.1"),
            ("https://registry.example:8443", "https://registry.example:8443"),
            (" https://ghcr.io".trim(), "https://ghcr.io"),
        ] {
            assert_eq!(registry_origin(value).as_deref(), Some(origin), "{value}");
        }
        for value in [
            "ghcr.io",
            "ghcr.io:443",
            "https:ghcr.io",
            "https://ghcr.io/v2",
            "https://ghcr.io/acme/",
            "https://ghcr.io?x=1",
            "https://ghcr.io#x",
            "https://user@ghcr.io",
            "https://user:pw@ghcr.io",
            "ftp://ghcr.io",
            "file:///tmp",
            "",
        ] {
            assert_eq!(registry_origin(value), None, "{value}");
        }
    }

    #[test]
    fn credentials_are_granted_only_to_the_configured_origin() {
        let bound = env(Some("u"), Some("p"), Some("https://registry.example"));
        assert_eq!(bound.grant("https://registry.example").unwrap(), Grant::Credentials("u".into(), "p".into()));
        // A default port written out is the same origin; anything else is not.
        let explicit_port = env(Some("u"), Some("p"), Some("https://registry.example:443/"));
        assert_eq!(explicit_port.grant("https://registry.example").unwrap(), Grant::Credentials("u".into(), "p".into()));
        for other in [
            "http://registry.example",
            "https://registry.example:8443",
            "https://registry.example.attacker.test",
            "https://attacker.test",
            "https://sub.registry.example",
        ] {
            assert_eq!(bound.grant(other).unwrap(), Grant::Elsewhere("https://registry.example".into()), "{other}");
        }
    }

    #[test]
    fn missing_credentials_stay_anonymous_and_broken_ones_fail_closed() {
        for registry in [None, Some(""), Some("https://registry.example"), Some("not an origin")] {
            assert_eq!(env(None, None, registry).grant("https://registry.example").unwrap(), Grant::Anonymous, "{registry:?}");
        }
        for (case, credentials) in [
            ("unbound", env(Some("u"), Some("p"), None)),
            ("empty binding", env(Some("u"), Some("p"), Some("  "))),
            ("host only", env(Some("u"), Some("p"), Some("registry.example"))),
            ("path", env(Some("u"), Some("p"), Some("https://registry.example/v2/acme"))),
            ("userinfo", env(Some("u"), Some("p"), Some("https://u:dummy-secret@registry.example"))),
            ("no password", env(Some("u"), None, Some("https://registry.example"))),
            ("no username", env(None, Some("p"), Some("https://registry.example"))),
        ] {
            let err = credentials.grant("https://registry.example").unwrap_err();
            assert_eq!(err.code, "oci_credentials_unbound", "{case}");
            assert!(err.message.contains("sent no credentials"), "{case}: {}", err.message);
            assert!(!err.message.contains("dummy-secret"), "{case}: {}", err.message);
        }
    }

    #[test]
    fn token_realms_are_the_registry_origin_or_explicitly_approved_origins() {
        let realm = |s: &str| Url::parse(s).unwrap();
        let registry = "https://registry.example";
        assert!(trusted_realm(&realm("https://registry.example/token"), registry, "").unwrap());
        assert!(!trusted_realm(&realm("https://auth.example/token"), registry, "").unwrap());
        for approved in ["https://auth.example", " https://auth.example/ ", "https://AUTH.example:443", "https://x.example, https://auth.example"] {
            assert!(trusted_realm(&realm("https://auth.example/token"), registry, approved).unwrap(), "{approved}");
        }
        for approved in ["http://auth.example", "https://auth.example:8443", "https://auth.example.attacker.test", "https://example"] {
            assert!(!trusted_realm(&realm("https://auth.example/token"), registry, approved).unwrap(), "{approved}");
        }
        // A listed value that is not an origin is reported, never loosely matched.
        for approved in ["auth.example", "https://auth.example/token"] {
            assert_eq!(trusted_realm(&realm("https://auth.example/token"), registry, approved).unwrap_err().code, "oci_auth_untrusted", "{approved}");
        }
    }

    #[test]
    fn basic_challenges_receive_credentials_only_at_the_configured_origin() {
        let challenged = Url::parse("https://registry.example/v2/b/manifests/1").unwrap();
        let registry = "https://registry.example";
        let client = Client::default();
        client
            .authenticate_with("Basic realm=\"r\"", &challenged, registry, &env(Some("u"), Some("p"), Some(registry)))
            .unwrap();
        assert_eq!(client.token.borrow().as_ref().unwrap(), &(registry.to_string(), basic("u", "p")));

        for (credentials, code) in [
            (env(Some("u"), Some("p"), Some("https://other.example")), "oci_unauthorized"),
            (env(None, None, None), "oci_unauthorized"),
            (env(Some("u"), Some("p"), None), "oci_credentials_unbound"),
        ] {
            let client = Client::default();
            let err = client.authenticate_with("Basic realm=\"r\"", &challenged, registry, &credentials).unwrap_err();
            assert_eq!(err.code, code, "{credentials:?}");
            assert!(client.token.borrow().is_none());
        }
        // A challenge from anywhere but the request's own registry is never answered.
        let redirected = Url::parse("https://other.example/v2/b/manifests/1").unwrap();
        let client = Client::default();
        let err = client
            .authenticate_with("Basic realm=\"r\"", &redirected, registry, &env(Some("u"), Some("p"), Some("https://other.example")))
            .unwrap_err();
        assert_eq!(err.code, "oci_auth_untrusted");
        assert!(client.token.borrow().is_none());
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

    /// Install into a fresh cache directory; return the result and what the directory holds.
    fn install_into(blob: &[u8], limits: ExtractLimits) -> (Result<()>, Vec<String>) {
        let cache = tempfile::tempdir().unwrap();
        let result = install(blob, &cache.path().join("oci-x"), limits);
        let mut names: Vec<String> = fs::read_dir(cache.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        (result, names)
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
            install(truncated, &dest, EXTRACT_LIMITS).unwrap_err().code,
            "bundle_invalid"
        );
        assert_eq!(leftovers(), [".oci-x.lock"], "failure left residue");

        install(&blob, &dest, EXTRACT_LIMITS).unwrap();
        assert_eq!(leftovers(), [".oci-x.lock", "oci-x"]);
        let hash = crate::hash::hash_dir(&dest).unwrap();
        assert_eq!(hash, crate::hash::hash_dir(source.path()).unwrap());
        assert!(is_executable(&dest.join("bin/tool")));

        // A published tree is never replaced or written into by a later installer.
        install(&files(&[10]), &dest, EXTRACT_LIMITS).unwrap();
        assert_eq!(crate::hash::hash_dir(&dest).unwrap(), hash);
        assert_eq!(leftovers(), [".oci-x.lock", "oci-x"]);
    }

    #[test]
    fn expansion_is_bounded_in_bytes_at_the_exact_limit() {
        let limits = ExtractLimits {
            bytes: 64 * 1024,
            entries: 100,
        };
        // Highly compressible: kilobytes download, the limit is on what they expand to.
        let at_limit = files(&[64 * 1024]);
        let over = files(&[64 * 1024 + 1]);
        assert!(over.len() < 1024);
        assert_eq!(install_into(&at_limit, limits).1, [".oci-x.lock", "oci-x"]);
        let (result, left) = install_into(&over, limits);
        assert_eq!(result.unwrap_err().code, "bundle_too_large");
        assert_eq!(left, [".oci-x.lock"]);

        // Aggregate across files.
        let limits = ExtractLimits {
            bytes: 1200,
            entries: 100,
        };
        assert!(install_into(&files(&[400, 400, 400]), limits).0.is_ok());
        let (result, left) = install_into(&files(&[400, 400, 401]), limits);
        assert_eq!(result.unwrap_err().code, "bundle_too_large");
        assert_eq!(left, [".oci-x.lock"]);
    }

    #[test]
    fn expansion_is_bounded_in_entries_at_the_exact_limit() {
        let limits = ExtractLimits {
            bytes: 1024,
            entries: 3,
        };
        assert!(install_into(&files(&[1, 1, 1]), limits).0.is_ok());
        let (result, left) = install_into(&files(&[1, 1, 1, 1]), limits);
        assert_eq!(result.unwrap_err().code, "bundle_too_large");
        assert_eq!(left, [".oci-x.lock"]);
        let empty_files = files(&[0; 4]);
        assert_eq!(
            install_into(&empty_files, limits).0.unwrap_err().code,
            "bundle_too_large"
        );
    }

    #[test]
    fn oversized_tar_metadata_is_bounded_before_it_is_buffered() {
        // A GNU long name is read into memory before any entry is returned.
        let blob = tar_gz(|tar| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::GNULongName);
            header.set_size(200_000);
            header.set_cksum();
            tar.append(&header, vec![b'a'; 200_000].as_slice()).unwrap();
            let mut file = tar::Header::new_gnu();
            file.set_size(0);
            file.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut file, "f", &[][..]).unwrap();
        });
        let (result, left) = install_into(
            &blob,
            ExtractLimits {
                bytes: 1024,
                entries: 4,
            },
        );
        assert_eq!(result.unwrap_err().code, "bundle_too_large");
        assert_eq!(left, [".oci-x.lock"]);
    }

    #[test]
    fn links_devices_and_escaping_paths_are_rejected_without_residue() {
        let symlink = tar_gz(|tar| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            tar.append_link(&mut header, "link", "/etc/passwd").unwrap();
        });
        let escaping = tar_gz(|tar| {
            let mut header = tar::Header::new_old();
            header.as_old_mut().name[..9].copy_from_slice(b"../escape");
            header.set_size(1);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append(&header, &b"x"[..]).unwrap();
        });
        for (blob, reason) in [
            (symlink, "unsupported layer entry type"),
            (escaping, "escapes the bundle"),
            (b"not a gzip stream".to_vec(), "cannot extract layer"),
        ] {
            let (result, left) = install_into(&blob, EXTRACT_LIMITS);
            let err = result.unwrap_err();
            assert_eq!(err.code, "bundle_invalid");
            assert!(err.message.contains(reason), "{}", err.message);
            assert_eq!(left, [".oci-x.lock"]);
        }
    }

    #[test]
    fn layers_cannot_carry_content_the_hash_would_skip() {
        let raw_name = |name: &[u8]| {
            gzip(&tar(|t| {
                let mut header = tar::Header::new_old();
                header.as_old_mut().name[..name.len()].copy_from_slice(name);
                header.set_size(1);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                t.append(&header, &b"x"[..]).unwrap();
            }))
        };
        for (blob, reason) in [
            (gzip(&tar(|t| file_entry(t, ".git/config", b"x"))), "inside .git"),
            (gzip(&tar(|t| file_entry(t, "bin/.stack/tool", b"x"))), "inside .stack"),
            (gzip(&tar(|t| dir_entry(t, ".stack", 0o755))), "inside .stack"),
            (raw_name(b"bin/\xff"), "not valid UTF-8"),
        ] {
            let (result, left) = install_into(&blob, EXTRACT_LIMITS);
            let err = result.unwrap_err();
            assert_eq!(err.code, "bundle_invalid", "{}", err.message);
            assert!(err.message.contains(reason), "{}", err.message);
            assert_eq!(left, [".oci-x.lock"]);
        }
        // Names that merely resemble them are ordinary content.
        let lookalikes = gzip(&tar(|t| {
            file_entry(t, ".gitignore", b"x");
            file_entry(t, "bin/.stackrc", b"x");
        }));
        assert!(install_into(&lookalikes, EXTRACT_LIMITS).0.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn publishing_refuses_links_instead_of_dropping_them() {
        let source = bundle_dir();
        std::os::unix::fs::symlink("/etc/hosts", source.path().join("bin/hosts")).unwrap();
        let err = archive(source.path()).unwrap_err();
        assert_eq!(err.code, "bundle_invalid");
        assert!(err.message.contains("symbolic link"), "{}", err.message);
    }

    fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut gz = GzBuilder::new().write(Vec::new(), Compression::best());
        std::io::Write::write_all(&mut gz, raw).unwrap();
        gz.finish().unwrap()
    }

    fn tar(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        build(&mut tar);
        tar.into_inner().unwrap()
    }

    fn file_entry(tar: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, path, data).unwrap();
    }

    /// A directory entry with `mode`, written raw so any path, including `..`, is kept.
    fn dir_entry(tar: &mut tar::Builder<Vec<u8>>, path: &str, mode: u32) {
        let mut header = tar::Header::new_old();
        header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_size(0);
        header.set_mode(mode);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_cksum();
        tar.append(&header, &[][..]).unwrap();
    }

    #[test]
    fn the_gzip_trailer_is_verified_before_publication() {
        let blob = files(&[10]);
        let mut bad_crc = blob.clone();
        let at = bad_crc.len() - 8;
        bad_crc[at] ^= 0xff;
        let mut bad_length = blob.clone();
        let at = bad_length.len() - 1;
        bad_length[at] ^= 0xff;
        let mut two_members = blob.clone();
        two_members.extend(files(&[10]));
        let mut trailing = blob.clone();
        trailing.extend(b"junk");
        for (blob, reason) in [
            (blob[..blob.len() - 8].to_vec(), "cannot extract layer"),
            (blob[..blob.len() - 1].to_vec(), "cannot extract layer"),
            (bad_crc, "cannot extract layer"),
            (bad_length, "cannot extract layer"),
            (two_members, "after the gzip stream"),
            (trailing, "after the gzip stream"),
        ] {
            let (result, left) = install_into(&blob, EXTRACT_LIMITS);
            let err = result.unwrap_err();
            assert_eq!(err.code, "bundle_invalid", "{}", err.message);
            assert!(err.message.contains(reason), "{}", err.message);
            assert_eq!(left, [".oci-x.lock"]);
        }
    }

    #[test]
    fn data_after_the_tar_end_marker_is_bounded_and_must_be_padding() {
        let limits = ExtractLimits {
            bytes: 1024,
            entries: 2,
        };
        let archive = tar(|t| file_entry(t, "f", b"x"));
        let mut padded = archive.clone();
        padded.extend(vec![0u8; 8192]);
        assert!(install_into(&gzip(&padded), limits).0.is_ok());

        // Expands far beyond the stream budget after the end marker.
        let mut expanding = archive.clone();
        expanding.extend(vec![0u8; 100_000]);
        let (result, left) = install_into(&gzip(&expanding), limits);
        assert_eq!(result.unwrap_err().code, "bundle_too_large");
        assert_eq!(left, [".oci-x.lock"]);

        let mut junk = archive;
        junk.extend(b"not padding");
        let (result, left) = install_into(&gzip(&junk), limits);
        let err = result.unwrap_err();
        assert!(
            err.message.contains("after the end of the archive"),
            "{}",
            err.message
        );
        assert_eq!(left, [".oci-x.lock"]);
    }

    #[test]
    fn restrictive_directory_modes_cannot_block_cleanup_or_reuse() {
        let cache = tempfile::tempdir().unwrap();
        let dest = cache.path().join("oci-x");
        let leftovers = || {
            let mut names: Vec<String> = fs::read_dir(cache.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        // An unreadable directory, then a path that is rejected after it was seen.
        let escaping = gzip(&tar(|t| {
            file_entry(t, "z/f", b"x");
            dir_entry(t, "z", 0o000);
            dir_entry(t, "../escape", 0o755);
        }));
        let err = install(&escaping, &dest, EXTRACT_LIMITS).unwrap_err();
        assert!(
            err.message.contains("escapes the bundle"),
            "{}",
            err.message
        );
        assert_eq!(leftovers(), [".oci-x.lock"]);

        // The same directory, then a failure while reading the gzip trailer.
        let mut late = gzip(&tar(|t| {
            file_entry(t, "z/f", b"x");
            dir_entry(t, "z", 0o000);
        }));
        let at = late.len() - 8;
        late[at] ^= 0xff;
        assert_eq!(
            install(&late, &dest, EXTRACT_LIMITS).unwrap_err().code,
            "bundle_invalid"
        );
        assert_eq!(leftovers(), [".oci-x.lock"]);

        // A published tree stays readable whatever modes its directories were archived with.
        let valid = gzip(&tar(|t| {
            file_entry(t, "z/f", b"x");
            dir_entry(t, "z", 0o000);
        }));
        install(&valid, &dest, EXTRACT_LIMITS).unwrap();
        assert_eq!(leftovers(), [".oci-x.lock", "oci-x"]);
        assert_eq!(fs::read(dest.join("z/f")).unwrap(), b"x");
        crate::hash::hash_dir(&dest).unwrap();
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

    /// Lookups that stall like an unresponsive resolver until `open` is called.
    struct StalledLookups {
        open: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        calls: Arc<AtomicUsize>,
        running: &'static AtomicUsize,
    }

    impl StalledLookups {
        fn new() -> Self {
            Self {
                open: Arc::default(),
                calls: Arc::default(),
                running: Box::leak(Box::new(AtomicUsize::new(0))),
            }
        }

        /// A resolver answering `answered` lookups with `addr` at once, then stalling.
        fn resolver(&self, answered: usize, addr: SocketAddr, max: usize) -> Resolver {
            let (open, calls) = (self.open.clone(), self.calls.clone());
            Resolver {
                lookup: Arc::new(move |_| {
                    if calls.fetch_add(1, Ordering::SeqCst) >= answered {
                        let (lock, cvar) = &*open;
                        let guard = lock.lock().unwrap();
                        drop(cvar.wait_timeout_while(guard, Duration::from_secs(20), |open| !*open).unwrap());
                    }
                    Ok(vec![addr])
                }),
                running: self.running,
                max,
            }
        }

        fn open(&self) {
            *self.open.0.lock().unwrap() = true;
            self.open.1.notify_all();
        }

        fn wait_until_finished(&self) {
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while self.running.load(Ordering::SeqCst) > 0 {
                assert!(std::time::Instant::now() < until, "abandoned lookups never finished");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn lookups_end_in_time_and_never_outnumber_their_limit() {
        let lookups = StalledLookups::new();
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let resolver = lookups.resolver(0, addr, 2);
        for _ in 0..2 {
            let start = std::time::Instant::now();
            let err = resolver.resolve_within("stalled.test:443", Duration::from_millis(200)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
            assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
        }
        // Both abandoned lookups still run; a third is refused at once rather than add a thread.
        let start = std::time::Instant::now();
        let err = resolver.resolve_within("stalled.test:443", Duration::from_secs(5)).unwrap_err();
        assert!(err.to_string().contains("have not finished"), "{err}");
        assert!(start.elapsed() < Duration::from_millis(500), "{:?}", start.elapsed());
        assert_eq!(lookups.calls.load(Ordering::SeqCst), 2);
        assert_eq!(lookups.running.load(Ordering::SeqCst), 2);
        // Addresses need no lookup, and an exhausted deadline starts none.
        assert_eq!(resolver.resolve_within("127.0.0.1:5000", Duration::ZERO).unwrap(), ["127.0.0.1:5000".parse().unwrap()]);
        assert_eq!(resolver.resolve_within("[::1]:5000", Duration::ZERO).unwrap(), ["[::1]:5000".parse().unwrap()]);
        assert_eq!(resolver.resolve_within("later.test:443", Duration::ZERO).unwrap_err().kind(), io::ErrorKind::TimedOut);

        // Once the resolver answers, the abandoned lookups end and free their places.
        lookups.open();
        lookups.wait_until_finished();
        assert_eq!(resolver.resolve_within("stalled.test:443", Duration::from_secs(5)).unwrap(), [addr]);
        assert_eq!(lookups.running.load(Ordering::SeqCst), 0);
    }

    /// A registry at `127.0.0.1:<port>`, reached as `localhost:<port>`. `/auth` is challenged
    /// by a token service on the same origin, `/redirect` moves, `/body` never sends its body.
    /// Returns the address and every request path received.
    fn registry() -> (SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let path = line.split_whitespace().nth(1).unwrap_or_default().to_string();
                    let mut header = String::new();
                    while reader.read_line(&mut header).unwrap_or(0) > 0 && header != "\r\n" {
                        header.clear();
                    }
                    log.lock().unwrap().push(path.clone());
                    let port = addr.port();
                    let head = if path.ends_with("/auth") {
                        format!("401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"http://localhost:{port}/token\"\r\nContent-Length: 0")
                    } else if path.ends_with("/redirect") {
                        "307 Moved\r\nLocation: /v2/x/manifests/moved\r\nContent-Length: 0".into()
                    } else {
                        "200 OK\r\nContent-Length: 100".into()
                    };
                    let _ = write!(stream, "HTTP/1.1 {head}\r\nConnection: close\r\n\r\n");
                    // The body is never sent; the connection stays open well past any deadline.
                    std::thread::sleep(Duration::from_secs(10));
                });
            }
        });
        (addr, seen)
    }

    #[test]
    fn registry_requests_end_at_the_deadline_whatever_they_wait_for() {
        let (addr, seen) = registry();
        let port = addr.port();
        // (path, lookups answered before the resolver stalls, what was cut short)
        for (path, answered, cut_short) in [
            ("latest", 0, "did not finish in time"),
            ("auth", 1, "did not finish in time"),
            ("redirect", 1, "did not finish in time"),
            ("body", 1, "timed out"),
        ] {
            let lookups = StalledLookups::new();
            let client = Client::with_resolver(lookups.resolver(answered, addr, MAX_LOOKUPS));
            let url = format!("http://localhost:{port}/v2/x/manifests/{path}");
            let requests = seen.lock().unwrap().len();
            let start = std::time::Instant::now();
            let result = {
                let _deadline = crate::process::deadline_scope(Some(start + Duration::from_millis(300)));
                client.send("GET", &url, None, None).and_then(Client::read_limited)
            };
            let took = start.elapsed();
            let err = result.unwrap_err();
            assert!(took < Duration::from_secs(2), "{path}: {took:?}");
            assert!(err.message.contains(cut_short), "{path}: {err:?}");
            assert_eq!(lookups.calls.load(Ordering::SeqCst), answered + usize::from(path != "body"), "{path}");

            // A lookup answering late makes no request with what it found.
            lookups.open();
            lookups.wait_until_finished();
            let after = seen.lock().unwrap()[requests..].to_vec();
            assert_eq!(after.len(), answered.max(usize::from(path == "body")), "{path}: {after:?}");
        }
    }
}

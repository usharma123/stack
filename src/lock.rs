use crate::error::{io_error, Result, StackError};
use crate::tool::ToolOptions;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const LOCK_FILE: &str = "stack.lock";
/// Version 3 embeds the provider's artifact lock (`[provider_lock]`). Version 2 (exact tool and
/// service versions, no artifacts) stays valid for locked operations under the `best-effort`
/// artifact policy; the next `stack compile` rewrites it as version 3. Version 1 locks are read
/// for migration: `stack compile` rewrites them, and every locked operation refuses them.
const LOCK_VERSION: u32 = 3;
const V2: u32 = 2;
const LEGACY_VERSION: u32 = 1;

/// Exactly which bundle contents and tool versions a project was compiled from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lockfile {
    pub version: u32,
    #[serde(default, rename = "bundle")]
    pub bundles: Vec<LockedBundle>,
    /// Every requested tool, including tools stack adds for its provider (pitchfork).
    #[serde(default, rename = "tool", skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<LockedVersion>,
    /// Preset services whose version stack resolves.
    #[serde(default, rename = "service", skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<LockedVersion>,
    /// The provider's own artifact lock (mise.lock), re-nested verbatim under one table with a
    /// `provider` key, so fields and tables stack does not know survive a round trip. See
    /// `artifacts` for the rules. Version 3 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_lock: Option<toml::Table>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedBundle {
    /// The `[[use]] bundle` string as written in stack.toml.
    pub source: String,
    pub name: String,
    /// Resolved commit for git sources. Tags and branches never re-resolve without `--update`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Resolved manifest digest for OCI sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub content_hash: String,
}

/// One version request and the exact version it resolved to. An exact version names a
/// release; it is not a checksum of the artifact the provider downloads for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedVersion {
    /// Tool name (`python`, `npm:prettier`) or service name (`postgres`).
    pub name: String,
    /// For services: the provider tool the preset installs (`postgres`, `redis`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The version as composed from bundles and the project, e.g. `3.13` or `latest`.
    pub requested: String,
    /// The exact version rendered into the provider config and installed.
    pub resolved: String,
    /// The platform the resolution ran on. One version applies to every platform; a release
    /// a platform lacks fails at install rather than resolving differently per machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_on: Option<String>,
    /// Allowlisted provider options declared with the request (`mr_boxington`, packslip trust
    /// options). Part of the pin's identity: a changed option is a changed request.
    #[serde(default, skip_serializing_if = "ToolOptions::is_empty")]
    pub options: ToolOptions,
}

impl Lockfile {
    pub fn new(bundles: Vec<LockedBundle>, tools: Vec<LockedVersion>, services: Vec<LockedVersion>) -> Self {
        Self {
            version: LOCK_VERSION,
            bundles,
            tools,
            services,
            provider_lock: None,
        }
    }

    pub fn find(&self, source: &str) -> Option<&LockedBundle> {
        self.bundles.iter().find(|b| b.source == source)
    }

    pub fn tool(&self, name: &str) -> Option<&LockedVersion> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn service(&self, name: &str) -> Option<&LockedVersion> {
        self.services.iter().find(|t| t.name == name)
    }

    /// A version 1 lock: bundle pins only, no exact versions.
    pub fn is_legacy(&self) -> bool {
        self.version == LEGACY_VERSION
    }

    /// A version 2 lock: exact versions, no artifact lock.
    pub fn is_v2(&self) -> bool {
        self.version == V2
    }
}

pub fn read(root: &Path) -> Result<Option<Lockfile>> {
    let path = root.join(LOCK_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| io_error(path.display(), e))?;
    let lock: Lockfile = toml::from_str(&text)
        .map_err(|e| StackError::new("lock_invalid", format!("{LOCK_FILE}: {}", e.message())))?;
    if ![LOCK_VERSION, V2, LEGACY_VERSION].contains(&lock.version) {
        return Err(StackError::new(
            "lock_invalid",
            format!("unsupported {LOCK_FILE} version {}", lock.version),
        )
        .hint("this stack.lock was written by a newer stack; upgrade stack"));
    }
    if lock.is_legacy() && (!lock.tools.is_empty() || !lock.services.is_empty()) {
        return Err(StackError::new(
            "lock_invalid",
            format!("{LOCK_FILE} version 1 cannot contain tool or service versions"),
        ));
    }
    // The provider lock's shape and agreement with the pins are checked where it is used
    // (`artifacts::validate`), so that `stack compile --update` can replace a bad one.
    if lock.provider_lock.is_some() && lock.version != LOCK_VERSION {
        return Err(StackError::new(
            "lock_invalid",
            format!("{LOCK_FILE} version {} cannot contain [provider_lock]", lock.version),
        ));
    }
    Ok(Some(lock))
}

pub fn write(root: &Path, lock: &Lockfile) -> Result<()> {
    let path = root.join(LOCK_FILE);
    let body = toml::to_string_pretty(lock).expect("lockfile serializes");
    let text = format!("# Generated by `stack compile`. Commit this file.\n{body}");
    fs::write(&path, text).map_err(|e| io_error(path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Lockfile> {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(LOCK_FILE), text).unwrap();
        read(dir.path()).map(Option::unwrap)
    }

    const V3: &str = r#"
version = 3

[[tool]]
name = "fnox"
requested = "1.39.0"
resolved = "1.39.0"
resolved_on = "macos-arm64"

[[service]]
name = "db"
tool = "postgres"
requested = "17"
resolved = "17.6"

[provider_lock]
provider = "mise"
lockfile_version = 3
future_top_level = { kept = true }

[[provider_lock.tools.fnox]]
version = "1.39.0"
backend = "packslip:github.com/jdx/fnox"
specifiers = ["1.39.0"]
future_entry_field = "kept"

[provider_lock.tools.fnox."platforms.macos-arm64"]
checksum = "sha256:5604196e"
url = "https://example.invalid/fnox.tar.gz"
signer = "sigstore-oidc:https://github.com/jdx/fnox/.github/workflows/release.yml"
repository_ids = { repository = "1078762196" }
future_platform_field = 7

[[provider_lock.tools.postgres]]
version = "17.6"
backend = "conda:postgresql"
specifiers = ["17.6"]

[provider_lock.tools.postgres.options]
channel = "conda-forge"

[provider_lock.tools.postgres."platforms.macos-arm64"]
checksum = "sha256:37d5"
url = "https://example.invalid/postgresql.conda"
conda_deps = ["libpq-17.6"]

[provider_lock.conda-packages.macos-arm64."libpq-17.6"]
url = "https://example.invalid/libpq.conda"
checksum = "sha256:f109"
"#;

    #[test]
    fn version_3_round_trips_with_unknown_provider_fields() {
        let lock = parse(V3).unwrap();
        assert_eq!(lock.version, 3);
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &lock).unwrap();
        let again = read(dir.path()).unwrap().unwrap();
        assert_eq!(again, lock);
        // Compared as parsed TOML documents, not as text or through mise's dry run.
        let a: toml::Table = toml::from_str(V3).unwrap();
        let b: toml::Table = toml::from_str(&fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap()).unwrap();
        assert_eq!(a["provider_lock"], b["provider_lock"]);
        let fnox = &b["provider_lock"]["tools"]["fnox"][0];
        assert_eq!(fnox["future_entry_field"].as_str(), Some("kept"));
        assert_eq!(fnox["platforms.macos-arm64"]["future_platform_field"].as_integer(), Some(7));
        assert_eq!(b["provider_lock"]["future_top_level"]["kept"].as_bool(), Some(true));
    }

    #[test]
    fn versions_1_and_2_still_parse_and_newer_versions_are_refused() {
        let v2 = parse("version = 2\n[[tool]]\nname = \"jq\"\nrequested = \"1.7\"\nresolved = \"1.7.1\"\nresolved_on = \"macos-aarch64\"\n").unwrap();
        assert!(v2.is_v2() && v2.provider_lock.is_none());
        assert!(parse("version = 1\n").unwrap().is_legacy());
        let e = parse("version = 4\n").unwrap_err();
        assert_eq!(e.code, "lock_invalid");
        assert!(e.hint.unwrap().contains("newer stack"));
        let e = parse("version = 2\n[provider_lock]\nprovider = \"mise\"\n").unwrap_err();
        assert_eq!(e.code, "lock_invalid", "{e:?}");
    }

    #[test]
    fn a_malformed_provider_lock_is_lock_invalid_where_it_is_used() {
        for bad in [
            "[provider_lock]\nprovider = \"other\"\n",
            "[provider_lock]\nlockfile_version = 3\n",
            "[provider_lock]\nprovider = \"mise\"\ntools = 3\n",
            "[provider_lock]\nprovider = \"mise\"\n[provider_lock.tools]\njq = { version = \"1\" }\n",
            "[provider_lock]\nprovider = \"mise\"\n[[provider_lock.tools.jq]]\nbackend = \"aqua:jqlang/jq\"\n",
            "[provider_lock]\nprovider = \"mise\"\n[[provider_lock.tools.jq]]\nversion = \"1\"\n\"platforms.linux-x64\" = 3\n",
            "[provider_lock]\nprovider = \"mise\"\n[provider_lock.conda-packages]\nlinux-x64 = 3\n",
            "[provider_lock]\nprovider = \"mise\"\n[[provider_lock.tools.jq]]\nversion = \"1\"\n[provider_lock.tools.jq.\"platforms.linux-x64\"]\nurl = \"{{ exec(command='touch sentinel') }}\"\n",
            "[provider_lock]\nprovider = \"mise\"\nfuture = [\"{% if 1 %}x{% endif %}\"]\n",
        ] {
            let lock = parse(&format!("version = 3\n{bad}")).unwrap();
            let e = crate::artifacts::validate(lock.provider_lock.as_ref().unwrap(), &[]).unwrap_err();
            assert_eq!(e.code, "lock_invalid", "{bad}: {e:?}");
        }
    }
}

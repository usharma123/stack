//! `bundle.toml` (a shareable definition) and `stack.toml` (a project that uses bundles).

use crate::error::{Result, StackError};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const BUNDLE_FILE: &str = "bundle.toml";
pub const PROJECT_FILE: &str = "stack.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub bundle: BundleMeta,
    #[serde(default)]
    pub tools: IndexMap<String, String>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub services: IndexMap<String, Service>,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
    #[serde(default)]
    pub paths: Paths,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleMeta {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    /// Directories inside the bundle to put on PATH.
    #[serde(default)]
    pub bin: Vec<String>,
}

/// A service definition. Either a provider `preset` or a custom `run` command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_port: Option<u16>,
    /// `"auto"` (default) or a fixed port. Fixed ports belong to projects, not bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<toml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Services that must be running and ready before the task starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    #[serde(rename = "use", default)]
    pub uses: Vec<UseEntry>,
    #[serde(default)]
    pub tools: IndexMap<String, String>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub services: IndexMap<String, Service>,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
    /// Explicit resolutions. The only way to replace a value another layer defines.
    #[serde(rename = "override", default)]
    pub overrides: Overrides,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseEntry {
    /// `git+<url>?ref=<ref>` or `path:<dir>`
    pub bundle: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default)]
    pub tools: IndexMap<String, String>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub services: IndexMap<String, Service>,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
}

impl Service {
    pub fn validate(&self, name: &str, origin: &str) -> Result<()> {
        if self.preset.is_some() == self.run.is_some() {
            return Err(StackError::new(
                "invalid_service",
                format!("service '{name}' in {origin} must set exactly one of `preset` or `run`"),
            ));
        }
        match &self.port {
            None => {}
            Some(toml::Value::String(s)) if s == "auto" => {}
            Some(toml::Value::Integer(p)) if (1..=65535).contains(p) => {}
            Some(other) => {
                return Err(StackError::new(
                    "invalid_service",
                    format!("service '{name}' in {origin} has port {other}; use \"auto\" or 1-65535"),
                ));
            }
        }
        Ok(())
    }

    /// The pinned port, if any. Only meaningful after `validate`.
    pub fn fixed_port(&self) -> Option<u16> {
        match &self.port {
            Some(toml::Value::Integer(p)) => u16::try_from(*p).ok(),
            _ => None,
        }
    }
}

pub fn read_project(root: &Path) -> Result<ProjectManifest> {
    let path = root.join(PROJECT_FILE);
    let text = fs::read_to_string(&path).map_err(|e| {
        StackError::new("manifest_missing", format!("cannot read {}: {e}", path.display()))
            .hint("create a stack.toml; see README.md for the format")
    })?;
    toml::from_str(&text).map_err(|e| {
        StackError::new("manifest_invalid", format!("{}: {}", path.display(), e.message()))
    })
}

pub fn read_bundle(dir: &Path, source: &str) -> Result<BundleManifest> {
    let path = dir.join(BUNDLE_FILE);
    let text = fs::read_to_string(&path).map_err(|e| {
        StackError::new("bundle_invalid", format!("{source} has no readable {BUNDLE_FILE}: {e}"))
    })?;
    toml::from_str(&text)
        .map_err(|e| StackError::new("bundle_invalid", format!("{source}: {}", e.message())))
}

//! Artifact checksums: the provider's lock, embedded in stack.lock and rendered for mise.
//!
//! Stack never hashes artifacts itself. mise records a checksum (and for packslip a signer) per
//! release and platform when it locks, and checks them when it downloads. Stack's part is to
//! make the lock mise checks against a function of committed state, to keep committed values
//! from changing without `stack compile --update`, and to say exactly which pins are covered on
//! which platforms.
//!
//! `[provider_lock]` in stack.lock is mise's `mise.lock` re-nested verbatim under one table,
//! plus two keys of stack's own: `provider = "mise"` and `stack_sidecars`, the releases whose
//! sidecar reference (npm's `aube`, Python's `uv`) stack removed at capture. Rendering strips
//! those two keys and writes the rest as `.config/mise/mise.lock`; capturing is the inverse.
//! Tables and fields stack does not know survive both directions untouched.
//!
//! Pins and provider entries are matched by tool name as rendered (a service's preset tool) and
//! version. The `options` inside an embedded entry are mise's own (a conda channel) and are
//! never compared with the options a manifest declares, which live on stack's `[[tool]]` pin.

use crate::error::{io_error, Result, StackError};
use crate::lock::Lockfile;
use crate::provider::mise::CalVer;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{json, Value as Json};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const PROVIDER: &str = "mise";
const PROVIDER_KEY: &str = "provider";
/// Stack's record of releases whose sidecar reference it stripped at capture.
const SIDECARS_KEY: &str = "stack_sidecars";
const TOOLS: &str = "tools";
const CONDA: &str = "conda-packages";
const PLATFORM_PREFIX: &str = "platforms.";
/// Entry-level keys that point at a sidecar file beside mise.lock, which stack does not carry.
const SIDECAR_KEYS: &[&str] = &["aube", "uv"];

/// Stack's own release matrix: what `[lock] platforms` means when a project does not say.
pub const DEFAULT_PLATFORMS: &[&str] = &["macos-arm64", "macos-x64", "linux-x64", "linux-arm64"];
/// The first mise release that reads and writes the lock format stack embeds (`mise.lock` v3).
pub const LOCK_MISE: CalVer = CalVer(2026, 9, 16);

/// Backends whose dependency graph mise locks in a sidecar stack does not carry. A cold
/// `mise install --locked` refuses them once the reference is stripped, so they are installed
/// only through plain `mise install`.
const UNSUPPORTED_BACKENDS: &[&str] = &["npm", "pypi", "pipx"];
/// Backends that record neither a download URL nor a dependency graph; `mise install --locked`
/// accepts them without a platform entry. From mise 2026.10.3's `supports_lockfile_url`
/// overrides, less npm and pipx/pypi (unsupported above). Plain `vfox:` tools record URLs and
/// are not exempt; custom vfox backend plugins are, but their names do not say so, so they are
/// reported `missing` (never called checked) rather than `exempt`.
const EXEMPT_BACKENDS: &[&str] = &["asdf", "cargo", "gem", "go", "spinel", "ubi", "core:dotnet", "core:rust", "core:swift"];

// ---- platforms and policy ------------------------------------------------------------------

/// The platform this process runs on, in mise's names (`macos-arm64`, `linux-x64`).
pub fn current_platform() -> String {
    platform_of(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_of(os: &str, arch: &str) -> String {
    let arch = match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// A platform name in mise's form. Version 2 locks recorded Rust's names (`macos-aarch64`).
pub fn canonical_platform(name: &str) -> String {
    match name.split_once('-') {
        Some((os, rest)) => {
            let (arch, suffix) = rest.split_once('-').map_or((rest, None), |(a, s)| (a, Some(s)));
            let base = platform_of(os, arch);
            suffix.map_or(base.clone(), |s| format!("{base}-{s}"))
        }
        None => name.to_string(),
    }
}

/// `<os>-<arch>[-<qualifier>]` with the components mise 2026.10.3 accepts for `--platform`.
/// Acceptance does not mean a backend publishes an artifact for it.
fn valid_platform(name: &str) -> bool {
    let mut parts = name.splitn(3, '-');
    let (Some(os), Some(arch)) = (parts.next(), parts.next()) else { return false };
    matches!(os, "linux" | "macos" | "windows" | "android")
        && matches!(arch, "x64" | "arm64" | "x86" | "loongarch64" | "riscv64")
        && parts.next().map_or(true, |q| matches!(q, "gnu" | "glibc" | "musl" | "msvc" | "baseline" | "musl-baseline"))
}

/// `[lock] artifacts`: whether every pin must be checked on every listed platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Requirement {
    #[default]
    BestEffort,
    Required,
}

/// `[lock]` in stack.toml, project only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Policy {
    pub platforms: Vec<String>,
    pub artifacts: Requirement,
}

impl Default for Policy {
    fn default() -> Self {
        Self { platforms: DEFAULT_PLATFORMS.iter().map(|p| p.to_string()).collect(), artifacts: Requirement::BestEffort }
    }
}

impl Policy {
    pub fn from_settings(settings: &crate::manifest::LockSettings) -> Result<Self> {
        let invalid = |why: String| {
            StackError::new("manifest_invalid", format!("stack.toml [lock]: {why}"))
                .hint(format!("platforms are mise's names ({}, or \"current\"); artifacts is \"best-effort\" or \"required\"", DEFAULT_PLATFORMS.join(", ")))
        };
        let artifacts = match settings.artifacts.as_deref() {
            None | Some("best-effort") => Requirement::BestEffort,
            Some("required") => Requirement::Required,
            Some(other) => return Err(invalid(format!("artifacts = {other:?} is not \"best-effort\" or \"required\""))),
        };
        let Some(listed) = &settings.platforms else {
            return Ok(Self { artifacts, ..Self::default() });
        };
        let mut platforms: Vec<String> = Vec::new();
        for name in listed {
            let name = if name == "current" { current_platform() } else { name.clone() };
            if !valid_platform(&name) {
                return Err(invalid(format!("platform {name:?} is not a mise platform name")));
            }
            if !platforms.contains(&name) {
                platforms.push(name);
            }
        }
        if platforms.is_empty() {
            return Err(invalid("platforms must name at least one platform".into()));
        }
        Ok(Self { platforms, artifacts })
    }

    pub fn required(&self) -> bool {
        self.artifacts == Requirement::Required
    }

    /// Locked operations under `required` refuse a machine whose platform `platforms` does not
    /// list: stack.lock commits nothing that says what may be installed or run there. Ordinary
    /// compile is not a locked operation and may lock other platforms from any machine.
    pub fn check_runtime_platform(&self) -> Result<()> {
        self.check_platform(&current_platform())
    }

    fn check_platform(&self, platform: &str) -> Result<()> {
        if !self.required() || self.platforms.iter().any(|p| p == platform) {
            return Ok(());
        }
        Err(StackError::new(
            "artifact_unlocked",
            format!("[lock] artifacts = \"required\", but this machine's platform {platform} is not in [lock] platforms ({})", self.platforms.join(", ")),
        )
        .hint("add the platform to `[lock] platforms` and run `stack compile`, or relax `[lock] artifacts`")
        .with_detail(json!({ "platform": platform, "state": "unlisted", "platforms": self.platforms })))
    }
}

// ---- shape ---------------------------------------------------------------------------------

fn invalid(why: impl Into<String>) -> StackError {
    StackError::new("lock_invalid", format!("stack.lock [provider_lock]: {}", why.into()))
        .hint("run `stack compile --update` to lock artifacts again, or upgrade stack if a newer one wrote this lock")
}

/// Text mise would render as a template (`exec()` included) wherever it reads it.
fn template_syntax(value: &str) -> bool {
    ["{{", "{%", "{#"].iter().any(|t| value.contains(t))
}

/// The dotted path of the first string (key or value) with template syntax, if any. Nothing
/// mise writes into a lock has it; a value that does would run commands where mise reads it.
fn templated_path(table: &Table, path: &str) -> Option<String> {
    fn walk(value: &Value, path: String) -> Option<String> {
        match value {
            Value::String(s) => template_syntax(s).then_some(path),
            Value::Array(a) => a.iter().enumerate().find_map(|(i, v)| walk(v, format!("{path}[{i}]"))),
            Value::Table(t) => t.iter().find_map(|(k, v)| {
                let here = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                if template_syntax(k) { Some(here) } else { walk(v, here) }
            }),
            _ => None,
        }
    }
    walk(&Value::Table(table.clone()), path.to_string())
}

/// What stack relies on in an embedded lock; everything else is carried as written.
pub fn check_shape(embedded: &Table) -> Result<()> {
    if let Some(at) = templated_path(embedded, "") {
        return Err(invalid(format!("{at} contains template syntax (`{{{{`, `{{%` or `{{#`), which mise would evaluate")));
    }
    match embedded.get(PROVIDER_KEY) {
        Some(Value::String(p)) if p == PROVIDER => {}
        Some(other) => return Err(invalid(format!("provider {other} is not \"{PROVIDER}\""))),
        None => return Err(invalid("missing `provider`")),
    }
    check_mise_shape(embedded)?;
    if let Some(sidecars) = embedded.get(SIDECARS_KEY) {
        let ok = sidecars.as_array().is_some_and(|a| {
            a.iter().all(|s| s.get("tool").is_some_and(Value::is_str) && s.get("version").is_some_and(Value::is_str))
        });
        if !ok {
            return Err(invalid(format!("`{SIDECARS_KEY}` must be a list of {{ tool, version }}")));
        }
    }
    Ok(())
}

/// The parts of a mise.lock stack reads: `tools.<name>` lists of entries with a `version`,
/// platform tables, and `conda-packages.<platform>.<package>` tables.
fn check_mise_shape(lock: &Table) -> Result<()> {
    if let Some(tools) = lock.get(TOOLS) {
        let tools = tools.as_table().ok_or_else(|| invalid("`tools` is not a table"))?;
        for (name, entries) in tools {
            let entries = entries.as_array().ok_or_else(|| invalid(format!("tools.{name} is not a list of entries")))?;
            for entry in entries {
                let entry = entry.as_table().ok_or_else(|| invalid(format!("tools.{name} has an entry that is not a table")))?;
                if !entry.get("version").is_some_and(Value::is_str) {
                    return Err(invalid(format!("tools.{name} has an entry without a version")));
                }
                for (key, value) in entry.iter().filter(|(k, _)| k.starts_with(PLATFORM_PREFIX)) {
                    if !value.is_table() {
                        return Err(invalid(format!("tools.{name}.\"{key}\" is not a table")));
                    }
                }
                if let Some(deps) = platform_tables(entry).find_map(|(_, t)| t.get("conda_deps").filter(|d| !d.as_array().is_some_and(|a| a.iter().all(Value::is_str)))) {
                    return Err(invalid(format!("tools.{name} has conda_deps {deps} that is not a list of names")));
                }
            }
        }
    }
    if let Some(conda) = lock.get(CONDA) {
        let conda = conda.as_table().ok_or_else(|| invalid(format!("`{CONDA}` is not a table")))?;
        for (platform, packages) in conda {
            let packages = packages.as_table().ok_or_else(|| invalid(format!("{CONDA}.{platform} is not a table")))?;
            if let Some((name, _)) = packages.iter().find(|(_, v)| !v.is_table()) {
                return Err(invalid(format!("{CONDA}.{platform}.\"{name}\" is not a table")));
            }
        }
    }
    Ok(())
}

fn platform_tables(entry: &Table) -> impl Iterator<Item = (&str, &Table)> {
    entry.iter().filter_map(|(k, v)| Some((k.strip_prefix(PLATFORM_PREFIX)?, v.as_table()?)))
}

fn entries<'a>(lock: &'a Table, tool: &str) -> impl Iterator<Item = &'a Table> {
    lock.get(TOOLS)
        .and_then(|t| t.get(tool))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_table)
}

fn version_of(entry: &Table) -> &str {
    entry.get("version").and_then(Value::as_str).unwrap_or_default()
}

fn str_field<'a>(table: &'a Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(Value::as_str)
}

// ---- pins and coverage ---------------------------------------------------------------------

/// A release stack.lock pins, as the provider names it: a tool, or a service's preset tool.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PinKey {
    pub tool: String,
    pub version: String,
}

impl std::fmt::Display for PinKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.tool, self.version)
    }
}

/// Every release `lock` pins, once each (see `scratch::pins`).
pub fn pin_keys(lock: &Lockfile) -> Vec<PinKey> {
    let mut out: Vec<PinKey> = Vec::new();
    for pin in crate::provider::scratch::pins(lock) {
        let key = PinKey { tool: pin.tool, version: pin.spec.version };
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The entry has `checksum` and `url` for the platform (and `signer` for packslip).
    Verified,
    /// The backend records neither a URL nor a dependency graph; mise's `--locked` accepts it.
    Exempt,
    /// The backend locks a dependency graph in a sidecar stack does not carry.
    Unsupported,
    /// No entry, or no checked entry for the platform.
    Missing,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Verified => "verified",
            State::Exempt => "exempt",
            State::Unsupported => "unsupported",
            State::Missing => "missing",
        }
    }

    /// Installed through `mise install --locked`.
    pub fn locked_install(self) -> bool {
        matches!(self, State::Verified | State::Exempt)
    }
}

/// The backend a pin installs through: what its entry records, else what its name says.
pub fn backend(lock: Option<&Table>, pin: &PinKey) -> Option<String> {
    lock.and_then(|l| entries(l, &pin.tool).filter(|e| version_of(e) == pin.version).find_map(|e| str_field(e, "backend")))
        .map(str::to_string)
        .or_else(|| pin.tool.contains(':').then(|| pin.tool.clone()))
}

/// `npm`, `aqua`, or for core plugins `core:rust`.
fn backend_kind(backend: &str) -> &str {
    let mut parts = backend.splitn(2, ':');
    let kind = parts.next().unwrap_or_default();
    if kind == "core" {
        let name = parts.next().unwrap_or_default();
        let end = name.find(['@', '/', '[']).unwrap_or(name.len());
        return &backend[..5 + end];
    }
    kind
}

fn stripped_sidecar(lock: Option<&Table>, pin: &PinKey) -> bool {
    lock.and_then(|l| l.get(SIDECARS_KEY)).and_then(Value::as_array).is_some_and(|list| {
        list.iter().any(|s| str_field_v(s, "tool") == Some(&pin.tool) && str_field_v(s, "version") == Some(&pin.version))
    })
}

fn str_field_v<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Coverage of one pin on one platform, and the checked values when it is verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub state: State,
    pub checksum: Option<String>,
    pub signer: Option<String>,
}

pub fn coverage(lock: Option<&Table>, pin: &PinKey, platform: &str) -> Coverage {
    let state = |state| Coverage { state, checksum: None, signer: None };
    let backend = backend(lock, pin);
    let kind = backend.as_deref().map(backend_kind);
    if stripped_sidecar(lock, pin) || kind.is_some_and(|k| UNSUPPORTED_BACKENDS.contains(&k)) {
        return state(State::Unsupported);
    }
    if kind.is_some_and(|k| EXEMPT_BACKENDS.contains(&k)) {
        return state(State::Exempt);
    }
    let Some(lock) = lock else { return state(State::Missing) };
    let key = format!("{PLATFORM_PREFIX}{platform}");
    for entry in entries(lock, &pin.tool).filter(|e| version_of(e) == pin.version) {
        let Some(table) = entry.get(&key).and_then(Value::as_table) else { continue };
        let packslip = str_field(entry, "backend").is_some_and(|b| b.starts_with("packslip:"));
        let checksum = str_field(table, "checksum").filter(|c| !c.is_empty());
        let url = str_field(table, "url").filter(|u| !u.is_empty());
        let signer = str_field(table, "signer").filter(|s| !s.is_empty());
        if checksum.is_some() && url.is_some() && (!packslip || signer.is_some()) {
            return Coverage { state: State::Verified, checksum: checksum.map(Into::into), signer: signer.map(Into::into) };
        }
    }
    state(State::Missing)
}

/// What one version report says about one platform.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlatformReport {
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
    /// mise's text for a `missing` platform, when this compile captured one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What this compile did to the committed value: `added`, `artifact_changed` (accepted by
    /// `--update`), `retained` (`--update` got no fresh value; the committed one is kept) or
    /// `differs_upstream` (kept; `--update` would accept the difference).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum_was: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url_was: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_was: Option<String>,
    /// Shared dependency records (`conda-packages`) that changed or differ upstream.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<Json>,
}

/// Coverage of `pin` on every listed platform, with what this compile changed.
pub fn report(lock: Option<&Table>, pin: &PinKey, platforms: &[String], events: &Events) -> IndexMap<String, PlatformReport> {
    platforms
        .iter()
        .map(|platform| {
            let c = coverage(lock, pin, platform);
            let event = events.get(&(pin.clone(), platform.clone())).cloned().unwrap_or_default();
            let reason = (c.state == State::Missing).then(|| event.reason.clone()).flatten();
            let report = PlatformReport {
                state: c.state,
                checksum: c.checksum,
                signer: c.signer,
                reason,
                change: event.change,
                checksum_was: event.checksum_was,
                url_was: event.url_was,
                signer_was: event.signer_was,
                deps: event.deps,
            };
            (platform.clone(), report)
        })
        .collect()
}

/// Coverage on one platform, by state: what `status` and the install step report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlatformSummary {
    pub platform: String,
    pub verified: Vec<String>,
    pub exempt: Vec<String>,
    pub unsupported: Vec<String>,
    pub missing: Vec<String>,
}

pub fn summary(lock: Option<&Table>, pins: &[PinKey], platform: &str) -> PlatformSummary {
    let mut out = PlatformSummary { platform: platform.into(), verified: vec![], exempt: vec![], unsupported: vec![], missing: vec![] };
    for pin in pins {
        let list = match coverage(lock, pin, platform).state {
            State::Verified => &mut out.verified,
            State::Exempt => &mut out.exempt,
            State::Unsupported => &mut out.unsupported,
            State::Missing => &mut out.missing,
        };
        list.push(pin.to_string());
    }
    out
}

/// Pins (as `[{ name, platform, state, reason? }]`) that are neither verified nor exempt.
pub fn unchecked(lock: Option<&Table>, pins: &[PinKey], platforms: &[String], reasons: &Reasons) -> Vec<Json> {
    let mut out = Vec::new();
    for pin in pins {
        for platform in platforms {
            let state = coverage(lock, pin, platform).state;
            if state.locked_install() {
                continue;
            }
            let mut detail = json!({ "name": pin.to_string(), "platform": platform, "state": state });
            if let Some(reason) = reasons.get(&(pin.tool.clone(), platform.clone())) {
                detail["reason"] = json!(reason);
            }
            out.push(detail);
        }
    }
    out
}

pub fn unlocked_error(details: Vec<Json>) -> StackError {
    let first = &details[0];
    StackError::new(
        "artifact_unlocked",
        format!(
            "[lock] artifacts = \"required\", but {} is {} on {}{}",
            first["name"].as_str().unwrap_or_default(),
            first["state"].as_str().unwrap_or_default(),
            first["platform"].as_str().unwrap_or_default(),
            if details.len() > 1 { format!(" ({} pin/platform pairs in all)", details.len()) } else { String::new() }
        ),
    )
    .hint("lock the platform (`stack compile`), relax `[lock] artifacts`, or change `[lock] platforms`; stack.lock and the provider config were not changed")
    .details(details)
}

/// Installation split for one platform: tool names installed with `mise install --locked`
/// and with plain `mise install`. A tool name is in the locked call only when every version
/// pinned under it is verified or exempt, because mise installs every configured version of
/// a name (with the configuration's options) when it is named.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Partition {
    pub locked: Vec<String>,
    pub plain: Vec<String>,
}

pub fn partition(lock: Option<&Table>, pins: &[PinKey], platform: &str) -> Partition {
    let mut by_tool: IndexMap<&str, bool> = IndexMap::new();
    for pin in pins {
        let ok = coverage(lock, pin, platform).state.locked_install();
        let slot = by_tool.entry(pin.tool.as_str()).or_insert(true);
        *slot &= ok;
    }
    let mut out = Partition::default();
    for (tool, ok) in by_tool {
        if ok { out.locked.push(tool.into()) } else { out.plain.push(tool.into()) }
    }
    out
}

// ---- consistency ---------------------------------------------------------------------------

/// An embedded lock agrees with stack's pins: every entry names a pinned release and every
/// `conda_deps` name has its record. `lock_invalid` otherwise, naming the entry.
pub fn validate(embedded: &Table, pins: &[PinKey]) -> Result<()> {
    check_shape(embedded)?;
    if let Some(tools) = embedded.get(TOOLS).and_then(Value::as_table) {
        for (tool, list) in tools {
            for entry in list.as_array().into_iter().flatten().filter_map(Value::as_table) {
                let version = version_of(entry);
                if !pins.iter().any(|p| p.tool == *tool && p.version == version) {
                    return Err(invalid(format!("tools.{tool} has an entry for version {version:?}, which stack.lock does not pin")).with_detail(json!({ "entry": format!("tools.{tool}"), "version": version })));
                }
            }
        }
    }
    dangling_deps(embedded)
}

fn dangling_deps(lock: &Table) -> Result<()> {
    let conda = lock.get(CONDA).and_then(Value::as_table);
    for (tool, entry) in all_entries(lock) {
        for (platform, table) in platform_tables(entry) {
            for dep in table.get("conda_deps").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                if conda.and_then(|c| c.get(platform)).and_then(|p| p.get(dep)).is_none() {
                    return Err(invalid(format!("tools.{tool}@{} on {platform} depends on {dep}, which {CONDA}.{platform} does not record", version_of(entry)))
                        .with_detail(json!({ "entry": format!("tools.{tool}"), "version": version_of(entry), "platform": platform, "dep": dep })));
                }
            }
        }
    }
    Ok(())
}

fn all_entries(lock: &Table) -> impl Iterator<Item = (&str, &Table)> {
    lock.get(TOOLS)
        .and_then(Value::as_table)
        .into_iter()
        .flat_map(|t| t.iter())
        .flat_map(|(name, list)| list.as_array().into_iter().flatten().filter_map(move |e| Some((name.as_str(), e.as_table()?))))
}

/// Keep only entries of pinned releases, platform tables of listed platforms, sidecar records
/// of pinned releases, and dependency records some kept entry references.
pub fn retain(lock: &mut Table, pins: &[PinKey], platforms: &[String]) {
    if let Some(tools) = lock.get_mut(TOOLS).and_then(Value::as_table_mut) {
        for (tool, list) in tools.iter_mut() {
            if let Some(list) = list.as_array_mut() {
                list.retain(|e| e.as_table().is_some_and(|e| pins.iter().any(|p| p.tool == *tool && p.version == version_of(e))));
                for entry in list.iter_mut().filter_map(Value::as_table_mut) {
                    entry.retain(|k, _| k.strip_prefix(PLATFORM_PREFIX).map_or(true, |p| platforms.iter().any(|l| l == p)));
                }
            }
        }
        tools.retain(|_, list| list.as_array().is_some_and(|a| !a.is_empty()));
    }
    if let Some(list) = lock.get_mut(SIDECARS_KEY).and_then(Value::as_array_mut) {
        list.retain(|s| pins.iter().any(|p| str_field_v(s, "tool") == Some(&p.tool) && str_field_v(s, "version") == Some(&p.version)));
        if list.is_empty() {
            lock.remove(SIDECARS_KEY);
        }
    }
    prune_deps(lock, platforms);
}

/// Drop `conda-packages` records no entry references, and platforms not listed.
fn prune_deps(lock: &mut Table, platforms: &[String]) {
    let mut referenced: BTreeSet<(String, String)> = BTreeSet::new();
    for (_, entry) in all_entries(lock) {
        for (platform, table) in platform_tables(entry) {
            for dep in table.get("conda_deps").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                referenced.insert((platform.to_string(), dep.to_string()));
            }
        }
    }
    if let Some(conda) = lock.get_mut(CONDA).and_then(Value::as_table_mut) {
        conda.retain(|platform, _| platforms.iter().any(|p| p == platform));
        for (platform, packages) in conda.iter_mut() {
            if let Some(packages) = packages.as_table_mut() {
                packages.retain(|name, _| referenced.contains(&(platform.clone(), name.to_string())));
            }
        }
        conda.retain(|_, packages| packages.as_table().is_some_and(|p| !p.is_empty()));
        if conda.is_empty() {
            lock.remove(CONDA);
        }
    }
}

// ---- render and capture --------------------------------------------------------------------

/// Where mise reads the lock for a root under stack's isolation.
pub fn rendered_path(root: &Path) -> PathBuf {
    root.join(".config/mise/mise.lock")
}

/// The mise.lock an embedded lock stands for: every key but stack's own.
pub fn to_mise(embedded: &Table) -> Table {
    embedded.iter().filter(|(k, _)| *k != PROVIDER_KEY && *k != SIDECARS_KEY).map(|(k, v)| (k.clone(), v.clone())).collect()
}

pub fn render(embedded: &Table) -> String {
    let body = toml::to_string_pretty(&to_mise(embedded)).expect("provider lock serializes");
    format!("# Generated by stack from stack.lock [provider_lock]. Do not edit; run `stack compile`.\n{body}")
}

/// Whether an embedded lock has anything for mise to check.
pub fn has_entries(embedded: Option<&Table>) -> bool {
    embedded.and_then(|e| e.get(TOOLS)).and_then(Value::as_table).is_some_and(|t| !t.is_empty())
}

/// Write `.config/mise/mise.lock` from the committed lock, or remove a stale one when the lock
/// embeds nothing (version 2, or no pins). The file is stack's, generated and gitignored.
pub fn write_rendered(root: &Path, lock: Option<&Lockfile>) -> Result<()> {
    let path = rendered_path(root);
    let embedded = lock.and_then(|l| l.provider_lock.as_ref()).filter(|e| has_entries(Some(e)));
    let Some(embedded) = embedded else {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(path.display(), e)),
        };
    };
    let text = render(embedded);
    if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    let dir = path.parent().expect("rendered lock has a parent");
    std::fs::create_dir_all(dir).map_err(|e| io_error(dir.display(), e))?;
    std::fs::write(&path, text).map_err(|e| io_error(path.display(), e))
}

/// A mise.lock as stack embeds it: `provider` first, then mise's keys in mise's order.
pub fn embed(mise_lock: Table) -> Table {
    let mut out = Table::new();
    out.insert(PROVIDER_KEY.into(), Value::String(PROVIDER.into()));
    out.extend(mise_lock);
    out
}

/// A mise.lock mise wrote, read back. Sidecar references are removed (the rendered lock must
/// never point at a file stack does not ship) and the releases that had one are returned.
pub fn capture(text: &str) -> std::result::Result<(Table, Vec<PinKey>), String> {
    let mut lock: Table = toml::from_str(text).map_err(|e| e.message().trim().to_string())?;
    check_mise_shape(&lock).map_err(|e| e.message)?;
    if let Some(at) = templated_path(&lock, "") {
        return Err(format!("{at} contains template syntax"));
    }
    if lock.contains_key(PROVIDER_KEY) || lock.contains_key(SIDECARS_KEY) {
        return Err(format!("mise.lock uses `{PROVIDER_KEY}` or `{SIDECARS_KEY}`, which stack reserves"));
    }
    let mut stripped = Vec::new();
    if let Some(tools) = lock.get_mut(TOOLS).and_then(Value::as_table_mut) {
        for (tool, list) in tools.iter_mut() {
            for entry in list.as_array_mut().into_iter().flatten().filter_map(Value::as_table_mut) {
                let before = entry.len();
                entry.retain(|key, value| !is_sidecar(key, value));
                if entry.len() != before {
                    let pin = PinKey { tool: tool.clone(), version: version_of(entry).to_string() };
                    if !stripped.contains(&pin) {
                        stripped.push(pin);
                    }
                }
            }
        }
    }
    Ok((lock, stripped))
}

/// A reference to a file beside mise.lock: a known sidecar key, or any `{ path, digest }`.
fn is_sidecar(key: &str, value: &Value) -> bool {
    !key.starts_with(PLATFORM_PREFIX)
        && (SIDECAR_KEYS.contains(&key) || value.as_table().is_some_and(|t| t.contains_key("path") && t.contains_key("digest")))
}

fn set_sidecars(embedded: &mut Table, sidecars: &BTreeSet<PinKey>) {
    embedded.remove(SIDECARS_KEY);
    if sidecars.is_empty() {
        return;
    }
    let list = sidecars
        .iter()
        .map(|p| {
            let mut t = Table::new();
            t.insert("tool".into(), Value::String(p.tool.clone()));
            t.insert("version".into(), Value::String(p.version.clone()));
            Value::Table(t)
        })
        .collect();
    embedded.insert(SIDECARS_KEY.into(), Value::Array(list));
}

fn sidecars_of(embedded: Option<&Table>) -> BTreeSet<PinKey> {
    embedded
        .and_then(|e| e.get(SIDECARS_KEY))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| Some(PinKey { tool: str_field_v(s, "tool")?.into(), version: str_field_v(s, "version")?.into() }))
        .collect()
}

/// The lock a scratch root starts from: the committed entries of every tool not being locked
/// this time and the dependency records they reference. Tools being locked get nothing, so an
/// entry for one after the run can only be freshly produced.
pub fn seed(committed: Option<&Table>, targets: &BTreeSet<String>, platforms: &[String]) -> String {
    let mut lock = committed.map(to_mise).unwrap_or_default();
    if let Some(tools) = lock.get_mut(TOOLS).and_then(Value::as_table_mut) {
        tools.retain(|name, _| !targets.contains(name));
    }
    if lock.get(TOOLS).and_then(Value::as_table).is_some_and(Table::is_empty) {
        lock.remove(TOOLS);
    }
    prune_deps(&mut lock, platforms);
    lock.entry("lockfile_version").or_insert(Value::Integer(3));
    let mut ordered = Table::new();
    if let Some(v) = lock.remove("lockfile_version") {
        ordered.insert("lockfile_version".into(), v);
    }
    ordered.extend(lock);
    toml::to_string_pretty(&ordered).expect("seed serializes")
}

// ---- merge ---------------------------------------------------------------------------------

/// Why a pin has no fresh value on a platform, keyed by (tool, platform): mise's own line.
pub type Reasons = HashMap<(String, String), String>;

/// mise's stderr lines that name a tool and a platform, other than its progress lines
/// (`mise lock  <tool>@<version> <platform>`): `failed to resolve redis for linux-x64: ...`.
pub fn reasons(stderr: &str, tools: &BTreeSet<String>, platforms: &[String]) -> Reasons {
    let mut out = Reasons::new();
    for raw in stderr.lines() {
        let line = raw.trim();
        let line = ["mise ERROR", "mise WARN"].iter().fold(line, |l, p| l.strip_prefix(p).map_or(l, str::trim));
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() == Some(&"mise") && words.get(1) == Some(&"lock") && words.len() <= 4 {
            continue;
        }
        for tool in tools {
            let named = line.contains(&format!("{tool}@")) || words.iter().any(|w| w.trim_end_matches([':', ',']) == tool);
            if !named {
                continue;
            }
            for platform in platforms.iter().filter(|p| words.iter().any(|w| w.trim_end_matches([':', ',']) == p.as_str())) {
                out.entry((tool.clone(), platform.clone())).or_insert_with(|| truncate(line, 300));
            }
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// What a compile did to one pin on one platform.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Event {
    pub change: Option<&'static str>,
    pub checksum_was: Option<String>,
    pub url_was: Option<String>,
    pub signer_was: Option<String>,
    pub reason: Option<String>,
    pub deps: Vec<Json>,
}

pub type Events = HashMap<(PinKey, String), Event>;

pub struct Merge<'a> {
    /// The committed lock in mise's form, already validated and restricted to current pins and
    /// listed platforms.
    pub committed: Option<&'a Table>,
    /// What the scratch run left, captured.
    pub fresh: Table,
    /// Tool names named on `mise lock` this time.
    pub targets: &'a BTreeSet<String>,
    pub pins: &'a [PinKey],
    pub platforms: &'a [String],
    pub update: bool,
    pub reasons: &'a Reasons,
}

pub struct Merged {
    /// In mise's form; `embed` it for stack.lock.
    pub lock: Table,
    pub events: Events,
    pub warnings: Vec<String>,
}

impl Merge<'_> {
    pub fn run(self) -> Result<Merged> {
        let mut events = Events::new();
        let mut warnings = Vec::new();
        let empty = Table::new();
        let committed = self.committed.unwrap_or(&empty);
        let mut out = Table::new();
        // Top-level keys stack does not know: mise's latest answer, else the committed one.
        let known = |k: &str| [TOOLS, CONDA, PROVIDER_KEY, SIDECARS_KEY].contains(&k);
        for (key, value) in committed.iter().filter(|(k, _)| !known(k) && !self.fresh.contains_key(*k)) {
            out.insert(key.clone(), value.clone());
        }
        for (key, value) in self.fresh.iter().filter(|(k, _)| !known(k)) {
            out.insert(key.clone(), value.clone());
        }
        if !out.contains_key("lockfile_version") {
            out.insert("lockfile_version".into(), Value::Integer(3));
        }

        let tools: BTreeSet<&str> = self.pins.iter().map(|p| p.tool.as_str()).collect();
        let mut merged_tools = Table::new();
        for tool in tools {
            let versions: Vec<&str> = self.pins.iter().filter(|p| p.tool == tool).map(|p| p.version.as_str()).collect();
            let committed_entries: Vec<&Table> = entries(committed, tool).collect();
            let list: Vec<Table> = if !self.targets.contains(tool) {
                committed_entries.into_iter().cloned().collect()
            } else {
                let mut list = Vec::new();
                for version in versions {
                    let pin = PinKey { tool: tool.into(), version: version.into() };
                    let cs: Vec<&Table> = committed_entries.iter().copied().filter(|e| version_of(e) == version).collect();
                    let fs: Vec<Table> = entries(&self.fresh, tool).filter(|e| version_of(e) == version).map(|e| self.restrict(e)).collect();
                    list.extend(self.merge_version(&pin, &cs, &fs, &mut events, &mut warnings));
                }
                list
            };
            if !list.is_empty() {
                merged_tools.insert(tool.into(), Value::Array(list.into_iter().map(Value::Table).collect()));
            }
        }
        if !merged_tools.is_empty() {
            out.insert(TOOLS.into(), Value::Table(merged_tools));
        }

        let conda = self.merge_conda(committed, &out, &mut events, &mut warnings);
        if !conda.is_empty() {
            out.insert(CONDA.into(), Value::Table(conda));
        }
        prune_deps(&mut out, self.platforms);
        dangling_deps(&out)?;
        // Missing platforms of pins named for locking get mise's reason when it gave one.
        for pin in self.pins.iter().filter(|p| self.targets.contains(&p.tool)) {
            for platform in self.platforms {
                if let Some(reason) = self.reasons.get(&(pin.tool.clone(), platform.clone())) {
                    events.entry((pin.clone(), platform.clone())).or_default().reason.get_or_insert_with(|| reason.clone());
                }
            }
        }
        Ok(Merged { lock: out, events, warnings })
    }

    /// An entry without platform tables for platforms not listed.
    fn restrict(&self, entry: &Table) -> Table {
        entry
            .iter()
            .filter(|(k, _)| k.strip_prefix(PLATFORM_PREFIX).map_or(true, |p| self.platforms.iter().any(|l| l == p)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn merge_version(&self, pin: &PinKey, cs: &[&Table], fs: &[Table], events: &mut Events, warnings: &mut Vec<String>) -> Vec<Table> {
        let options = |e: &Table| e.get("options").cloned();
        let mut out = Vec::new();
        if self.update {
            for f in fs {
                let c = cs.iter().find(|c| options(c) == options(f));
                let mut entry = f.clone();
                for platform in self.platforms {
                    let key = format!("{PLATFORM_PREFIX}{platform}");
                    let event = |events: &mut Events| events.entry((pin.clone(), platform.clone())).or_default().clone();
                    match (c.and_then(|c| c.get(&key)), f.get(&key)) {
                        (Some(was), Some(now)) if was != now => {
                            let mut e = event(events);
                            e.change = Some("artifact_changed");
                            e.checksum_was = was.get("checksum").and_then(Value::as_str).map(Into::into);
                            e.url_was = was.get("url").and_then(Value::as_str).map(Into::into);
                            e.signer_was = was.get("signer").and_then(Value::as_str).map(Into::into);
                            events.insert((pin.clone(), platform.clone()), e);
                        }
                        (None, Some(_)) => {
                            events.entry((pin.clone(), platform.clone())).or_default().change = Some("added");
                        }
                        (Some(was), None) => {
                            entry.insert(key.clone(), was.clone());
                            self.retained(pin, platform, events);
                        }
                        _ => {}
                    }
                }
                out.push(entry);
            }
            // A committed variant mise no longer produced: kept only where nothing fresh covers
            // the platform, and reported as retained, never as refreshed.
            for c in cs.iter().filter(|c| !fs.iter().any(|f| options(f) == options(c))) {
                let covered = |platform: &str| fs.iter().any(|f| f.contains_key(&format!("{PLATFORM_PREFIX}{platform}")));
                let has_platforms = platform_tables(c).next().is_some();
                if fs.is_empty() || has_platforms {
                    let kept: Table = c
                        .iter()
                        .filter(|(k, _)| k.strip_prefix(PLATFORM_PREFIX).map_or(true, |p| !covered(p)))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    let platforms: Vec<String> = platform_tables(&kept).map(|(p, _)| p.to_string()).collect();
                    if fs.is_empty() || !platforms.is_empty() {
                        for platform in &platforms {
                            self.retained(pin, platform, events);
                        }
                        out.push(kept);
                    }
                }
            }
        } else {
            for c in cs {
                let mut entry = (*c).clone();
                if let Some(f) = fs.iter().find(|f| options(f) == options(c)) {
                    let fields = |e: &Table| -> Table { e.iter().filter(|(k, _)| !k.starts_with(PLATFORM_PREFIX)).map(|(k, v)| (k.clone(), v.clone())).collect() };
                    if fields(c) != fields(f) {
                        warnings.push(format!("artifacts.{pin} entry fields differ upstream; run `stack compile --update` to accept"));
                    }
                    for platform in self.platforms {
                        let key = format!("{PLATFORM_PREFIX}{platform}");
                        match (c.get(&key), f.get(&key)) {
                            (Some(was), Some(now)) if was != now => {
                                warnings.push(format!("artifacts.{pin}.{platform} differs upstream; run `stack compile --update` to accept"));
                                events.entry((pin.clone(), platform.clone())).or_default().change = Some("differs_upstream");
                            }
                            (None, Some(now)) => {
                                entry.insert(key, now.clone());
                                events.entry((pin.clone(), platform.clone())).or_default().change = Some("added");
                            }
                            _ => {}
                        }
                    }
                }
                out.push(entry);
            }
            for f in fs.iter().filter(|f| !cs.iter().any(|c| options(c) == options(f))) {
                for (platform, _) in platform_tables(f) {
                    events.entry((pin.clone(), platform.to_string())).or_default().change = Some("added");
                }
                out.push(f.clone());
            }
        }
        out
    }

    fn retained(&self, pin: &PinKey, platform: &str, events: &mut Events) {
        let e = events.entry((pin.clone(), platform.to_string())).or_default();
        e.change = Some("retained");
        let why = self.reasons.get(&(pin.tool.clone(), platform.to_string())).cloned();
        e.reason = Some(why.unwrap_or_else(|| "mise produced no fresh entry (offline, skipped or unavailable); the committed value is kept".into()));
    }

    /// `conda-packages` merged by (platform, package): committed records are kept unless
    /// `--update`; differences are reported on the pins whose entries reference the record.
    fn merge_conda(&self, committed: &Table, out: &Table, events: &mut Events, warnings: &mut Vec<String>) -> Table {
        let mut merged: Table = committed.get(CONDA).and_then(Value::as_table).cloned().unwrap_or_default();
        let Some(fresh) = self.fresh.get(CONDA).and_then(Value::as_table) else { return merged };
        let referencing = |platform: &str, dep: &str| -> Vec<PinKey> {
            all_entries(out)
                .filter(|(_, e)| {
                    e.get(&format!("{PLATFORM_PREFIX}{platform}"))
                        .and_then(|t| t.get("conda_deps"))
                        .and_then(Value::as_array)
                        .is_some_and(|d| d.iter().any(|n| n.as_str() == Some(dep)))
                })
                .map(|(tool, e)| PinKey { tool: tool.into(), version: version_of(e).into() })
                .collect()
        };
        for (platform, packages) in fresh.iter().filter(|(p, _)| self.platforms.iter().any(|l| l == *p)) {
            let Some(packages) = packages.as_table() else { continue };
            let slot = merged.entry(platform.clone()).or_insert_with(|| Value::Table(Table::new()));
            let Some(slot) = slot.as_table_mut() else { continue };
            for (name, record) in packages {
                match slot.get(name) {
                    None => {
                        slot.insert(name.clone(), record.clone());
                    }
                    Some(was) if was != record => {
                        let mut dep = json!({ "dep": name, "platform": platform });
                        if let Some(c) = was.get("checksum").and_then(Value::as_str) {
                            dep["checksum_was"] = json!(c);
                        }
                        if let Some(u) = was.get("url").and_then(Value::as_str) {
                            dep["url_was"] = json!(u);
                        }
                        if self.update {
                            dep["change"] = json!("artifact_changed");
                            slot.insert(name.clone(), record.clone());
                        } else {
                            dep["change"] = json!("differs_upstream");
                            warnings.push(format!("artifacts.{CONDA}.{platform}.{name} differs upstream; run `stack compile --update` to accept"));
                        }
                        for pin in referencing(platform, name) {
                            events.entry((pin, platform.clone())).or_default().deps.push(dep.clone());
                        }
                    }
                    Some(_) => {}
                }
            }
        }
        merged
    }
}

/// The embedded lock for stack.lock after a merge: mise's form with stack's keys added and the
/// sidecar records of the releases whose reference was stripped, now or in the committed lock.
pub fn finish(merged: Table, committed: Option<&Table>, stripped: Vec<PinKey>, pins: &[PinKey], platforms: &[String]) -> Table {
    let mut embedded = embed(merged);
    let mut sidecars = sidecars_of(committed);
    sidecars.extend(stripped);
    sidecars.retain(|p| pins.contains(p));
    set_sidecars(&mut embedded, &sidecars);
    retain(&mut embedded, pins, platforms);
    embedded
}

/// Pins are written into a scratch configuration mise loads, and mise renders tool versions and
/// option strings there as templates. Refuse any that carries template syntax rather than let a
/// lock query run what it says.
pub fn check_pins_untemplated(pins: &[crate::provider::scratch::Pin]) -> Result<()> {
    for pin in pins {
        let option = pin.spec.options.iter().find(|(_, v)| matches!(v, crate::tool::OptionValue::String(s) if template_syntax(s)));
        if template_syntax(&pin.tool) || template_syntax(&pin.spec.version) || option.is_some() {
            let what = option.map_or("its version".to_string(), |(k, _)| format!("option `{k}`"));
            return Err(StackError::new("invalid_tool", format!("tools.{}: {what} must not contain template syntax (`{{{{`, `{{%` or `{{#`)", pin.tool))
                .hint("mise evaluates templates in tool versions and option strings; write the literal value")
                .with_detail(json!({ "tool": pin.tool })));
        }
    }
    Ok(())
}

/// Tool names whose pins need entries: every one under `--update`, else those with a `missing`
/// state on a listed platform (new or changed pins have no entry yet).
pub fn targets(committed: Option<&Table>, pins: &[PinKey], platforms: &[String], update: bool) -> BTreeSet<String> {
    pins.iter()
        .filter(|pin| update || platforms.iter().any(|p| coverage(committed, pin, p).state == State::Missing))
        .map(|pin| pin.tool.clone())
        .collect()
}

#[cfg(test)]
mod tests;

//! `stack compile`: resolve bundles → lock → compose → render provider config.

use crate::compose::{compose, Composed, LoadedBundle};
use crate::error::{io_error, Result, StackError};
use crate::lock::{self, LockedBundle, LockedVersion, Lockfile};
use crate::manifest::{read_bundle, read_project};
use crate::ports::{self, Request};
use crate::provider::mise;
use crate::source::{Mode, Source};
use crate::tool::{ToolOptions, ToolSpec};
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Options {
    pub root: PathBuf,
    pub mode: Mode,
    /// Write stack.lock and the provider config. `false` for `stack inspect`.
    pub write: bool,
    pub cache: PathBuf,
    /// Machine-wide state (port reservations, session index).
    pub state: PathBuf,
    /// Drop this project's port reservations and assign fresh ones.
    pub reassign_ports: bool,
    /// Resolves version requests to exact versions. `None` uses mise.
    pub resolver: Option<Arc<dyn mise::Resolver>>,
}

#[derive(Debug, Serialize)]
pub struct BundleReport {
    pub name: String,
    pub version: Option<String>,
    pub source: String,
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub content_hash: String,
    pub dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

/// A tool or preset service version: what was requested and what stack.lock pins.
#[derive(Debug, Clone, Serialize)]
pub struct VersionReport {
    /// `tool` or `service`.
    pub kind: &'static str,
    pub name: String,
    /// For services, the provider tool the preset installs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub requested: String,
    /// Allowlisted provider options declared with the request; omitted when there are none.
    #[serde(skip_serializing_if = "ToolOptions::is_empty")]
    pub options: ToolOptions,
    /// `None` only when nothing is locked yet and the command may not resolve (`inspect`).
    pub resolved: Option<String>,
    /// `bundle:<name>`, `project`, `override`, or `provider` for tools stack adds itself.
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub bundles: Vec<BundleReport>,
    pub stack: Composed,
    /// Ports assigned to this checkout. Machine-specific; never written to bundles or stack.lock.
    pub ports: IndexMap<String, u16>,
    /// Requested and exact versions of every tool and resolvable preset service.
    pub versions: Vec<VersionReport>,
    /// Instance tokens of services with identity probes. Machine-specific, like ports.
    #[serde(skip)]
    pub identities: IndexMap<String, String>,
    pub lock_changed: bool,
    pub provider: &'static str,
    pub output: PathBuf,
    pub written: bool,
    /// Valid but risky choices, such as tools that are not pinned to a version.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Agent skills of the pinned releases (`inspect`, `compile`); see `skills::discover`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<crate::skills::Skill>>,
    /// Skills of tools stack adds for its provider, listed only when asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_skills: Option<Vec<crate::skills::Skill>>,
    /// `[skills] dir`: where `up` and `install` link the stack's skills.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills_dir: Option<String>,
    /// The lock this report describes: what stack.lock holds after this command.
    #[serde(skip)]
    pub lock: Lockfile,
}

impl Report {
    /// Add the skills of the releases this report pins. `all` also lists the provider's.
    pub fn attach_skills(&mut self, cache: &Path, all: bool) {
        let found = crate::skills::discover(cache, &self.lock, &self.versions);
        self.warnings.extend(found.warnings);
        self.skills = Some(found.skills);
        if all {
            self.provider_skills = Some(found.provider_skills);
        }
    }
}

/// `stack inspect` previews a project that has no lock yet; once locked it reports drift.
pub fn inspect_mode(root: &Path) -> Mode {
    if root.join(lock::LOCK_FILE).exists() {
        Mode::Frozen
    } else {
        Mode::UseLock
    }
}

/// Presets `mise daemons` documents. Newer mise releases may add more, so others only warn.
const KNOWN_PRESETS: &[&str] = &["cockroachdb", "nats", "postgres", "redis", "spicedb"];

fn warnings(stack: &Composed) -> Vec<String> {
    let mut out = unpinnable_tools(stack);
    for (name, e) in &stack.services {
        let Some(preset) = e.value.preset.as_deref() else { continue };
        if !KNOWN_PRESETS.contains(&preset) {
            out.push(format!(
                "services.{name} ({}) uses preset '{preset}', which mise does not document (known: {}); `stack up` will fail if your mise lacks it",
                e.origin,
                KNOWN_PRESETS.join(", ")
            ));
        }
        if mise::preset_tool(preset).is_none() {
            out.push(format!(
                "services.{name} ({}) uses preset '{preset}'; stack does not know which tool it installs, so stack.lock cannot pin its version",
                e.origin
            ));
        } else if e.value.version.as_deref().is_some_and(mise::unversioned) {
            out.push(format!("services.{name} ({}) names no release; stack.lock cannot pin it", e.origin));
        }
    }
    out
}

/// Requests that name no release (`system`, `path:`, `ref:`), so stack.lock cannot pin them.
fn unpinnable_tools(stack: &Composed) -> Vec<String> {
    stack
        .tools
        .iter()
        .filter(|(_, e)| mise::unversioned(&e.value.version))
        .map(|(name, e)| {
            format!("tools.{name} = \"{}\" ({}) names no release; stack.lock cannot pin it", e.value.version, e.origin)
        })
        .collect()
}

/// The provider releases this stack needs, beyond what any stack needs. Commands that install
/// (`install`, `up`) and `doctor` check them; an older mise would silently ignore what the
/// configuration asked for.
pub fn provider_requirements(stack: &Composed) -> Vec<mise::Requirement> {
    let mut out = Vec::new();
    for (name, e) in &stack.tools {
        if e.value.mr_boxington() {
            out.push(mise::Requirement {
                minimum: mise::MR_BOXINGTON_MISE,
                reason: format!("tools.{name} ({}) sets mr_boxington", e.origin),
            });
        }
        if e.value.has_packslip_options() {
            out.push(mise::Requirement {
                minimum: mise::PACKSLIP_OPTIONS_MISE,
                reason: format!("tools.{name} ({}) sets packslip trust options", e.origin),
            });
        }
    }
    out
}

pub fn default_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STACK_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("stack");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("stack")
}

pub fn compile(opts: &Options) -> Result<Report> {
    // Inspect must remain read-only, including on a new machine.
    if !opts.write {
        return compile_locked(opts);
    }
    let _guard = crate::state::project_lock(&opts.state, &opts.root)?;
    compile_locked(opts)
}

/// The caller holds the project lock when publishing configuration or changing a session.
pub(crate) fn compile_locked(opts: &Options) -> Result<Report> {
    let project = read_project(&opts.root)?;
    if let Some(skills) = &project.skills {
        crate::skills::validate_dir(&skills.dir)?;
    }
    let previous = lock::read(&opts.root)?;
    if opts.mode == Mode::Frozen && previous.is_none() {
        return Err(
            StackError::new("lock_outdated", "stack.lock does not exist")
                .hint("run `stack compile` and commit stack.lock"),
        );
    }
    if opts.mode == Mode::Frozen && previous.as_ref().is_some_and(Lockfile::is_legacy) {
        return Err(StackError::new(
            "lock_outdated",
            "stack.lock is version 1 and records no exact tool or service versions",
        )
        .hint("run `stack compile` once to resolve and record them (bundle pins are kept), then commit stack.lock"));
    }

    let mut loaded = Vec::new();
    let mut reports = Vec::new();
    let mut locked = Vec::new();
    let mut names = HashSet::new();

    for entry in &project.uses {
        let spec = entry.bundle.as_str();
        let source = Source::parse(spec, &opts.root)?;
        let prior = previous.as_ref().and_then(|l| l.find(spec));
        let fetched = source.fetch(spec, prior, opts.mode, &opts.cache)?;
        let manifest = read_bundle(&fetched.dir, spec)?;
        let name = manifest.bundle.name.clone();
        if !names.insert(name.clone()) {
            return Err(StackError::new(
                "duplicate_bundle",
                format!("two [[use]] entries resolve to a bundle named '{name}'"),
            ));
        }

        locked.push(LockedBundle {
            source: spec.to_string(),
            name: name.clone(),
            commit: fetched.commit.clone(),
            digest: fetched.digest.clone(),
            content_hash: fetched.content_hash.clone(),
        });
        reports.push(BundleReport {
            name,
            version: manifest.bundle.version.clone(),
            source: spec.to_string(),
            commit: fetched.commit.clone(),
            digest: fetched.digest.clone(),
            content_hash: fetched.content_hash.clone(),
            dir: fetched.dir.clone(),
            moved_from: fetched.moved_from.clone(),
        });
        loaded.push(LoadedBundle::new(manifest, fetched.dir)?);
    }

    let stack = compose(&loaded, &project)?;
    let probed = probed_services(&stack)?;
    let (tools, services, versions) = lock_versions(&stack, previous.as_ref(), opts)?;
    let new_lock = Lockfile::new(locked, tools.clone(), services.clone());
    let lock_changed = previous.as_ref() != Some(&new_lock);
    if opts.mode == Mode::Frozen && lock_changed {
        return Err(
            StackError::new("lock_outdated", "stack.toml and stack.lock disagree")
                .hint("run `stack compile` and commit stack.lock"),
        );
    }

    let output = mise::output_path(&opts.root);
    let (ports, identities) = if opts.write {
        let requests: Vec<Request> = stack
            .services
            .iter()
            .map(|(name, e)| Request {
                service: name.clone(),
                fixed: e.value.fixed_port(),
            })
            .collect();
        let ports = ports::assign(&opts.state, &opts.root, &requests, opts.reassign_ports)?;
        if lock_changed {
            lock::write(&opts.root, &new_lock)?;
        }
        let exact = mise::Versions {
            tools: tools.iter().map(|t| (t.name.clone(), t.resolved.clone())).collect(),
            services: services.iter().map(|t| (t.name.clone(), t.resolved.clone())).collect(),
        };
        let identities = crate::identity::assign(&opts.state, &opts.root, &probed)?;
        write_if_changed(&output, &mise::render(&stack, &ports, &exact, &identities))?;
        (ports, identities)
    } else {
        let mut identities = crate::identity::lookup(&opts.state, &opts.root)?;
        identities.retain(|service, _| probed.contains(service));
        (ports::lookup(&opts.state, &opts.root)?, identities)
    };

    let warnings = warnings(&stack);
    Ok(Report {
        bundles: reports,
        warnings,
        stack,
        ports,
        versions,
        identities,
        lock_changed,
        provider: "mise",
        output,
        written: opts.write,
        skills: None,
        provider_skills: None,
        skills_dir: project.skills.map(|s| s.dir),
        lock: new_lock,
    })
}

/// Services with identity probes. Their token variables must not collide with each other or
/// with variables the stack defines, or a service could be handed another's token.
fn probed_services(stack: &Composed) -> Result<Vec<String>> {
    let mut seen: IndexMap<String, &String> = IndexMap::new();
    for (name, _) in stack.services.iter().filter(|(_, e)| e.value.identity.is_some()) {
        let var = crate::manifest::identity_var(name);
        if let Some(other) = seen.insert(var.clone(), name) {
            return Err(StackError::new(
                "invalid_service",
                format!("services '{other}' and '{name}' would share the identity variable {var}; rename one"),
            ));
        }
    }
    if let Some((var, e)) = stack.env.iter().find(|(k, _)| k.starts_with("STACK_IDENTITY_")) {
        return Err(StackError::new(
            "invalid_env",
            format!("env.{var} ({}) uses the STACK_IDENTITY_ prefix, which stack reserves for instance tokens", e.origin),
        ));
    }
    Ok(seen.into_values().cloned().collect())
}

/// The platform a resolution ran on, recorded for reviewers of stack.lock.
fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

struct VersionRequest {
    kind: &'static str,
    name: String,
    tool: Option<String>,
    requested: String,
    options: ToolOptions,
    origin: String,
}

/// Exact versions for every tool and resolvable preset service.
///
/// Ordinary compile keeps a pin while its request is unchanged and resolves only new or
/// changed requests; `--update` resolves everything again; locked mode never resolves and
/// fails on any missing or stale pin. Without `write` (inspect, doctor) nothing is resolved,
/// so a project without a lock reports `resolved: null`.
fn lock_versions(
    stack: &Composed,
    previous: Option<&Lockfile>,
    opts: &Options,
) -> Result<(Vec<LockedVersion>, Vec<LockedVersion>, Vec<VersionReport>)> {
    if let Some((key, _)) = stack.env.iter().find(|(key, _)| key.starts_with("MISE_") || key.starts_with("__MISE")) {
        return Err(StackError::new("invalid_env", format!("env.{key} changes Stack's isolated provider configuration; use tools, services or tasks instead")));
    }
    let mut requests: Vec<VersionRequest> = stack
        .tools
        .iter()
        .map(|(name, e)| VersionRequest { kind: "tool", name: name.clone(), tool: None, requested: e.value.version.clone(), options: e.value.options.clone(), origin: e.origin.clone() })
        .collect();
    if !stack.services.is_empty() && !stack.tools.contains_key("pitchfork") {
        requests.push(VersionRequest {
            kind: "tool",
            name: "pitchfork".into(),
            tool: None,
            requested: mise::PITCHFORK_VERSION.into(),
            options: ToolOptions::new(),
            origin: "provider".into(),
        });
    }
    for (name, e) in &stack.services {
        let Some(preset) = e.value.preset.as_deref() else { continue };
        if let Some(tool) = mise::preset_tool(preset) {
            requests.push(VersionRequest { kind: "service", name: name.clone(), tool: Some(tool.into()), requested: e.value.version.clone().unwrap_or_else(|| "latest".into()), options: ToolOptions::new(), origin: e.origin.clone() });
        } else if opts.mode == Mode::Frozen {
            return Err(StackError::new("unlocked_service", format!("services.{name} uses unsupported preset {preset:?}; locked mode cannot establish its tool or version")));
        }
    }

    // Legacy locks pin bundles only; their (absent) versions never count as pins.
    let previous = previous.filter(|l| !l.is_legacy());
    let default_resolver;
    let resolver: &dyn mise::Resolver = match &opts.resolver {
        Some(r) => r.as_ref(),
        None => {
            default_resolver = mise::MiseResolver { cache: opts.cache.clone() };
            &default_resolver
        }
    };
    // Whenever requests may be resolved; locked and read-only modes reuse what compile checked.
    if opts.write && opts.mode != Mode::Frozen {
        check_packslip_backends(stack, resolver)?;
    }
    let (mut tools, mut services, mut reports) = (Vec::new(), Vec::new(), Vec::new());
    let (mut stale, mut failed) = (Vec::new(), Vec::new());
    for r in requests {
        let prior = previous.and_then(|l| if r.kind == "tool" { l.tool(&r.name) } else { l.service(&r.name) });
        let same = prior.filter(|p| p.requested == r.requested && p.tool == r.tool && p.options == r.options);
        if !mise::unversioned(&r.requested) && opts.mode != Mode::Update {
            if let Some(pin) = same {
                if !mise::exact_release(r.tool.as_deref().unwrap_or(&r.name), &pin.resolved) {
                    return Err(StackError::new("lock_invalid", format!("{}.{} has non-exact resolved version {:?}", r.kind, r.name, pin.resolved))
                        .hint("run `stack compile --update` to regenerate exact release pins"));
                }
            }
        }
        let mut moved_from = None;
        let resolved: Option<(String, Option<String>)> = if mise::unversioned(&r.requested) {
            Some((r.requested.clone(), None))
        } else if opts.mode == Mode::Frozen {
            match same {
                Some(p) => Some((p.resolved.clone(), p.resolved_on.clone())),
                None => {
                    let mut detail = serde_json::json!({ "kind": r.kind, "name": r.name, "requested": r.requested, "locked": prior.map(|p| &p.requested) });
                    if !r.options.is_empty() || prior.is_some_and(|p| !p.options.is_empty()) {
                        detail["options"] = serde_json::json!(r.options);
                        detail["locked_options"] = serde_json::json!(prior.map(|p| &p.options));
                    }
                    stale.push(detail);
                    continue;
                }
            }
        } else if let (Mode::UseLock, Some(p)) = (opts.mode, same) {
            Some((p.resolved.clone(), p.resolved_on.clone()))
        } else if !opts.write {
            None
        } else {
            let spec = ToolSpec { version: r.requested.clone(), options: r.options.clone() };
            match resolver.resolve_spec(r.tool.as_deref().unwrap_or(&r.name), &spec) {
                Ok(v) if mise::exact_release(r.tool.as_deref().unwrap_or(&r.name), &v) => {
                    moved_from = prior.map(|p| p.resolved.clone()).filter(|p| *p != v);
                    Some((v, Some(platform())))
                }
                Ok(v) => {
                    failed.push(serde_json::json!({ "kind": r.kind, "name": r.name, "requested": r.requested, "error": format!("resolver returned non-exact release {v:?}") }));
                    continue;
                }
                Err(e) => {
                    failed.push(serde_json::json!({ "kind": r.kind, "name": r.name, "requested": r.requested, "code": e.code, "error": e.message }));
                    continue;
                }
            }
        };
        if let Some((version, resolved_on)) = &resolved {
            let entry = LockedVersion {
                name: r.name.clone(),
                tool: r.tool.clone(),
                requested: r.requested.clone(),
                resolved: version.clone(),
                resolved_on: resolved_on.clone(),
                options: r.options.clone(),
            };
            if r.kind == "tool" { tools.push(entry) } else { services.push(entry) }
        }
        reports.push(VersionReport {
            kind: r.kind,
            name: r.name,
            tool: r.tool,
            requested: r.requested,
            options: r.options,
            resolved: resolved.map(|(v, _)| v),
            origin: r.origin,
            moved_from,
        });
    }
    if !stale.is_empty() {
        return Err(StackError::new(
            "lock_outdated",
            format!("{} version(s) are not pinned in stack.lock for their current request", stale.len()),
        )
        .hint("run `stack compile` to resolve them, then commit stack.lock")
        .details(stale));
    }
    if !failed.is_empty() {
        return Err(StackError::new(
            "resolve_failed",
            format!("{} version request(s) could not be resolved; stack.lock and the provider config were not changed", failed.len()),
        )
        .hint("fix the tool name or version, or check network access to the tool's release source")
        .details(failed));
    }
    Ok((tools, services, reports))
}

/// Packslip trust options on a registry name (`fnox = { version, identity }`) are valid only
/// when mise's registry installs that tool through packslip; mise would otherwise ignore them.
fn check_packslip_backends(stack: &Composed, resolver: &dyn mise::Resolver) -> Result<()> {
    for (name, e) in stack.tools.iter().filter(|(name, e)| e.value.needs_packslip_backend(name)) {
        let backend = resolver.registry_backend(name)?;
        if backend.as_deref().is_some_and(crate::tool::is_packslip) {
            continue;
        }
        let why = match &backend {
            Some(b) => format!("mise's registry installs it through {b}"),
            None => "mise's registry does not say which backend installs it".to_string(),
        };
        return Err(StackError::new(
            "invalid_tool",
            format!("tools.{name} ({}) sets packslip trust options, but {why}", e.origin),
        )
        .hint(format!("{}; to name the backend yourself, use `\"packslip:<host>/<owner>/<repo>\"` as the tool name", crate::tool::accepted(name)))
        .with_detail(serde_json::json!({ "tool": name, "origin": e.origin, "backend": backend })));
    }
    Ok(())
}

fn write_if_changed(path: &Path, contents: &str) -> Result<()> {
    if fs::read_to_string(path).ok().as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    }
    fs::write(path, contents).map_err(|e| io_error(path.display(), e))
}

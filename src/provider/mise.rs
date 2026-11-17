//! mise installs tools and renders the stack's environment. Services are Stack's own (see
//! `crate::supervisor`); what is left of Pitchfork here only stops services an older Stack
//! started under it.

use crate::compose::Composed;
use crate::tool::ToolSpec;
pub use super::scratch::{Pin, ScratchRoot};
use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

/// mise auto-loads `conf.d/*.toml`, so plain `mise` commands see the stack too.
pub fn output_path(root: &Path) -> PathBuf {
    root.join(".config/mise/conf.d/stack.toml")
}

/// Exact versions rendered in place of the composed requests.
#[derive(Debug, Default)]
pub struct Versions {
    pub tools: IndexMap<String, String>,
    pub services: IndexMap<String, String>,
}

/// The provider tool a service preset installs, for presets whose mapping stack has verified
/// against mise (a `postgres` preset with `version = "17"` installs `postgres@17`).
pub fn preset_tool(preset: &str) -> Option<&'static str> {
    match preset {
        "postgres" => Some("postgres"),
        "redis" => Some("redis"),
        "cockroachdb" => Some("cockroach"),
        "nats" => Some("nats-server"),
        "spicedb" => Some("spicedb"),
        _ => None,
    }
}

/// Requests that name something other than a release, so no version can be locked.
pub fn unversioned(request: &str) -> bool {
    let r = request.trim();
    r == "system" || ["path:", "ref:"].iter().any(|p| r.starts_with(p))
}

/// Finds the exact release a version request currently means.
pub trait Resolver: Send + Sync {
    fn resolve(&self, tool: &str, request: &str) -> Result<String>;

    /// Resolution with the tool's allowlisted options, which some backends need to list
    /// releases at all (packslip trust options). Resolvers that need no options keep this.
    fn resolve_spec(&self, tool: &str, spec: &ToolSpec) -> Result<String> {
        self.resolve(tool, &spec.version)
    }

    /// The backend mise's registry installs a short name through (`packslip:github.com/jdx/fnox`
    /// for `fnox`), or `None` when it cannot say. Asked only for registry names that carry
    /// packslip trust options.
    fn registry_backend(&self, _tool: &str) -> Result<Option<String>> {
        Ok(None)
    }
}

/// `mise latest <tool>@<request>` in a scratch provider root of its own under the cache, whose
/// only configuration is the one tool and its options. Neither the project's nor the user's
/// configuration can change which release a request means, and no `[env]` template or task
/// of the project's is evaluated.
pub struct MiseResolver {
    pub cache: PathBuf,
}

const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl Resolver for MiseResolver {
    fn resolve(&self, tool: &str, request: &str) -> Result<String> {
        self.resolve_spec(tool, &ToolSpec::new(request))
    }

    fn resolve_spec(&self, tool: &str, request: &ToolSpec) -> Result<String> {
        let scratch = ScratchRoot::create(&self.cache, "resolve")?;
        scratch.write_tools(&[Pin { tool: tool.to_string(), spec: request.clone() }])?;
        // `latest` already interprets a bare version as a prefix; it rejects the explicit
        // `prefix:` spelling that other mise commands accept.
        let version = request.version.as_str();
        let version = version.strip_prefix("prefix:").unwrap_or(version);
        let spec = if version.trim() == "latest" { tool.to_string() } else { format!("{tool}@{version}") };
        let mut command = scratch.command(&["latest", &spec]);
        let out = crate::process::capture(&mut command, RESOLVE_TIMEOUT, 16 * 1024).map_err(|e| {
            StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
                .hint(crate::setup::MISE_INSTALL_HINT)
        })?;
        interpret_latest(tool, &spec, out)
    }

    /// `mise registry <tool>`: backends in mise's order of preference; the first is the one a
    /// current release installs through. No configuration is loaded.
    fn registry_backend(&self, tool: &str) -> Result<Option<String>> {
        let scratch = ScratchRoot::create(&self.cache, "registry")?;
        let mut command = scratch.command(&["registry", tool]);
        command.env("MISE_NO_CONFIG", "1");
        let out = crate::process::capture(&mut command, VERSION_TIMEOUT, 16 * 1024).map_err(|e| {
            StackError::new("provider_unavailable", format!("cannot run mise: {e}")).hint(crate::setup::MISE_INSTALL_HINT)
        })?;
        if out.timed_out {
            let code = if crate::process::expired() { "timed_out" } else { "provider_unavailable" };
            return Err(StackError::new(code, format!("`mise registry {tool}` did not answer")));
        }
        if out.exit_code != Some(0) {
            return Ok(None);
        }
        Ok(out.stdout.split_whitespace().next().map(str::to_string))
    }
}

/// Locks artifacts in a scratch root and reports the provider release. `stack compile` uses
/// [`MiseLocker`]; tests substitute a fake that writes a lock into the scratch root.
pub trait Locker: Send + Sync {
    /// The release of the provider that will read the rendered lock.
    fn version(&self, root: &Path) -> Result<CalVer> {
        provider_version(root)
    }

    /// `mise lock --platform <platforms> <tools>...` in `scratch`, bounded. The lock it leaves
    /// is read from the scratch root by the caller; a nonzero exit is not a failure by itself.
    fn lock(&self, scratch: &ScratchRoot, platforms: &[String], tools: &[String]) -> Result<LockRun>;

    /// The machine whose lock entries mise will look up: `"current"` in `[lock] platforms` and
    /// the checks locked operations make here. Tests substitute another machine.
    fn host(&self) -> crate::artifacts::Host {
        crate::artifacts::Host::current()
    }
}

/// How a `mise lock` run ended. Exit status and the presence of an entry prove nothing about
/// whether a refresh happened (an offline run exits 0 and keeps what it was given).
#[derive(Debug, Clone, Default)]
pub struct LockRun {
    pub exit_code: Option<i32>,
    pub stderr: String,
}

/// Locking fetches metadata and, for some backends, artifacts.
pub const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const LOCK_OUTPUT_LIMIT: usize = 64 * 1024;

pub struct MiseLocker;

impl Locker for MiseLocker {
    fn lock(&self, scratch: &ScratchRoot, platforms: &[String], tools: &[String]) -> Result<LockRun> {
        let platforms = platforms.join(",");
        let mut args = vec!["lock", "--platform", platforms.as_str()];
        args.extend(tools.iter().map(String::as_str));
        let mut command = scratch.command(&args);
        let out = crate::process::capture(&mut command, LOCK_TIMEOUT, LOCK_OUTPUT_LIMIT).map_err(|e| {
            StackError::new("artifact_lock_failed", format!("cannot run mise lock: {e}"))
                .hint(format!("{}; stack.lock was not changed", crate::setup::MISE_INSTALL_HINT))
        })?;
        lock_run(out)
    }
}

/// A finished `mise lock`, or the failure a timeout is.
pub fn lock_run(out: crate::process::Captured) -> Result<LockRun> {
    if out.timed_out && crate::process::expired() {
        return Err(StackError::new("timed_out", "`mise lock` was cut short by the deadline; stack.lock was not changed"));
    }
    if out.timed_out {
        return Err(StackError::new("artifact_lock_failed", format!("`mise lock` did not finish within {}s", LOCK_TIMEOUT.as_secs()))
            .hint("check mise and network access; stack.lock was not changed")
            .with_detail(serde_json::json!({ "output": readable_tail(&out.stderr, TAIL_LINES) })));
    }
    Ok(LockRun { exit_code: out.exit_code, stderr: out.stderr })
}

/// The last lines of provider text in readable form, as kept in error details.
pub fn text_tail(text: &str) -> String {
    without_lock_advice(&readable_tail(text, TAIL_LINES))
}

/// What `mise latest` answered, as an exact release or `resolve_failed`.
fn interpret_latest(tool: &str, spec: &str, out: crate::process::Captured) -> Result<String> {
    let fail = |why: String| {
        StackError::new("resolve_failed", format!("cannot resolve {spec}: {why}"))
            .hint("check the tool name and version; `mise ls-remote <tool>` lists releases")
    };
    if out.timed_out && crate::process::expired() {
        return Err(StackError::new("timed_out", format!("resolving {spec} was cut short by the deadline")));
    }
    if out.timed_out {
        return Err(fail(format!("mise did not answer within {}s", RESOLVE_TIMEOUT.as_secs())));
    }
    if out.exit_code != Some(0) {
        let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("mise failed");
        return Err(fail(err.to_string()));
    }
    parse_resolved(&out.stdout).filter(|v| exact_release(tool, v)).ok_or_else(|| {
        // mise exits 0 with no output when no release matches the prefix.
        fail(if out.stdout.trim().is_empty() {
            "no release matches".to_string()
        } else {
            format!("unexpected output {:?}", out.stdout.trim())
        })
    })
}

/// A mise release by its calendar version (`2026.9.2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalVer(pub u32, pub u32, pub u32);

impl CalVer {
    /// The release `mise version` prints first on stdout (`2026.10.3 macos-arm64 (2026-10-05)`).
    /// Only stdout is read: mise's update notice on stderr names another release.
    pub fn parse(stdout: &str) -> Option<Self> {
        let token = stdout.lines().map(str::trim).find(|l| !l.is_empty())?.split_whitespace().next()?;
        let mut parts = token.strip_prefix('v').unwrap_or(token).split('.');
        let mut part = || parts.next()?.parse::<u32>().ok();
        let version = Self(part()?, part()?, part()?);
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for CalVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The first mise release that honours `mr_boxington` on Rust tools.
pub const MR_BOXINGTON_MISE: CalVer = CalVer(2026, 9, 2);
/// The first mise release whose packslip backend reads `pubkey`, `identity`, `identity_prefix`
/// and `issuer` tool options.
pub const PACKSLIP_OPTIONS_MISE: CalVer = CalVer(2026, 9, 2);

/// A provider release the configuration needs, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub minimum: CalVer,
    /// Names what needs it, e.g. "tools.rust (bundle:rust-mbx) sets mr_boxington".
    pub reason: String,
}

const VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// A second, longer wait for a `mise version` that missed [`VERSION_TIMEOUT`] once: a first run
/// in a fresh mise root is slower than any later one.
const VERSION_RETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The release of the mise on PATH, from `mise version` with no configuration loaded.
///
/// mise's update check is turned off for it (`MISE_DISABLE_UPDATE_WARNING`): with a fresh
/// cache, `mise version` otherwise asks GitHub for the latest release before it answers, and
/// stack reads only the release it prints. One that still misses the deadline is asked once
/// more, for longer; one that never answers is reported as installed but not answering,
/// never as missing.
pub fn provider_version(root: &Path) -> Result<CalVer> {
    let ask = |wait: std::time::Duration| {
        let mut command = Command::new("mise");
        configure_command(&mut command, root);
        command
            .args(["version"])
            .current_dir(root)
            .env("MISE_NO_CONFIG", "1")
            .env("MISE_DISABLE_UPDATE_WARNING", "1")
            .env("NO_COLOR", "1");
        crate::process::capture(&mut command, wait, 16 * 1024).map_err(|e| {
            StackError::new("provider_unavailable", format!("cannot run mise: {e}")).hint(crate::setup::MISE_INSTALL_HINT)
        })
    };
    let mut out = ask(VERSION_TIMEOUT)?;
    if out.timed_out && !crate::process::expired() {
        out = ask(VERSION_RETRY_TIMEOUT)?;
    }
    if out.timed_out && crate::process::expired() {
        return Err(StackError::new("timed_out", "`mise version` was cut short by the deadline"));
    }
    if out.timed_out {
        let mise = crate::setup::find_on_path_from(root).map_or_else(|| "mise".to_string(), |p| p.display().to_string());
        return Err(StackError::new(
            "provider_unavailable",
            format!("`mise version` did not answer within {}s, nor within {}s when asked again", VERSION_TIMEOUT.as_secs(), VERSION_RETRY_TIMEOUT.as_secs()),
        )
        .hint(format!("{mise} is installed but is not answering; check that it runs (`{mise} version`) and that nothing holds its state directory, then retry"))
        .with_detail(serde_json::json!({ "mise": mise })));
    }
    if out.exit_code != Some(0) {
        return Err(StackError::new("provider_unavailable", "`mise version` failed").hint(crate::setup::MISE_INSTALL_HINT));
    }
    CalVer::parse(&out.stdout).ok_or_else(|| {
        StackError::new("provider_outdated", format!("cannot read a release from `mise version`: {:?}", out.stdout.trim()))
            .hint("upgrade mise (`mise self-update`), or run `stack setup --force` for the release stack is tested against")
    })
}

/// `Err(provider_outdated)` naming every requirement `version` does not meet.
pub fn check_requirements(version: CalVer, requirements: &[Requirement]) -> Result<()> {
    let unmet: Vec<&Requirement> = requirements.iter().filter(|r| version < r.minimum).collect();
    let Some(first) = unmet.first() else { return Ok(()) };
    let minimum = unmet.iter().map(|r| r.minimum).max().expect("unmet is not empty");
    Err(StackError::new(
        "provider_outdated",
        format!("mise {version} is older than {}, which {} needs", first.minimum, first.reason),
    )
    .hint(format!("upgrade mise to {minimum} or newer (`mise self-update`, or `stack setup --force` for the release stack is tested against)"))
    .details(unmet.iter().map(|r| serde_json::json!({ "minimum": r.minimum.to_string(), "actual": version.to_string(), "reason": r.reason })).collect()))
}

/// Runs `mise version` once, only when there is something to check, and fails with
/// `provider_outdated` before anything is installed when the release is too old.
pub fn require(root: &Path, requirements: &[Requirement]) -> Result<Option<CalVer>> {
    if requirements.is_empty() {
        return Ok(None);
    }
    let version = provider_version(root)?;
    check_requirements(version, requirements)?;
    Ok(Some(version))
}

/// [`require`], with the release Stack recorded for this exact mise binary in `state` when
/// there is one (see [`super::env_cache::recorded_version`]), and recording what it asked.
pub fn require_in(state: &Path, root: &Path, requirements: &[Requirement]) -> Result<Option<CalVer>> {
    if requirements.is_empty() {
        return Ok(None);
    }
    let caller = super::env_cache::Caller::current();
    let recorded = super::env_cache::recorded_version(&caller, state).and_then(|v| CalVer::parse(&v));
    let version = match recorded {
        Some(version) => version,
        None => {
            let before = super::env_cache::version_identity(&caller);
            let version = provider_version(root)?;
            super::env_cache::record_version(&caller, state, before.as_deref(), &version.to_string());
            version
        }
    };
    check_requirements(version, requirements)?;
    Ok(Some(version))
}

/// One version on one line; anything else is not an answer stack can lock.
pub fn parse_resolved(stdout: &str) -> Option<String> {
    let lines: Vec<&str> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    match lines.as_slice() {
        [v] if v.len() <= 128 && !v.contains(char::is_whitespace) && *v != "latest" => Some(v.to_string()),
        _ => None,
    }
}

/// Offline validation of release pins. Known providers have different release shapes:
/// PostgreSQL and jq use two numeric components, while Python/Node/Redis require three.
/// Other backends may use calendar or named releases, but never floating selectors.
pub fn exact_release(tool: &str, version: &str) -> bool {
    if version.is_empty() || version.len() > 128
        || !version.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        || !version.bytes().any(|b| b.is_ascii_digit())
        || version.starts_with("sub-") || version.starts_with("lts-")
    {
        return false;
    }
    let name = tool.rsplit([':', '/']).next().unwrap_or(tool);
    let components = match name {
        "python" | "node" | "nodejs" | "ruby" | "rust" | "uv" | "redis" | "pitchfork"
        | "cockroach" | "nats-server" | "spicedb" => 3,
        "postgres" | "postgresql" | "jq" | "go" => 2,
        _ => return !version.bytes().all(|b| b.is_ascii_digit()) || version.len() >= 8,
    };
    let numeric = version.trim_start_matches(|c: char| !c.is_ascii_digit());
    numeric.split('.').take(components).filter(|part| part.starts_with(|c: char| c.is_ascii_digit())).count() == components
}

/// `ports` are this checkout's assigned ports; every service gets a concrete one. `versions`
/// replace composed version requests with the exact versions stack.lock records.
/// `identities` are instance tokens, exported as `STACK_IDENTITY_<NAME>` for the service to
/// report back to its identity probe.
/// Environment the provider config adds beyond what the stack declares, given the exact
/// Python release stack.lock pins (if any). Part of the session fingerprint, so a change here
/// restarts services the way a declared change would.
pub fn implicit_env(stack: &Composed, python: Option<&str>) -> IndexMap<String, String> {
    let mut env = IndexMap::new();
    if !stack.tools.contains_key("python") {
        return env;
    }
    // uv prefers an active Conda environment, then Python builds it manages itself, over the
    // locked interpreter on PATH; `uv run` and `uv sync` would quietly use another release.
    // Name the locked release and allow only installed interpreters, so uv finds this one on
    // PATH (or a virtualenv built on it). A project's own `[env]` value wins for each variable.
    let mut add = |key: &str, value: &str| {
        if !stack.env.contains_key(key) {
            env.insert(key.to_string(), value.to_string());
        }
    };
    add("UV_PYTHON_PREFERENCE", "only-system");
    if let Some(version) = python.filter(|v| exact_release("python", v)) {
        add("UV_PYTHON", version);
    }
    env
}

/// The generated provider configuration. `checkout` is the checkout's token (see
/// [`crate::checkout`]), given to every service in its own environment.
pub fn render(
    stack: &Composed,
    ports: &IndexMap<String, u16>,
    versions: &Versions,
    identities: &IndexMap<String, String>,
    _checkout: Option<&str>,
) -> String {
    let mut doc = Table::new();
    let mut tools: Table = stack
        .tools
        .iter()
        .map(|(k, e)| {
            let version = versions.tools.get(k).unwrap_or(&e.value.version);
            (k.clone(), e.value.at(version.as_str()).to_toml())
        })
        .collect();
    // Each preset service's server, at the version stack.lock pins, unless the stack names that
    // tool itself. Services of one preset at different versions list each (mise installs every
    // one; the first is first on PATH).
    let mut presets: IndexMap<&str, Vec<String>> = IndexMap::new();
    for (name, e) in &stack.services {
        let Some(tool) = e.value.preset.as_deref().and_then(preset_tool) else { continue };
        if stack.tools.contains_key(tool) {
            continue;
        }
        let version = versions.services.get(name).or(e.value.version.as_ref()).cloned().unwrap_or_else(|| "latest".into());
        let listed = presets.entry(tool).or_default();
        if !listed.contains(&version) {
            listed.push(version);
        }
    }
    for (tool, mut listed) in presets {
        let value = if listed.len() == 1 { Value::String(listed.remove(0)) } else { Value::Array(listed.into_iter().map(Value::String).collect()) };
        tools.insert(tool.into(), value);
    }
    if !tools.is_empty() {
        doc.insert("tools".into(), Value::Table(tools));
    }

    let mut env: Table = stack
        .env
        .iter()
        .map(|(k, e)| (k.clone(), Value::String(e.value.clone())))
        .collect();
    for (key, value) in implicit_env(stack, versions.tools.get("python").map(String::as_str)) {
        env.insert(key, Value::String(value));
    }
    for (service, token) in identities {
        env.insert(crate::manifest::identity_var(service), Value::String(token.clone()));
    }
    for (key, url) in loopback_urls(stack, ports) {
        env.entry(key).or_insert(Value::String(url));
    }
    let mut directives = Table::new();
    if !stack.bin_paths.is_empty() {
        let paths = stack
            .bin_paths
            .iter()
            .map(|p| Value::String(p.to_string_lossy().into()))
            .collect();
        directives.insert("path".into(), Value::Array(paths));
    }
    if !directives.is_empty() {
        env.insert("_".into(), Value::Table(directives));
    }
    if !env.is_empty() {
        doc.insert("env".into(), Value::Table(env));
    }

    if !stack.tasks.is_empty() {
        let tasks = stack
            .tasks
            .iter()
            .map(|(name, e)| {
                let mut t = Table::new();
                let task = &e.value;
                t.insert("run".into(), Value::String(task.run.clone()));
                put(&mut t, "description", task.description.clone().map(Value::String));
                (name.clone(), Value::Table(t))
            })
            .collect();
        doc.insert("tasks".into(), Value::Table(tasks));
    }

    let body = toml::to_string_pretty(&doc).expect("mise config serializes");
    format!("{HEADER}\n{body}")
}

/// The first line of the generated provider configuration.
const HEADER: &str = "# Generated by `stack compile` from stack.toml + stack.lock. Do not edit.";

/// Environment a preset's daemon (and so its one-time initialization) always gets, whatever
/// the caller's. mise's postgres preset runs a bare `initdb`, which takes the cluster's
/// encoding and collation from the locale it inherits: `SQL_ASCII` with collation `C` under
/// `LC_ALL=C` or no locale at all (CI, `env -i`, containers), `UTF8` in a UTF-8 shell, and fixed
/// at the first `up`. `C.UTF-8` gives every checkout a `UTF8` cluster with the same
/// code-point collation on macOS and Linux. Only a new cluster is affected: an existing one
/// keeps the encoding and collation it was created with, and `up` warns when it is not UTF8.
/// A locale the project sets in `[env]` reaches the daemon too and takes precedence (mise
/// applies `[env]` after a daemon's own environment): that is the project's committed choice,
/// the same on every machine, not the caller's.
///
/// Redis takes its collation (for `SORT ... ALPHA`) from the inherited locale and exits at
/// once when that names a locale the machine lacks (`LANG=en_US.UTF-8` in a minimal Linux
/// image): `C`, which every system has, makes it start anywhere and sort the same everywhere.
pub fn preset_env(preset: Option<&str>) -> &'static [(&'static str, &'static str)] {
    match preset {
        Some("postgres") => &[("LC_ALL", POSTGRES_LOCALE)],
        Some("redis") => &[("LC_ALL", "C")],
        _ => &[],
    }
}

/// The locale a new Postgres cluster is initialized with (see [`preset_env`]).
pub const POSTGRES_LOCALE: &str = "C.UTF-8";

/// The connection variables of every service, as the loopback address stack assigns and
/// verifies: a preset's (`DATABASE_URL`, `PGPORT`, `REDIS_URL`, ..., see
/// [`crate::supervisor::preset_exports`]) and `<NAME>_URL` and `<NAME>_PORT` of a service without
/// one. mise exported these for its daemons; Stack renders them into `[env]`, so services,
/// tasks and `exec` agree. An explicit `[env]` value is kept. Names whose variable would start
/// with a digit get nothing.
fn loopback_urls(stack: &Composed, ports: &IndexMap<String, u16>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, e) in &stack.services {
        let Some(port) = ports.get(name) else { continue };
        match e.value.preset.as_deref() {
            Some(preset) => {
                // The preset's further ports, reserved as `<service>.<role>` (see `secondary_roles`).
                let secondary: crate::supervisor::Secondary = crate::supervisor::secondary_roles(preset)
                    .iter()
                    .filter_map(|role| Some((role.to_string(), *ports.get(&format!("{name}.{role}"))?)))
                    .collect();
                out.extend(crate::supervisor::preset_exports(preset, *port, &secondary).into_iter().map(|(k, v)| (k.to_string(), v)))
            }
            None if !name.starts_with(|c: char| c.is_ascii_digit()) => {
                let base: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
                out.push((format!("{base}_URL"), format!("http://127.0.0.1:{port}")));
                out.push((format!("{base}_PORT"), port.to_string()));
            }
            None => {}
        }
    }
    out
}

fn put(t: &mut Table, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        t.insert(key.into(), v);
    }
}

// ---- runtime -------------------------------------------------------------------------------

use crate::error::{Result, StackError};
use std::process::{Command, Output};

pub use crate::supervisor::DaemonStatus;

/// Inherited provider settings may redefine aliases, backends or the config search path.
/// Keep storage and network credentials, but reserve configuration selection to Stack.
pub(crate) fn config_key(key: &str) -> bool {
    (key.starts_with("MISE_") && !matches!(key,
        "MISE_DATA_DIR" | "MISE_CACHE_DIR" | "MISE_STATE_DIR" | "MISE_GITHUB_TOKEN"))
        || key.starts_with("__MISE")
}

pub fn inherited_config_keys() -> Vec<String> {
    std::env::vars_os().filter_map(|(key, _)| key.into_string().ok())
        .filter(|key| config_key(key)).collect()
}

/// One boundary for resolution, install, supervisor operations and nested mise commands.
pub fn config_env(root: &Path) -> IndexMap<String, String> {
    let isolated = root.join(".stack/provider-config");
    [
        ("MISE_CONFIG_DIR", isolated.clone()),
        ("MISE_SYSTEM_CONFIG_DIR", isolated.clone()),
        ("MISE_GLOBAL_CONFIG_FILE", isolated.join("global.toml")),
        ("MISE_SYSTEM_CONFIG_FILE", isolated.join("system.toml")),
        ("MISE_CEILING_PATHS", root.parent().unwrap_or(root).to_path_buf()),
    ].into_iter().map(|(key, value)| (key.into(), value.to_string_lossy().into_owned()))
        .chain([
            ("MISE_OVERRIDE_CONFIG_FILENAMES".into(), ".config/mise/conf.d/stack.toml".into()),
            ("MISE_OVERRIDE_TOOL_VERSIONS_FILENAMES".into(), "none".into()),
            ("MISE_IDIOMATIC_VERSION_FILE_ENABLE_TOOLS".into(), "".into()),
            ("MISE_ENV".into(), "".into()),
            ("MISE_AUTO_ENV".into(), "false".into()),
            ("MISE_YES".into(), "1".into()),
        ]).collect()
}

pub fn configure_command(command: &mut Command, root: &Path) {
    for key in inherited_config_keys() {
        command.env_remove(key);
    }
    // Only a request that asks for it carries an owner mark or a binding, never one stack
    // inherited.
    command.env_remove(OWNER_VAR);
    command.env_remove(BINDING_VAR);
    command.envs(config_env(root));
}

/// Set by Stack 0.1.23 and earlier on requests to Pitchfork; removed from whatever any provider
/// command inherits.
pub const BINDING_VAR: &str = "STACK_SUPERVISOR_BINDING";

/// A mise command, bounded by the caller's deadline (see `process::output`), with `overrides` set after the provider selection (to point mise at another
/// configuration, such as a task's copy).
fn mise_with(root: &Path, args: &[&str], overrides: &[(String, String)]) -> Result<Output> {
    let mut command = Command::new("mise");
    configure_command(&mut command, root);
    command.envs(overrides.iter().map(|(k, v)| (k, v)));
    command.args(args)
        .current_dir(root)
        .env("MISE_YES", "1")
        .env("NO_COLOR", "1");
    crate::process::output(&mut command).map_err(|e| match e.kind() {
        std::io::ErrorKind::TimedOut => StackError::new("timed_out", format!("mise {}: {e}", args.join(" "))),
        _ => StackError::new("provider_unavailable", format!("cannot run mise: {e}"))
            .hint(crate::setup::MISE_INSTALL_HINT),
    })
}

fn checked(root: &Path, args: &[&str], code: &'static str) -> Result<Output> {
    checked_with(root, args, code, &[])
}

fn checked_with(root: &Path, args: &[&str], code: &'static str, overrides: &[(String, String)]) -> Result<Output> {
    let out = mise_with(root, args, overrides)?;
    if out.status.success() {
        return Ok(out);
    }
    let failed = if crate::process::expired() { "was cut short by the deadline" } else { "failed" };
    Err(StackError::new(code, format!("mise {} {failed}", args.join(" ")))
        .details(vec![serde_json::json!({ "output": tail(&out) })]))
}

/// Lines of provider output kept in an error: enough for a traceback and the final error.
const TAIL_LINES: usize = 20;
/// Lines of a service's own output kept when the supervisor reports it failed, and when it
/// did not (its startup chatter says little about someone else's failure).
const FAILED_SERVICE_LINES: usize = 12;
const OTHER_SERVICE_LINES: usize = 2;
/// Bytes of one kept line, and of the whole tail.
const LINE_BYTES: usize = 300;
const TAIL_BYTES: usize = 2048;

/// Last lines of combined output, as a terminal would have left them, without mise's advice to
/// edit mise.lock (see [`without_lock_advice`]), with each service's log lines summarized (see
/// [`summarize_services`]) and bounded in bytes as well as lines.
pub fn tail(out: &Output) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let lines = readable_lines(&text);
    let mut kept = summarize_services(&lines);
    // Over the limit, other services' lines go first, oldest first.
    let mut excess = kept.len().saturating_sub(TAIL_LINES);
    kept.retain(|(_, other)| {
        let drop = excess > 0 && *other;
        excess -= usize::from(drop);
        !drop
    });
    let kept: Vec<String> = kept.into_iter().map(|(line, _)| line).collect();
    bounded_tail(&without_lock_advice(&kept[kept.len().saturating_sub(TAIL_LINES)..].join("\n")))
}

/// The supervisor streams every starting service's output as `• [<project>-<hash>/<name>] …`.
/// A failed start shows the failing service's last lines, a start the deadline cut short only
/// what each service printed last; everything else the supervisor said (its errors, `✗` lines)
/// is kept. Service lines are shortened to `[<name>] …`. Each kept line says whether it is a
/// service's that did not fail.
fn summarize_services(lines: &[String]) -> Vec<(String, bool)> {
    // (qualified id, name, message), parsed once per line.
    let parsed: Vec<Option<(&str, &str, &str)>> = lines
        .iter()
        .map(|line| {
            let (id, message) = line.strip_prefix("\u{2022} [")?.split_once("] ")?;
            Some((id, id.rsplit('/').next().unwrap_or(id), message))
        })
        .collect();
    // A service the supervisor itself names (`Daemon <id> failed`, `✗ [<id>] failed`) failed.
    let ids: std::collections::HashSet<&str> = parsed.iter().flatten().map(|(id, _, _)| *id).collect();
    let mut failed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (line, parsed) in lines.iter().zip(&parsed) {
        if parsed.is_none() {
            failed.extend(ids.iter().filter(|id| line.contains(**id)));
        }
    }
    let mut budget: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut kept: Vec<(String, bool)> = Vec::new();
    for (line, parsed) in lines.iter().zip(&parsed).rev() {
        match parsed {
            Some((id, name, message)) => {
                let left = budget.entry(id).or_insert(if failed.contains(id) { FAILED_SERVICE_LINES } else { OTHER_SERVICE_LINES });
                if *left > 0 {
                    *left -= 1;
                    kept.push((format!("[{name}] {message}"), !failed.contains(id)));
                }
            }
            None => kept.push((line.clone(), false)),
        }
    }
    kept.reverse();
    kept
}

/// `text` with each line cut at [`LINE_BYTES`] and only the last [`TAIL_BYTES`] kept, at line
/// boundaries; what was dropped is said in the first line.
fn bounded_tail(text: &str) -> String {
    let cut = |line: &str| {
        if line.len() <= LINE_BYTES {
            return line.to_string();
        }
        let mut end = LINE_BYTES;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &line[..end])
    };
    let lines: Vec<String> = text.lines().map(cut).collect();
    let mut total = 0;
    let mut start = lines.len();
    while start > 0 && total + lines[start - 1].len() < TAIL_BYTES {
        start -= 1;
        total += lines[start].len() + 1;
    }
    let mut out = lines[start..].to_vec();
    if start > 0 {
        out.insert(0, format!("({start} earlier lines omitted; `stack logs <service>` shows a service's output)"));
    }
    out.join("\n")
}

/// The last `max` lines of terminal output in readable form: escape sequences removed, each
/// carriage-return redraw reduced to what it finally showed, blank lines dropped, and spinner
/// frames dropped unless they are all there is. The supervisor animates progress even when
/// it is captured, between the lines a failing service printed.
fn readable_tail(text: &str, max: usize) -> String {
    let lines = readable_lines(text);
    lines[lines.len().saturating_sub(max)..].join("\n")
}

/// Every line of [`readable_tail`].
fn readable_lines(text: &str) -> Vec<String> {
    let plain = strip_escapes(text);
    let lines: Vec<&str> = plain
        .split('\n')
        .filter_map(|line| line.split('\r').rev().find(|s| !s.trim().is_empty()))
        .map(str::trim_end)
        .collect();
    let content: Vec<&str> = lines.iter().copied().filter(|l| !spinner_frame(l)).collect();
    let kept = if content.is_empty() { &lines[lines.len().saturating_sub(1)..] } else { &content[..] };
    kept.iter().map(|l| l.to_string()).collect()
}

/// A progress line: it starts with a Braille spinner glyph.
fn spinner_frame(line: &str) -> bool {
    line.trim_start().chars().next().is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
}

/// `text` without ANSI escape sequences (CSI such as colours and line clearing, OSC such as
/// titles and hyperlinks, and two-character escapes) or other control characters. Newlines,
/// carriage returns and tabs are kept.
fn strip_escapes(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || matches!(c, '\n' | '\r' | '\t') {
                clean.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte.
            Some('[') => {
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST (ESC \\).
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    clean
}

/// Where the digest of the generated configuration `mise trust` last accepted is kept.
fn trusted_marker(state: &Path, root: &Path) -> PathBuf {
    state.join("projects").join(format!("{}.trusted", crate::state::project_key(root)))
}

/// [`trust`], skipped when mise already trusted exactly this generated configuration, as
/// recorded the last time. Returns whether it was skipped: a caller whose next provider call
/// then fails should [`forget_trust`] and trust again, in case mise's own record was removed.
pub fn trust_once(state: &Path, root: &Path) -> Result<bool> {
    let digest = std::fs::read(output_path(root)).ok().map(|bytes| crate::hash::sha256_hex(&bytes));
    let marker = trusted_marker(state, root);
    if digest.is_some() && std::fs::read_to_string(&marker).ok() == digest {
        return Ok(true);
    }
    trust(root)?;
    if let Some(digest) = digest {
        // Best effort: without the marker the next call only trusts again.
        let _ = marker.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(&marker, digest);
    }
    Ok(false)
}

/// Forget what [`trust_once`] recorded, so the next call asks mise again.
pub fn forget_trust(state: &Path, root: &Path) {
    let _ = std::fs::remove_file(trusted_marker(state, root));
}

pub fn trust(root: &Path) -> Result<()> {
    checked(root, &["trust", "--quiet", &output_path(root).to_string_lossy()], "provider_failed").map(|_| ())
}

/// `mise install` of the named tools (with `--locked` when `locked`), or of everything the
/// configuration names when `tools` is empty. Naming a tool installs every version the
/// configuration gives it, with the configuration's options, services' preset tools included.
/// A download mise refuses because it does not match the rendered lock is `artifact_mismatch`
/// with what mise said parsed into details; any other failure is `install_failed`.
pub fn install_tools(root: &Path, tools: &[String], locked: bool) -> Result<()> {
    install_tools_with(root, &[], tools, locked)
}

/// [`install_tools`] with `overrides` naming the configuration (see [`mise_with`]).
pub fn install_tools_with(root: &Path, overrides: &[(String, String)], tools: &[String], locked: bool) -> Result<()> {
    let mut args = vec!["install"];
    if locked {
        args.push("--locked");
    }
    args.extend(["--yes", "--quiet"]);
    args.extend(tools.iter().map(String::as_str));
    let out = mise_with(root, &args, overrides)?;
    if out.status.success() {
        return Ok(());
    }
    let failed = if crate::process::expired() { "was cut short by the deadline" } else { "failed" };
    let text = strip_escapes(&format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
    let refused = refusals(&text);
    if let Some(first) = refused.first() {
        let mut details: Vec<serde_json::Value> = refused.iter().map(|r| serde_json::to_value(r).expect("refusal serializes")).collect();
        details.push(serde_json::json!({ "output": tail(&out) }));
        return Err(StackError::new("artifact_mismatch", refusal_message(first)).hint(MISMATCH_HINT).details(details));
    }
    Err(StackError::new("install_failed", format!("mise {} {failed}", args.join(" ")))
        .details(vec![serde_json::json!({ "output": tail(&out) })]))
}

/// What to do about a refusal. mise.lock is rendered from stack.lock, so mise's own advice to
/// edit it is never the remedy.
pub const MISMATCH_HINT: &str = "verify the release upstream; if the change is expected, run `stack compile --update` and review the stack.lock diff";

/// `mise refused a download for jq@1.7.1 on macos-arm64: it does not match stack.lock`.
pub fn refusal_message(refusal: &Refusal) -> String {
    format!(
        "mise refused {} for {}{}: it does not match stack.lock",
        match refusal.kind { "checksum" => "a download", "signer" => "the signer", _ => "the signing repository" },
        refusal.name,
        refusal.platform.as_deref().map(|p| format!(" on {p}")).unwrap_or_default()
    )
}

/// mise's output without its advice to edit `mise.lock` (a `hint:` line, or a `; remove the
/// entry from mise.lock ...` clause), which stack renders from stack.lock and replaces on every
/// install. What mise observed is kept; a refusal also carries GitHub's digest note as `upstream`.
pub fn without_lock_advice(text: &str) -> String {
    let advice = |clause: &str| {
        let first = clause.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
        clause.contains("mise.lock") && matches!(first.as_str(), "remove" | "update" | "edit" | "delete" | "change")
    };
    text.lines()
        .filter(|line| !(line.trim_start().starts_with("hint:") && line.contains("mise.lock")))
        .map(|line| {
            let mut clauses = line.split("; ");
            let head = clauses.next().unwrap_or_default().to_string();
            clauses.filter(|c| !advice(c)).fold(head, |kept, c| format!("{kept}; {c}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One download or signer mise refused against the lock.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Refusal {
    /// `checksum`, `signer`, or `repository` (a packslip repository identity mise.lock pins).
    pub kind: &'static str,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What mise found upstream, as it said it: GitHub's current digest for the asset and when
    /// it was updated. Its advice to edit mise.lock is not kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

/// Checksum and signer refusals in mise's install output (mise 2026.10.3). A locked download
/// whose bytes differ is reported, through the error chain, as
///
/// ```text
/// mise ERROR Failed to install aqua:jqlang/jq@1.7.1: lockfile entry for jq@1.7.1 on macos-arm64 locks <url>: Checksum mismatch for file <path>:
/// Expected: sha256:...
/// Actual:   sha256:...
/// ```
///
/// conda says `checksum mismatch for <url>: expected <checksum>`; packslip says `mise.lock says
/// <locked> signed <tool>@<version>, but this release is signed by <signer>; ...`, and for a
/// repository whose identity changed `packslip:<tool>: this release was signed by <project> as
/// <kind> <actual>, but mise.lock pins <kind> <expected> for it`. Only refusals whose tool can be
/// named are returned; anything else stays `install_failed`.
pub fn refusals(text: &str) -> Vec<Refusal> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Refusal> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let installing = line.split("Failed to install ").nth(1).and_then(|r| r.split(": ").next()).map(|n| n.trim().to_string());
        let refusal = if line.contains("Checksum mismatch") {
            let (mut name, mut platform, mut url) = (None, None, None);
            if let Some((tool, rest)) = line.split("lockfile entry for ").nth(1).and_then(|r| r.split_once(" on ")) {
                name = Some(tool.trim().to_string());
                let (p, rest) = rest.split_once(" locks ").unwrap_or((rest.split(':').next().unwrap_or(rest), ""));
                platform = Some(p.split_whitespace().next().unwrap_or(p).to_string());
                url = rest.split(": ").next().map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
            }
            let field = |key: &str| lines[i..].iter().take(6).find_map(|l| l.trim().strip_prefix(key)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            // `hint: GitHub's current digest for <asset> ... matches this download (asset updated
            // <t>, release published <t>), so the expected checksum is out of date: ...`
            let upstream = field("hint:").filter(|h| h.contains("matches this download")).map(|h| {
                let observed = h.split(", so ").next().unwrap_or(&h);
                observed.trim_end_matches('.').to_string()
            });
            name.or(installing).map(|name| Refusal { kind: "checksum", name, platform, expected: field("Expected:"), actual: field("Actual:"), url, upstream })
        } else if let Some(rest) = line.split("checksum mismatch for ").nth(1) {
            let (url, expected) = rest.rsplit_once(": expected ").map_or((rest, None), |(u, e)| (u, Some(e.trim().to_string())));
            installing.map(|name| Refusal { kind: "checksum", name, platform: None, expected, actual: None, url: Some(url.trim().to_string()), upstream: None })
        } else if let Some(rest) = line.split("mise.lock says ").nth(1).filter(|r| r.contains(" but this release is signed by ")) {
            let (expected, rest) = rest.split_once(" signed ").unwrap_or((rest, ""));
            let (name, rest) = rest.split_once(", but this release is signed by ").unwrap_or((rest, ""));
            let actual = rest.split("; ").next().unwrap_or(rest);
            Some(Refusal { kind: "signer", name: name.trim().to_string(), platform: None, expected: Some(expected.trim().to_string()), actual: Some(actual.trim().to_string()), url: None, upstream: None })
        } else if line.contains("this release was signed by ") && line.contains(", but mise.lock pins ") {
            let name = line.split("packslip:").nth(1).and_then(|r| r.split(": ").next()).map(|n| format!("packslip:{}", n.trim())).or(installing);
            let actual = line.split(" as ").nth(1).and_then(|r| r.split(", but").next()).map(|a| a.trim().to_string());
            let expected = line.split(", but mise.lock pins ").nth(1).and_then(|r| r.split(" for it").next()).map(|e| e.trim().to_string());
            name.map(|name| Refusal { kind: "repository", name, platform: None, expected, actual, url: None, upstream: None })
        } else {
            None
        };
        if let Some(refusal) = refusal.filter(|r| !r.name.is_empty()) {
            if !out.contains(&refusal) {
                out.push(refusal);
            }
        }
    }
    out
}

/// `mise env --json` as mise printed it: the environment mise would give a command (tools on
/// PATH, env, service connection vars). Commands get it through [`provide`].
fn env_raw(root: &Path) -> Result<IndexMap<String, String>> {
    let out = checked(root, &["env", "--json"], "provider_failed")?;
    serde_json::from_slice(&out.stdout)
        .map_err(|e| StackError::new("provider_failed", format!("unexpected `mise env --json` output: {e}")))
}

/// What a command gets of mise's answer: without mise's own selection variables, with Stack's.
fn finish_env(root: &Path, mut env: IndexMap<String, String>) -> IndexMap<String, String> {
    env.retain(|key, _| !config_key(key));
    env.extend(config_env(root));
    env
}

/// The environment mise gives a command in `root` (see [`env_raw`] and [`finish_env`]) and
/// which of `pins` are not installed (as [`scratch::not_installed`]), from the answer Stack
/// kept when it is current (see [`super::env_cache`]); otherwise mise is asked both, at once,
/// and the answer kept when it can be. `state` is Stack's state directory, `cache` its cache
/// (for the scratch root).
pub fn provide(state: &Path, cache: &Path, root: &Path, pins: &[Pin]) -> (Result<IndexMap<String, String>>, Result<Vec<Pin>>) {
    let (env, missing) = provide_some(state, cache, root, pins, true, true);
    (env.expect("asked"), missing.expect("asked"))
}

/// [`provide`] when only the environment is needed: the installed releases are asked too only
/// when the answer could be kept.
pub fn provide_env(state: &Path, cache: &Path, root: &Path, pins: &[Pin]) -> Result<IndexMap<String, String>> {
    provide_some(state, cache, root, pins, true, false).0.expect("asked")
}

/// [`provide`] when only the missing pins are needed: the environment is asked too only when
/// the answer could be kept.
pub fn provide_missing(state: &Path, cache: &Path, root: &Path, pins: &[Pin]) -> Result<Vec<Pin>> {
    provide_some(state, cache, root, pins, false, true).1.expect("asked")
}

type Provided = (Option<Result<IndexMap<String, String>>>, Option<Result<Vec<Pin>>>);

fn provide_some(state: &Path, cache: &Path, root: &Path, pins: &[Pin], want_env: bool, want_missing: bool) -> Provided {
    let caller = super::env_cache::Caller::current();
    let pending = match super::env_cache::lookup(&caller, state, root, pins) {
        Ok(env) => return (Some(Ok(finish_env(root, env))), Some(Ok(Vec::new()))),
        Err(pending) => pending,
    };
    // Both answers are needed to keep one.
    let keep = pending.keepable();
    let (ask_env, ask_missing) = (want_env || keep, want_missing || keep);
    let (raw, installed) = std::thread::scope(|scope| {
        let bound = crate::process::inherit();
        let installed = ask_missing.then(|| {
            scope.spawn(move || {
                let _deadline = bound.enter();
                super::scratch::installed(cache, pins)
            })
        });
        let raw = ask_env.then(|| env_raw(root));
        (raw, installed.map(|h| h.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))))
    });
    if let (Some(Ok(env)), Some(Ok(found))) = (&raw, &installed) {
        if found.missing.is_empty() {
            super::env_cache::remember(&caller, state, root, pins, pending, env, &found.paths);
        }
    }
    (raw.map(|r| r.map(|env| finish_env(root, env))), installed.map(|r| r.map(|found| found.missing)))
}

/// Whether mise expands `$VAR` in `[env]` values after rendering their templates (its
/// `env_shell_expand` setting), as it reads its settings for `root`.
pub fn shell_expands(root: &Path) -> Result<bool> {
    let out = checked(root, &["settings", "get", "env_shell_expand"], "provider_failed")?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(StackError::new(
            "provider_failed",
            format!("unexpected `mise settings get env_shell_expand` output: {other:?}"),
        )),
    }
}

/// What Pitchfork reports about one qualified daemon id.
#[derive(Debug, Clone, PartialEq)]
pub enum Supervised {
    NotFound,
    Found { status: String, pid: Option<u32>, port: Option<u16> },
}

const SUPERVISOR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn pitchfork(bin: &Path, state_dir: &Path, args: &[&str]) -> std::result::Result<crate::process::Captured, String> {
    let mut command = Command::new(bin);
    command
        .args(args)
        .env("PITCHFORK_STATE_DIR", state_dir)
        .env("NO_COLOR", "1")
        .env_remove(OWNER_VAR);
    let out = crate::process::capture(&mut command, SUPERVISOR_TIMEOUT, 64 * 1024)
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if out.timed_out {
        return Err(format!("pitchfork {} did not answer within {}s", args.join(" "), SUPERVISOR_TIMEOUT.as_secs()));
    }
    Ok(out)
}

/// One daemon the supervisor tracks: its qualified id, status and process, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked {
    pub id: String,
    pub status: String,
    pub pid: Option<u32>,
}

/// Daemons the supervisor of `state_dir` tracks (`pitchfork list --json`). Pitchfork 2.29.0
/// starts a supervisor for this when none answers, in the command's process group, which
/// stack ends when the command does: ask only once [`supervisor_running`] says one runs.
pub fn supervisor_daemons(bin: &Path, state_dir: &Path) -> std::result::Result<Vec<Tracked>, String> {
    let out = pitchfork(bin, state_dir, &["list", "--json"])?;
    if out.exit_code != Some(0) {
        let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed");
        return Err(format!("pitchfork list: {err}"));
    }
    let list: Vec<serde_json::Value> = serde_json::from_str(&out.stdout).map_err(|e| format!("unexpected `pitchfork list --json` output: {e}"))?;
    Ok(list
        .iter()
        .map(|d| Tracked {
            id: d["id"].as_str().unwrap_or_default().to_string(),
            status: d["status"].as_str().unwrap_or_default().to_string(),
            pid: d["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
        })
        .collect())
}

/// Whether the supervisor of `state_dir` answers (`pitchfork supervisor status`, which never
/// starts one).
pub fn supervisor_running(bin: &Path, state_dir: &Path) -> std::result::Result<bool, String> {
    Ok(pitchfork(bin, state_dir, &["supervisor", "status"])?.exit_code == Some(0))
}

/// `pitchfork supervisor stop` for `state_dir`, then wait (up to 10 seconds) until it no longer
/// answers. Whether it stopped.
pub fn stop_supervisor(bin: &Path, state_dir: &Path) -> std::result::Result<bool, String> {
    let out = pitchfork(bin, state_dir, &["supervisor", "stop"])?;
    if out.exit_code != Some(0) {
        let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed");
        return Err(format!("pitchfork supervisor stop: {err}"));
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < until {
        if !supervisor_running(bin, state_dir)? {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(false)
}

/// `pitchfork status --json <id>` against the recorded supervisor state directory.
pub fn supervised(bin: &Path, state_dir: &Path, id: &str) -> std::result::Result<Supervised, String> {
    let Some(v) = status_json(bin, state_dir, id)? else { return Ok(Supervised::NotFound) };
    Ok(Supervised::Found {
        status: v["status"].as_str().unwrap_or_default().to_string(),
        pid: v["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
        port: v["active_port"].as_u64().and_then(|p| u16::try_from(p).ok()),
    })
}

/// `pitchfork stop -- <id>` against the recorded supervisor state directory. Pitchfork stops
/// whatever runs under the id when it receives the request; the caller checks first.
pub fn stop_supervised(bin: &Path, state_dir: &Path, id: &str) -> std::result::Result<(), String> {
    let out = pitchfork(bin, state_dir, &["stop", "--", id])?;
    if out.exit_code == Some(0) {
        return Ok(());
    }
    let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed");
    Err(format!("pitchfork stop {id}: {err}"))
}

/// `pitchfork status --json <id>`, which reads the supervisor's state and starts none: what it
/// says about `id`, or `None` when it knows no such daemon.
fn status_json(bin: &Path, state_dir: &Path, id: &str) -> std::result::Result<Option<serde_json::Value>, String> {
    let out = pitchfork(bin, state_dir, &["status", "--json", id])?;
    if out.exit_code != Some(0) {
        if out.stderr.contains("not found") {
            return Ok(None);
        }
        let err = out.stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed");
        return Err(format!("pitchfork status {id}: {err}"));
    }
    let v: serde_json::Value = serde_json::from_str(&out.stdout)
        .map_err(|e| format!("unexpected `pitchfork status --json` output: {e}"))?;
    if v["id"].as_str() != Some(id) {
        return Err(format!("pitchfork status {id} described '{}'", v["id"]));
    }
    Ok(Some(v))
}

/// The schedule on which the supervisor of `state_dir` starts daemon `id` (the `cron_schedule`
/// `pitchfork status --json` reports, which Pitchfork 2.29.0 keeps between runs and omits for a
/// daemon without one), if it has one. A daemon Pitchfork no longer knows, or an answer that
/// does not read as Pitchfork's, is an error: Stack cannot tell.
pub fn schedule(bin: &Path, state_dir: &Path, id: &str) -> std::result::Result<Option<String>, String> {
    let v = status_json(bin, state_dir, id)?.ok_or_else(|| format!("pitchfork status {id}: not found"))?;
    if !v["status"].is_string() {
        return Err(format!("pitchfork status {id} reported no status"));
    }
    match v.get("cron_schedule") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(cron)) if !cron.trim().is_empty() => Ok(Some(cron.clone())),
        Some(other) => Err(format!("pitchfork status {id} reported the schedule {other}")),
    }
}

const LOGS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const LOGS_LIMIT: usize = 1024 * 1024;

/// `pitchfork logs <id> -n <tail> --raw --no-pager` against the recorded supervisor: the output
/// Pitchfork stored for a service an older Stack started under it. Bounded by a deadline and an
/// output cap; never follows. `since` (Unix seconds) keeps only lines stored from that second.
pub fn legacy_logs(bin: &Path, state_dir: &Path, id: &str, tail: usize, since: Option<u64>) -> Result<crate::process::Captured> {
    let n = tail.to_string();
    let mut command = Command::new(bin);
    command.args(["logs", id, "-n", &n, "--raw", "--no-pager"]);
    if let Some(at) = since {
        command.args(["--since", &since_argument(at, crate::state::now())]);
    }
    command.env("PITCHFORK_STATE_DIR", state_dir).env("NO_COLOR", "1").env_remove(OWNER_VAR);
    let out = crate::process::capture(&mut command, LOGS_TIMEOUT, LOGS_LIMIT)
        .map_err(|e| StackError::new("logs_failed", format!("cannot run {}: {e}", bin.display())))?;
    if out.timed_out {
        return Err(StackError::new("logs_failed", format!("pitchfork logs did not finish within {}s", LOGS_TIMEOUT.as_secs())));
    }
    if out.exit_code != Some(0) {
        let detail = out.stderr.lines().chain(out.stdout.lines()).map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("failed").to_string();
        return Err(StackError::new("logs_failed", format!("pitchfork logs {id} failed"))
            .hint("`stack up` moves the service to Stack's own supervisor, whose logs `stack logs` reads")
            .details(vec![serde_json::json!({ "output": detail })]));
    }
    Ok(out)
}

/// Pitchfork's `--since` for the instant `at`: local wall-clock time, which it reads as an
/// exact instant. A wall-clock time that names two instants (repeated when clocks go back, by
/// any amount up to three hours in 15-minute steps, which covers every zone in use) or that
/// cannot be formatted falls back to a relative age, rounded up so the start second is kept.
fn since_argument(at: u64, now: u64) -> String {
    since_argument_in(at, now, local_datetime)
}

fn since_argument_in(at: u64, now: u64, local_datetime: impl Fn(u64) -> Option<String>) -> String {
    let local = local_datetime(at);
    let ambiguous = local.is_none()
        || (1..=12u64).map(|k| k * 900).any(|shift| {
            local == local_datetime(at + shift) || local == at.checked_sub(shift).and_then(&local_datetime)
        });
    match local {
        Some(local) if !ambiguous => local,
        _ => format!("{}s", now.saturating_sub(at) + 1),
    }
}

/// `YYYY-MM-DD HH:MM:SS` in local time, the form Pitchfork's `--since` takes for an instant.
#[cfg(unix)]
fn local_datetime(secs: u64) -> Option<String> {
    let t = libc::time_t::try_from(secs).ok()?;
    // SAFETY: localtime_r writes only the provided struct.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

#[cfg(not(unix))]
fn local_datetime(_: u64) -> Option<String> {
    None
}

/// Set by Stack 0.1.23 and earlier, to the Stack state's owner mark, on every request that
/// could start Pitchfork's supervisor; the supervisor inherited it. `down` stops a supervisor
/// an older Stack started only when its environment carries this state's mark.
pub const OWNER_VAR: &str = "STACK_SUPERVISOR_OWNER";

#[cfg(test)]
mod tests {
    use super::*;

    /// What `mise daemons start` printed for a service that failed on a bad edit, captured
    /// with NO_COLOR and CI set (DX evaluation of 0.1.18, log 130), with a shorter path.
    const FAILED_START: &str = "\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api] Traceback (most recent call last):\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api]   File \"/p/server.py\", line 2, in <module>\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api]     raise RuntimeError(\"bad edit\")\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{2022} [app-9df2/api] RuntimeError: bad edit\n\u{280b} [app-9df2/api] waiting for delay (3s)...\n\r\u{280b} [app-9df2/api] waiting for delay (3s)...\n\rpitchfork ERROR Daemon app-9df2/api failed to start\n\u{2717} [app-9df2/api] failed (exit code 1): Daemon app-9df2/api failed with exit code 1";

    #[test]
    fn an_inherited_binding_or_owner_mark_reaches_no_provider_command() {
        let mut command = Command::new("mise");
        command.env(BINDING_VAR, "/elsewhere.json").env(OWNER_VAR, "someone");
        configure_command(&mut command, Path::new("/p/app"));
        let removed: Vec<_> = command.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        assert!(removed.contains(&BINDING_VAR.to_string()) && removed.contains(&OWNER_VAR.to_string()), "{removed:?}");
    }

    #[test]
    fn captured_spinner_frames_leave_the_traceback_and_the_final_error() {
        assert_eq!(
            readable_tail(FAILED_START, TAIL_LINES),
            "\u{2022} [app-9df2/api] Traceback (most recent call last):
\u{2022} [app-9df2/api]   File \"/p/server.py\", line 2, in <module>
\u{2022} [app-9df2/api]     raise RuntimeError(\"bad edit\")
\u{2022} [app-9df2/api] RuntimeError: bad edit
pitchfork ERROR Daemon app-9df2/api failed to start
\u{2717} [app-9df2/api] failed (exit code 1): Daemon app-9df2/api failed with exit code 1"
        );
    }

    #[test]
    fn redraws_keep_what_the_terminal_finally_showed() {
        assert_eq!(readable_tail("Downloading 10%\rDownloading 100%\r\nError: disk full\r\n", 5), "Downloading 100%\nError: disk full");
        assert_eq!(readable_tail("\u{280b} starting\r\u{2819} starting\r\u{2839} starting\n", 5), "\u{2839} starting");
        assert_eq!(readable_tail("", 5), "");
    }

    #[test]
    fn escape_sequences_and_controls_are_removed_but_text_is_kept() {
        let text = "\u{1b}[31mError:\u{1b}[0m port \u{1b}]8;;https://x/\u{7}5432\u{1b}]8;;\u{7} in use\u{1b}[2K\u{8}\n\u{1b}]0;title\u{1b}\\\tdétail ✓\u{1b}7";
        assert_eq!(readable_tail(text, 5), "Error: port 5432 in use\n\tdétail ✓");
    }

    #[test]
    fn only_the_last_lines_are_kept_with_the_error_last() {
        let text: String = (1..=30).map(|n| format!("\u{280b} wait\nline {n}\n")).collect::<String>() + "Error: final";
        let tail = readable_tail(&text, TAIL_LINES);
        assert_eq!(tail.lines().count(), TAIL_LINES);
        assert!(tail.starts_with("line 12\n") && tail.ends_with("line 30\nError: final"), "{tail}");
    }

    fn output(stdout: &str, stderr: &str) -> Output {
        use std::os::unix::process::ExitStatusExt;
        Output { status: std::process::ExitStatus::from_raw(256), stdout: stdout.into(), stderr: stderr.into() }
    }

    #[test]
    fn a_failed_start_keeps_the_failing_service_and_the_supervisor_and_drops_others_chatter() {
        // Healthy services stream their startup while another one fails.
        let chatter: String = (1..=15).map(|n| format!("\u{2022} [app-9df2/redis] startup line {n}\n")).collect();
        let tail = tail(&output(&format!("{chatter}{FAILED_START}"), ""));
        assert_eq!(
            tail,
            "[redis] startup line 14
[redis] startup line 15
[api] Traceback (most recent call last):
[api]   File \"/p/server.py\", line 2, in <module>
[api]     raise RuntimeError(\"bad edit\")
[api] RuntimeError: bad edit
pitchfork ERROR Daemon app-9df2/api failed to start
\u{2717} [app-9df2/api] failed (exit code 1): Daemon app-9df2/api failed with exit code 1"
        );
    }

    #[test]
    fn a_start_cut_short_keeps_what_each_service_said_last() {
        // `mise daemons start` cut short by the deadline in the field (0.1.21): every line was
        // a healthy service's.
        let mut text = String::new();
        for n in 1..=18 {
            text.push_str(&format!("\u{2022} [appA-3f6a77c577bb049e/redis] 85472:M 14 Oct 2026 09:15:34.733 * redis line {n}\n"));
        }
        text.push_str("\u{2022} [appA-3f6a77c577bb049e/postgres] LOG:  checkpoint starting: time\n");
        text.push_str("\u{2022} [appA-3f6a77c577bb049e/postgres] LOG:  checkpoint complete\n");
        let tail = tail(&output(&text, ""));
        assert_eq!(tail.lines().count(), 4, "{tail}");
        assert!(tail.starts_with("[redis] 85472:M") && tail.contains("redis line 18") && tail.ends_with("[postgres] LOG:  checkpoint complete"), "{tail}");
    }

    #[test]
    fn summarizing_a_long_start_takes_linear_time() {
        let text: String = (0..200_000).map(|n| format!("\u{2022} [app-1/s{}] line {n}\n", n % 50)).collect();
        let started = std::time::Instant::now();
        let kept = tail(&output(&text, "pitchfork ERROR Daemon app-1/s7 failed to start\n"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(kept.ends_with("pitchfork ERROR Daemon app-1/s7 failed to start") && kept.contains("[s7] line 199957"), "{kept}");
    }

    #[test]
    fn a_tail_is_bounded_in_bytes_whatever_its_lines() {
        let long = "x".repeat(100_000);
        let text: String = (0..TAIL_LINES).map(|n| format!("Error {n}: {long}\n")).collect();
        let kept = tail(&output("", &text));
        assert!(kept.len() <= TAIL_BYTES + 200, "{} bytes", kept.len());
        assert!(kept.starts_with("(") && kept.contains("earlier lines omitted"), "{}", &kept[..200]);
        assert!(kept.ends_with("x…") && kept.lines().last().unwrap().starts_with(&format!("Error {}: ", TAIL_LINES - 1)), "the last line is kept");
        // Multibyte characters are never split.
        assert!(tail(&output(&"é".repeat(400), "")).ends_with("é…"));
    }

    #[test]
    fn provider_releases_are_read_from_the_first_stdout_line_only() {
        assert_eq!(CalVer::parse("2026.10.3 macos-arm64 (2026-10-05)\n"), Some(CalVer(2026, 10, 3)));
        assert_eq!(CalVer::parse("\nv2026.9.2 linux-x64\n"), Some(CalVer(2026, 9, 2)));
        for bad in ["", "mise WARN  mise version 2026.10.4 available", "2026.10", "2026.10.3.1 x", "latest"] {
            assert_eq!(CalVer::parse(bad), None, "{bad:?}");
        }
        assert!(CalVer(2026, 10, 0) > CalVer(2026, 9, 2) && CalVer(2026, 9, 1) < MR_BOXINGTON_MISE);
    }

    #[test]
    fn requirements_fail_provider_outdated_naming_what_needs_them() {
        let need = |minimum, reason: &str| Requirement { minimum, reason: reason.into() };
        let reqs = [need(MR_BOXINGTON_MISE, "tools.rust (project) sets mr_boxington"), need(CalVer(2026, 9, 16), "the lock")];
        assert!(check_requirements(CalVer(2026, 9, 16), &reqs).is_ok());
        let e = check_requirements(CalVer(2026, 9, 2), &reqs).unwrap_err();
        assert_eq!(e.code, "provider_outdated");
        assert_eq!(e.details.len(), 1, "{e:?}");
        let e = check_requirements(CalVer(2026, 8, 30), &reqs).unwrap_err();
        assert_eq!(e.message, "mise 2026.8.30 is older than 2026.9.2, which tools.rust (project) sets mr_boxington needs");
        assert!(e.hint.unwrap().contains("2026.9.16 or newer"));
        assert_eq!(e.details[0]["actual"], "2026.8.30");
        assert!(require(Path::new("/nonexistent"), &[]).unwrap().is_none(), "nothing to check runs nothing");
    }

    #[test]
    fn resolved_versions_are_one_exact_line() {
        assert_eq!(parse_resolved("3.13.16\n").as_deref(), Some("3.13.16"));
        assert_eq!(parse_resolved("\n  17.11  \n").as_deref(), Some("17.11"));
        for bad in ["", "\n", "latest\n", "3.13.1\n3.13.2\n", "a b\n"] {
            assert_eq!(parse_resolved(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn log_cutoffs_avoid_wall_clock_times_that_name_two_instants() {
        // A zone whose clocks go back an hour at 7200: 3600..7200 and 7200..10800 read alike.
        let wall = |t: u64| Some(format!("wall {}", if (7200..10800).contains(&t) { t - 3600 } else { t }));
        assert_eq!(since_argument_in(1000, 2000, wall), "wall 1000");
        assert_eq!(since_argument_in(5000, 9000, wall), "4001s", "first pass through the repeated hour");
        assert_eq!(since_argument_in(8000, 9000, wall), "1001s", "second pass");
        assert_eq!(since_argument_in(20_000, 20_005, wall), "wall 20000");
        assert_eq!(since_argument_in(5, 10, |_| None), "6s");
        // A 30-minute rollback at 7200 (as on Lord Howe Island): both passes are ambiguous.
        let half = |t: u64| Some(format!("wall {}", if (7200..9000).contains(&t) { t - 1800 } else { t }));
        assert_eq!(since_argument_in(6000, 9500, half), "3501s");
        assert_eq!(since_argument_in(7800, 9500, half), "1701s");
        assert_eq!(since_argument_in(5000, 9500, half), "wall 5000");
        assert!(local_datetime(1_791_404_602).is_some_and(|t| t.len() == 19));
    }

    /// `mise install --locked jq` with a tampered checksum, mise 2026.10.3 (temp paths shortened).
    const CHECKSUM_REFUSAL: &str = "mise by @jdx \u{2013} installing 1 tool
mise \u{2717} jq@1.7.1  467ms \u{b7} failed: lockfile entry for jq@1.7.1 on macos-arm64 locks https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64
mise ERROR Failed to install aqua:jqlang/jq@1.7.1: lockfile entry for jq@1.7.1 on macos-arm64 locks https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64: Checksum mismatch for file /tmp/d/downloads/jq/1.7.1/jq-macos-arm64:
Expected: sha256:0000000000000000000000000000000000000000000000000000000000000000
Actual:   sha256:0bbe619e663e0de2c550be2fe0d240d076799d6f8a652b70fa04aea8a8362e8a
hint: GitHub's current digest for jq-macos-arm64 in jqlang/jq jq-1.7.1 matches this download (asset updated 2023-12-13T19:19:09Z, release published 2023-12-13T19:22:49Z), so the expected checksum is out of date: the maintainer likely re-uploaded the asset. If you trust the new upload, update the checksum in mise.lock.
mise ERROR Version: 2026.10.3 macos-arm64 (2026-10-05)";

    #[test]
    fn checksum_refusals_are_parsed_from_real_output() {
        let r = refusals(CHECKSUM_REFUSAL);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0], Refusal {
            kind: "checksum",
            name: "jq@1.7.1".into(),
            platform: Some("macos-arm64".into()),
            expected: Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".into()),
            actual: Some("sha256:0bbe619e663e0de2c550be2fe0d240d076799d6f8a652b70fa04aea8a8362e8a".into()),
            url: Some("https://github.com/jqlang/jq/releases/download/jq-1.7.1/jq-macos-arm64".into()),
            upstream: Some("GitHub's current digest for jq-macos-arm64 in jqlang/jq jq-1.7.1 matches this download (asset updated 2023-12-13T19:19:09Z, release published 2023-12-13T19:22:49Z)".into()),
        });
    }

    #[test]
    fn mise_advice_to_edit_its_lock_is_dropped_and_what_it_observed_is_kept() {
        let kept = without_lock_advice(CHECKSUM_REFUSAL);
        assert!(!kept.contains("mise.lock") && !kept.contains("hint:"), "{kept}");
        for line in ["Expected: sha256:0000", "Actual:   sha256:0bbe", "Checksum mismatch for file", "mise ERROR Version"] {
            assert!(kept.contains(line), "{line} in {kept}");
        }
        let signer = "mise ERROR Failed to install packslip:github.com/jdx/fnox@1.39.0: mise.lock says a signed fnox@1.39.0, but this release is signed by b; remove the entry from mise.lock to accept the new signer";
        assert_eq!(without_lock_advice(signer), "mise ERROR Failed to install packslip:github.com/jdx/fnox@1.39.0: mise.lock says a signed fnox@1.39.0, but this release is signed by b");
        let plain = "hint: try again later; check the network";
        assert_eq!(without_lock_advice(plain), plain, "other hints are mise's to give");
    }

    /// Formats from mise 2026.10.3's source (`backend/conda.rs`, `backend/packslip.rs`,
    /// `packslip_forge.rs`); not produced by a run here.
    #[test]
    fn conda_signer_and_repository_refusals_are_parsed_from_their_source_formats() {
        let conda = "mise ERROR Failed to install conda:postgresql@17.6: checksum mismatch for https://conda.anaconda.org/x/postgresql.conda: expected sha256:abc";
        let r = refusals(conda);
        assert_eq!((r[0].kind, r[0].name.as_str(), r[0].expected.as_deref(), r[0].url.as_deref()), ("checksum", "conda:postgresql@17.6", Some("sha256:abc"), Some("https://conda.anaconda.org/x/postgresql.conda")));
        let signer = "mise ERROR Failed to install packslip:github.com/jdx/fnox@1.39.0: mise.lock says sigstore-oidc:https://github.com/jdx/fnox/.github/workflows/release.yml signed fnox@1.39.0, but this release is signed by sigstore-oidc:https://github.com/evil/fnox/.github/workflows/release.yml; remove the entry from mise.lock to accept the new signer";
        let r = refusals(signer);
        assert_eq!(r[0].kind, "signer");
        assert_eq!(r[0].name, "fnox@1.39.0");
        assert_eq!(r[0].expected.as_deref(), Some("sigstore-oidc:https://github.com/jdx/fnox/.github/workflows/release.yml"));
        assert_eq!(r[0].actual.as_deref(), Some("sigstore-oidc:https://github.com/evil/fnox/.github/workflows/release.yml"));
        let repo = "mise ERROR packslip:github.com/jdx/fnox: this release was signed by jdx/fnox as GitHub repository ID 999, but mise.lock pins GitHub repository ID 1078762196 for it. The name now belongs to a different repository";
        let r = refusals(repo);
        assert_eq!((r[0].kind, r[0].name.as_str()), ("repository", "packslip:github.com/jdx/fnox"));
        assert_eq!((r[0].actual.as_deref(), r[0].expected.as_deref()), (Some("GitHub repository ID 999"), Some("GitHub repository ID 1078762196")));
        assert!(refusals("mise ERROR Failed to install jq@1.7.1: network unreachable").is_empty());
        assert!(refusals("Checksum mismatch for file /x:").is_empty(), "no tool can be named");
    }

    #[test]
    fn unversioned_requests_are_recognised() {
        for r in ["system", "path:/opt/x", "ref:main"] {
            assert!(unversioned(r), "{r}");
        }
        for r in ["3.13", "latest", "lts", "17", "prefix:3", "sub-1:latest"] {
            assert!(!unversioned(r), "{r}");
        }
    }
}

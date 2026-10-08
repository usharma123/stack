//! Agent skills the stack's pinned tools ship, at exactly the releases stack.lock pins.
//!
//! Discovery asks mise, in a scratch provider root of its own whose only configuration is the
//! lock's pins (`[tools]`, nothing else), which releases are installed (`mise ls --json`) and
//! which skills the active releases declare (`mise skills ls --json`). Rows for tools or
//! versions stack.lock does not pin are ignored. Nothing is installed and nothing is written
//! into the project; the scratch root is removed afterwards. When the provider cannot answer,
//! every entry is `unavailable` and a `skills_unavailable` warning says why: discovery never
//! fails the command that asked.
//!
//! Skills of tools stack adds for its provider (Pitchfork) are never listed by default, never
//! returned and never linked: Pitchfork's skill teaches an agent to drive daemons directly,
//! around stack's ownership and identity checks.
//!
//! Sync (`[skills] dir`, project only) links each available skill into a directory inside the
//! project and records what it linked in `<dir>/.stack-skills.json`. Only a symbolic link whose
//! current target is the one recorded is ever replaced or removed; anything else is kept.

use crate::error::{Result, StackError};
use crate::lock::Lockfile;
use crate::project::VersionReport;
use crate::provider::scratch::{self, Pin, ScratchRoot};
use crate::tool::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Each provider query gets this long (less when the command's deadline is nearer).
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// `mise ls --json` lists every installed release on the machine; more than this is not read.
const LIST_LIMIT: usize = 4 * 1024 * 1024;
/// The most `stack_skill` returns: larger entrypoints are `skill_too_large`.
pub const TEXT_LIMIT: u64 = 64 * 1024;
/// Where sync records the links it made, inside the skills directory.
pub const REGISTRY: &str = ".stack-skills.json";
const REGISTRY_LIMIT: u64 = 1024 * 1024;
const ENTRYPOINT: &str = "SKILL.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The pinned release is installed and declares this skill.
    Available,
    /// The pinned release is installed and declares no usable skill.
    NoSkill,
    /// The pinned release is not installed here (`stack install` installs it).
    NotInstalled,
    /// Stack could not find out: no pin yet, or the provider did not answer.
    Unavailable,
}

/// One skill, or the absence of one, for one pinned release.
#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    /// The provider tool (a service's preset tool for service pins).
    pub tool: String,
    /// The exact release stack.lock pins; `None` when it pins none.
    pub version: Option<String>,
    /// `project`, `bundle:<name>`, `override`, or `provider` for tools stack adds itself.
    pub origin: String,
    /// Services whose preset installs this release.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The skill directory, as mise reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    /// `<directory>/SKILL.md`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The release directory mise reports for the pin, which the skill must lie inside.
    #[serde(skip)]
    install_path: Option<PathBuf>,
}

impl Skill {
    fn provider(&self) -> bool {
        self.origin == "provider"
    }
}

/// What discovery found. Provider skills are kept apart and only listed on request.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub skills: Vec<Skill>,
    pub provider_skills: Vec<Skill>,
    /// `skills_unavailable: ...` when entries are unavailable.
    pub warnings: Vec<String>,
}

/// A pinned release and every report entry that names it.
struct Target {
    pin: Option<Pin>,
    tool: String,
    version: Option<String>,
    origin: String,
    services: Vec<String>,
    reason: Option<String>,
    /// No pin yet (as opposed to a request that names no release).
    unlocked: bool,
}

/// Every tool and preset service of the report, matched to the pin stack.lock records for it.
/// Two entries naming one release (a tool and a service of its preset) are one target, which
/// counts as the provider's only when every entry is.
fn targets(lock: &Lockfile, versions: &[VersionReport]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    for v in versions {
        let tool = v.tool.clone().unwrap_or_else(|| v.name.clone());
        let locked = if v.kind == "service" { lock.service(&v.name) } else { lock.tool(&v.name) };
        let locked = locked.filter(|l| l.requested == v.requested && l.options == v.options);
        // mise renders versions and option strings as templates when it loads the scratch
        // config; a pin that carries one is never written there.
        let templated = locked.is_some_and(|l| {
            crate::tool::templated(&l.resolved)
                || l.options.values().any(|o| matches!(o, crate::tool::OptionValue::String(s) if crate::tool::templated(s)))
        });
        let pin = locked
            .filter(|l| !templated && !crate::provider::mise::unversioned(&l.resolved))
            .map(|l| Pin { tool: tool.clone(), spec: ToolSpec { version: l.resolved.clone(), options: l.options.clone() } });
        let service = (v.kind == "service").then(|| v.name.clone());
        if let Some(existing) = out.iter_mut().find(|t| t.pin.is_some() && t.pin == pin) {
            existing.services.extend(service);
            if existing.origin == "provider" && v.origin != "provider" {
                existing.origin = v.origin.clone();
            }
            continue;
        }
        let reason = match (&pin, locked) {
            (Some(_), _) => None,
            (None, Some(_)) if templated => Some("its stack.lock entry contains template syntax, which mise would evaluate".to_string()),
            (None, Some(l)) => Some(format!("`{}` names no release, so no skill can be matched to one", l.resolved)),
            (None, None) => Some("stack.lock does not pin a release yet; run `stack compile`".to_string()),
        };
        out.push(Target {
            unlocked: pin.is_none() && locked.is_none(),
            version: pin.as_ref().map(|p| p.spec.version.clone()),
            pin,
            tool,
            origin: v.origin.clone(),
            services: service.into_iter().collect(),
            reason,
        });
    }
    out
}

/// Skills of every release stack.lock pins for `versions` (a compile or inspect report).
/// Never fails: provider trouble makes entries `unavailable` and adds a warning.
pub fn discover(cache: &Path, lock: &Lockfile, versions: &[VersionReport]) -> Discovery {
    discover_with(lock, versions, |pins| query(cache, pins))
}

fn discover_with(
    lock: &Lockfile,
    versions: &[VersionReport],
    query: impl FnOnce(&[Pin]) -> std::result::Result<Answer, String>,
) -> Discovery {
    let targets = targets(lock, versions);
    let pins: Vec<Pin> = scratch::pins(lock)
        .into_iter()
        .filter(|p| targets.iter().any(|t| t.pin.as_ref() == Some(p)))
        .collect();
    let mut warnings = Vec::new();
    let unpinned = targets.iter().filter(|t| t.unlocked).count();
    if unpinned > 0 {
        warnings.push(format!(
            "skills_unavailable: stack.lock pins no release for {unpinned} tool(s) yet, so their skills cannot be matched; run `stack compile`"
        ));
    }
    // Nothing pinned, nothing to ask: an unpinned request is never looked up as whatever
    // release happens to be active.
    let answer = if pins.is_empty() { Ok(Answer::default()) } else { query(&pins) };
    let answer = match answer {
        Ok(a) => Some(a),
        Err(why) => {
            warnings.push(format!("skills_unavailable: {why}; every skill is listed as unavailable"));
            None
        }
    };
    let mut skills = Vec::new();
    for t in targets {
        let base = Skill {
            tool: t.tool.clone(),
            version: t.version.clone(),
            origin: t.origin.clone(),
            services: t.services.clone(),
            status: Status::Unavailable,
            name: None,
            directory: None,
            entrypoint: None,
            reason: t.reason.clone(),
            install_path: None,
        };
        let (Some(pin), Some(answer)) = (&t.pin, &answer) else {
            if t.pin.is_some() {
                skills.push(Skill { reason: Some("the provider could not list skills".into()), ..base });
            } else {
                skills.push(base);
            }
            continue;
        };
        let Some(install) = answer.installed(&pin.tool, &pin.spec.version) else {
            skills.push(Skill { status: Status::NotInstalled, reason: Some("the pinned release is not installed; run `stack install`".into()), ..base });
            continue;
        };
        let rows: Vec<&Row> = answer.skills.iter().filter(|r| r.tool == pin.tool && r.version == pin.spec.version).collect();
        if rows.is_empty() {
            skills.push(Skill { status: Status::NoSkill, install_path: Some(install.clone()), ..base });
            continue;
        }
        for row in rows {
            let mut skill = Skill {
                name: Some(row.name.clone()),
                directory: Some(row.path.clone()),
                entrypoint: Some(row.path.join(ENTRYPOINT)),
                install_path: Some(install.clone()),
                ..base.clone()
            };
            match contained(install, &row.name, &row.path) {
                Ok(_) => skill.status = Status::Available,
                Err(why) => {
                    skill.status = Status::NoSkill;
                    skill.reason = Some(why);
                }
            }
            skills.push(skill);
        }
    }
    let (provider_skills, skills) = skills.into_iter().partition(Skill::provider);
    Discovery { skills, provider_skills, warnings }
}

/// A skill name stack accepts: it becomes a path component when linked.
pub fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The skill's entrypoint, resolved, if the skill directory and its `SKILL.md` lie inside the
/// release directory mise reports for the pin (after resolving links, so a link cannot lead
/// elsewhere) and the entrypoint is a regular file.
fn contained(install: &Path, name: &str, directory: &Path) -> std::result::Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("mise listed a skill named {name:?}; stack accepts only names matching [a-z0-9][a-z0-9_-]*"));
    }
    if !directory.is_absolute() {
        return Err("mise reported a relative skill directory".into());
    }
    let release = install.canonicalize().map_err(|e| format!("the release directory cannot be read: {e}"))?;
    let dir = directory.canonicalize().map_err(|e| format!("the skill directory cannot be read: {e}"))?;
    if !dir.starts_with(&release) || !dir.is_dir() {
        return Err("the skill directory is not inside the pinned release's install directory".into());
    }
    let entry = dir.join(ENTRYPOINT).canonicalize().map_err(|_| format!("the skill directory has no {ENTRYPOINT}"))?;
    if !entry.starts_with(&dir) {
        return Err(format!("{ENTRYPOINT} links outside the skill directory"));
    }
    if !entry.is_file() {
        return Err(format!("{ENTRYPOINT} is not a regular file"));
    }
    Ok(entry)
}

// ---- provider answers ----------------------------------------------------------------------

/// One row of `mise skills ls --json`.
#[derive(Debug, Clone, Deserialize)]
struct Row {
    name: String,
    tool: String,
    version: String,
    path: PathBuf,
}

/// One release row of `mise ls --json`.
#[derive(Debug, Clone, Deserialize)]
struct Release {
    version: String,
    #[serde(default)]
    install_path: Option<PathBuf>,
    #[serde(default)]
    installed: bool,
}

#[derive(Debug, Default)]
struct Answer {
    releases: BTreeMap<String, Vec<Release>>,
    skills: Vec<Row>,
}

impl Answer {
    /// The install directory of exactly `tool@version`, when mise says it is installed.
    fn installed(&self, tool: &str, version: &str) -> Option<&PathBuf> {
        self.releases
            .get(tool)?
            .iter()
            .find(|r| r.version == version && r.installed)
            .and_then(|r| r.install_path.as_ref())
            .filter(|p| p.is_absolute())
    }

    fn parse(ls: &str, skills: &str) -> std::result::Result<Self, String> {
        let releases = serde_json::from_str(ls).map_err(|_| "`mise ls --json` printed something stack cannot read".to_string())?;
        let skills = serde_json::from_str(skills).map_err(|_| "`mise skills ls --json` printed something stack cannot read".to_string())?;
        Ok(Self { releases, skills })
    }
}

/// `mise ls --json` and `mise skills ls --json` in one scratch root naming `pins`, at once,
/// each bounded by [`QUERY_TIMEOUT`] and the caller's deadline.
fn query(cache: &Path, pins: &[Pin]) -> std::result::Result<Answer, String> {
    let root = ScratchRoot::create(cache, "skills").map_err(|e| format!("cannot create a scratch provider root: {}", e.message))?;
    root.write_tools(pins).map_err(|e| format!("cannot write the scratch provider config: {}", e.message))?;
    let deadline = crate::process::deadline();
    let run = |args: &[&str], what: &str| -> std::result::Result<String, String> {
        let _deadline = crate::process::deadline_scope(deadline);
        let mut command = root.command(args);
        let out = crate::process::capture(&mut command, QUERY_TIMEOUT, LIST_LIMIT).map_err(|e| format!("cannot run mise: {e}"))?;
        if out.timed_out {
            return Err(format!("`{what}` did not answer within {}s", crate::process::bounded(QUERY_TIMEOUT).as_secs().max(1)));
        }
        if out.stdout_truncated {
            return Err(format!("`{what}` printed more than {} bytes", LIST_LIMIT));
        }
        match out.exit_code {
            Some(0) => Ok(out.stdout),
            // Provider text is not forwarded: only what stack can say about the call.
            Some(code) => Err(format!("`{what}` failed (exit {code}); a mise without the `skills` command cannot list them")),
            None => Err(format!("`{what}` was ended by a signal")),
        }
    };
    let (ls, skills) = std::thread::scope(|scope| {
        let skills = scope.spawn(|| run(&["skills", "ls", "--json"], "mise skills ls --json"));
        let ls = run(&["ls", "--json"], "mise ls --json");
        (ls, skills.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
    });
    Answer::parse(&ls?, &skills?)
}

// ---- retrieval -----------------------------------------------------------------------------

/// The text of one skill, as `stack_skill` returns it.
#[derive(Debug, Serialize)]
pub struct SkillText {
    pub tool: String,
    pub version: String,
    pub name: String,
    pub entrypoint: PathBuf,
    pub bytes: u64,
    /// The tool's own documentation at the pinned release. Stack neither runs nor interprets it.
    pub text: String,
}

/// The `SKILL.md` of an available, non-provider skill that discovery enumerated.
pub fn read(discovery: &Discovery, tool: &str, name: &str) -> Result<SkillText> {
    let not_found = |why: String| {
        StackError::new("skill_not_found", format!("no skill {name:?} of tool {tool:?}: {why}"))
            .hint("`stack_inspect` lists the stack's skills under `skills`; only `available` ones can be read")
            .with_detail(json!({ "tool": tool, "name": name }))
    };
    if discovery.provider_skills.iter().any(|s| s.tool == tool) {
        return Err(not_found("it belongs to a tool stack adds for its provider, whose skills stack never returns".into()));
    }
    let named: Vec<&Skill> = discovery.skills.iter().filter(|s| s.tool == tool).collect();
    if named.is_empty() {
        return Err(not_found("the stack pins no such tool".into()));
    }
    let Some(skill) = named.iter().find(|s| s.name.as_deref() == Some(name)) else {
        let why = named.iter().find_map(|s| s.reason.clone()).unwrap_or_else(|| match named[0].status {
            Status::NotInstalled => "the pinned release is not installed".into(),
            Status::Unavailable => "skills could not be listed".into(),
            _ => "the pinned release declares no skill by that name".into(),
        });
        return Err(not_found(why));
    };
    if skill.status != Status::Available {
        return Err(not_found(skill.reason.clone().unwrap_or_else(|| "it is not available".into())));
    }
    let (Some(install), Some(directory), Some(version)) = (&skill.install_path, &skill.directory, &skill.version) else {
        return Err(not_found("it is not available".into()));
    };
    let unreadable = |why: String| {
        StackError::new("skill_unreadable", format!("cannot read skill {name:?} of {tool}@{version}: {why}"))
            .with_detail(json!({ "tool": tool, "name": name, "version": version }))
    };
    // Checked again now, against links that changed since discovery.
    let entry = contained(install, name, directory).map_err(unreadable)?;
    let mut file = open_nofollow(&entry).map_err(|e| unreadable(e.to_string()))?;
    let meta = file.metadata().map_err(|e| unreadable(e.to_string()))?;
    if !meta.is_file() {
        return Err(unreadable(format!("{ENTRYPOINT} is not a regular file")));
    }
    let too_large = |bytes: u64| {
        StackError::new("skill_too_large", format!("skill {name:?} of {tool}@{version} is {bytes} bytes; stack returns at most {TEXT_LIMIT}"))
            .hint(format!("read {} directly", skill.entrypoint.as_ref().unwrap_or(&entry).display()))
            .with_detail(json!({ "tool": tool, "name": name, "version": version, "bytes": bytes, "limit": TEXT_LIMIT }))
    };
    if meta.len() > TEXT_LIMIT {
        return Err(too_large(meta.len()));
    }
    let mut bytes = Vec::new();
    file.by_ref().take(TEXT_LIMIT + 1).read_to_end(&mut bytes).map_err(|e| unreadable(e.to_string()))?;
    if bytes.len() as u64 > TEXT_LIMIT {
        return Err(too_large(bytes.len() as u64));
    }
    let text = String::from_utf8(bytes).map_err(|_| unreadable(format!("{ENTRYPOINT} is not UTF-8 text")))?;
    Ok(SkillText {
        tool: tool.into(),
        version: version.clone(),
        name: name.into(),
        entrypoint: skill.entrypoint.clone().unwrap_or(entry),
        bytes: text.len() as u64,
        text,
    })
}

/// Open a file whose last component must not be a symbolic link.
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

// ---- sync ----------------------------------------------------------------------------------

/// `[skills] dir` as written: relative, inside the project, and not stack's own or git's
/// directory. Links in the existing path are checked when syncing.
pub fn validate_dir(dir: &str) -> Result<PathBuf> {
    let invalid = |why: &str| {
        StackError::new("invalid_path", format!("skills.dir {dir:?} {why}"))
            .hint("use a directory relative to the project root, such as \".claude/skills\"")
            .with_detail(json!({ "key": "skills.dir", "value": dir }))
    };
    let path = Path::new(dir);
    if dir.trim().is_empty() {
        return Err(invalid("is empty"));
    }
    if path.is_absolute() || dir.starts_with('/') || dir.starts_with('\\') {
        return Err(invalid("must be relative to the project root"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            _ => return Err(invalid("must stay inside the project (no `..`)")),
        }
    }
    let Some(first) = parts.first() else { return Err(invalid("names the project root itself")) };
    if *first == ".stack" || *first == ".git" {
        return Err(invalid("must not be inside .stack or .git"));
    }
    Ok(parts.iter().collect())
}

/// What one sync did. Nothing here is a failure of the command that ran it.
#[derive(Debug, Default, Serialize)]
pub struct SyncReport {
    pub dir: PathBuf,
    /// Links created or retargeted by this sync.
    pub linked: Vec<Value>,
    /// Links already in place.
    pub unchanged: Vec<String>,
    /// Stack's own links to skills no longer available, removed.
    pub pruned: Vec<Value>,
    /// Paths stack did not create, or that changed since: left alone.
    pub kept: Vec<Value>,
    /// Skills not linked, with why (duplicate names across tools).
    pub skipped: Vec<Value>,
    /// `{ code, message }`: `skills_unavailable`, `invalid_path`, `skills_failed`.
    pub warnings: Vec<Value>,
}

impl SyncReport {
    fn warn(&mut self, code: &str, message: impl Into<String>) {
        self.warnings.push(json!({ "code": code, "message": message.into() }));
    }
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    /// Link name to the exact target stack created it with.
    links: BTreeMap<String, PathBuf>,
}

const REGISTRY_VERSION: u32 = 1;

/// Link every available, non-provider skill into `<root>/<dir>`. `root` must be canonical.
pub fn sync(root: &Path, dir: &Path, discovery: &Discovery) -> SyncReport {
    let mut report = SyncReport { dir: dir.to_path_buf(), ..Default::default() };
    for w in &discovery.warnings {
        let message = w.strip_prefix("skills_unavailable: ").unwrap_or(w);
        report.warn("skills_unavailable", message);
    }
    // Wanted links: one per available skill name that exactly one pinned release declares.
    let mut wanted: BTreeMap<String, Vec<&Skill>> = BTreeMap::new();
    for s in discovery.skills.iter().filter(|s| s.status == Status::Available && !s.provider()) {
        if let Some(name) = s.name.as_deref().filter(|n| valid_name(n)) {
            wanted.entry(name.to_string()).or_default().push(s);
        }
    }
    let mut targets: BTreeMap<String, (&Skill, PathBuf)> = BTreeMap::new();
    for (name, skills) in wanted {
        if skills.len() > 1 {
            let by: Vec<String> = skills.iter().map(|s| format!("{}@{}", s.tool, s.version.as_deref().unwrap_or("?"))).collect();
            report.skipped.push(json!({ "name": name, "tools": by, "reason": "more than one pinned release declares a skill by this name; neither is linked" }));
            continue;
        }
        let skill = skills[0];
        if let Some(directory) = &skill.directory {
            targets.insert(name, (skill, directory.clone()));
        }
    }
    let path = root.join(dir);
    // A directory that does not exist yet holds nothing of stack's; create it only to link.
    let exists = std::fs::symlink_metadata(&path).is_ok();
    if !exists && targets.is_empty() {
        return report;
    }
    if let Err((code, message)) = prepare_dir(root, dir) {
        report.warn(code, message);
        return report;
    }
    let mut registry = match read_registry(&path) {
        Ok(r) => r,
        Err(message) => {
            report.warn("skills_failed", message);
            return report;
        }
    };
    // Recorded before anything is linked, so an interrupted sync never leaves a link it made
    // unowned. A record whose link does not appear is dropped on the next sync.
    let mut planned = registry.links.clone();
    for (name, (_, target)) in &targets {
        if matches!(std::fs::symlink_metadata(path.join(name)), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
            planned.insert(name.clone(), target.clone());
        }
    }
    if planned != registry.links {
        if let Err(message) = write_registry(&path, &planned) {
            report.warn("skills_failed", message);
            return report;
        }
    }
    let mut owned: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (name, (skill, target)) in &targets {
        let link = path.join(name);
        let recorded = registry.links.get(name);
        let detail = || json!({ "name": name, "tool": skill.tool, "version": skill.version, "target": target });
        match std::fs::symlink_metadata(&link) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match symlink(target, &link) {
                Ok(()) => {
                    owned.insert(name.clone(), target.clone());
                    report.linked.push(detail());
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    report.kept.push(json!({ "name": name, "reason": "something appeared at this path while linking" }));
                }
                Err(e) => report.warn("skills_failed", format!("cannot link {}: {e}", link.display())),
            },
            Err(e) => report.warn("skills_failed", format!("cannot inspect {}: {e}", link.display())),
            Ok(meta) if meta.file_type().is_symlink() => {
                let current = std::fs::read_link(&link).ok();
                if current.as_ref() == recorded && recorded == Some(target) {
                    owned.insert(name.clone(), target.clone());
                    report.unchanged.push(name.clone());
                } else if current.is_some() && current.as_ref() == recorded {
                    match replace_link(&path, &link, recorded.expect("recorded"), target) {
                        Ok(()) => {
                            owned.insert(name.clone(), target.clone());
                            report.linked.push(detail());
                        }
                        Err(why) => {
                            if std::fs::read_link(&link).ok().as_ref() == recorded {
                                owned.insert(name.clone(), recorded.expect("recorded").clone());
                            }
                            report.warn("skills_failed", format!("cannot relink {}: {why}", link.display()));
                        }
                    }
                } else if recorded.is_some() {
                    report.kept.push(json!({ "name": name, "reason": "a link stack made was pointed elsewhere since; left alone and no longer stack's" }));
                } else {
                    report.kept.push(json!({ "name": name, "reason": "a link stack did not create" }));
                }
            }
            Ok(meta) => {
                let what = if meta.is_dir() { "a real directory" } else { "a file" };
                report.kept.push(json!({ "name": name, "reason": format!("{what} stack did not create") }));
            }
        }
    }
    // Stack's links that no wanted skill names any more.
    for (name, recorded) in registry.links.iter().filter(|(n, _)| !targets.contains_key(*n)) {
        let link = path.join(name);
        match std::fs::symlink_metadata(&link) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(meta) if meta.file_type().is_symlink() && std::fs::read_link(&link).ok().as_ref() == Some(recorded) => {
                match std::fs::remove_file(&link) {
                    Ok(()) => report.pruned.push(json!({ "name": name, "target": recorded })),
                    Err(e) => {
                        owned.insert(name.clone(), recorded.clone());
                        report.warn("skills_failed", format!("cannot remove {}: {e}", link.display()));
                    }
                }
            }
            Ok(_) => report.kept.push(json!({ "name": name, "reason": "stack's link was replaced or pointed elsewhere since; left alone and no longer stack's" })),
            Err(e) => {
                owned.insert(name.clone(), recorded.clone());
                report.warn("skills_failed", format!("cannot inspect {}: {e}", link.display()));
            }
        }
    }
    registry.links = owned;
    if registry.links != planned {
        if let Err(message) = write_registry(&path, &registry.links) {
            report.warn("skills_failed", message);
        }
    }
    report
}

/// Create `<root>/<dir>` component by component. Every component must be a real directory:
/// a symbolic link anywhere in the path could lead outside the project, so it is refused.
fn prepare_dir(root: &Path, dir: &Path) -> std::result::Result<(), (&'static str, String)> {
    let mut at = root.to_path_buf();
    for component in dir.components() {
        at.push(component);
        let meta = match std::fs::symlink_metadata(&at) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&at) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(("skills_failed", format!("cannot create {}: {e}", at.display()))),
                }
                std::fs::symlink_metadata(&at).map_err(|e| ("skills_failed", format!("cannot inspect {}: {e}", at.display())))?
            }
            Err(e) => return Err(("skills_failed", format!("cannot inspect {}: {e}", at.display()))),
        };
        if meta.file_type().is_symlink() {
            return Err(("invalid_path", format!("{} is a symbolic link; stack links skills only into real directories inside the project", at.display())));
        }
        if !meta.is_dir() {
            return Err(("invalid_path", format!("{} is not a directory", at.display())));
        }
    }
    // No component is a link, so the path cannot resolve outside the project.
    match at.canonicalize() {
        Ok(real) if real == at && real.starts_with(root) => Ok(()),
        _ => Err(("invalid_path", format!("{} does not resolve inside the project", at.display()))),
    }
}

/// The ownership record, refusing anything stack cannot trust: a link (which could redirect a
/// write), something other than a regular file, an oversized or malformed record, or names
/// that are not plain skill names.
fn read_registry(dir: &Path) -> std::result::Result<Registry, String> {
    let path = dir.join(REGISTRY);
    let refuse = |why: String| format!("{} {why}; nothing was linked or removed (move it aside to let stack start afresh)", path.display());
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry { version: REGISTRY_VERSION, links: BTreeMap::new() }),
        Err(e) => return Err(refuse(format!("cannot be inspected: {e}"))),
        Ok(meta) if meta.file_type().is_symlink() => return Err(refuse("is a symbolic link".into())),
        Ok(meta) if !meta.is_file() => return Err(refuse("is not a regular file".into())),
        Ok(_) => {}
    }
    let mut file = open_nofollow(&path).map_err(|e| refuse(format!("cannot be read: {e}")))?;
    let mut bytes = Vec::new();
    file.by_ref().take(REGISTRY_LIMIT + 1).read_to_end(&mut bytes).map_err(|e| refuse(format!("cannot be read: {e}")))?;
    if bytes.len() as u64 > REGISTRY_LIMIT {
        return Err(refuse("is too large".into()));
    }
    let registry: Registry = serde_json::from_slice(&bytes).map_err(|e| refuse(format!("is malformed: {e}")))?;
    if registry.version != REGISTRY_VERSION {
        return Err(refuse(format!("has unsupported version {}", registry.version)));
    }
    if let Some(bad) = registry.links.keys().find(|n| !valid_name(n)) {
        return Err(refuse(format!("records an invalid link name {bad:?}")));
    }
    Ok(registry)
}

/// Replace the record atomically. Renaming over the path replaces whatever is there without
/// following it, and `read_registry` already refused a link.
fn write_registry(dir: &Path, links: &BTreeMap<String, PathBuf>) -> std::result::Result<(), String> {
    let path = dir.join(REGISTRY);
    let fail = |e: std::io::Error| format!("cannot record stack's links in {}: {e}", path.display());
    if links.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(fail(e)),
            _ => Ok(()),
        };
    }
    let registry = Registry { version: REGISTRY_VERSION, links: links.clone() };
    let mut body = serde_json::to_vec_pretty(&registry).expect("registry serializes");
    body.push(b'\n');
    let mut temp = tempfile::Builder::new().prefix(".stack-skills-").suffix(".tmp").tempfile_in(dir).map_err(fail)?;
    std::io::Write::write_all(&mut temp, &body).map_err(fail)?;
    temp.persist(&path).map_err(|e| fail(e.error))?;
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "skill links require Unix"))
}

/// Point stack's link at `target`: a new link beside it, renamed over it once it is confirmed
/// still to be the link stack recorded. A rename cannot replace a directory with a link.
fn replace_link(dir: &Path, link: &Path, recorded: &Path, target: &Path) -> std::result::Result<(), String> {
    // A fresh name: creating the link fails rather than replacing anything already there.
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let temp = dir.join(format!(".stack-skills-{}-{nanos}.tmp", std::process::id()));
    symlink(target, &temp).map_err(|e| e.to_string())?;
    let still_ours = std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::read_link(link).ok().as_deref() == Some(recorded);
    let result = if still_ours { std::fs::rename(&temp, link).map_err(|e| e.to_string()) } else { Err("it changed while relinking".into()) };
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The `skills` step of `up` and `install`: discovery, then sync. Its outcome is never an error.
pub fn sync_step(cache: &Path, root: &Path, dir: &str, lock: &Lockfile, versions: &[VersionReport]) -> SyncReport {
    let dir = match validate_dir(dir) {
        Ok(d) => d,
        Err(e) => {
            let mut report = SyncReport { dir: PathBuf::from(dir), ..Default::default() };
            report.warn(e.code, e.message);
            return report;
        }
    };
    let discovery = discover(cache, lock, versions);
    sync(root, &dir, &discovery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockedVersion;

    fn locked(name: &str, tool: Option<&str>, resolved: &str) -> LockedVersion {
        LockedVersion { name: name.into(), tool: tool.map(Into::into), requested: resolved.into(), resolved: resolved.into(), resolved_on: None, options: Default::default() }
    }

    fn version(kind: &'static str, name: &str, tool: Option<&str>, resolved: Option<&str>, origin: &str) -> VersionReport {
        VersionReport {
            kind,
            name: name.into(),
            tool: tool.map(Into::into),
            requested: resolved.unwrap_or("1").into(),
            options: Default::default(),
            resolved: resolved.map(Into::into),
            origin: origin.into(),
            moved_from: None,
        }
    }

    struct Release {
        _dir: tempfile::TempDir,
        install: PathBuf,
    }

    /// A fake install directory with skills `names` under `.mise-packslip/repo/skills/`.
    fn release(names: &[&str]) -> Release {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("installs/fnox/1.39.0");
        for name in names {
            let skill = install.join(".mise-packslip/repo/skills").join(name);
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(skill.join("SKILL.md"), format!("# {name}\n")).unwrap();
        }
        std::fs::create_dir_all(&install).unwrap();
        let install = install.canonicalize().unwrap();
        Release { _dir: dir, install }
    }

    fn answer(rel: &Release, tool: &str, version: &str, names: &[&str]) -> Answer {
        let mut a = Answer::default();
        a.releases.insert(tool.into(), vec![super::Release { version: version.into(), install_path: Some(rel.install.clone()), installed: true }]);
        for n in names {
            a.skills.push(Row { name: (*n).into(), tool: tool.into(), version: version.into(), path: rel.install.join(".mise-packslip/repo/skills").join(n) });
        }
        a
    }

    #[test]
    fn several_skills_of_one_release_are_each_available() {
        let rel = release(&["fnox", "fnox-setup"]);
        let lock = Lockfile::new(vec![], vec![locked("fnox", None, "1.39.0")], vec![]);
        let d = discover_with(&lock, &[version("tool", "fnox", None, Some("1.39.0"), "project")], |pins| {
            assert_eq!(pins.len(), 1);
            Ok(answer(&rel, "fnox", "1.39.0", &["fnox", "fnox-setup"]))
        });
        let names: Vec<_> = d.skills.iter().map(|s| (s.name.clone().unwrap(), s.status)).collect();
        assert_eq!(names, [("fnox".into(), Status::Available), ("fnox-setup".into(), Status::Available)]);
        assert!(d.warnings.is_empty());
        let text = read(&d, "fnox", "fnox-setup").unwrap();
        assert_eq!(text.text, "# fnox-setup\n");
    }

    #[test]
    fn templated_lock_entries_are_never_written_for_mise() {
        let mut fnox = locked("fnox", None, "1.39.0");
        fnox.options.insert("identity".into(), crate::tool::OptionValue::String("{{ exec(command='x') }}".into()));
        let lock = Lockfile::new(vec![], vec![fnox.clone(), locked("jq", None, "1.7.1")], vec![]);
        let mut v = version("tool", "fnox", None, Some("1.39.0"), "project");
        v.options = fnox.options.clone();
        let d = discover_with(&lock, &[v, version("tool", "jq", None, Some("1.7.1"), "project")], |pins| {
            assert_eq!(pins.iter().map(|p| p.tool.as_str()).collect::<Vec<_>>(), ["jq"]);
            Ok(Answer::default())
        });
        assert_eq!(d.skills[0].status, Status::Unavailable);
        assert!(d.skills[0].reason.as_deref().unwrap().contains("template"));
    }

    #[test]
    fn unpinned_tools_are_never_queried() {
        let lock = Lockfile::new(vec![], vec![], vec![]);
        let d = discover_with(&lock, &[version("tool", "fnox", None, None, "project")], |_| panic!("queried"));
        assert_eq!(d.skills[0].status, Status::Unavailable);
        assert!(d.warnings[0].starts_with("skills_unavailable: stack.lock pins no release"), "{:?}", d.warnings);
    }

    #[test]
    fn skills_outside_the_release_or_with_unsafe_names_are_not_available() {
        let rel = release(&["ok"]);
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("SKILL.md"), "x").unwrap();
        let escape = rel.install.join(".mise-packslip/repo/skills/escape");
        std::os::unix::fs::symlink(elsewhere.path(), &escape).unwrap();
        let entry_escape = rel.install.join(".mise-packslip/repo/skills/entry");
        std::fs::create_dir_all(&entry_escape).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("SKILL.md"), entry_escape.join("SKILL.md")).unwrap();
        let lock = Lockfile::new(vec![], vec![locked("fnox", None, "1.39.0")], vec![]);
        let d = discover_with(&lock, &[version("tool", "fnox", None, Some("1.39.0"), "project")], |_| {
            let mut a = answer(&rel, "fnox", "1.39.0", &["ok", "escape", "entry"]);
            a.skills.push(Row { name: "../x".into(), tool: "fnox".into(), version: "1.39.0".into(), path: rel.install.clone() });
            Ok(a)
        });
        let status: Vec<_> = d.skills.iter().map(|s| (s.name.clone().unwrap(), s.status)).collect();
        assert_eq!(status[0], ("ok".into(), Status::Available));
        for s in &d.skills[1..] {
            assert_eq!(s.status, Status::NoSkill, "{s:?}");
            assert!(s.reason.is_some());
        }
        assert_eq!(read(&d, "fnox", "escape").unwrap_err().code, "skill_not_found");
    }

    #[test]
    fn retrieval_is_bounded_and_rechecked() {
        let rel = release(&["big"]);
        let entry = rel.install.join(".mise-packslip/repo/skills/big/SKILL.md");
        std::fs::write(&entry, vec![b'a'; 65 * 1024]).unwrap();
        let lock = Lockfile::new(vec![], vec![locked("fnox", None, "1.39.0")], vec![]);
        let d = discover_with(&lock, &[version("tool", "fnox", None, Some("1.39.0"), "project")], |_| Ok(answer(&rel, "fnox", "1.39.0", &["big"])));
        assert_eq!(read(&d, "fnox", "big").unwrap_err().code, "skill_too_large");
        std::fs::write(&entry, vec![b'a'; 64 * 1024]).unwrap();
        assert_eq!(read(&d, "fnox", "big").unwrap().bytes, 64 * 1024);
        // Swapped for a link after discovery: refused, never followed.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), &entry).unwrap();
        assert_eq!(read(&d, "fnox", "big").unwrap_err().code, "skill_unreadable");
        std::fs::remove_file(&entry).unwrap();
        std::fs::write(&entry, [0xff, 0xfe]).unwrap();
        assert_eq!(read(&d, "fnox", "big").unwrap_err().code, "skill_unreadable");
    }

    #[test]
    fn dirs_must_be_relative_and_inside_the_project() {
        assert_eq!(validate_dir(".claude/skills").unwrap(), PathBuf::from(".claude/skills"));
        assert_eq!(validate_dir("./agents/skills/").unwrap(), PathBuf::from("agents/skills"));
        for bad in ["", " ", "/abs", "../x", "a/../../b", "a/..", ".", ".stack/skills", ".git/skills", "\\x"] {
            assert_eq!(validate_dir(bad).unwrap_err().code, "invalid_path", "{bad:?}");
        }
    }

    #[test]
    fn names_are_plain_path_components() {
        for good in ["fnox", "a", "0x", "my_skill-2"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["", "-x", "_x", "A", "a.b", "a/b", "..", ".stack-skills.json", &"a".repeat(129)] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
    fn available(tool: &str, name: &str, target: &Path, origin: &str) -> Skill {
        Skill {
            tool: tool.into(),
            version: Some("1.0.0".into()),
            origin: origin.into(),
            services: vec![],
            status: Status::Available,
            name: Some(name.into()),
            directory: Some(target.to_path_buf()),
            entrypoint: Some(target.join(ENTRYPOINT)),
            reason: None,
            install_path: None,
        }
    }

    struct Project {
        _dir: tempfile::TempDir,
        root: PathBuf,
        store: PathBuf,
    }

    fn project() -> Project {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&root).unwrap();
        for n in ["a", "b", "c", "pitchfork"] {
            std::fs::create_dir_all(store.join(n)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let store = store.canonicalize().unwrap();
        Project { _dir: dir, root, store }
    }

    fn discovery(skills: Vec<Skill>) -> Discovery {
        let (provider_skills, skills) = skills.into_iter().partition(Skill::provider);
        Discovery { skills, provider_skills, warnings: vec![] }
    }

    fn names(values: &[Value]) -> Vec<String> {
        values.iter().map(|v| v["name"].as_str().unwrap().to_string()).collect()
    }

    fn registry(p: &Project, dir: &str) -> BTreeMap<String, PathBuf> {
        read_registry(&p.root.join(dir)).unwrap().links
    }

    #[test]
    fn sync_links_available_skills_and_never_the_providers() {
        let p = project();
        let d = discovery(vec![
            available("fnox", "a", &p.store.join("a"), "project"),
            available("mbx", "b", &p.store.join("b"), "bundle:rust-mbx"),
            available("pitchfork", "pitchfork", &p.store.join("pitchfork"), "provider"),
        ]);
        let dir = Path::new(".claude/skills");
        let r = sync(&p.root, dir, &d);
        assert_eq!(names(&r.linked), ["a", "b"], "{r:?}");
        assert!(r.warnings.is_empty(), "{r:?}");
        let skills = p.root.join(dir);
        assert_eq!(std::fs::read_link(skills.join("a")).unwrap(), p.store.join("a"));
        assert!(std::fs::symlink_metadata(skills.join("pitchfork")).is_err(), "provider skill linked");
        assert_eq!(registry(&p, ".claude/skills").keys().collect::<Vec<_>>(), ["a", "b"]);
        let again = sync(&p.root, dir, &d);
        assert!(again.linked.is_empty() && again.pruned.is_empty());
        assert_eq!(again.unchanged, ["a", "b"]);
    }

    #[test]
    fn nothing_is_created_when_there_is_nothing_to_link() {
        let p = project();
        let r = sync(&p.root, Path::new(".claude/skills"), &discovery(vec![]));
        assert!(r.warnings.is_empty());
        assert!(!p.root.join(".claude").exists());
    }

    #[test]
    fn real_directories_foreign_and_retargeted_links_are_kept() {
        let p = project();
        let dir = Path::new("skills");
        let skills = p.root.join(dir);
        let all = discovery(vec![
            available("t", "a", &p.store.join("a"), "project"),
            available("t", "b", &p.store.join("b"), "project"),
            available("t", "c", &p.store.join("c"), "project"),
        ]);
        sync(&p.root, dir, &all);
        // The user replaces a with a real directory, points b elsewhere, and links d themselves.
        std::fs::remove_file(skills.join("a")).unwrap();
        std::fs::create_dir(skills.join("a")).unwrap();
        std::fs::write(skills.join("a/mine"), "keep").unwrap();
        std::fs::remove_file(skills.join("b")).unwrap();
        std::os::unix::fs::symlink(p.store.join("c"), skills.join("b")).unwrap();
        let mut d = all.clone();
        d.skills.push(available("u", "d", &p.store.join("a"), "project"));
        std::os::unix::fs::symlink("/somewhere/else", skills.join("d")).unwrap();
        let r = sync(&p.root, dir, &d);
        assert_eq!(names(&r.kept), ["a", "b", "d"], "{r:?}");
        assert_eq!(std::fs::read_to_string(skills.join("a/mine")).unwrap(), "keep");
        assert_eq!(std::fs::read_link(skills.join("b")).unwrap(), p.store.join("c"));
        assert_eq!(std::fs::read_link(skills.join("d")).unwrap(), PathBuf::from("/somewhere/else"));
        // What the user took over is no longer stack's: a later prune leaves it alone.
        assert_eq!(registry(&p, "skills").keys().collect::<Vec<_>>(), ["c"]);
        let r = sync(&p.root, dir, &discovery(vec![]));
        assert_eq!(names(&r.pruned), ["c"]);
        assert!(skills.join("a/mine").exists() && std::fs::read_link(skills.join("b")).is_ok());
        assert!(!skills.join(REGISTRY).exists(), "an empty record is removed");
    }

    #[test]
    fn stale_links_are_pruned_and_owned_links_follow_a_new_release() {
        let p = project();
        let dir = Path::new("skills");
        sync(&p.root, dir, &discovery(vec![available("t", "a", &p.store.join("a"), "project"), available("t", "b", &p.store.join("b"), "project")]));
        let r = sync(&p.root, dir, &discovery(vec![available("t", "a", &p.store.join("c"), "project")]));
        assert_eq!(names(&r.linked), ["a"]);
        assert_eq!(names(&r.pruned), ["b"]);
        assert_eq!(std::fs::read_link(p.root.join("skills/a")).unwrap(), p.store.join("c"));
        assert!(std::fs::symlink_metadata(p.root.join("skills/b")).is_err());
        assert_eq!(registry(&p, "skills").get("a"), Some(&p.store.join("c")));
    }

    #[test]
    fn duplicate_names_across_tools_are_reported_and_neither_linked() {
        let p = project();
        let d = discovery(vec![available("t", "a", &p.store.join("a"), "project"), available("u", "a", &p.store.join("b"), "project"), available("u", "c", &p.store.join("c"), "project")]);
        let r = sync(&p.root, Path::new("skills"), &d);
        assert_eq!(names(&r.skipped), ["a"]);
        assert_eq!(names(&r.linked), ["c"]);
        assert!(std::fs::symlink_metadata(p.root.join("skills/a")).is_err());
    }

    #[test]
    fn a_linked_ancestor_is_refused_and_nothing_is_written_through_it() {
        let p = project();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), p.root.join(".claude")).unwrap();
        let d = discovery(vec![available("t", "a", &p.store.join("a"), "project")]);
        let r = sync(&p.root, Path::new(".claude/skills"), &d);
        assert_eq!(r.warnings[0]["code"], "invalid_path", "{r:?}");
        assert!(r.linked.is_empty());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        // A link as the directory itself, even one into the project, is refused too.
        std::fs::remove_file(p.root.join(".claude")).unwrap();
        std::fs::create_dir(p.root.join("real")).unwrap();
        std::os::unix::fs::symlink(p.root.join("real"), p.root.join("skills")).unwrap();
        assert_eq!(sync(&p.root, Path::new("skills"), &d).warnings[0]["code"], "invalid_path");
        std::fs::write(p.root.join("file"), "").unwrap();
        assert_eq!(sync(&p.root, Path::new("file/skills"), &d).warnings[0]["code"], "invalid_path");
    }

    #[test]
    fn an_untrustworthy_record_changes_nothing() {
        let p = project();
        let skills = p.root.join("skills");
        std::fs::create_dir(&skills).unwrap();
        std::os::unix::fs::symlink(p.store.join("b"), skills.join("b")).unwrap();
        let d = discovery(vec![available("t", "a", &p.store.join("a"), "project")]);
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = outside.path().join("victim.json");
        std::fs::write(&elsewhere, "user data").unwrap();
        type Make<'a> = Box<dyn Fn() + 'a>;
        let cases: Vec<(&str, Make)> = vec![
            ("garbage", Box::new(|| std::fs::write(skills.join(REGISTRY), "not json").unwrap())),
            ("version", Box::new(|| std::fs::write(skills.join(REGISTRY), r#"{"version":2,"links":{}}"#).unwrap())),
            ("traversal", Box::new(|| std::fs::write(skills.join(REGISTRY), r#"{"version":1,"links":{"../../victim":"/x"}}"#).unwrap())),
            ("link", Box::new(|| std::os::unix::fs::symlink(&elsewhere, skills.join(REGISTRY)).unwrap())),
            ("directory", Box::new(|| std::fs::create_dir(skills.join(REGISTRY)).unwrap())),
        ];
        for (what, make) in cases {
            make();
            let r = sync(&p.root, Path::new("skills"), &d);
            assert_eq!(r.warnings.len(), 1, "{what}: {r:?}");
            assert_eq!(r.warnings[0]["code"], "skills_failed", "{what}");
            assert!(r.linked.is_empty() && r.pruned.is_empty(), "{what}");
            assert!(std::fs::symlink_metadata(skills.join("a")).is_err(), "{what}");
            assert_eq!(std::fs::read_link(skills.join("b")).unwrap(), p.store.join("b"), "{what}");
            assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "user data", "{what}");
            let path = skills.join(REGISTRY);
            if std::fs::symlink_metadata(&path).unwrap().is_dir() { std::fs::remove_dir(&path).unwrap() } else { std::fs::remove_file(&path).unwrap() }
        }
    }

    #[test]
    fn discovery_warnings_and_invalid_dirs_are_step_warnings() {
        let p = project();
        let mut d = discovery(vec![]);
        d.warnings.push("skills_unavailable: mise did not answer; every skill is listed as unavailable".into());
        let r = sync(&p.root, Path::new("skills"), &d);
        assert_eq!(r.warnings[0]["code"], "skills_unavailable");
        let lock = Lockfile::new(vec![], vec![], vec![]);
        let r = sync_step(&p.root, &p.root, "../out", &lock, &[]);
        assert_eq!(r.warnings[0]["code"], "invalid_path");
    }
}

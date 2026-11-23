//! `stack init`: a starting `stack.toml`, so a new project need not be written from the docs.

use crate::error::{io_error, Result, StackError};
use crate::manifest::PROJECT_FILE;
use serde::Serialize;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

/// What `init` wrote.
#[derive(Debug, Serialize)]
pub struct InitReport {
    pub path: PathBuf,
    pub tools: Vec<String>,
    pub services: Vec<String>,
    pub next: Vec<&'static str>,
    /// With `--detect`: the tools and services the project's files suggested and `init` wrote
    /// (an explicit `--tool` or `--service` of the same name replaces one, which is then not
    /// listed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detected: Vec<DetectedItem>,
}

/// A tool or preset service `--detect` found, and the file that suggested it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedItem {
    /// `tool` or `service`.
    pub kind: &'static str,
    pub name: String,
    /// A tool's version request (`latest` when no clean one was found), or a service's preset
    /// with its version when one was found, as `--service` takes them (`postgres@17`).
    pub value: String,
    /// The file, relative to the project directory.
    pub source: String,
}

/// Presets `--service` accepts, as `NAME`, `NAME=PRESET` or either with `@VERSION`.
const PRESETS: &[&str] = &["postgres", "redis", "cockroachdb", "nats", "spicedb"];

/// Write `root/stack.toml` with `tools` (`NAME` or `NAME=VERSION`, `latest` when no version is
/// given) and preset `services` (`NAME[=PRESET][@VERSION]`). The file is parsed back as a
/// project before anything is written, then published whole, by an atomic rename that never
/// replaces whatever has the name: a file, a link (dangling or not, never followed), or what
/// another `init` published meanwhile. Any of those is `project_exists`, and is left as it was.
/// An interrupted write leaves at most a hidden temporary file, never a partial `stack.toml`.
///
/// With `detect`, the tools and preset services the project's own files suggest are added (see
/// [`detect`]); an explicit tool or service of the same name replaces a detected one.
pub fn init(root: &Path, tools: &[String], services: &[String], detect: bool) -> Result<InitReport> {
    let path = root.join(PROJECT_FILE);
    let exists = || {
        StackError::new("project_exists", format!("{} already exists", path.display()))
            .hint("edit it, or run `stack init` in a directory without one")
    };
    // Settles nothing (another `init` may publish next); spares the work when one is there.
    if path.symlink_metadata().is_ok() {
        return Err(exists());
    }
    let detected: Vec<DetectedItem> = if detect {
        let named_tools: Vec<&str> = tools.iter().map(|t| t.split_once('=').map_or(t.as_str(), |(n, _)| n).trim()).collect();
        let named_services: Vec<&str> = services.iter().map(|s| service_name(s)).collect();
        self::detect(root)
            .into_iter()
            .filter(|d| !(if d.kind == "tool" { &named_tools } else { &named_services }).contains(&d.name.as_str()))
            .collect()
    } else {
        Vec::new()
    };
    let detected_tools = detected.iter().filter(|d| d.kind == "tool").map(|d| format!("{}={}", d.name, d.value));
    let tools: Vec<String> = tools.iter().cloned().chain(detected_tools).collect();
    // A detected service is named after its preset, so its value is already a `--service` spec.
    let detected_services = detected.iter().filter(|d| d.kind == "service").map(|d| d.value.clone());
    let services: Vec<String> = services.iter().cloned().chain(detected_services).collect();
    let usage = |what: &str, value: &str, why: String| {
        StackError::new("usage", format!("{what} {value:?}: {why}")).hint("--tool NAME[=VERSION], --service NAME[=PRESET][@VERSION]; see `stack init --help`")
    };
    let mut tool_table = Table::new();
    for spec in &tools {
        let (name, version) = spec.split_once('=').unwrap_or((spec, "latest"));
        if name.trim().is_empty() || version.trim().is_empty() {
            return Err(usage("--tool", spec, "needs a name, and a version after `=` when one is given".into()));
        }
        tool_table.insert(name.trim().into(), Value::String(version.trim().into()));
    }
    let mut service_table = Table::new();
    for spec in &services {
        let (named, version) = match spec.split_once('@') {
            Some((named, version)) => (named, Some(version.trim())),
            None => (spec.as_str(), None),
        };
        let (name, preset) = named.split_once('=').unwrap_or((named, named));
        let (name, preset) = (name.trim(), preset.trim());
        if !PRESETS.contains(&preset) {
            return Err(usage("--service", spec, format!("presets are {}; write a service with its own `run` in stack.toml", PRESETS.join(", "))));
        }
        if name.is_empty() || version.is_some_and(str::is_empty) {
            return Err(usage("--service", spec, "needs a name, and a version after `@` when one is given".into()));
        }
        let mut service = Table::new();
        service.insert("preset".into(), Value::String(preset.into()));
        if let Some(version) = version {
            service.insert("version".into(), Value::String(version.into()));
        }
        service_table.insert(name.into(), Value::Table(service));
    }
    let mut doc = Table::new();
    doc.insert("tools".into(), Value::Table(tool_table.clone()));
    if !service_table.is_empty() {
        doc.insert("services".into(), Value::Table(service_table.clone()));
    }
    let body = toml::to_string_pretty(&doc).map_err(|e| StackError::new("usage", e.to_string()))?;
    let task_services: Vec<String> = service_table.keys().map(|s| format!("{s:?}")).collect();
    let text = format!(
        "# Stack project: tools, services and tasks. Commit this file and stack.lock.\n\
         # Reference: https://github.com/usharma123/stack/blob/main/docs/user/bundles.md\n\
         {body}\n\
         # A service of your own; it gets $PORT, and the app gets $<NAME>_PORT and $<NAME>_URL.\n\
         # [services.api]\n\
         # run = \"exec python3 -m http.server $PORT --bind 127.0.0.1\"\n\
         # ready_cmd = \"curl -fsS http://127.0.0.1:$PORT/ >/dev/null\"\n\
         # depends_on = [{}]\n\
         # watch = [\"app.py\"]\n\
         \n\
         # [tasks.test]\n\
         # run = \"pytest -q\"\n",
        task_services.join(", ")
    );
    // Whatever was asked for must make a project Stack reads.
    let check = tempfile::tempdir().map_err(|e| io_error("temporary directory", e))?;
    std::fs::write(check.path().join(PROJECT_FILE), &text).map_err(|e| io_error("temporary directory", e))?;
    crate::manifest::read_project(check.path())?;
    let mut staged = tempfile::Builder::new();
    staged.prefix(".stack.toml.");
    // Readable as a file `fs::write` creates: 0666 less the umask, not a temporary file's 0600.
    #[cfg(unix)]
    staged.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let mut staged = staged
        .tempfile_in(root)
        .map_err(|e| io_error(root.display(), e))?;
    std::io::Write::write_all(&mut staged, text.as_bytes())
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|e| io_error(staged.path().display(), e))?;
    // Dropping the staged file on failure removes it.
    staged.persist_noclobber(&path).map_err(|e| match e.error.kind() {
        std::io::ErrorKind::AlreadyExists => exists(),
        _ => io_error(path.display(), e.error),
    })?;
    Ok(InitReport {
        path,
        tools: tool_table.keys().cloned().collect(),
        services: service_table.keys().cloned().collect(),
        next: vec!["stack compile", "stack up --ttl 30m", "stack exec -- <command>", "stack down"],
        detected,
    })
}

/// The name a `--service` spec gives its service: before `@` and `=`.
fn service_name(spec: &str) -> &str {
    let named = spec.split_once('@').map_or(spec, |(n, _)| n);
    named.split_once('=').map_or(named, |(n, _)| n).trim()
}

// ---- detection -----------------------------------------------------------------------------

/// The most of one file detection reads. A larger file counts as present, but its contents are
/// not examined.
const READ_LIMIT: u64 = 256 * 1024;

/// The project directory as detection sees it: read-only, one file at a time, never outside it.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(root: &Path) -> Option<Self> {
        Some(Self { root: root.canonicalize().ok()? })
    }

    /// `name` as a regular file inside the root: a link is followed only when it resolves
    /// inside the root.
    fn file(&self, name: &str) -> Option<PathBuf> {
        let path = self.root.join(name);
        let meta = path.symlink_metadata().ok()?;
        let path = if meta.file_type().is_symlink() { path.canonicalize().ok()? } else { path };
        if !path.starts_with(&self.root) || !path.metadata().ok()?.is_file() {
            return None;
        }
        Some(path)
    }

    fn has(&self, name: &str) -> bool {
        self.file(name).is_some()
    }

    /// `name`'s text, when it is a file inside the root of at most [`READ_LIMIT`] bytes of UTF-8.
    fn read(&self, name: &str) -> Option<String> {
        use std::io::Read;
        let file = std::fs::File::open(self.file(name)?).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(READ_LIMIT + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > READ_LIMIT {
            return None;
        }
        String::from_utf8(bytes).ok()
    }

    fn toml(&self, name: &str) -> Option<Table> {
        self.read(name)?.parse::<Table>().ok()
    }
}

/// What detection found so far: one entry per kind and name, the first found kept unless it
/// names no version and a later one does.
#[derive(Default)]
struct Found(Vec<DetectedItem>);

impl Found {
    fn add(&mut self, kind: &'static str, name: &str, value: Option<String>, source: &str) {
        let versioned = value.is_some();
        let value = value.unwrap_or_else(|| if kind == "tool" { "latest".into() } else { name.into() });
        let item = DetectedItem { kind, name: name.into(), value, source: source.into() };
        match self.0.iter_mut().find(|d| d.kind == kind && d.name == name) {
            None => self.0.push(item),
            Some(existing) if versioned && !existing_versioned(existing) => *existing = item,
            Some(_) => {}
        }
    }

    fn tool(&mut self, name: &str, version: Option<String>, source: &str) {
        self.add("tool", name, version, source);
    }

    fn service(&mut self, preset: &str, version: Option<String>, source: &str) {
        self.add("service", preset, version.map(|v| format!("{preset}@{v}")), source);
    }
}

fn existing_versioned(d: &DetectedItem) -> bool {
    if d.kind == "tool" { d.value != "latest" } else { d.value.contains('@') }
}

/// The tools and preset services the files at the top of `root` suggest, without running
/// anything: language version files and manifests, `.mise.toml` and `.tool-versions` pins, the
/// images of a Compose file, and the URLs of an example `.env`. Each file is read at most
/// [`READ_LIMIT`] bytes, and only when it is inside `root` (a link out of it is ignored).
pub fn detect(root: &Path) -> Vec<DetectedItem> {
    let Some(tree) = Tree::new(root) else { return Vec::new() };
    let mut found = Found::default();

    // Dedicated version files first: they say exactly what the project runs.
    if let Some(v) = tree.read(".python-version").and_then(|t| first_line(&t).and_then(clean_version)) {
        found.tool("python", Some(v), ".python-version");
    }
    for file in [".nvmrc", ".node-version"] {
        if let Some(v) = tree.read(file).and_then(|t| first_line(&t).map(|l| l.trim_start_matches('v').to_string())).and_then(|l| clean_version(&l)) {
            found.tool("node", Some(v), file);
        }
    }
    if let Some(v) = tree.toml("rust-toolchain.toml").and_then(|t| t.get("toolchain")?.get("channel")?.as_str().and_then(clean_version)) {
        found.tool("rust", Some(v), "rust-toolchain.toml");
    }
    if let Some(v) = tree.read("rust-toolchain").and_then(|t| first_line(&t).and_then(clean_version)) {
        found.tool("rust", Some(v), "rust-toolchain");
    }
    if let Some(v) = tree.read("go.mod").and_then(|t| go_directive(&t)) {
        found.tool("go", Some(v), "go.mod");
    }

    // Pins of other version managers. A preset's server is a service in Stack, not a tool.
    if let Some(tools) = tree.toml(".mise.toml").or_else(|| tree.toml("mise.toml")).and_then(|t| t.get("tools")?.as_table().cloned()) {
        let source = if tree.has(".mise.toml") { ".mise.toml" } else { "mise.toml" };
        for (name, value) in &tools {
            let version = match value {
                Value::String(v) => Some(v.as_str()),
                Value::Array(a) => a.first().and_then(Value::as_str),
                Value::Table(t) => t.get("version").and_then(Value::as_str),
                _ => None,
            };
            if let (Some(name), Some(version)) = (tool_name(name).filter(|n| !PRESETS.contains(&n.as_str())), version.and_then(pin)) {
                found.tool(&name, Some(version), source);
            }
        }
    }
    if let Some(text) = tree.read(".tool-versions") {
        for line in text.lines().map(|l| l.split('#').next().unwrap_or_default()) {
            let mut words = line.split_whitespace();
            let name = words.next().and_then(tool_name).filter(|n| !PRESETS.contains(&n.as_str()));
            if let (Some(name), Some(version)) = (name, words.next().and_then(pin)) {
                found.tool(&name, Some(version), ".tool-versions");
            }
        }
    }

    // Manifests: the language, at the version they require when it is a clean one.
    if let Some(pyproject) = tree.read("pyproject.toml") {
        let parsed = pyproject.parse::<Table>().ok();
        let requires = parsed.as_ref().and_then(|t| t.get("project")?.get("requires-python")?.as_str().and_then(requires_python));
        found.tool("python", requires, "pyproject.toml");
        if parsed.as_ref().and_then(|t| t.get("tool")?.get("uv")).is_some() {
            found.tool("uv", None, "pyproject.toml");
        }
    } else if tree.has("pyproject.toml") {
        found.tool("python", None, "pyproject.toml");
    }
    if tree.has("requirements.txt") {
        found.tool("python", None, "requirements.txt");
    }
    if tree.has("uv.lock") {
        found.tool("uv", None, "uv.lock");
    }
    if tree.has("package.json") {
        found.tool("node", None, "package.json");
    }
    if tree.has("pnpm-lock.yaml") {
        found.tool("pnpm", None, "pnpm-lock.yaml");
    }
    for file in ["bun.lock", "bun.lockb"] {
        if tree.has(file) {
            found.tool("bun", None, file);
        }
    }
    if tree.has("Cargo.toml") {
        found.tool("rust", None, "Cargo.toml");
    }

    // Services: the images a Compose file runs, then the URLs an example environment names.
    for file in ["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"] {
        let Some(text) = tree.read(file) else { continue };
        for (preset, version) in text.lines().filter_map(compose_image) {
            found.service(preset, version, file);
        }
    }
    for file in [".env.example", ".env.sample"] {
        let Some(text) = tree.read(file) else { continue };
        for preset in text.lines().filter_map(env_url_preset) {
            found.service(preset, None, file);
        }
    }
    found.0
}

/// The first line that is neither blank nor a comment, trimmed.
fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'))
}

/// `v` when it is one to three dot-separated numbers (`3`, `3.12`, `3.12.4`).
fn clean_version(v: &str) -> Option<String> {
    let v = v.trim();
    let parts: Vec<&str> = v.split('.').collect();
    (parts.len() <= 3 && parts.iter().all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))).then(|| v.to_string())
}

/// The `major.minor` a `requires-python` names, when it is a lower bound or an exact match on
/// one: `>=3.12`, `~=3.12`, `==3.12.*`, `3.12`, `>=3.12,<4`.
fn requires_python(spec: &str) -> Option<String> {
    let first = spec.split(',').next()?.trim();
    let version = [">=", "~=", "=="].iter().find_map(|op| first.strip_prefix(op)).unwrap_or(first).trim();
    if version.starts_with(|c: char| !c.is_ascii_digit()) {
        return None;
    }
    let version = version.strip_suffix(".*").unwrap_or(version);
    let mut parts = version.split('.');
    let (major, minor) = (parts.next()?, parts.next()?);
    clean_version(&format!("{major}.{minor}"))
}

/// The version of `go.mod`'s `go` directive.
fn go_directive(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.trim().strip_prefix("go ").and_then(|v| clean_version(v.split("//").next()?)))
}

/// A tool name a version manager wrote, as Stack names it; `None` for anything but a plain name.
fn tool_name(name: &str) -> Option<String> {
    let plain = !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    plain.then(|| match name {
        "nodejs" => "node".into(),
        "golang" => "go".into(),
        other => other.into(),
    })
}

/// A plain version pin (`3.12`, `20`, `latest`, `1.22.3`, `stable`); not `system`, a path or a
/// template.
fn pin(version: &str) -> Option<String> {
    let plain = !version.is_empty()
        && version.len() <= 64
        && version != "system"
        && version.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b));
    plain.then(|| version.to_string())
}

/// The preset a Compose `image:` line runs, and its major version when the tag starts with one.
fn compose_image(line: &str) -> Option<(&'static str, Option<String>)> {
    let image = line.trim().trim_start_matches("- ").strip_prefix("image:")?;
    let image = image.split('#').next()?.trim().trim_matches(|c| c == '"' || c == '\'');
    if image.is_empty() || image.contains('$') {
        return None;
    }
    let image = image.split('@').next()?;
    let last = image.rsplit('/').next()?;
    let (repo, tag) = last.split_once(':').unwrap_or((last, ""));
    let repo = repo.to_ascii_lowercase();
    let preset = match repo.as_str() {
        "postgres" | "postgresql" | "postgis" | "pgvector" => "postgres",
        "redis" | "redis-stack" | "redis-stack-server" | "valkey" => "redis",
        "cockroach" | "cockroachdb" => "cockroachdb",
        "nats" => "nats",
        "spicedb" => "spicedb",
        _ => return None,
    };
    // Valkey's versions are not Redis's: the preset is right, the number would not be.
    if repo == "valkey" {
        return Some((preset, None));
    }
    let tag = tag.trim_start_matches("pg").trim_start_matches('v');
    let major: String = tag.chars().take_while(char::is_ascii_digit).collect();
    let numeric = !major.is_empty() && major.len() <= 4 && !tag[major.len()..].starts_with(|c: char| c.is_ascii_alphanumeric());
    Some((preset, numeric.then_some(major)))
}

/// The preset an example environment's `DATABASE_URL`, `REDIS_URL` or `NATS_URL` points at.
fn env_url_preset(line: &str) -> Option<&'static str> {
    let line = line.trim();
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
    match key.trim() {
        "DATABASE_URL" if value.starts_with("postgres://") || value.starts_with("postgresql://") => Some("postgres"),
        "REDIS_URL" if value.starts_with("redis://") || value.starts_with("rediss://") => Some("redis"),
        "NATS_URL" if value.starts_with("nats://") => Some("nats"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_a_project_stack_reads_and_never_replaces_one() {
        let dir = tempfile::tempdir().unwrap();
        let report = init(dir.path(), &["python=3.13".into(), "uv".into()], &["postgres@17".into(), "cache=redis".into()], false).unwrap();
        assert_eq!((report.tools, report.services), (vec!["python".to_string(), "uv".into()], vec!["postgres".to_string(), "cache".into()]));
        let project = crate::manifest::read_project(dir.path()).unwrap();
        assert_eq!(project.tools["python"].as_str(), Some("3.13"));
        assert_eq!(project.tools["uv"].as_str(), Some("latest"));
        assert_eq!(project.services["postgres"].version.as_deref(), Some("17"));
        assert_eq!(project.services["cache"].preset.as_deref(), Some("redis"));
        let e = init(dir.path(), &[], &[], false).unwrap_err();
        assert_eq!(e.code, "project_exists");

        let empty = tempfile::tempdir().unwrap();
        init(empty.path(), &[], &[], false).unwrap();
        assert!(crate::manifest::read_project(empty.path()).unwrap().services.is_empty());
        for (tools, services) in [(vec!["=1".to_string()], vec![]), (vec![], vec!["web=nginx".to_string()]), (vec![], vec!["db@".to_string()])] {
            let other = tempfile::tempdir().unwrap();
            assert_eq!(init(other.path(), &tools, &services, false).unwrap_err().code, "usage");
            assert!(!other.path().join(PROJECT_FILE).exists());
        }
    }

    /// A directory holding `files` (name, contents).
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    /// What detection found, as (kind, name, value, source).
    fn found(dir: &Path) -> Vec<(String, String, String, String)> {
        detect(dir).into_iter().map(|d| (d.kind.to_string(), d.name, d.value, d.source)).collect()
    }

    fn item(kind: &str, name: &str, value: &str, source: &str) -> (String, String, String, String) {
        (kind.into(), name.into(), value.into(), source.into())
    }

    #[test]
    fn detect_finds_python_and_uv() {
        let dir = project(&[
            ("pyproject.toml", "[project]\nname = 'x'\nrequires-python = '>=3.12,<4'\n[tool.uv]\ndev-dependencies = []\n"),
            ("requirements.txt", "flask\n"),
        ]);
        assert_eq!(found(dir.path()), [item("tool", "python", "3.12", "pyproject.toml"), item("tool", "uv", "latest", "pyproject.toml")]);
        // .python-version says exactly; uv.lock alone means uv.
        let dir = project(&[("pyproject.toml", "[project]\nrequires-python = '>=3.11'\n"), (".python-version", "# pinned\n3.13.1\n"), ("uv.lock", "")]);
        assert_eq!(found(dir.path()), [item("tool", "python", "3.13.1", ".python-version"), item("tool", "uv", "latest", "uv.lock")]);
        for (requires, version) in [("~=3.13", "3.13"), ("==3.12.*", "3.12"), ("3.11", "3.11"), (">=3.12.1", "3.12"), ("<3.13", "latest"), ("!=3.9", "latest"), (">=3", "latest")] {
            let dir = project(&[("pyproject.toml", &format!("[project]\nrequires-python = '{requires}'\n"))]);
            assert_eq!(found(dir.path()), [item("tool", "python", version, "pyproject.toml")], "{requires}");
        }
        // Not a clean version, or not TOML: still Python, at latest.
        let dir = project(&[(".python-version", "pypy3.10\n"), ("requirements.txt", "")]);
        assert_eq!(found(dir.path()), [item("tool", "python", "latest", "requirements.txt")]);
        let dir = project(&[("pyproject.toml", "[project\n")]);
        assert_eq!(found(dir.path()), [item("tool", "python", "latest", "pyproject.toml")]);
    }

    #[test]
    fn detect_finds_node_and_its_package_managers() {
        let dir = project(&[("package.json", "{}"), (".nvmrc", "v20\n"), ("pnpm-lock.yaml", "")]);
        assert_eq!(found(dir.path()), [item("tool", "node", "20", ".nvmrc"), item("tool", "pnpm", "latest", "pnpm-lock.yaml")]);
        let dir = project(&[("package.json", "{}"), (".node-version", "22.11\n"), ("bun.lockb", "")]);
        assert_eq!(found(dir.path()), [item("tool", "node", "22.11", ".node-version"), item("tool", "bun", "latest", "bun.lockb")]);
        let dir = project(&[("package.json", "{}"), (".nvmrc", "lts/iron\n"), ("bun.lock", "")]);
        assert_eq!(found(dir.path()), [item("tool", "node", "latest", "package.json"), item("tool", "bun", "latest", "bun.lock")]);
    }

    #[test]
    fn detect_finds_rust_and_go() {
        let dir = project(&[("Cargo.toml", "[package]\nname = 'x'\n"), ("rust-toolchain.toml", "[toolchain]\nchannel = '1.82.0'\n")]);
        assert_eq!(found(dir.path()), [item("tool", "rust", "1.82.0", "rust-toolchain.toml")]);
        let dir = project(&[("Cargo.toml", ""), ("rust-toolchain.toml", "[toolchain]\nchannel = 'nightly'\n")]);
        assert_eq!(found(dir.path()), [item("tool", "rust", "latest", "Cargo.toml")]);
        let dir = project(&[("go.mod", "module example.com/x\n\ngo 1.22.3 // toolchain\n")]);
        assert_eq!(found(dir.path()), [item("tool", "go", "1.22.3", "go.mod")]);
    }

    #[test]
    fn detect_reads_other_version_managers_pins() {
        let dir = project(&[
            (".mise.toml", "[tools]\npython = '3.12'\nnode = ['20', '18']\njq = { version = '1.7.1' }\n\"npm:prettier\" = 'latest'\nterraform = '{{ env.TF }}'\npostgres = '17'\n"),
            (".tool-versions", "nodejs 18.20.0\ngolang 1.23.1 # comment\nruby system\nredis 7\n"),
            ("package.json", "{}"),
        ]);
        assert_eq!(
            found(dir.path()),
            [
                item("tool", "python", "3.12", ".mise.toml"),
                item("tool", "node", "20", ".mise.toml"),
                item("tool", "jq", "1.7.1", ".mise.toml"),
                item("tool", "go", "1.23.1", ".tool-versions"),
            ]
        );
    }

    #[test]
    fn detect_finds_preset_services_in_compose_files_and_example_envs() {
        let compose = "services:\n  db:\n    image: \"pgvector/pgvector:pg17\"\n  cache:\n    image: valkey/valkey:8\n  q:\n    image: nats:2.10-alpine\n  authz:\n    image: authzed/spicedb:v1.35.0\n  crdb:\n    image: cockroachdb/cockroach:latest\n  web:\n    image: nginx:1.27\n  other:\n    image: ${PG_IMAGE:-postgres:16}\n";
        let dir = project(&[("docker-compose.yml", compose)]);
        assert_eq!(
            found(dir.path()),
            [
                item("service", "postgres", "postgres@17", "docker-compose.yml"),
                item("service", "redis", "redis", "docker-compose.yml"),
                item("service", "nats", "nats@2", "docker-compose.yml"),
                item("service", "spicedb", "spicedb@1", "docker-compose.yml"),
                item("service", "cockroachdb", "cockroachdb", "docker-compose.yml"),
            ]
        );
        let dir = project(&[("compose.yaml", "services:\n  db:\n    image: postgres:16.4-bookworm\n  r:\n    image: 'redis:7'\n")]);
        assert_eq!(found(dir.path()), [item("service", "postgres", "postgres@16", "compose.yaml"), item("service", "redis", "redis@7", "compose.yaml")]);
        let env = "# local\nexport DATABASE_URL=\"postgresql://app:app@localhost/app\"\nREDIS_URL=rediss://cache:6380\nNATS_URL=nats://127.0.0.1:4222\nOTHER_URL=postgres://x\nDATABASE_URL_RO=postgres://y\n";
        let dir = project(&[(".env.example", env)]);
        assert_eq!(
            found(dir.path()),
            [item("service", "postgres", "postgres", ".env.example"), item("service", "redis", "redis", ".env.example"), item("service", "nats", "nats", ".env.example")]
        );
        // A versioned image wins over an unversioned mention, whichever file came first.
        let dir = project(&[(".env.sample", "DATABASE_URL=postgres://x\n"), ("compose.yml", "services:\n  db:\n    image: postgres:15\n")]);
        assert_eq!(found(dir.path()), [item("service", "postgres", "postgres@15", "compose.yml")]);
        let dir = project(&[("docker-compose.yaml", "services:\n  db:\n    image: mysql:8\n"), (".env.example", "DATABASE_URL=mysql://x\n")]);
        assert!(found(dir.path()).is_empty());
    }

    #[test]
    fn init_detect_writes_what_it_found_and_explicit_flags_win() {
        let dir = project(&[
            ("pyproject.toml", "[project]\nrequires-python = '>=3.12'\n"),
            ("uv.lock", ""),
            ("package.json", "{}"),
            ("compose.yml", "services:\n  db:\n    image: postgres:16\n  cache:\n    image: redis:7\n"),
        ]);
        let report = init(dir.path(), &["python=3.13".into()], &["redis=redis@8".into()], true).unwrap();
        let written = crate::manifest::read_project(dir.path()).unwrap();
        assert_eq!(written.tools["python"].as_str(), Some("3.13"), "--tool wins");
        assert_eq!(written.tools["uv"].as_str(), Some("latest"));
        assert_eq!(written.tools["node"].as_str(), Some("latest"));
        assert_eq!(written.services["redis"].version.as_deref(), Some("8"), "--service wins");
        assert_eq!(written.services["postgres"].version.as_deref(), Some("16"));
        assert_eq!(written.services["postgres"].preset.as_deref(), Some("postgres"));
        assert_eq!(report.tools, ["python", "uv", "node"]);
        assert_eq!(report.services, ["redis", "postgres"]);
        // What detection contributed; the replaced entries are not listed.
        let listed: Vec<(&str, &str, &str)> = report.detected.iter().map(|d| (d.kind, d.name.as_str(), d.value.as_str())).collect();
        assert_eq!(listed, [("tool", "uv", "latest"), ("tool", "node", "latest"), ("service", "postgres", "postgres@16")]);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["detected"][2], serde_json::json!({ "kind": "service", "name": "postgres", "value": "postgres@16", "source": "compose.yml" }));

        // Without --detect nothing is read, and the result has no `detected` key at all.
        let plain = project(&[("package.json", "{}"), ("compose.yml", "services:\n  db:\n    image: postgres:16\n")]);
        let report = init(plain.path(), &[], &[], false).unwrap();
        assert!(report.tools.is_empty() && report.services.is_empty() && report.detected.is_empty());
        assert!(serde_json::to_value(&report).unwrap().get("detected").is_none());
        // Nothing to detect: the plain starting file.
        let empty = tempfile::tempdir().unwrap();
        let report = init(empty.path(), &[], &[], true).unwrap();
        assert!(report.detected.is_empty() && report.tools.is_empty());
        // An existing project is refused before anything is read.
        assert_eq!(init(dir.path(), &[], &[], true).unwrap_err().code, "project_exists");
    }

    #[test]
    fn detect_reads_nothing_large_or_outside_the_project() {
        // Larger than the limit: present (so the language counts), contents unread.
        let big = format!("[project]\nrequires-python = '>=3.12'\n#{}\n", "x".repeat(READ_LIMIT as usize));
        let dir = project(&[("pyproject.toml", &big), (".nvmrc", &format!("20\n{}", " ".repeat(READ_LIMIT as usize)))]);
        assert_eq!(found(dir.path()), [item("tool", "python", "latest", "pyproject.toml")]);
        // Exactly at the limit is read.
        let at = format!("20\n{}", " ".repeat(READ_LIMIT as usize - 3));
        let dir = project(&[(".nvmrc", &at)]);
        assert_eq!(found(dir.path()), [item("tool", "node", "20", ".nvmrc")]);
        // Not UTF-8: unread.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".python-version"), b"3.12\xff\n").unwrap();
        assert!(found(dir.path()).is_empty());
        // A directory where a file is expected is no file.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("package.json")).unwrap();
        assert!(found(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn detect_never_follows_a_link_out_of_the_project() {
        use std::os::unix::fs::symlink;
        let outside = project(&[(".python-version", "3.11\n"), ("compose.yml", "services:\n  db:\n    image: postgres:16\n"), ("package.json", "{}")]);
        let dir = project(&[("versions", "3.13\n")]);
        for name in ["compose.yml", "package.json"] {
            symlink(outside.path().join(name), dir.path().join(name)).unwrap();
        }
        symlink(outside.path().join(".python-version"), dir.path().join(".nvmrc")).unwrap();
        symlink(dir.path().join("nowhere"), dir.path().join("go.mod")).unwrap();
        // A link inside the project is followed.
        symlink("versions", dir.path().join(".python-version")).unwrap();
        assert_eq!(found(dir.path()), [item("tool", "python", "3.13", ".python-version")]);
        // A project reached through a link is still the project.
        let via = tempfile::tempdir().unwrap();
        symlink(dir.path(), via.path().join("p")).unwrap();
        assert_eq!(found(&via.path().join("p")), [item("tool", "python", "3.13", ".python-version")]);
    }

    /// What the directory holds, by name.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn concurrent_inits_leave_exactly_one_whole_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let writers = 12;
        let barrier = std::sync::Barrier::new(writers);
        let results: Vec<Result<InitReport>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..writers)
                .map(|i| {
                    let (dir, barrier) = (dir.path(), &barrier);
                    scope.spawn(move || {
                        let tools: Vec<String> = (0..250).map(|k| format!("probe{k}=1.{i}")).collect();
                        barrier.wait();
                        init(dir, &tools, &[], false)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let won: Vec<usize> = results.iter().enumerate().filter(|(_, r)| r.is_ok()).map(|(i, _)| i).collect();
        assert_eq!(won.len(), 1, "one writer wins: {won:?}");
        for r in &results {
            if let Err(e) = r {
                assert_eq!(e.code, "project_exists", "{e}");
            }
        }
        // The file is the winner's, whole, and nothing else was left beside it.
        let project = crate::manifest::read_project(dir.path()).unwrap();
        assert_eq!(project.tools.len(), 250);
        assert_eq!(project.tools["probe249"].as_str(), Some(format!("1.{}", won[0]).as_str()));
        assert_eq!(names(dir.path()), [PROJECT_FILE]);
        // Created like any file the user writes there, not private like a temporary file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let plain = dir.path().join("plain");
            std::fs::write(&plain, "").unwrap();
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.path().join(PROJECT_FILE)), mode(&plain));
        }
    }

    #[cfg(unix)]
    #[test]
    fn init_never_writes_through_a_link_in_the_projects_place() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // A dangling link: `exists()` says no file is there.
        let target = outside.path().join("elsewhere.toml");
        std::os::unix::fs::symlink(&target, dir.path().join(PROJECT_FILE)).unwrap();
        assert_eq!(init(dir.path(), &[], &[], false).unwrap_err().code, "project_exists");
        assert!(!target.exists(), "written through the link");
        assert_eq!(std::fs::read_link(dir.path().join(PROJECT_FILE)).unwrap(), target);
        // A link to a file someone has not saved elsewhere: kept as it is.
        std::fs::write(&target, "unsaved = true\n").unwrap();
        assert_eq!(init(dir.path(), &["uv".into()], &[], false).unwrap_err().code, "project_exists");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unsaved = true\n");
        assert_eq!(names(dir.path()), [PROJECT_FILE]);
    }
}

//! End-to-end `stack compile` tests against real git repositories in temp dirs.

use stack::error::StackError;
use stack::lock;
use stack::project::{compile, Options, Report};
use stack::provider::mise::{self, Resolver};
use stack::source::Mode;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// A controlled upstream: the releases each tool has published so far. Resolution picks the
/// highest release matching a prefix, as `mise latest` does, and every call is counted.
#[derive(Default)]
struct Upstream {
    releases: Mutex<BTreeMap<String, Vec<String>>>,
    calls: Mutex<Vec<String>>,
}

impl Upstream {
    fn new() -> Arc<Self> {
        let up = Arc::new(Self::default());
        for (tool, versions) in [
            ("python", &["3.12.9", "3.13.1", "3.13.2"][..]),
            ("uv", &["0.11.0", "0.12.1"]),
            ("jq", &["1.7.1", "1.8.0"]),
            ("pitchfork", &["2.29.0"]),
            ("postgres", &["16.4", "17.1", "17.2"]),
            ("redis", &["7.4.1", "8.0.2", "8.2.1"]),
            ("cockroach", &["25.2.0"]),
            ("nats-server", &["2.11.0"]),
            ("spicedb", &["1.45.0"]),
        ] {
            up.publish(tool, versions);
        }
        up
    }

    fn publish(&self, tool: &str, versions: &[&str]) {
        let mut releases = self.releases.lock().unwrap();
        releases.entry(tool.into()).or_default().extend(versions.iter().map(|v| v.to_string()));
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

fn numeric(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

impl Resolver for Upstream {
    fn resolve(&self, tool: &str, request: &str) -> Result<String, StackError> {
        self.calls.lock().unwrap().push(format!("{tool}@{request}"));
        let releases = self.releases.lock().unwrap();
        let fail = |why: &str| StackError::new("resolve_failed", format!("cannot resolve {tool}@{request}: {why}"));
        let all = releases.get(tool).ok_or_else(|| fail("not found in registry"))?;
        all.iter()
            .filter(|v| request == "latest" || *v == request || v.starts_with(&format!("{request}.")))
            .max_by_key(|v| numeric(v))
            .cloned()
            .ok_or_else(|| fail("no release matches"))
    }
}

const PYBASE: &str = r#"
[bundle]
name = "pybase"
version = "1.0.0"

[tools]
python = "3.13"
uv = "latest"

[env]
LOG_LEVEL = "info"
SEED_FILE = "{{bundle_dir}}/fixtures/seed.sql"

[services.postgres]
preset = "postgres"
version = "17"

[services.redis]
preset = "redis"
version = "8"

[tasks.seed]
run = "psql \"$DATABASE_URL\" -f {{bundle_dir}}/fixtures/seed.sql"
services = ["postgres"]

[paths]
bin = ["bin"]
"#;

const OBS: &str = r#"
[bundle]
name = "obs"

[tools]
jq = "latest"
uv = "latest"

[env]
LOG_LEVEL = "debug"
"#;

struct Sandbox {
    tmp: TempDir,
    upstream: Arc<Upstream>,
}

impl Sandbox {
    fn new() -> Self {
        Self { tmp: TempDir::new().unwrap(), upstream: Upstream::new() }
    }

    fn options(&self, root: &Path, mode: Mode, write: bool) -> Options {
        Options {
            root: root.to_path_buf(),
            mode,
            write,
            cache: self.path("cache"),
            state: self.path("state"),
            reassign_ports: false,
            resolver: Some(self.upstream.clone()),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    /// A git repo with `bundle.toml` and supporting files, tagged v1.
    fn bundle(&self, name: &str, manifest: &str) -> PathBuf {
        let repo = self.path(&format!("bundles/{name}"));
        fs::create_dir_all(repo.join("bin")).unwrap();
        fs::create_dir_all(repo.join("fixtures")).unwrap();
        fs::write(repo.join("bundle.toml"), manifest).unwrap();
        fs::write(repo.join("fixtures/seed.sql"), "select 1;\n").unwrap();
        fs::write(repo.join("bin/acme"), "#!/bin/sh\necho acme\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        commit_all(&repo, "v1");
        git(&repo, &["tag", "v1"]);
        repo
    }

    fn project(&self, stack_toml: &str) -> PathBuf {
        let root = self.path("app");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("stack.toml"), stack_toml).unwrap();
        root
    }

    fn compile(&self, root: &Path, mode: Mode) -> Result<Report, StackError> {
        compile(&self.options(root, mode, true))
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_all(repo: &Path, msg: &str) -> String {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", msg]);
    git(repo, &["rev-parse", "HEAD"])
}

fn use_git(repo: &Path, reference: &str) -> String {
    format!("[[use]]\nbundle = \"git+file://{}?ref={reference}\"\n", repo.display())
}

#[test]
fn compiles_git_bundle_with_services_files_and_pinned_commit() {
    let sb = Sandbox::new();
    let repo = sb.bundle("pybase", PYBASE);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let root = sb.project(&use_git(&repo, "v1"));

    let report = sb.compile(&root, Mode::UseLock).unwrap();

    let locked = lock::read(&root).unwrap().unwrap();
    assert_eq!(locked.bundles[0].commit.as_deref(), Some(head.as_str()));
    assert!(locked.bundles[0].content_hash.starts_with("sha256:"));

    // Supporting files travel with the bundle and paths resolve to the bundle, not the project.
    let bundle_dir = &report.bundles[0].dir;
    let seed = &report.stack.env["SEED_FILE"].value;
    assert_eq!(seed, &format!("{}/fixtures/seed.sql", bundle_dir.display()));
    assert!(Path::new(seed).exists());
    assert!(report.stack.bin_paths[0].join("acme").exists());

    let rendered = fs::read_to_string(mise::output_path(&root)).unwrap();
    for expected in [
        "experimental = true",
        "pitchfork = \"2.29.0\"",
        "python = \"3.13.2\"",
        "uv = \"0.12.1\"",
        "version = \"17.2\"",
        "version = \"8.2.1\"",
        "[daemons.postgres]",
        "preset = \"postgres\"",
        "daemons = [\"postgres\"]",
        "UV_PYTHON_PREFERENCE = \"only-system\"",
        "UV_PYTHON = \"3.13.2\"",
    ] {
        assert!(rendered.contains(expected), "missing `{expected}` in:\n{rendered}");
    }
    assert!(!rendered.contains("{{bundle_dir}}"));

    // Every service gets a concrete port from stack's range, never the service default.
    for (service, port) in &report.ports {
        assert!((40000..=49999).contains(port), "{service} got {port}");
        assert!(rendered.contains(&format!("port = {port}")));
    }
    assert_eq!(report.ports.len(), 2);
}

#[test]
fn uv_python_selection_is_left_to_the_project_when_it_sets_one() {
    let sb = Sandbox::new();
    let root = sb.project("[tools]\npython = \"3.13\"\n[env]\nUV_PYTHON_PREFERENCE = \"managed\"\n");
    sb.compile(&root, Mode::UseLock).unwrap();
    let rendered = fs::read_to_string(mise::output_path(&root)).unwrap();
    assert!(rendered.contains("UV_PYTHON_PREFERENCE = \"managed\""), "{rendered}");
    assert!(!rendered.contains("only-system"), "{rendered}");
    assert!(rendered.contains("UV_PYTHON = \"3.13.2\""), "each variable is overridden on its own: {rendered}");

    let root = sb.project("[tools]\npython = \"3.13\"\n[env]\nUV_PYTHON = \"3.12\"\n");
    sb.compile(&root, Mode::UseLock).unwrap();
    let rendered = fs::read_to_string(mise::output_path(&root)).unwrap();
    assert!(rendered.contains("UV_PYTHON = \"3.12\"") && !rendered.contains("3.13.2\"\nUV"), "{rendered}");

    let root = sb.project("[tools]\njq = \"1.8\"\n");
    sb.compile(&root, Mode::UseLock).unwrap();
    let rendered = fs::read_to_string(mise::output_path(&root)).unwrap();
    assert!(!rendered.contains("UV_PYTHON_PREFERENCE"), "only projects that pin python: {rendered}");
}

#[test]
fn independent_checkouts_get_distinct_stable_ports() {
    let sb = Sandbox::new();
    let repo = sb.bundle("pybase", PYBASE);
    let a = sb.path("a");
    let b = sb.path("b");
    for dir in [&a, &b] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("stack.toml"), use_git(&repo, "v1")).unwrap();
    }
    let pa = sb.compile(&a, Mode::UseLock).unwrap().ports;
    let pb = sb.compile(&b, Mode::UseLock).unwrap().ports;
    for port in pa.values() {
        assert!(!pb.values().any(|p| p == port), "a and b share port {port}");
    }
    assert_eq!(sb.compile(&a, Mode::UseLock).unwrap().ports, pa, "ports are stable across compiles");
}

#[test]
fn pinned_ports_cannot_collide_across_projects() {
    let sb = Sandbox::new();
    let repo = sb.bundle("pybase", PYBASE);
    let pin = "[override.services.redis]\npreset = \"redis\"\nversion = \"8\"\nport = 46379\n";
    let a = sb.path("a");
    let b = sb.path("b");
    for dir in [&a, &b] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("stack.toml"), format!("{}{pin}", use_git(&repo, "v1"))).unwrap();
    }
    assert_eq!(sb.compile(&a, Mode::UseLock).unwrap().ports["redis"], 46379);
    assert_eq!(sb.compile(&b, Mode::UseLock).unwrap_err().code, "port_conflict");
}

#[test]
fn deleted_projects_release_their_ports() {
    let sb = Sandbox::new();
    let repo = sb.bundle("pybase", PYBASE);
    let a = sb.path("a");
    fs::create_dir_all(&a).unwrap();
    fs::write(a.join("stack.toml"), use_git(&repo, "v1")).unwrap();
    sb.compile(&a, Mode::UseLock).unwrap();
    assert_eq!(stack::ports::lookup(&sb.path("state"), &a).unwrap().len(), 2);

    fs::remove_dir_all(&a).unwrap();
    let b = sb.project(&use_git(&repo, "v1"));
    sb.compile(&b, Mode::UseLock).unwrap();
    assert!(stack::ports::lookup(&sb.path("state"), &a).unwrap().is_empty());
}

#[test]
fn moved_tag_does_not_change_a_locked_project() {
    let sb = Sandbox::new();
    let repo = sb.bundle("pybase", PYBASE);
    let v1 = git(&repo, &["rev-parse", "HEAD"]);
    let root = sb.project(&use_git(&repo, "v1"));
    sb.compile(&root, Mode::UseLock).unwrap();

    // Publisher retags v1 onto new content.
    fs::write(repo.join("fixtures/seed.sql"), "select 2;\n").unwrap();
    let v2 = commit_all(&repo, "v2");
    git(&repo, &["tag", "-f", "v1"]);

    let again = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(again.bundles[0].commit.as_deref(), Some(v1.as_str()));
    assert!(!again.lock_changed);

    let updated = sb.compile(&root, Mode::Update).unwrap();
    assert_eq!(updated.bundles[0].commit.as_deref(), Some(v2.as_str()));
    assert_eq!(updated.bundles[0].moved_from.as_deref(), Some(v1.as_str()));
}

#[test]
fn locked_mode_refuses_to_change_the_lock() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let obs = sb.bundle("obs", OBS);
    let root = sb.project(&use_git(&base, "v1"));

    let err = sb.compile(&root, Mode::Frozen).unwrap_err();
    assert_eq!(err.code, "lock_outdated");

    sb.compile(&root, Mode::UseLock).unwrap();
    fs::write(
        root.join("stack.toml"),
        format!("{}{}[override.env]\nLOG_LEVEL = \"warn\"\n", use_git(&base, "v1"), use_git(&obs, "v1")),
    )
    .unwrap();
    let err = sb.compile(&root, Mode::Frozen).unwrap_err();
    assert_eq!(err.code, "lock_outdated");
}

#[test]
fn conflicting_bundles_require_an_explicit_override() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let obs = sb.bundle("obs", OBS);
    let uses = format!("{}{}", use_git(&base, "v1"), use_git(&obs, "v1"));
    let root = sb.project(&uses);

    let err = sb.compile(&root, Mode::UseLock).unwrap_err();
    assert_eq!(err.code, "conflict");
    assert_eq!(err.details.len(), 1, "identical uv definitions must not conflict");
    assert_eq!(err.details[0]["key"], "LOG_LEVEL");
    assert!(!mise::output_path(&root).exists(), "nothing written on conflict");

    fs::write(root.join("stack.toml"), format!("{uses}[override.env]\nLOG_LEVEL = \"warn\"\n")).unwrap();
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(report.stack.env["LOG_LEVEL"].value, "warn");
    assert_eq!(report.stack.overrides[0].replaced, vec!["bundle:pybase", "bundle:obs"]);
}

#[test]
fn project_cannot_silently_redefine_a_bundle_value() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let root = sb.project(&format!("{}[env]\nLOG_LEVEL = \"trace\"\n", use_git(&base, "v1")));
    let err = sb.compile(&root, Mode::UseLock).unwrap_err();
    assert_eq!(err.code, "conflict");
    assert!(err.hint.unwrap().contains("[override.env] LOG_LEVEL"));
}

#[test]
fn project_can_add_its_own_service_and_pin_a_port() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let root = sb.project(&format!(
        "{}[services.api]\nrun = \"exec ./serve\"\nready_port = 8080\n\n\
         [override.services.postgres]\npreset = \"postgres\"\nversion = \"17\"\nport = 15432\n",
        use_git(&base, "v1")
    ));
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(report.stack.services["api"].origin, "project");
    let rendered = fs::read_to_string(mise::output_path(&root)).unwrap();
    assert!(rendered.contains("port = 15432"));
}

#[test]
fn bundles_may_not_pin_ports() {
    let sb = Sandbox::new();
    let manifest = PYBASE.replace("version = \"8\"", "version = \"8\"\nport = 6379");
    let base = sb.bundle("pybase", &manifest);
    let root = sb.project(&use_git(&base, "v1"));
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, "bundle_fixed_port");
}

#[test]
fn tasks_must_reference_defined_services() {
    let sb = Sandbox::new();
    let manifest = PYBASE.replace("services = [\"postgres\"]", "services = [\"mysql\"]");
    let base = sb.bundle("pybase", &manifest);
    let root = sb.project(&use_git(&base, "v1"));
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, "unknown_service");
}

#[test]
fn git_sources_must_be_pinned() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let root = sb.project(&format!("[[use]]\nbundle = \"git+file://{}\"\n", base.display()));
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, "ref_required");
}

#[test]
fn inspect_writes_nothing() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let root = sb.project(&use_git(&base, "v1"));
    sb.compile(&root, Mode::UseLock).unwrap();
    fs::remove_file(mise::output_path(&root)).unwrap();

    let report = compile(&sb.options(&root, Mode::Frozen, false)).unwrap();
    assert_eq!(report.stack.services.len(), 2);
    assert!(!mise::output_path(&root).exists());
}

#[test]
fn git_bundles_can_live_in_a_repository_subdirectory() {
    let sb = Sandbox::new();
    let repo = sb.path("bundles/mono");
    fs::create_dir_all(repo.join("bundles/obs")).unwrap();
    fs::write(repo.join("README"), "not a bundle\n").unwrap();
    fs::write(repo.join("bundles/obs/bundle.toml"), OBS).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    commit_all(&repo, "v1");
    let spec = format!("git+file://{}?ref=main&dir=bundles/obs", repo.display());
    let root = sb.project(&format!("[[use]]\nbundle = \"{spec}\"\n"));
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(report.bundles[0].name, "obs");
    assert!(report.bundles[0].dir.ends_with("bundles/obs"));

    // Same content through a path source hashes identically.
    let path_root = sb.path("by-path");
    fs::create_dir_all(&path_root).unwrap();
    fs::write(path_root.join("stack.toml"), format!("[[use]]\nbundle = \"path:{}\"\n", repo.join("bundles/obs").display())).unwrap();
    assert_eq!(sb.compile(&path_root, Mode::UseLock).unwrap().bundles[0].content_hash, report.bundles[0].content_hash);

    fs::write(root.join("stack.toml"), format!("[[use]]\nbundle = \"{spec}x\"\n")).unwrap();
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, "bundle_not_found");

    // A symlinked directory cannot point the bundle outside the checkout.
    let outside = sb.path("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("bundle.toml"), OBS).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("escape")).unwrap();
    commit_all(&repo, "symlink");
    let escape = format!("git+file://{}?ref=main&dir=escape", repo.display());
    fs::write(root.join("stack.toml"), format!("[[use]]\nbundle = \"{escape}\"\n")).unwrap();
    assert_eq!(sb.compile(&root, Mode::Update).unwrap_err().code, "bundle_not_found");
}

#[test]
fn a_project_may_use_no_bundles() {
    let sb = Sandbox::new();
    let root = sb.project("[tools]\njq = \"1.7.1\"\n[services.web]\nrun = \"exec ./serve\"\n");
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert!(report.bundles.is_empty());
    assert_eq!(report.stack.services["web"].origin, "project");
    assert!(report.warnings.is_empty());
}

#[test]
fn undocumented_service_presets_are_reported() {
    let sb = Sandbox::new();
    let root = sb.project("[services.db]\npreset = \"mysql\"\n[services.q]\npreset = \"nats\"\n");
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    assert!(report.warnings[0].starts_with("services.db (project) uses preset 'mysql'"));
    // Presets whose installed tool stack has not verified are never silently claimed pinned.
    assert!(report.warnings[1].contains("services.db (project) uses preset 'mysql'; stack does not know which tool"));
    assert_eq!(resolved(&report, "q"), "2.11.0");
    assert_eq!(sb.compile(&root, Mode::Frozen).unwrap_err().code, "unlocked_service");
}

#[test]
fn requests_that_name_no_release_are_reported() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let report = sb.compile(&sb.project(&use_git(&base, "v1")), Mode::UseLock).unwrap();
    assert!(report.warnings.is_empty(), "`latest` is pinned by the lock now: {:?}", report.warnings);
    let root = sb.path("app");
    fs::write(root.join("stack.toml"), "[tools]\nnode = \"system\"\n[services.db]\npreset = \"postgres\"\n").unwrap();
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("tools.node = \"system\" (project) names no release"));
    assert_eq!(resolved(&report, "db"), "17.2");
}

#[test]
fn inspect_previews_a_project_before_its_first_compile() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let root = sb.project(&use_git(&base, "v1"));
    let calls = sb.upstream.calls();
    let report = compile(&sb.options(&root, stack::project::inspect_mode(&root), false)).unwrap();
    assert_eq!(sb.upstream.calls(), calls, "inspect never resolves versions");
    assert!(report.versions.iter().all(|v| v.resolved.is_none()), "{:?}", report.versions);
    assert!(report.lock_changed);
    assert_eq!(report.stack.services.len(), 2);
    assert!(!root.join("stack.lock").exists());
    assert!(!mise::output_path(&root).exists());
}

fn lock_text(root: &Path) -> String {
    fs::read_to_string(root.join("stack.lock")).unwrap()
}

fn resolved(report: &Report, name: &str) -> String {
    report.versions.iter().find(|v| v.name == name).and_then(|v| v.resolved.clone()).unwrap()
}

#[test]
fn exact_tool_and_service_versions_survive_upstream_releases_and_a_fresh_cache() {
    let sb = Sandbox::new();
    let base = sb.bundle("pybase", PYBASE);
    let root = sb.project(&use_git(&base, "v1"));
    let first = sb.compile(&root, Mode::UseLock).unwrap();
    for (name, version) in [("python", "3.13.2"), ("uv", "0.12.1"), ("pitchfork", "2.29.0"), ("postgres", "17.2"), ("redis", "8.2.1")] {
        assert_eq!(resolved(&first, name), version, "{name}");
    }
    let locked = lock::read(&root).unwrap().unwrap();
    assert_eq!(locked.version, 2);
    let pg = locked.service("postgres").unwrap();
    assert_eq!((pg.tool.as_deref(), pg.requested.as_str(), pg.resolved.as_str()), (Some("postgres"), "17", "17.2"));
    assert_eq!(locked.tool("pitchfork").unwrap().resolved, "2.29.0", "provider tools are locked too");
    let pinned = lock_text(&root);
    let config = fs::read_to_string(mise::output_path(&root)).unwrap();

    // Upstream publishes newer releases that match every request.
    for (tool, v) in [("python", "3.13.9"), ("uv", "0.13.0"), ("postgres", "17.9"), ("redis", "8.4.0")] {
        sb.upstream.publish(tool, &[v]);
    }
    // A fresh machine: empty bundle cache, no generated config.
    fs::remove_dir_all(sb.path("cache")).unwrap();
    fs::remove_file(mise::output_path(&root)).unwrap();
    let calls = sb.upstream.calls();
    for mode in [Mode::UseLock, Mode::Frozen] {
        let again = sb.compile(&root, mode).unwrap();
        assert!(!again.lock_changed, "{mode:?}");
        assert_eq!(resolved(&again, "python"), "3.13.2");
    }
    assert_eq!(sb.upstream.calls(), calls, "unchanged requests are never re-resolved");
    assert_eq!(lock_text(&root), pinned);
    assert_eq!(fs::read_to_string(mise::output_path(&root)).unwrap(), config, "identical provider config");

    // Only an explicit update moves pins, and it reports each move.
    let updated = sb.compile(&root, Mode::Update).unwrap();
    assert!(updated.lock_changed);
    for (name, from, to) in [("python", "3.13.2", "3.13.9"), ("uv", "0.12.1", "0.13.0"), ("postgres", "17.2", "17.9"), ("redis", "8.2.1", "8.4.0")] {
        let v = updated.versions.iter().find(|v| v.name == name).unwrap();
        assert_eq!((v.resolved.as_deref(), v.moved_from.as_deref()), (Some(to), Some(from)), "{name}");
    }
    let pitchfork = updated.versions.iter().find(|v| v.name == "pitchfork").unwrap();
    assert_eq!(pitchfork.moved_from, None);
    let rendered = fs::read_to_string(mise::output_path(&root)).unwrap();
    assert!(rendered.contains("python = \"3.13.9\"") && rendered.contains("version = \"17.9\""), "{rendered}");
}

#[test]
fn changed_requests_resolve_on_compile_and_are_stale_in_locked_mode() {
    let sb = Sandbox::new();
    let root = sb.project("[tools]\npython = \"3.13\"\njq = \"1.7\"\n");
    sb.compile(&root, Mode::UseLock).unwrap();
    sb.upstream.publish("python", &["3.13.5"]);
    fs::write(root.join("stack.toml"), "[tools]\npython = \"3.12\"\njq = \"1.7\"\n").unwrap();
    let pinned = lock_text(&root);

    let err = sb.compile(&root, Mode::Frozen).unwrap_err();
    assert_eq!(err.code, "lock_outdated");
    assert_eq!(err.details[0]["name"], "python");
    assert_eq!(err.details[0]["locked"], "3.13");
    assert_eq!(lock_text(&root), pinned, "locked mode never writes");

    let calls = sb.upstream.calls();
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(sb.upstream.calls(), calls + 1, "only the changed request resolves");
    let python = report.versions.iter().find(|v| v.name == "python").unwrap();
    assert_eq!((python.resolved.as_deref(), python.moved_from.as_deref()), (Some("3.12.9"), Some("3.13.2")));
    assert_eq!(resolved(&report, "jq"), "1.7.1", "unchanged pin kept despite a newer 1.x");

    // A removed tool is a stale pin in locked mode.
    sb.compile(&root, Mode::UseLock).unwrap();
    let with_jq = lock_text(&root);
    fs::write(root.join("stack.toml"), "[tools]\npython = \"3.12\"\n").unwrap();
    assert_eq!(sb.compile(&root, Mode::Frozen).unwrap_err().code, "lock_outdated");
    assert_eq!(lock_text(&root), with_jq);
    assert!(sb.compile(&root, Mode::UseLock).unwrap().lock_changed);
    assert!(lock::read(&root).unwrap().unwrap().tool("jq").is_none());
}

#[test]
fn failed_resolution_changes_nothing_and_reports_every_request() {
    let sb = Sandbox::new();
    let root = sb.project("[tools]\npython = \"3.13\"\n");
    sb.compile(&root, Mode::UseLock).unwrap();
    let pinned = lock_text(&root);
    let config = fs::read_to_string(mise::output_path(&root)).unwrap();
    fs::write(root.join("stack.toml"), "[tools]\npython = \"3.99\"\nnosuchtool = \"1\"\n").unwrap();
    let err = sb.compile(&root, Mode::UseLock).unwrap_err();
    assert_eq!(err.code, "resolve_failed");
    assert_eq!(err.details.len(), 2, "{:?}", err.details);
    assert_eq!(lock_text(&root), pinned);
    assert_eq!(fs::read_to_string(mise::output_path(&root)).unwrap(), config);
}

#[test]
fn legacy_locks_keep_bundle_pins_and_migrate_only_through_compile() {
    let sb = Sandbox::new();
    let repo = sb.bundle("pybase", PYBASE);
    let v1 = git(&repo, &["rev-parse", "HEAD"]);
    let root = sb.project(&use_git(&repo, "v1"));
    sb.compile(&root, Mode::UseLock).unwrap();
    let hash = lock::read(&root).unwrap().unwrap().bundles[0].content_hash.clone();
    // What stack 0.1.3 wrote: bundle pins only.
    fs::write(
        root.join("stack.lock"),
        format!("version = 1\n\n[[bundle]]\nsource = \"git+file://{}?ref=v1\"\nname = \"pybase\"\ncommit = \"{v1}\"\ncontent_hash = \"{hash}\"\n", repo.display()),
    )
    .unwrap();
    let legacy = lock_text(&root);
    // The tag moves upstream; migration must not follow it.
    fs::write(repo.join("fixtures/seed.sql"), "select 2;\n").unwrap();
    commit_all(&repo, "v2");
    git(&repo, &["tag", "-f", "v1"]);

    let err = sb.compile(&root, Mode::Frozen).unwrap_err();
    assert_eq!(err.code, "lock_outdated");
    assert!(err.message.contains("version 1"), "{}", err.message);
    let inspect = compile(&sb.options(&root, stack::project::inspect_mode(&root), false)).unwrap_err();
    assert_eq!(inspect.code, "lock_outdated");
    assert_eq!(lock_text(&root), legacy, "locked operations never migrate");

    let migrated = sb.compile(&root, Mode::UseLock).unwrap();
    assert!(migrated.lock_changed);
    let lock = lock::read(&root).unwrap().unwrap();
    assert_eq!(lock.version, 2);
    assert_eq!(lock.bundles[0].commit.as_deref(), Some(v1.as_str()), "bundle pin kept");
    assert_eq!(lock.bundles[0].content_hash, hash);
    assert_eq!(lock.tool("python").unwrap().resolved, "3.13.2");
    sb.compile(&root, Mode::Frozen).unwrap();
}

#[test]
fn unsupported_or_inconsistent_lock_versions_are_rejected() {
    let sb = Sandbox::new();
    let root = sb.project("[tools]\njq = \"1.7\"\n");
    fs::write(root.join("stack.lock"), "version = 3\n").unwrap();
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, "lock_invalid");
    fs::write(root.join("stack.lock"), "version = 1\n[[tool]]\nname = \"jq\"\nrequested = \"1.7\"\nresolved = \"1.7.1\"\n").unwrap();
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, "lock_invalid");
}

#[test]
fn identity_probes_are_validated() {
    let sb = Sandbox::new();
    let root = sb.project("");
    for (toml, code) in [
        ("[services.db]\npreset='postgres'\nversion='17'\n[services.db.identity]\ncommand='true'\n", "invalid_service"),
        ("[services.w]\nrun='x'\n[services.w.identity]\ncommand='  '\n", "invalid_service"),
        ("[services.w]\nrun='x'\n[services.w.identity]\ncommand='p'\ntimeout='31s'\n", "invalid_service"),
        ("[services.w]\nrun='x'\n[services.w.identity]\ncommand='p'\ntimeout='0s'\n", "invalid_service"),
        ("[services.w]\nrun='x'\n[services.w.identity]\ncommand='p'\nexpect='x'\n", "manifest_invalid"),
        ("[services.a-b]\nrun='x'\n[services.a-b.identity]\ncommand='p'\n[services.a_b]\nrun='x'\n[services.a_b.identity]\ncommand='p'\n", "invalid_service"),
        ("[env]\nSTACK_IDENTITY_W='forged'\n[services.w]\nrun='x'\n[services.w.identity]\ncommand='p'\n", "invalid_env"),
    ] {
        fs::write(root.join("stack.toml"), toml).unwrap();
        assert_eq!(sb.compile(&root, Mode::UseLock).unwrap_err().code, code, "{toml}");
    }
    fs::write(root.join("stack.toml"), "[services.w]\nrun='x'\n[services.w.identity]\ncommand='p'\ntimeout='30s'\n").unwrap();
    let report = sb.compile(&root, Mode::UseLock).unwrap();
    assert_eq!(report.identities.len(), 1);
    // Tokens are per checkout and stable across compiles.
    assert_eq!(sb.compile(&root, Mode::UseLock).unwrap().identities, report.identities);
    let other = sb.path("other");
    fs::create_dir_all(&other).unwrap();
    fs::copy(root.join("stack.toml"), other.join("stack.toml")).unwrap();
    assert_ne!(sb.compile(&other, Mode::UseLock).unwrap().identities["w"], report.identities["w"]);
}

#[test]
fn damaged_release_pins_fail_offline_without_rewriting_outputs() {
    let sb = Sandbox::new();
    let root = sb.project("[tools]\npython='3.13'\n");
    sb.compile(&root, Mode::UseLock).unwrap();
    let original = fs::read_to_string(root.join("stack.lock")).unwrap();
    let provider = fs::read(mise::output_path(&root)).unwrap();
    let calls = sb.upstream.calls();
    for bad in ["latest", "3.13", "system", "path:/tmp/foreign", "", "lts", "prefix:3", "sub-1:latest"] {
        let damaged = original.replace("resolved = \"3.13.2\"", &format!("resolved = {bad:?}"));
        fs::write(root.join("stack.lock"), &damaged).unwrap();
        for mode in [Mode::Frozen, Mode::UseLock] {
            assert_eq!(sb.compile(&root, mode).unwrap_err().code, "lock_invalid", "{bad}");
        }
        assert_eq!(fs::read_to_string(root.join("stack.lock")).unwrap(), damaged);
        assert_eq!(fs::read(mise::output_path(&root)).unwrap(), provider);
        assert_eq!(sb.upstream.calls(), calls);
    }
}

#[test]
fn exact_release_rules_preserve_provider_specific_and_non_semver_releases() {
    for (tool, release) in [("python", "3.13.16"), ("python", "3.14.0rc1"), ("postgres", "17.11"),
        ("jq", "1.7"), ("go", "1.20"), ("java", "temurin-21.0.4+7"), ("github:vendor/tool", "20260918")] {
        assert!(mise::exact_release(tool, release), "{tool}@{release}");
    }
    for bad in ["3", "3.13", "latest", "stable", "nightly", "system", "ref:main", "lts-22", "sub-1", "3.*"] {
        assert!(!mise::exact_release("python", bad), "{bad}");
    }
}

#[test]
fn every_known_preset_and_omitted_version_gets_a_reusable_exact_pin() {
    let sb = Sandbox::new();
    let root = sb.project("[services.pg]\npreset='postgres'\n[services.redis]\npreset='redis'\n[services.cr]\npreset='cockroachdb'\n[services.nats]\npreset='nats'\n[services.spice]\npreset='spicedb'\n");
    let first = sb.compile(&root, Mode::UseLock).unwrap();
    for (name, version) in [("pg", "17.2"), ("redis", "8.2.1"), ("cr", "25.2.0"), ("nats", "2.11.0"), ("spice", "1.45.0")] {
        assert_eq!(resolved(&first, name), version);
    }
    for (tool, version) in [("postgres", "18.1"), ("redis", "9.0.0"), ("cockroach", "26.0.0"), ("nats-server", "3.0.0"), ("spicedb", "2.0.0")] {
        sb.upstream.publish(tool, &[version]);
    }
    let calls = sb.upstream.calls();
    let frozen = sb.compile(&root, Mode::Frozen).unwrap();
    assert_eq!(sb.upstream.calls(), calls);
    for name in ["pg", "redis", "cr", "nats", "spice"] { assert_eq!(resolved(&frozen, name), resolved(&first, name)); }
    let updated = sb.compile(&root, Mode::Update).unwrap();
    assert_eq!(resolved(&updated, "nats"), "3.0.0");
    assert_eq!(resolved(&updated, "pg"), "18.1");
}

//! End-to-end `stack compile` tests against real git repositories in temp dirs.

use stack::error::StackError;
use stack::lock;
use stack::project::{compile, Options, Report};
use stack::provider::mise;
use stack::source::Mode;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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
}

impl Sandbox {
    fn new() -> Self {
        Self { tmp: TempDir::new().unwrap() }
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
        compile(&Options {
            root: root.to_path_buf(),
            mode,
            write: true,
            cache: self.path("cache"),
        })
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
        "[daemons.postgres]",
        "preset = \"postgres\"",
        "port = \"auto\"",
        "daemons = [\"postgres\"]",
    ] {
        assert!(rendered.contains(expected), "missing `{expected}` in:\n{rendered}");
    }
    assert!(!rendered.contains("{{bundle_dir}}"));
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

    let report = compile(&Options {
        root: root.clone(),
        mode: Mode::Frozen,
        write: false,
        cache: sb.path("cache"),
    })
    .unwrap();
    assert_eq!(report.stack.services.len(), 2);
    assert!(!mise::output_path(&root).exists());
}

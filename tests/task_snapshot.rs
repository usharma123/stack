#![cfg(unix)]
//! A saved task plan, through the public API: planned, then a normal compile publishes a new
//! definition, then the plan runs. Its own test binary, because stack finds `mise` (here the
//! fake provider) on this process's PATH, which no other test may share.

use serde_json::{json, Value};
use stack::project::{self, Options};
use stack::session::{self, Ctx};
use stack::source::Mode;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

#[path = "support/python.rs"]
mod python;
const FAKE_FNOX: &str = include_str!("fakes/fnox.sh");
const SENTINELS: &[&str] = &["leak-sentinel-deploy-0001", "leak-sentinel-sentry-0002", "leak-sentinel-dependency-0003"];

fn write_exe(path: &Path, script: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `ASSET` is a template mise evaluates against the configuration that declares it; `KEPT` is
/// one the fake provider's planning environment does not report.
fn manifest(body: &str, secret: &str, asset: &str) -> String {
    format!("[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n[env]\nASSET = '{{{{ config_source }}}}-{asset}'\nKEPT = 'unplanned'\n[tasks.showenv]\nrun = '{body}'\nsecrets = ['{secret}']\n")
}

/// What `mise env` reports for the project's own configuration, as planning reads it.
fn planning_env(fixture: &Path, path: &str, asset: &str) {
    let env = json!({ "PATH": path, "ASSET": format!("{}/app/.config/mise/conf.d/stack.toml-{asset}", fixture.display()) });
    fs::write(fixture.join("env.json"), env.to_string()).unwrap();
}

fn run(ctx: &Ctx, plan: &session::ExecPlan, fixture: &Path) -> (String, String) {
    let result = stack::mcp::run_captured(ctx, plan, Duration::from_secs(20)).unwrap();
    assert_eq!(result["exit_code"], 0, "{result}");
    let text = result.to_string();
    for sentinel in SENTINELS {
        assert!(!text.contains(sentinel), "{sentinel} leaked: {text}");
    }
    let ran = fs::read_to_string(fixture.join("run-config")).unwrap();
    (result["stdout"].as_str().unwrap().to_string(), ran)
}

#[test]
fn a_saved_task_plan_runs_its_planned_definition_and_grant_after_a_later_compile() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = dir.path().canonicalize().unwrap();
    write_exe(&fixture.join("bin/mise"), FAKE_MISE);
    let install = fixture.join("installs/fnox/1.39.0");
    write_exe(&install.join("fnox"), FAKE_FNOX);
    fs::create_dir_all(install.join(".mise-bins")).unwrap();
    std::os::unix::fs::symlink(install.join("fnox"), install.join(".mise-bins/fnox")).unwrap();
    let rows = json!([{ "version": "1.39.0", "install_path": install, "installed": true, "active": true }]);
    fs::write(fixture.join("ls.json"), rows.to_string()).unwrap();
    let path = format!("{}:{}", fixture.join("bin").display(), std::env::var("PATH").unwrap());
    let stack_path = format!("{}:{path}", install.join(".mise-bins").display());
    planning_env(&fixture, &stack_path, "v1");
    fs::write(fixture.join("daemons.json"), "[]").unwrap();
    fs::create_dir_all(fixture.join("bundle/bin")).unwrap();
    fs::write(fixture.join("bundle/bundle.toml"), "[bundle]\nname='test'\n[paths]\nbin=['bin']\n").unwrap();
    fs::create_dir_all(fixture.join("app")).unwrap();
    // The only test in this binary: nothing else reads the environment while it changes.
    std::env::set_var("PATH", &path);
    std::env::set_var("REVIEW_FIXTURE", &fixture);
    std::env::set_var("REVIEW_PYTHON", python::python());

    let root = fixture.join("app");
    let ctx = Ctx { root: root.clone(), cache: fixture.join("cache"), state: fixture.join("state") };
    let opts = Options { root: root.clone(), mode: Mode::UseLock, write: true, cache: ctx.cache.clone(), state: ctx.state.clone(), reassign_ports: false, resolver: None, locker: None };
    fs::write(root.join("stack.toml"), manifest("old-definition", "DEPLOY_KEY", "v1")).unwrap();
    project::compile(&opts).unwrap();

    let saved = session::plan_task(&ctx, "showenv", &[], true).unwrap();
    assert_eq!(saved.secrets, ["DEPLOY_KEY"]);
    let copy = saved.task_config.as_ref().expect("a task plan holds its configuration copy");
    let held = fs::read_to_string(copy.config()).unwrap();
    assert!(held.contains("old-definition") && SENTINELS.iter().all(|s| !held.contains(s)), "{held}");
    assert_eq!(saved.env["MISE_GLOBAL_CONFIG_ROOT"], root.to_string_lossy());
    // A planned variable is declared as the value the command inherits, not as the template
    // mise would evaluate against the copy; one planning did not report, the `_.path`
    // directive and the tools are kept.
    let planned_asset = format!("{}/app/.config/mise/conf.d/stack.toml-v1", fixture.display());
    assert_eq!(saved.env["ASSET"], planned_asset);
    let doc: toml::Table = toml::from_str(&held).unwrap();
    let env = doc["env"].as_table().unwrap();
    assert_eq!(env["ASSET"].as_str(), Some(r#"{{ env["ASSET"] }}"#), "{held}");
    assert_eq!(env["KEPT"].as_str(), Some("unplanned"), "{held}");
    assert_eq!(env["_"]["path"][0].as_str(), Some(fixture.join("bundle/bin").to_str().unwrap()), "{held}");
    assert_eq!(doc["tools"]["fnox"].as_str(), Some("1.39.0"), "{held}");
    assert!(!held.contains(&planned_asset), "a planned value is never written into the copy: {held}");
    // Control: run at once.
    let (stdout, ran) = run(&ctx, &saved, &fixture);
    assert!(ran.contains("old-definition"), "{ran}");
    assert!(stdout.contains("\nDEPLOY_KEY=[redacted:DEPLOY_KEY]\n") && !stdout.contains("SENTRY_DSN="), "{stdout}");
    assert!(stdout.contains(&format!("\nASSET={planned_asset}\n")), "{stdout}");

    // A normal compile on another thread publishes a new body, grant and env value.
    fs::write(root.join("stack.toml"), manifest("new-definition", "SENTRY_DSN", "v2")).unwrap();
    planning_env(&fixture, &stack_path, "v2");
    let published = std::thread::spawn(move || project::compile(&opts)).join().unwrap().unwrap();
    assert!(fs::read_to_string(&published.output).unwrap().contains("new-definition"));

    // The saved plan still runs what it was planned and granted for, with its planned env.
    let (stdout, ran) = run(&ctx, &saved, &fixture);
    assert!(ran.contains("old-definition") && !ran.contains("new-definition"), "{ran}");
    assert!(stdout.contains("\nDEPLOY_KEY=[redacted:DEPLOY_KEY]\n") && !stdout.contains("SENTRY_DSN="), "{stdout}");
    assert!(stdout.contains(&format!("\nASSET={planned_asset}\n")) && !stdout.contains("-v2\n"), "{stdout}");

    // A plan made now runs the new definition with the new grant and env value.
    let fresh = session::plan_task(&ctx, "showenv", &[], true).unwrap();
    assert_eq!(fresh.secrets, ["SENTRY_DSN"]);
    let (stdout, ran) = run(&ctx, &fresh, &fixture);
    assert!(ran.contains("new-definition") && !ran.contains("config_source"), "{ran}");
    assert!(stdout.contains("\nSENTRY_DSN=[redacted:SENTRY_DSN]\n") && !stdout.contains("DEPLOY_KEY="), "{stdout}");
    assert!(stdout.contains("/app/.config/mise/conf.d/stack.toml-v2\n"), "{stdout}");

    // Deleted after planning: the plan made before still runs; planning now is refused.
    let opts = Options { root: root.clone(), mode: Mode::UseLock, write: true, cache: ctx.cache.clone(), state: ctx.state.clone(), reassign_ports: false, resolver: None, locker: None };
    fs::write(root.join("stack.toml"), "[[use]]\nbundle='path:../bundle'\n[tools]\nfnox = \"1.39.0\"\n").unwrap();
    project::compile(&opts).unwrap();
    let (_, ran) = run(&ctx, &fresh, &fixture);
    assert!(ran.contains("new-definition"), "{ran}");
    let refused = session::plan_task(&ctx, "showenv", &[], true).err().expect("a deleted task is refused");
    assert_eq!(refused.code, "unknown_task");

    let copies = [saved.task_config.as_ref().unwrap().dir().to_path_buf(), fresh.task_config.as_ref().unwrap().dir().to_path_buf()];
    drop(saved);
    drop(fresh);
    for copy in copies {
        assert!(!copy.exists(), "{} outlived its plan", copy.display());
    }
    let left: Vec<Value> = fs::read_dir(ctx.cache.join("task-config")).unwrap().map(|e| json!(e.unwrap().path())).collect();
    assert!(left.is_empty(), "{left:?}");
}

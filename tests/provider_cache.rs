//! Stack's kept provider answers (`src/provider/env_cache.rs`) taken through commands. The
//! shared fake mise is a script, which Stack never caches answers for (a script runs another
//! file it cannot identify), so here it runs behind a native stand-in: a small executable,
//! built with the system's C compiler, that executes it. The test is skipped without one.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;
use tempfile::TempDir;

#[path = "support/python.rs"]
mod python;

const FAKE_MISE: &str = include_str!("fakes/mise.sh");

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    /// A checkout pinning jq 1.7.1 (locked with aqua), with `mise` a native executable running
    /// the fake. `None` when no C compiler can build it.
    fn new() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        for name in ["bin", "app"] {
            fs::create_dir(base.join(name)).unwrap();
        }
        let script = base.join("fake-mise.sh");
        fs::write(&script, FAKE_MISE).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let source = base.join("mise.c");
        fs::write(&source, format!("#include <unistd.h>\nint main(int argc, char **argv) {{ (void)argc; execv(\"{}\", argv); return 127; }}\n", script.display())).unwrap();
        let built = Command::new("cc").arg("-o").arg(base.join("bin/mise")).arg(&source).output();
        if !built.is_ok_and(|out| out.status.success()) {
            eprintln!("skipping: no C compiler to build a native mise stand-in");
            return None;
        }
        fs::write(base.join("app/stack.toml"), "[tools]\njq = \"1.7.1\"\n\n[env]\nGREETING = \"hello\"\n").unwrap();
        // What `mise lock` writes for the pin (the fake copies it), and what mise answers.
        fs::write(base.join("mise-lock.toml"), "lockfile_version = 3\n\n[[tools.jq]]\nversion = \"1.7.1\"\nbackend = \"aqua:jqlang/jq\"\n").unwrap();
        fs::write(base.join("daemons.json"), "[]").unwrap();
        let fixture = Self { dir };
        let env = serde_json::json!({ "PATH": format!("{}:{}", fixture.install().join("bin").display(), fixture.path()), "GREETING": "hello" });
        fs::write(fixture.base().join("env.json"), env.to_string()).unwrap();
        fixture.ok(&["compile"]);
        Some(fixture)
    }

    fn base(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn install(&self) -> PathBuf {
        self.base().join("installs/jq/1.7.1")
    }

    /// The PATH Stack runs with: the stand-in first.
    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.base().join("bin").display())
    }

    fn run(&self, args: &[&str]) -> Output {
        // `STACK_TEST_BIN` runs another build (to show the test fails without its fix).
        Command::new(std::env::var("STACK_TEST_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_stack").to_string()))
            .args(["-C", self.base().join("app").to_str().unwrap()])
            .args(args)
            .env_clear()
            .env("PATH", self.path())
            .env("HOME", self.base())
            .env("REVIEW_FIXTURE", self.base())
            .env("REVIEW_PYTHON", python::python())
            .env("STACK_STATE_DIR", self.base().join("state"))
            .env("STACK_CACHE_DIR", self.base().join("cache"))
            .env("MISE_DATA_DIR", self.base().join("mise-data"))
            .env("MISE_STATE_DIR", self.base().join("mise-state"))
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(out.status.success(), "stack {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }

    /// What mise was asked since the last call, one request per line.
    fn asked(&self) -> Vec<String> {
        let log = self.base().join("mise.log");
        let text = fs::read_to_string(&log).unwrap_or_default();
        fs::write(&log, "").unwrap();
        text.lines().map(String::from).collect()
    }

    /// Install the pin, as mise would: its directory, and the fake's record of it.
    fn put_install(&self) {
        fs::create_dir_all(self.install().join("bin")).unwrap();
        fs::write(self.install().join("bin/jq"), "#!/bin/sh\necho jq-1.7.1\n").unwrap();
        fs::set_permissions(self.install().join("bin/jq"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn uninstall(&self) {
        fs::remove_dir_all(self.install()).unwrap();
        fs::write(self.base().join("installed"), "").unwrap();
    }

    fn kept(&self) -> bool {
        fs::read_dir(self.base().join("state/projects"))
            .map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".provider-env.json")))
            .unwrap_or(false)
    }
}

fn asks_mise(asked: &[String], what: &str) -> bool {
    asked.iter().any(|line| line.starts_with(what))
}

#[test]
fn a_kept_answer_serves_exec_status_env_up_and_install_until_the_release_is_uninstalled() {
    let Some(fx) = Fixture::new() else { return };
    fx.put_install();
    fx.ok(&["install"]);
    // Kept only once what it depends on settled (see `env_cache::SETTLE`).
    std::thread::sleep(Duration::from_millis(2300));
    fx.ok(&["exec", "--", "true"]);
    fx.ok(&["install"]);
    assert!(fx.kept(), "nothing was kept");
    fx.asked();

    // Each command now answers from it: mise is not asked for the environment, the installed
    // releases, or to install.
    let out = fx.ok(&["exec", "--", "sh", "-c", "echo \"$PATH|$GREETING\""]);
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(printed.starts_with(&format!("{}/bin:", fx.install().display())) && printed.trim_end().ends_with("|hello"), "{printed}");
    fx.ok(&["status", "--json"]);
    let out = fx.ok(&["env"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&fx.install().join("bin").display().to_string()));
    fx.ok(&["up"]);
    let out = fx.ok(&["install", "--json"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("every pinned release is already installed"));
    let asked = fx.asked();
    for what in ["env --json", "ls --json", "install", "version"] {
        assert!(!asks_mise(&asked, what), "mise was asked `{what}`: {asked:?}");
    }

    // Uninstalled: the kept answer no longer matches, mise is asked, and exec refuses rather
    // than run whatever PATH holds.
    fx.uninstall();
    let out = fx.run(&["exec", "--json", "--", "true"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("tools_not_installed"), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(asks_mise(&fx.asked(), "ls --json"));
    let out = fx.run(&["env"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("tools_not_installed") || !out.status.success());
    assert!(asks_mise(&fx.asked(), "env --json"));

    // Installed again: served again, from mise first.
    fx.put_install();
    fx.ok(&["install"]);
    fx.ok(&["exec", "--", "true"]);
    let asked = fx.asked();
    assert!(asks_mise(&asked, "env --json"), "{asked:?}");
}

/// The same stand-in, made a script again: nothing is kept, every command asks mise.
#[test]
fn a_mise_that_is_a_script_is_asked_every_time() {
    let Some(fx) = Fixture::new() else { return };
    fs::remove_file(fx.base().join("bin/mise")).unwrap();
    fs::copy(fx.base().join("fake-mise.sh"), fx.base().join("bin/mise")).unwrap();
    fx.put_install();
    fx.ok(&["install"]);
    std::thread::sleep(Duration::from_millis(2300));
    fx.ok(&["exec", "--", "true"]);
    fx.asked();
    fx.ok(&["exec", "--", "true"]);
    assert!(asks_mise(&fx.asked(), "env --json"));
    assert!(!fx.kept());
}

/// A release that is a link elsewhere (`mise link`): once its target is gone, mise reports it
/// not installed and exec refuses, whatever ran before.
#[test]
fn a_linked_release_whose_target_goes_is_refused() {
    let Some(fx) = Fixture::new() else { return };
    let target = fx.base().join("elsewhere/jq-1.7.1");
    fs::create_dir_all(target.join("bin")).unwrap();
    fs::write(target.join("bin/jq"), "#!/bin/sh\necho jq\n").unwrap();
    fs::create_dir_all(fx.install().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, fx.install()).unwrap();
    fx.ok(&["install"]);
    std::thread::sleep(Duration::from_millis(2300));
    fx.ok(&["exec", "--", "true"]);
    fx.ok(&["exec", "--", "true"]);
    // The target goes; mise (here, the fake's record) no longer lists the release installed.
    fs::remove_dir_all(&target).unwrap();
    fs::write(fx.base().join("installed"), "").unwrap();
    let out = fx.run(&["exec", "--json", "--", "true"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stdout).contains("tools_not_installed"), "{}", String::from_utf8_lossy(&out.stdout));
}

#![cfg(unix)]
//! Template syntax in a service version never reaches a mise configuration.
//!
//! mise renders `[tools]` versions as templates (`exec()` included) whenever it loads a
//! config, and stack's scratch configurations are trusted. A preset service's version becomes
//! such a `[tools]` version, so one with template syntax would run its command as soon as stack
//! resolved, locked or discovered the release. These tests drive the stack binary against a
//! fake mise that logs every call and records any configuration it could load that carries
//! template syntax. The payload would touch `SENTINEL-RAN` under a real mise.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const FAKE_MISE: &str = r#"#!/bin/sh
echo "$*" >>"$FIXTURE/mise.log"
for config in .config/mise/conf.d/stack.toml "$MISE_GLOBAL_CONFIG_FILE"; do
  if test -f "$config" && grep -qE '\{\{|\{%|\{#' "$config"; then
    { echo "$(pwd -P)/$config ($*):"; cat "$config"; } >>"$FIXTURE/TEMPLATE-SEEN"
  fi
done
case "$1 $2" in
  'latest '*)
    case "$2" in postgres@17) echo 17.11 ;; jq@1.7) echo 1.7.1 ;; *) echo "${2#*@}" ;; esac ;;
  'version ') echo '2026.10.3 macos-arm64 (2026-10-05)' ;;
  'env --json') printf '{"PATH":"%s"}\n' "$PATH" ;;
  'daemons --json') echo '[]' ;;
  'ls --json') echo '{}' ;;
  'skills ls') echo '[]' ;;
esac
"#;

/// Every place a service can be declared, as `(stack.toml, bundle.toml)`; `@V@` is the version.
const DECLARATIONS: [(&str, &str, Option<&str>); 3] = [
    ("project", "[services.db]\npreset = \"postgres\"\nversion = \"@V@\"\n", None),
    (
        "bundle:pg",
        "[[use]]\nbundle = \"path:../pg\"\n",
        Some("[bundle]\nname = \"pg\"\n\n[services.db]\npreset = \"postgres\"\nversion = \"@V@\"\n"),
    ),
    (
        "[override.services]",
        "[[use]]\nbundle = \"path:../pg\"\n\n[override.services.db]\npreset = \"postgres\"\nversion = \"@V@\"\n",
        Some("[bundle]\nname = \"pg\"\n\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n"),
    ),
];

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(project: &str, bundle: Option<&str>) -> Self {
        let f = Self { dir: tempfile::tempdir().unwrap() };
        for sub in ["bin", "app", "home", "pg"] {
            fs::create_dir(f.path(sub)).unwrap();
        }
        let mise = f.path("bin/mise");
        fs::write(&mise, FAKE_MISE).unwrap();
        fs::set_permissions(&mise, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(f.path("app/stack.toml"), project).unwrap();
        if let Some(bundle) = bundle {
            fs::write(f.path("pg/bundle.toml"), bundle).unwrap();
        }
        f
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// A payload that would leave `SENTINEL-RAN` behind if anything evaluated it.
    fn payload(&self, open: &str) -> String {
        let sentinel = self.path("SENTINEL-RAN");
        match open {
            "{{" => format!("{{{{ exec(command='touch {}') }}}}", sentinel.display()),
            "{%" => format!("{{% set _ = exec(command='touch {}') %}}17", sentinel.display()),
            _ => format!("17{{# {} #}}", sentinel.display()),
        }
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_stack"))
            .args(["--json", "-C", self.path("app").to_str().unwrap()])
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", self.path("bin").display()))
            .env("HOME", self.path("home"))
            .env("FIXTURE", self.dir.path())
            .env("STACK_DATA_DIR", self.path("data"))
            .env("STACK_STATE_DIR", self.path("state"))
            .env("STACK_CACHE_DIR", self.path("cache"))
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)));
        assert_eq!(out.status.success(), v["ok"] == true, "{args:?}: {v}");
        v
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    /// No evaluation could have happened: no configuration mise could load carried template
    /// syntax, and the payload never ran.
    fn assert_never_evaluated(&self, context: &str) {
        assert_eq!(self.read("TEMPLATE-SEEN"), "", "{context}: mise could load a templated config");
        assert!(!self.path("SENTINEL-RAN").exists(), "{context}: the payload ran");
        let scratch = files(&self.path("cache")).into_iter().filter(|(p, _)| p.ends_with("stack.toml"));
        for (path, text) in scratch {
            assert!(!["{{", "{%", "{#"].iter().any(|t| text.contains(t)), "{context}: {} carries {text}", path.display());
        }
    }

    /// The project's files, by path and contents.
    fn outputs(&self) -> BTreeMap<PathBuf, String> {
        files(&self.path("app"))
    }
}

/// Every file under `dir`, by path and contents.
fn files(dir: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.insert(path.clone(), String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned());
        }
    }
    out
}

const DELIMITERS: [&str; 3] = ["{{", "{%", "{#"];

#[test]
fn templated_service_versions_are_refused_before_mise_runs_or_anything_is_written() {
    for (origin, project, bundle) in DECLARATIONS {
        for open in DELIMITERS {
            // Locked commands need a stack.lock, so the project first compiles at a literal
            // version and then changes to the templated one.
            let f = Fixture::new(&project.replace("@V@", "17"), bundle.map(|b| b.replace("@V@", "17")).as_deref());
            assert_eq!(f.json(&["compile"])["ok"], true);
            let payload = f.payload(open);
            assert!(stack::tool::templated(&payload), "{payload}");
            fs::write(f.path("app/stack.toml"), project.replace("@V@", &payload)).unwrap();
            if let Some(bundle) = bundle {
                fs::write(f.path("pg/bundle.toml"), bundle.replace("@V@", &payload)).unwrap();
            }
            fs::remove_dir_all(f.path("cache")).ok();
            fs::remove_file(f.path("mise.log")).unwrap();
            let before = f.outputs();
            for command in [&["compile"][..], &["compile", "--update"], &["inspect"], &["install"], &["up"], &["exec", "--", "true"], &["status"]] {
                let context = format!("{origin} {open} {command:?}");
                let v = f.json(command);
                let message = v["error"]["message"].as_str().unwrap();
                if origin.starts_with("bundle:") && command[0] != "compile" {
                    // Locked commands refuse a bundle that changed since stack.lock before
                    // they read it.
                    assert_eq!(v["error"]["code"], "lock_outdated", "{context}: {v}");
                } else {
                    assert_eq!(v["error"]["code"], "invalid_service", "{context}: {v}");
                    assert!(message.starts_with(&format!("service 'db' in {origin}: `version` must not contain template syntax")), "{context}: {message}");
                }
                assert_eq!(f.read("mise.log"), "", "{context}: mise ran");
                assert_eq!(f.outputs(), before, "{context}: project files changed");
                assert!(files(&f.path("cache")).is_empty(), "{context}: {:?}", files(&f.path("cache")));
                f.assert_never_evaluated(&context);
            }
        }
    }
}

/// A stack.lock edited by hand is the other way a templated version could arrive: the
/// manifest is literal and the pin is not. No command passes it to mise.
#[test]
fn templated_pins_in_an_edited_lock_never_reach_mise() {
    let project = "[tools]\njq = \"1.7\"\n\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n";
    for (pinned, quoted) in [("17.11", "\"17.11\""), ("1.7.1", "\"1.7.1\"")] {
        for open in DELIMITERS {
            let f = Fixture::new(project, None);
            let v = f.json(&["compile"]);
            assert_eq!(v["ok"], true, "{v}");
            f.assert_never_evaluated("literal compile");
            let lock = f.read("app/stack.lock");
            assert!(lock.contains(quoted), "{lock}");
            let payload = f.payload(open);
            fs::write(f.path("app/stack.lock"), lock.replacen(quoted, &format!("{:?}", payload), 1)).unwrap();
            for command in [&["compile"][..], &["inspect"], &["install"], &["up"], &["exec", "--", "true"], &["status"]] {
                let context = format!("{pinned} {open} {command:?}");
                let before = f.outputs();
                let v = f.json(command);
                if v["ok"] != true {
                    assert_eq!(v["error"]["code"], "lock_invalid", "{context}: {v}");
                    assert_eq!(f.outputs(), before, "{context}: a refused command changed project files");
                }
                f.assert_never_evaluated(&context);
            }
        }
    }
}

/// The resolver writes its own scratch configuration, so the guard holds for a caller that
/// skips manifest validation altogether. Run against the fake mise on this process's PATH: a
/// literal request reaches it, a templated one does not.
#[test]
fn the_resolver_never_asks_mise_about_a_templated_request() {
    use stack::provider::mise::{MiseResolver, Resolver};
    use stack::tool::{OptionValue, ToolSpec};
    let f = Fixture::new("", None);
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", f.path("bin").display()));
    std::env::set_var("FIXTURE", f.dir.path());
    let resolver = MiseResolver { cache: f.path("cache") };
    assert_eq!(resolver.resolve_spec("postgres", &ToolSpec::new("17")).unwrap(), "17.11");
    assert_eq!(f.read("mise.log"), "latest postgres@17\n", "the fake mise is the one on PATH");
    fs::remove_file(f.path("mise.log")).unwrap();
    for open in DELIMITERS {
        let payload = f.payload(open);
        let mut option = ToolSpec::new("1.39.0");
        option.options.insert("identity".into(), OptionValue::String(payload.clone()));
        for (tool, spec) in [("postgres", ToolSpec::new(payload.clone())), ("fnox", option)] {
            let e = resolver.resolve_spec(tool, &spec).unwrap_err();
            assert_eq!(e.code, "invalid_tool", "{spec:?}");
            assert!(e.message.contains("must not contain template syntax"), "{}", e.message);
        }
        let e = resolver.resolve("redis", &payload).unwrap_err();
        assert_eq!(e.code, "invalid_tool");
    }
    assert_eq!(f.read("mise.log"), "", "mise ran for a templated request");
    f.assert_never_evaluated("direct resolver");
    // Each query's root is gone, refused or not.
    let leftovers: Vec<_> = fs::read_dir(f.path("cache/resolve")).unwrap().collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

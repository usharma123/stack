#![cfg(unix)]
//! Agent skills through the stack binary, against a fake mise that answers `ls --json` and
//! `skills ls --json` from files and logs the configuration each query could see.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const FAKE_MISE: &str = r#"#!/bin/sh
echo "$*" >>"$SKILLS_FIXTURE/mise.log"
case "$1 $2" in
  'latest '*)
    v=${2#*@}
    case "$2" in fnox@1.39) v=1.39.0 ;; postgres@17) v=17.11 ;; esac
    echo "$v" ;;
  'version ') echo '2026.10.3 macos-arm64 (2026-10-05)' ;;
  'env --json') printf '{"PATH":"%s","PITCHFORK_STATE_DIR":"/tmp/stack-skills-pf"}\n' "$PATH" ;;
  'daemons --json') echo '[]' ;;
  'ls --json'|'skills ls')
    kind=$1
    mkdir -p "$SKILLS_FIXTURE/queries"
    { echo "call=$kind dir=$(pwd -P) trusted=${MISE_TRUSTED_CONFIG_PATHS-unset} filenames=${MISE_OVERRIDE_CONFIG_FILENAMES-unset}"; cat .config/mise/conf.d/stack.toml; echo '--end'; } >"$SKILLS_FIXTURE/queries/$kind-$$"
    if test -f "$SKILLS_FIXTURE/$kind-sleep"; then sleep "$(cat "$SKILLS_FIXTURE/$kind-sleep")"; fi
    if test -f "$SKILLS_FIXTURE/$kind-hang"; then echo $$ >"$SKILLS_FIXTURE/$kind-hang-pid"; exec sleep 60; fi
    if test -f "$SKILLS_FIXTURE/$kind-fail"; then echo "error: unrecognized subcommand '$kind' PROVIDER-NOISE" >&2; echo PROVIDER-NOISE; exit 2; fi
    if test -f "$SKILLS_FIXTURE/$kind-garbage"; then echo 'PROVIDER-NOISE not json'; exit 0; fi
    sed "s|@STORE@|$SKILLS_FIXTURE/store|g" "$SKILLS_FIXTURE/$kind.json" ;;
esac
"#;

/// A project using fnox, mbx and jq, with a postgres service (so stack adds Pitchfork), and
/// an `[env]` template that must never be evaluated by discovery.
const PROJECT: &str = r#"[tools]
fnox = "1.39"
mbx = "1.22.0"
jq = "1.7.1"

[services.db]
preset = "postgres"
version = "17"

[env]
SENTINEL = "{{ exec(command='touch SENTINEL-RAN') }}"
"#;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(project: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let f = Self { dir };
        for sub in ["bin", "app", "home"] {
            fs::create_dir(f.path(sub)).unwrap();
        }
        let mise = f.path("bin/mise");
        fs::write(&mise, FAKE_MISE).unwrap();
        fs::set_permissions(&mise, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(f.path("app/stack.toml"), project).unwrap();
        // Installed releases: the pinned ones, plus another fnox and a tool nobody pins.
        let store = f.path("store");
        for (tool, version, skills) in [
            ("fnox", "1.39.0", &["fnox"][..]),
            ("fnox", "1.38.0", &["fnox-old"][..]),
            ("mbx", "1.22.0", &["mbx", "mbx-advanced"][..]),
            ("jq", "1.7.1", &[][..]),
            ("pitchfork", "2.29.0", &["pitchfork"][..]),
            ("other", "1.0.0", &["other"][..]),
        ] {
            let install = store.join(tool).join(version);
            fs::create_dir_all(&install).unwrap();
            for name in skills {
                let skill = install.join(".mise-packslip/repo/skills").join(name);
                fs::create_dir_all(&skill).unwrap();
                fs::write(skill.join("SKILL.md"), format!("# {name} {version}\nDocumentation only. SENTINEL-DOC\n")).unwrap();
            }
        }
        let release = |tool: &str, version: &str, installed: bool| {
            json!({ "version": version, "requested_version": version, "install_path": format!("@STORE@/{tool}/{version}"), "installed": installed, "active": installed })
        };
        let ls = json!({
            "fnox": [release("fnox", "1.38.0", true), release("fnox", "1.39.0", true)],
            "mbx": [release("mbx", "1.22.0", true)],
            "jq": [release("jq", "1.7.1", true)],
            "pitchfork": [release("pitchfork", "2.29.0", true)],
            "postgres": [release("postgres", "17.11", false)],
            "other": [release("other", "1.0.0", true)],
        });
        fs::write(f.path("ls.json"), ls.to_string()).unwrap();
        let row = |tool: &str, version: &str, name: &str| {
            json!({ "name": name, "tool": tool, "version": version, "path": format!("@STORE@/{tool}/{version}/.mise-packslip/repo/skills/{name}") })
        };
        let skills = json!([
            row("fnox", "1.38.0", "fnox-old"),
            row("fnox", "1.39.0", "fnox"),
            row("mbx", "1.22.0", "mbx"),
            row("mbx", "1.22.0", "mbx-advanced"),
            row("pitchfork", "2.29.0", "pitchfork"),
            row("other", "1.0.0", "other"),
        ]);
        fs::write(f.path("skills.json"), skills.to_string()).unwrap();
        f.ok(&["compile"]);
        f
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn command(&self, args: &[&str]) -> Command {
        self.command_at(&self.path("app"), args)
    }

    fn command_at(&self, dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stack"));
        command
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", self.path("bin").display()))
            .env("HOME", self.path("home"))
            .env("SKILLS_FIXTURE", self.dir.path())
            .env("STACK_DATA_DIR", self.path("data"))
            .env("STACK_STATE_DIR", self.path("state"))
            .env("STACK_CACHE_DIR", self.path("cache"));
        command
    }

    fn ok(&self, args: &[&str]) -> Output {
        let out = self.command(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {} {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend(args);
        let out = self.command(&all).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
        assert_eq!(out.status.success(), v["ok"] == true, "{v}");
        v
    }

    fn mise_log(&self) -> String {
        fs::read_to_string(self.path("mise.log")).unwrap_or_default()
    }

    /// What each provider query saw, one record per call.
    fn queries(&self) -> String {
        let Ok(dir) = fs::read_dir(self.path("queries")) else { return String::new() };
        dir.map(|e| fs::read_to_string(e.unwrap().path()).unwrap()).collect()
    }

    fn forget_queries(&self) {
        fs::remove_dir_all(self.path("queries")).ok();
    }

    fn touch(&self, name: &str) {
        fs::write(self.path(name), "").unwrap();
    }

    /// Every path under the project, with file contents and link targets.
    fn snapshot(&self) -> BTreeMap<PathBuf, String> {
        fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<PathBuf, String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                if meta.file_type().is_symlink() {
                    out.insert(rel, format!("-> {}", fs::read_link(&path).unwrap().display()));
                } else if meta.is_dir() {
                    out.insert(rel, "dir".into());
                    walk(&path, base, out);
                } else {
                    out.insert(rel, format!("{:?} {}", meta.modified().unwrap(), String::from_utf8_lossy(&fs::read(&path).unwrap())));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.path("app"), &self.path("app"), &mut out);
        out
    }
}

fn statuses(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|s| format!("{}@{} {} {}", s["tool"].as_str().unwrap(), s["version"].as_str().unwrap_or("-"), s["status"].as_str().unwrap(), s["name"].as_str().unwrap_or("-")))
        .collect()
}

const EXPECTED: [&str; 5] = [
    "fnox@1.39.0 available fnox",
    "mbx@1.22.0 available mbx",
    "mbx@1.22.0 available mbx-advanced",
    "jq@1.7.1 no_skill -",
    "postgres@17.11 not_installed -",
];

#[test]
fn inspect_lists_the_skills_of_exactly_the_locked_releases() {
    let f = Fixture::new(PROJECT);
    let v = f.json(&["inspect"]);
    let data = &v["data"];
    assert_eq!(statuses(&data["skills"]), EXPECTED, "{data}");
    assert!(data.get("provider_skills").is_none(), "provider skills listed without asking");
    assert!(data["warnings"].as_array().is_none_or(|w| w.iter().all(|w| !w.as_str().unwrap().starts_with("skills_"))), "{data}");
    let fnox = &data["skills"][0];
    assert_eq!(fnox["origin"], "project");
    let dir = f.path("store/fnox/1.39.0/.mise-packslip/repo/skills/fnox");
    assert_eq!(fnox["directory"], dir.to_str().unwrap());
    assert_eq!(fnox["entrypoint"], dir.join("SKILL.md").to_str().unwrap());
    let db = &data["skills"][4];
    assert_eq!(db["services"], json!(["db"]));
    assert!(db["reason"].as_str().unwrap().contains("stack install"));

    // People can ask for the provider's too; they are listed apart.
    let all = f.json(&["inspect", "--all-skills"]);
    assert_eq!(statuses(&all["data"]["provider_skills"]), ["pitchfork@2.29.0 available pitchfork"]);
    assert_eq!(all["data"]["provider_skills"][0]["origin"], "provider");
    assert_eq!(statuses(&all["data"]["skills"]), EXPECTED);

    // compile --json reports them too.
    assert_eq!(statuses(&f.json(&["compile"])["data"]["skills"]), EXPECTED);
}

#[test]
fn discovery_sees_only_the_locks_pins_in_a_scratch_root_and_writes_nothing() {
    let f = Fixture::new(PROJECT);
    let before = f.snapshot();
    let calls = f.mise_log().lines().count();
    f.forget_queries();
    let v = f.json(&["inspect"]);
    assert_eq!(statuses(&v["data"]["skills"]), EXPECTED);
    assert_eq!(f.snapshot(), before, "inspect changed the project");
    let new_calls: Vec<String> = f.mise_log().lines().skip(calls).map(String::from).collect();
    assert_eq!(new_calls.len(), 2, "{new_calls:?}");
    assert!(new_calls.iter().all(|c| c == "ls --json" || c == "skills ls --json"), "{new_calls:?}");
    let log = f.queries();
    let configs: Vec<&str> = log.split("--end\n").filter(|c| !c.trim().is_empty()).collect();
    assert_eq!(configs.len(), 2, "{log}");
    for c in configs {
        let (head, config) = c.split_once('\n').unwrap();
        let dir = head.split(" dir=").nth(1).unwrap().split(' ').next().unwrap();
        assert!(dir.starts_with(f.path("cache/skills").canonicalize().unwrap().to_str().unwrap()), "{head}");
        assert!(head.contains(&format!("trusted={}", Path::new(dir).display())) || head.contains("trusted=/"), "{head}");
        assert!(head.contains("filenames=.config/mise/conf.d/stack.toml"), "{head}");
        let doc: toml::Table = toml::from_str(config).unwrap();
        assert_eq!(doc.keys().collect::<Vec<_>>(), ["tools"], "{config}");
        let tools = doc["tools"].as_table().unwrap();
        assert_eq!(tools["fnox"].as_str(), Some("1.39.0"), "exact pin, not the request: {config}");
        assert_eq!(tools["postgres"].as_str(), Some("17.11"), "service pins under their preset tool: {config}");
        assert_eq!(tools["pitchfork"].as_str(), Some("2.29.0"));
        assert!(!config.contains("SENTINEL") && !config.contains("exec("), "{config}");
    }
    assert!(!f.path("app/SENTINEL-RAN").exists());
    assert_eq!(fs::read_dir(f.path("cache/skills")).unwrap().count(), 0, "scratch roots left behind");
}

#[test]
fn a_fresh_or_stale_generated_config_gives_the_same_answer() {
    let f = Fixture::new(PROJECT);
    let generated = f.path("app/.config/mise/conf.d/stack.toml");
    // Stale: names other releases than stack.lock pins.
    fs::write(&generated, "[tools]\nfnox = \"1.38.0\"\nother = \"1.0.0\"\n").unwrap();
    assert_eq!(statuses(&f.json(&["inspect"])["data"]["skills"]), EXPECTED);
    assert_eq!(fs::read_to_string(&generated).unwrap(), "[tools]\nfnox = \"1.38.0\"\nother = \"1.0.0\"\n", "inspect rewrote it");
    // Fresh worktree: stack.lock committed, nothing generated.
    fs::remove_dir_all(f.path("app/.config")).unwrap();
    assert_eq!(statuses(&f.json(&["inspect"])["data"]["skills"]), EXPECTED);
    assert!(!f.path("app/.config").exists());
}

#[test]
fn concurrent_inspects_use_distinct_scratch_roots() {
    let f = Fixture::new(PROJECT);
    fs::write(f.path("ls-sleep"), "1").unwrap();
    f.forget_queries();
    let children: Vec<_> = (0..2).map(|_| f.command(&["--json", "inspect"]).stdout(Stdio::piped()).spawn().unwrap()).collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(statuses(&v["data"]["skills"]), EXPECTED);
    }
    let log = f.queries();
    let mut dirs: Vec<&str> = log.lines().filter_map(|l| l.split(" dir=").nth(1)?.split(' ').next()).collect();
    assert_eq!(dirs.len(), 4, "{log}");
    dirs.sort();
    dirs.dedup();
    assert_eq!(dirs.len(), 2, "each inspect shares one root between its two queries: {log}");
    assert!(dirs.iter().all(|d| !Path::new(d).exists()));
}

fn assert_unavailable(f: &Fixture, why: &str) -> Duration {
    let start = Instant::now();
    let out = f.command(&["--json", "inspect"]).output().unwrap();
    let elapsed = start.elapsed();
    assert!(out.status.success(), "{why}: {}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let skills = v["data"]["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 4, "{why}: one entry per pinned release: {v}");
    assert!(skills.iter().all(|s| s["status"] == "unavailable"), "{why}: {v}");
    let warnings: Vec<&str> = v["data"]["warnings"].as_array().unwrap().iter().filter_map(|w| w.as_str()).filter(|w| w.starts_with("skills_unavailable: ")).collect();
    assert_eq!(warnings.len(), 1, "{why}: {v}");
    assert!(!out.stdout.windows(14).any(|w| w == b"PROVIDER-NOISE"), "{why}: provider output forwarded: {v}");
    assert_eq!(fs::read_dir(f.path("cache/skills")).unwrap().count(), 0, "{why}: scratch root left behind");
    elapsed
}

#[test]
fn a_provider_that_cannot_answer_makes_skills_unavailable_not_the_command_fail() {
    let f = Fixture::new(PROJECT);
    f.touch("skills-fail");
    assert_unavailable(&f, "mise without skills");
    fs::remove_file(f.path("skills-fail")).unwrap();
    f.touch("ls-garbage");
    assert_unavailable(&f, "garbage from ls");
    fs::remove_file(f.path("ls-garbage")).unwrap();
    f.touch("skills-garbage");
    assert_unavailable(&f, "garbage from skills");
    fs::remove_file(f.path("skills-garbage")).unwrap();
    f.touch("skills-hang");
    let elapsed = assert_unavailable(&f, "hanging skills");
    assert!(elapsed < Duration::from_secs(20), "{elapsed:?}");
    let pid = fs::read_to_string(f.path("skills-hang-pid")).unwrap();
    let alive = Command::new("kill").args(["-0", pid.trim()]).stderr(Stdio::null()).status().unwrap().success();
    assert!(!alive, "the hanging query outlived its deadline");
    fs::remove_file(f.path("skills-hang")).unwrap();
    // No mise at all.
    let mut command = f.command(&["--json", "inspect"]);
    command.env("PATH", "/usr/bin:/bin");
    let out = command.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["data"]["skills"].as_array().unwrap().iter().all(|s| s["status"] == "unavailable"));
    assert!(v["data"]["warnings"].to_string().contains("skills_unavailable: cannot run mise"), "{v}");
}

#[test]
fn an_unlocked_checkout_never_asks_about_whatever_release_is_active() {
    let f = Fixture::new(PROJECT);
    fs::remove_file(f.path("app/stack.lock")).unwrap();
    f.forget_queries();
    let v = f.json(&["inspect"]);
    assert!(f.queries().is_empty(), "queried without a lock");
    let skills = v["data"]["skills"].as_array().unwrap();
    assert!(!skills.is_empty() && skills.iter().all(|s| s["status"] == "unavailable" && s["version"].is_null()), "{v}");
    assert!(v["data"]["warnings"].to_string().contains("skills_unavailable: stack.lock pins no release"), "{v}");
}

struct Mcp {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(f: &Fixture) -> Self {
        let mut command = f.command(&["mcp"]);
        command.current_dir(f.path("app")).stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdout, id: 0 }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let line = json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params });
        writeln!(self.child.stdin.as_mut().unwrap(), "{line}").unwrap();
        let mut reply = String::new();
        self.stdout.read_line(&mut reply).unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({ "name": tool, "arguments": args }))["result"]["structuredContent"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_lists_and_returns_skills_but_never_the_providers() {
    let f = Fixture::new(PROJECT);
    let mut mcp = Mcp::start(&f);
    let init = mcp.request("initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert!(init["result"]["instructions"].as_str().unwrap().contains(
        "Tools in this stack may ship agent skills; `stack_inspect` lists them under `skills` and `stack_skill` returns one."
    ));
    let tools = mcp.request("tools/list", json!({}));
    let skill_tool = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "stack_skill").unwrap().clone();
    assert_eq!(skill_tool["inputSchema"]["required"], json!(["tool"]));

    let inspect = mcp.call("stack_inspect", json!({}));
    assert_eq!(statuses(&inspect["data"]["skills"]), EXPECTED);
    assert!(inspect["data"].get("provider_skills").is_none());
    let all = mcp.call("stack_inspect", json!({ "all_skills": true }));
    assert_eq!(statuses(&all["data"]["provider_skills"]), ["pitchfork@2.29.0 available pitchfork"]);

    let text = mcp.call("stack_skill", json!({ "tool": "mbx", "name": "mbx-advanced" }));
    assert_eq!(text["ok"], true, "{text}");
    assert_eq!(text["data"]["version"], "1.22.0");
    assert_eq!(text["data"]["text"], "# mbx-advanced 1.22.0\nDocumentation only. SENTINEL-DOC\n");

    for (tool, name, why) in [
        ("pitchfork", "pitchfork", "provider"),
        ("postgres", "postgres", "not installed"),
        ("fnox", "fnox-old", "another release's skill"),
        ("other", "other", "a tool the stack does not pin"),
        ("jq", "jq", "a release without skills"),
    ] {
        let r = mcp.call("stack_skill", json!({ "tool": tool, "name": name }));
        assert_eq!(r["error"]["code"], "skill_not_found", "{why}: {r}");
    }
    // Without a name: a tool's one available skill, or the choice when it has several.
    let r = mcp.call("stack_skill", json!({ "tool": "fnox" }));
    assert_eq!((r["data"]["name"].as_str(), r["data"]["version"].as_str()), (Some("fnox"), Some("1.39.0")), "{r}");
    let r = mcp.call("stack_skill", json!({ "tool": "mbx" }));
    assert_eq!(r["error"]["code"], "usage", "{r}");
    assert_eq!(r["error"]["details"][0]["names"], json!(["mbx", "mbx-advanced"]), "{r}");
    assert!(r["error"]["hint"].as_str().unwrap().contains("mbx, mbx-advanced"), "{r}");
    for tool in ["jq", "pitchfork", "postgres"] {
        let r = mcp.call("stack_skill", json!({ "tool": tool }));
        assert_eq!(r["error"]["code"], "skill_not_found", "{tool}: {r}");
    }
    let r = mcp.call("stack_skill", json!({ "tool": "fnox", "name": "" }));
    assert_eq!(r["error"]["code"], "usage");

    let entry = f.path("store/fnox/1.39.0/.mise-packslip/repo/skills/fnox/SKILL.md");
    fs::write(&entry, vec![b'#'; 65 * 1024]).unwrap();
    let r = mcp.call("stack_skill", json!({ "tool": "fnox", "name": "fnox" }));
    assert_eq!(r["error"]["code"], "skill_too_large", "{r}");
    assert!(!r.to_string().contains("####"), "text of an oversized skill returned");
    fs::remove_file(&entry).unwrap();
    let r = mcp.call("stack_skill", json!({ "tool": "fnox", "name": "fnox" }));
    assert_eq!(r["error"]["code"], "skill_not_found", "an entrypoint gone before discovery: {r}");
}

const SYNC_PROJECT: &str = "[tools]\nfnox = \"1.39\"\nmbx = \"1.22.0\"\n\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n\n[skills]\ndir = \".claude/skills\"\n";

fn step<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["data"]["steps"].as_array().unwrap_or_else(|| panic!("no steps: {report}")).iter().find(|s| s["step"] == name).unwrap_or_else(|| panic!("no {name} step: {report}"))
}

#[test]
fn install_links_available_skills_into_the_configured_directory() {
    let f = Fixture::new(SYNC_PROJECT);
    let v = f.json(&["install"]);
    let skills = step(&v, "skills");
    assert_eq!(skills["status"], "ok", "{v}");
    let linked: Vec<&str> = skills["detail"]["linked"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(linked, ["fnox", "mbx", "mbx-advanced"]);
    let dir = f.path("app/.claude/skills");
    assert_eq!(fs::read_link(dir.join("fnox")).unwrap(), f.path("store/fnox/1.39.0/.mise-packslip/repo/skills/fnox"));
    assert!(fs::symlink_metadata(dir.join("pitchfork")).is_err(), "the provider's skill was linked");
    let registry: Value = serde_json::from_slice(&fs::read(dir.join(".stack-skills.json")).unwrap()).unwrap();
    assert_eq!(registry["links"].as_object().unwrap().len(), 3);
    assert!(!f.mise_log().contains("skills sync"), "mise skills sync links the provider's skill too");
    // A user's own directory under a skill's name is kept; a second install changes nothing.
    fs::remove_file(dir.join("mbx")).unwrap();
    fs::create_dir(dir.join("mbx")).unwrap();
    let v = f.json(&["install"]);
    let detail = &step(&v, "skills")["detail"];
    assert_eq!(detail["kept"][0]["name"], "mbx", "{detail}");
    assert_eq!(detail["unchanged"], json!(["fnox", "mbx-advanced"]));
    assert!(dir.join("mbx").is_dir());
}

#[test]
fn skill_links_and_their_registry_are_excluded_from_git_but_a_users_own_skills_are_not() {
    let f = Fixture::new(SYNC_PROJECT);
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(f.path("app")).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    git(&["init", "-q"]);
    let dir = f.path("app/.claude/skills");
    fs::create_dir_all(dir.join("mine")).unwrap();
    fs::write(dir.join("mine/SKILL.md"), "mine\n").unwrap();
    f.json(&["install"]);
    let untracked = || -> Vec<String> {
        git(&["status", "--porcelain", "--untracked-files=all"]).lines().filter_map(|l| l.strip_prefix("?? ")).filter(|p| p.starts_with(".claude/")).map(String::from).collect()
    };
    assert!(dir.join("fnox").exists() && dir.join(".stack-skills.json").exists());
    assert_eq!(untracked(), [".claude/skills/mine/SKILL.md"]);
    // A name stack no longer links is no longer excluded: a user's own skill there shows.
    fs::remove_file(dir.join("mbx")).unwrap();
    fs::create_dir(dir.join("mbx")).unwrap();
    fs::write(dir.join("mbx/SKILL.md"), "mine too\n").unwrap();
    f.json(&["install"]);
    assert_eq!(untracked(), [".claude/skills/mbx/SKILL.md", ".claude/skills/mine/SKILL.md"]);
}

/// `git <args>` in `dir`, which must succeed; its stdout.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// Untracked paths under `dir` starting with `under`, ignored ones left out.
fn untracked_in(dir: &Path, under: &str) -> Vec<String> {
    git_in(dir, &["status", "--porcelain", "--untracked-files=all"]).lines().filter_map(|l| l.strip_prefix("?? ")).filter(|p| p.starts_with(under)).map(String::from).collect()
}

#[test]
fn a_link_made_in_one_checkout_never_hides_the_same_path_in_another() {
    let f = Fixture::new(SYNC_PROJECT);
    let app = f.path("app");
    git_in(&app, &["init", "-q"]);
    // An earlier release's block in the shared exclude, naming this link for checkout A.
    let exclude = app.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::write(&exclude, format!("*.log\n\n# stack: generated files of {a}\n/.claude/skills/fnox\n# stack: end of {a}\n", a = app.display())).unwrap();
    f.json(&["install"]);
    assert!(app.join(".claude/skills/fnox").exists());
    assert_eq!(untracked_in(&app, ".claude/"), Vec::<String>::new());
    assert_eq!(fs::read_to_string(&exclude).unwrap(), "*.log\n", "the shared block is gone");
    let ignore = fs::read_to_string(app.join(".claude/skills/.gitignore")).unwrap();
    assert!(ignore.contains("\n/.stack-skills.json\n") && ignore.contains("\n/fnox\n") && !ignore.contains("\n/*\n"), "{ignore}");

    // Checkout B, a linked worktree without [skills]: a skill the user writes at the same path.
    git_in(&app, &["add", "stack.toml", "stack.lock"]);
    git_in(&app, &["commit", "-qm", "stack"]);
    let b = f.path("b");
    git_in(&app, &["worktree", "add", "-q", b.to_str().unwrap()]);
    fs::write(b.join("stack.toml"), SYNC_PROJECT.replace("\n[skills]\ndir = \".claude/skills\"\n", "")).unwrap();
    let out = f.command_at(&b, &["compile"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    fs::create_dir_all(b.join(".claude/skills/fnox")).unwrap();
    fs::write(b.join(".claude/skills/fnox/SKILL.md"), "the user's\n").unwrap();
    assert_eq!(untracked_in(&b, ".claude/"), [".claude/skills/fnox/SKILL.md"]);
    // And in A, still ignored.
    assert_eq!(untracked_in(&app, ".claude/"), Vec::<String>::new());
}

#[test]
fn a_skills_dir_linked_into_another_checkout_leaves_that_checkouts_files_alone() {
    let f = Fixture::new(SYNC_PROJECT);
    let app = f.path("app");
    git_in(&app, &["init", "-q"]);
    f.json(&["install"]);
    let theirs = app.join(".config/mise/.gitignore");
    let original = fs::read_to_string(&theirs).unwrap();
    git_in(&app, &["add", "stack.toml", "stack.lock"]);
    git_in(&app, &["commit", "-qm", "stack"]);
    // Checkout B's skills dir is a link to A's generated provider directory.
    let b = f.path("b");
    git_in(&app, &["worktree", "add", "-q", b.to_str().unwrap()]);
    std::os::unix::fs::symlink(app.join(".config/mise"), b.join("linked-skills")).unwrap();
    fs::write(b.join("stack.toml"), SYNC_PROJECT.replace("dir = \".claude/skills\"", "dir = \"linked-skills\"")).unwrap();
    let out = f.command_at(&b, &["--json", "compile"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("symbolic link"), "an honest warning: {text}");
    assert_eq!(fs::read_to_string(&theirs).unwrap(), original, "A's generated .gitignore is untouched");
    assert_eq!(untracked_in(&app, ".config/"), Vec::<String>::new());
    // The same for a provider directory that is itself a link into A.
    let c = f.path("c");
    git_in(&app, &["worktree", "add", "-q", c.to_str().unwrap()]);
    fs::create_dir_all(c.join(".config")).unwrap();
    std::os::unix::fs::symlink(app.join(".config/mise"), c.join(".config/mise")).unwrap();
    fs::write(c.join("stack.toml"), SYNC_PROJECT.replace("\n[skills]\ndir = \".claude/skills\"\n", "")).unwrap();
    let _ = f.command_at(&c, &["--json", "compile"]).output().unwrap();
    assert_eq!(fs::read_to_string(&theirs).unwrap(), original, "A's generated .gitignore is untouched");
}

#[test]
fn a_compile_stops_ignoring_a_generated_link_the_user_replaced() {
    let f = Fixture::new(SYNC_PROJECT);
    let app = f.path("app");
    git_in(&app, &["init", "-q"]);
    f.json(&["install"]);
    let link = app.join(".claude/skills/fnox");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(untracked_in(&app, ".claude/"), Vec::<String>::new());
    // A skill the user writes in its place: visible after a routine compile, no install needed.
    fs::remove_file(&link).unwrap();
    fs::create_dir(&link).unwrap();
    fs::write(link.join("SKILL.md"), "the user's\n").unwrap();
    let out = f.command_at(&app, &["compile"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(untracked_in(&app, ".claude/"), [".claude/skills/fnox/SKILL.md"]);
    // A link of the user's own, pointing elsewhere: theirs too.
    fs::remove_dir_all(&link).unwrap();
    let theirs = f.path("theirs");
    fs::create_dir_all(&theirs).unwrap();
    std::os::unix::fs::symlink(&theirs, &link).unwrap();
    let out = f.command_at(&app, &["compile"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(untracked_in(&app, ".claude/"), [".claude/skills/fnox"]);
    // Removed: nothing left to name.
    fs::remove_file(&link).unwrap();
    f.command_at(&app, &["compile"]).output().unwrap();
    let ignore = fs::read_to_string(app.join(".claude/skills/.gitignore")).unwrap();
    assert!(!ignore.contains("/fnox"), "{ignore}");
}

#[test]
fn a_skills_dir_written_with_dot_or_doubled_separators_is_ignored_where_the_links_are() {
    for dir in ["./.claude/skills", ".claude//skills/", "./.claude/./skills"] {
        let f = Fixture::new(&SYNC_PROJECT.replace("dir = \".claude/skills\"", &format!("dir = {dir:?}")));
        let app = f.path("app");
        git_in(&app, &["init", "-q"]);
        f.json(&["install"]);
        assert!(app.join(".claude/skills/fnox").exists() && app.join(".claude/skills/.gitignore").exists(), "{dir}");
        assert_eq!(untracked_in(&app, ".claude/"), Vec::<String>::new(), "{dir}");
    }
}

#[test]
fn up_succeeds_with_a_warning_when_skills_cannot_be_linked() {
    let project = "[tools]\nfnox = \"1.39\"\n\n[skills]\ndir = \"agents/skills\"\n";
    let f = Fixture::new(project);
    f.touch("skills-fail");
    let v = f.json(&["up"]);
    let skills = step(&v, "skills");
    assert_eq!(skills["status"], "warning", "{v}");
    assert_eq!(skills["detail"]["warnings"][0]["code"], "skills_unavailable");
    assert!(v["data"]["warnings"][0].as_str().unwrap().starts_with("skills: skills_unavailable: "), "{v}");
    assert!(!f.path("app/agents").exists(), "nothing to link, nothing created");
    fs::remove_file(f.path("skills-fail")).unwrap();

    // A malformed ownership record: nothing linked, nothing removed, the record untouched.
    fs::create_dir_all(f.path("app/agents/skills")).unwrap();
    fs::write(f.path("app/agents/skills/.stack-skills.json"), "{ not json").unwrap();
    let v = f.json(&["up"]);
    assert_eq!(step(&v, "skills")["detail"]["warnings"][0]["code"], "skills_failed", "{v}");
    assert_eq!(fs::read_to_string(f.path("app/agents/skills/.stack-skills.json")).unwrap(), "{ not json");
    assert!(fs::symlink_metadata(f.path("app/agents/skills/fnox")).is_err());

    // A link in the directory's ancestry pointing out of the project is refused.
    fs::remove_dir_all(f.path("app/agents")).unwrap();
    let outside = f.path("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, f.path("app/agents")).unwrap();
    let v = f.json(&["up"]);
    assert_eq!(step(&v, "skills")["detail"]["warnings"][0]["code"], "invalid_path", "{v}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn skills_dir_is_project_only_and_must_stay_inside_the_project() {
    let f = Fixture::new("[tools]\njq = \"1.7.1\"\n");
    for bad in ["/tmp/skills", "../skills", "a/../../b", ".stack/skills"] {
        fs::write(f.path("app/stack.toml"), format!("[tools]\njq = \"1.7.1\"\n[skills]\ndir = {bad:?}\n")).unwrap();
        let v = f.json(&["compile"]);
        assert_eq!(v["error"]["code"], "invalid_path", "{bad}: {v}");
    }
    fs::create_dir(f.path("bundle")).unwrap();
    fs::write(f.path("bundle/bundle.toml"), "[bundle]\nname = \"b\"\n[skills]\ndir = \"x\"\n").unwrap();
    fs::write(f.path("app/stack.toml"), "[[use]]\nbundle = \"path:../bundle\"\n").unwrap();
    let v = f.json(&["compile"]);
    assert_eq!(v["error"]["code"], "bundle_invalid", "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("skills"), "{v}");
}

fn names(list: &Value) -> Vec<&str> {
    list.as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect()
}

#[test]
fn repeated_installs_keep_stacks_links_until_discovery_settles_then_prune() {
    let project = "[tools]\nfnox = \"1.39\"\nmbx = \"1.22.0\"\n\n[skills]\ndir = \".claude/skills\"\n";
    let f = Fixture::new(project);
    let v = f.json(&["install"]);
    assert_eq!(names(&step(&v, "skills")["detail"]["linked"]), ["fnox", "mbx", "mbx-advanced"], "{v}");
    let dir = f.path("app/.claude/skills");
    let record = fs::read(dir.join(".stack-skills.json")).unwrap();
    let links = || -> BTreeMap<String, PathBuf> {
        fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_symlink()).map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_link(&p).unwrap())).collect()
    };
    let linked = links();
    assert_eq!(linked.len(), 3);
    let unchanged = |v: &Value, why: &str| {
        let detail = &step(v, "skills")["detail"];
        assert_eq!(names(&detail["preserved"]), ["fnox", "mbx", "mbx-advanced"], "{why}: {v}");
        assert!(detail["pruned"].as_array().unwrap().is_empty() && detail["kept"].as_array().unwrap().is_empty(), "{why}: {v}");
        assert_eq!(links(), linked, "{why}: links changed");
        assert_eq!(fs::read(dir.join(".stack-skills.json")).unwrap(), record, "{why}: the ownership record changed");
    };

    // The provider fails, twice: install still succeeds and removes nothing.
    f.touch("skills-fail");
    for _ in 0..2 {
        let v = f.json(&["install"]);
        assert_eq!(step(&v, "skills")["status"], "warning", "{v}");
        assert_eq!(step(&v, "skills")["detail"]["warnings"][0]["code"], "skills_unavailable");
        unchanged(&v, "provider failure");
    }
    fs::remove_file(f.path("skills-fail")).unwrap();

    // The provider hangs past its deadline: the same.
    f.touch("skills-hang");
    let v = f.json(&["install"]);
    assert!(step(&v, "skills")["detail"]["warnings"][0]["message"].as_str().unwrap().contains("did not answer"), "{v}");
    unchanged(&v, "provider timeout");
    let pid = fs::read_to_string(f.path("skills-hang-pid")).unwrap();
    assert!(!Command::new("kill").args(["-0", pid.trim()]).stderr(Stdio::null()).status().unwrap().success(), "the hanging query outlived its deadline");
    fs::remove_file(f.path("skills-hang")).unwrap();

    // mbx leaves the stack while the pinned fnox is not installed: whether fnox still ships the
    // skills behind these links is unknown, so none is removed yet.
    fs::write(f.path("app/stack.toml"), "[tools]\nfnox = \"1.39\"\n\n[skills]\ndir = \".claude/skills\"\n").unwrap();
    f.ok(&["compile"]);
    let ls = fs::read_to_string(f.path("ls.json")).unwrap();
    let mut missing: Value = serde_json::from_str(&ls).unwrap();
    for release in missing["fnox"].as_array_mut().unwrap() {
        release["installed"] = json!(false);
    }
    fs::write(f.path("ls.json"), missing.to_string()).unwrap();
    let v = f.json(&["install"]);
    let detail = &step(&v, "skills")["detail"];
    assert!(detail["preserved"][0]["reason"].as_str().unwrap().contains("fnox@1.39.0"), "{v}");
    unchanged(&v, "pinned release not installed");

    // Discovery answers for every pin again: stack's links to mbx's skills go, fnox's stays.
    fs::write(f.path("ls.json"), ls).unwrap();
    let v = f.json(&["install"]);
    let detail = &step(&v, "skills")["detail"];
    assert_eq!(step(&v, "skills")["status"], "ok", "{v}");
    assert_eq!(names(&detail["pruned"]), ["mbx", "mbx-advanced"], "{v}");
    assert!(detail["preserved"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(detail["unchanged"], json!(["fnox"]));
    assert_eq!(links().keys().collect::<Vec<_>>(), ["fnox"]);
    let registry: Value = serde_json::from_slice(&fs::read(dir.join(".stack-skills.json")).unwrap()).unwrap();
    assert_eq!(registry["links"].as_object().unwrap().keys().collect::<Vec<_>>(), ["fnox"]);
}

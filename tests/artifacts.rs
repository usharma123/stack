//! `stack compile` artifact locking against a fake `mise lock` that writes into the scratch root.

use stack::error::StackError;
use stack::lock;
use stack::project::{compile, Options, Report};
use stack::provider::mise::{CalVer, LockRun, Locker, Resolver, ScratchRoot};
use stack::source::Mode;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use toml::{Table, Value};

/// Exact releases only, plus a few prefixes.
struct Upstream;

impl Resolver for Upstream {
    fn resolve(&self, tool: &str, request: &str) -> Result<String, StackError> {
        Ok(match (tool, request) {
            ("postgres", "17") => "17.6".into(),
            ("redis", "8") => "8.2.1".into(),
            ("python", "3.13") => "3.13.2".into(),
            (_, r) => r.into(),
        })
    }

    fn registry_backend(&self, tool: &str) -> Result<Option<String>, StackError> {
        Ok((tool == "fnox").then(|| "packslip:github.com/jdx/fnox".into()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Behaviour {
    /// Lock what is named, as mise does online.
    Online,
    /// Exit 0 and leave the seeded lock as it was ("skipped").
    Offline,
    /// Leave something that is not TOML.
    Garbage,
    /// Fail as a timeout or an unrunnable mise does.
    Unrunnable,
}

/// One `mise lock` the fake answered.
#[derive(Debug, Clone)]
struct Call {
    root: PathBuf,
    tools: Vec<String>,
    platforms: Vec<String>,
    config: String,
    seed: String,
}

/// mise as artifact locking sees it. Checksums carry a generation, so a test can make
/// "upstream" change; `dep_generation` changes the shared conda record alone.
struct FakeMise {
    version: Mutex<CalVer>,
    behaviour: Mutex<Behaviour>,
    generation: Mutex<u32>,
    dep_generation: Mutex<u32>,
    /// Platforms with no artifact, per tool (pitchfork has none for macos-x64).
    unpublished: Mutex<Vec<(String, String)>>,
    dangling: Mutex<bool>,
    calls: Mutex<Vec<Call>>,
    version_calls: Mutex<usize>,
}

impl FakeMise {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            version: Mutex::new(CalVer(2026, 10, 3)),
            behaviour: Mutex::new(Behaviour::Online),
            generation: Mutex::new(1),
            dep_generation: Mutex::new(1),
            unpublished: Mutex::new(vec![]),
            dangling: Mutex::new(false),
            calls: Mutex::new(vec![]),
            version_calls: Mutex::new(0),
        })
    }

    fn set(&self, b: Behaviour) {
        *self.behaviour.lock().unwrap() = b;
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn entries(&self, tool: &str, version: &str, platforms: &[String], lock: &mut Table, stderr: &mut String, failed: &mut bool) {
        let gen = *self.generation.lock().unwrap();
        let dep_gen = *self.dep_generation.lock().unwrap();
        let unpublished = self.unpublished.lock().unwrap().clone();
        let mut entry = Table::new();
        entry.insert("version".into(), version.into());
        let backend = match tool {
            "fnox" => "packslip:github.com/jdx/fnox",
            "rust" => "core:rust",
            "npm:prettier" => "npm:prettier",
            "postgres" | "conda:pgcli" => "conda:postgresql",
            "redis" => {
                for p in platforms {
                    stderr.push_str(&format!("failed to resolve redis for {p}: failed to solve redis-server for {p}\n"));
                }
                *failed = true;
                return;
            }
            _ => "aqua:example/tool",
        };
        entry.insert("backend".into(), backend.into());
        entry.insert("specifiers".into(), Value::Array(vec![version.into()]));
        if tool == "npm:prettier" {
            let aube: Table = toml::from_str("path = \"locks/npm-prettier/3.6.2\"\ndigest = \"sha256:0b89\"").unwrap();
            entry.insert("aube".into(), aube.into());
        }
        if backend.starts_with("conda:") {
            let options: Table = toml::from_str("channel = \"conda-forge\"").unwrap();
            entry.insert("options".into(), options.into());
        }
        let conda_packages = lock.entry("conda-packages").or_insert_with(|| Table::new().into()).as_table_mut().unwrap().clone();
        let mut conda_packages = conda_packages;
        if backend != "core:rust" && backend != "npm:prettier" {
            for p in platforms {
                if unpublished.iter().any(|(t, up)| t == tool && up == p) {
                    continue;
                }
                let mut t = Table::new();
                t.insert("checksum".into(), format!("sha256:{tool}-{version}-{p}-g{gen}").into());
                t.insert("url".into(), format!("https://example.invalid/{tool}/{version}/{p}").into());
                if backend.starts_with("packslip:") {
                    t.insert("signer".into(), "sigstore-oidc:https://github.com/jdx/fnox/.github/workflows/release.yml".into());
                    let ids: Table = toml::from_str("repository = \"1078762196\"").unwrap();
                    t.insert("repository_ids".into(), ids.into());
                }
                if backend.starts_with("conda:") {
                    let own = format!("{}-{version}-h0", tool.trim_start_matches("conda:"));
                    let mut deps = vec![Value::from(own.clone()), Value::from("openssl-3.6.5-h0")];
                    if *self.dangling.lock().unwrap() {
                        deps.push("ghost-1-h0".into());
                    }
                    t.insert("conda_deps".into(), Value::Array(deps));
                    let records = conda_packages.entry(p.clone()).or_insert_with(|| Table::new().into()).as_table_mut().unwrap();
                    let rec = |name: &str, g: u32| -> Value {
                        toml::from_str::<Table>(&format!("url = \"https://example.invalid/conda/{name}\"\nchecksum = \"sha256:{name}-{p}-d{g}\"")).unwrap().into()
                    };
                    records.insert(own.clone(), rec(&own, gen));
                    records.insert("openssl-3.6.5-h0".into(), rec("openssl-3.6.5-h0", dep_gen));
                }
                entry.insert(format!("platforms.{p}"), t.into());
            }
        }
        lock.insert("conda-packages".into(), conda_packages.into());
        let tools = lock.entry("tools").or_insert_with(|| Table::new().into()).as_table_mut().unwrap();
        let list = tools.entry(tool).or_insert_with(|| Value::Array(vec![])).as_array_mut().unwrap();
        list.push(entry.into());
    }
}

impl Locker for FakeMise {
    fn version(&self, _: &Path) -> Result<CalVer, StackError> {
        *self.version_calls.lock().unwrap() += 1;
        Ok(*self.version.lock().unwrap())
    }

    fn lock(&self, scratch: &ScratchRoot, platforms: &[String], tools: &[String]) -> Result<LockRun, StackError> {
        let lock_path = scratch.path().join(".config/mise/mise.lock");
        let config = fs::read_to_string(scratch.config_path()).unwrap();
        let seed = fs::read_to_string(&lock_path).unwrap();
        self.calls.lock().unwrap().push(Call { root: scratch.path().into(), tools: tools.into(), platforms: platforms.into(), config: config.clone(), seed: seed.clone() });
        match *self.behaviour.lock().unwrap() {
            Behaviour::Offline => return Ok(LockRun { exit_code: Some(0), stderr: String::new() }),
            Behaviour::Garbage => {
                fs::write(&lock_path, "lockfile_version = [[[").unwrap();
                return Ok(LockRun { exit_code: Some(0), stderr: String::new() });
            }
            Behaviour::Unrunnable => {
                return Err(StackError::new("artifact_lock_failed", "`mise lock` did not finish within 600s").hint("check mise and network access; stack.lock was not changed"));
            }
            Behaviour::Online => {}
        }
        let mut lock: Table = toml::from_str(&seed).unwrap();
        let doc: Table = toml::from_str(&config).unwrap();
        let (mut stderr, mut failed) = (String::new(), false);
        for tool in tools {
            if let Some(t) = lock.get_mut("tools").and_then(Value::as_table_mut) {
                t.remove(tool);
            }
            let versions: Vec<String> = match &doc["tools"][tool.as_str()] {
                Value::Array(a) => a.iter().map(version_of).collect(),
                other => vec![version_of(other)],
            };
            for v in versions {
                self.entries(tool, &v, platforms, &mut lock, &mut stderr, &mut failed);
            }
        }
        if lock.get("conda-packages").and_then(Value::as_table).is_some_and(|t| t.is_empty()) {
            lock.remove("conda-packages");
        }
        fs::write(&lock_path, toml::to_string_pretty(&lock).unwrap()).unwrap();
        Ok(LockRun { exit_code: Some(if failed { 1 } else { 0 }), stderr })
    }
}

fn version_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Table(t) => t["version"].as_str().unwrap().into(),
        other => panic!("{other}"),
    }
}

struct Sandbox {
    tmp: TempDir,
    mise: Arc<FakeMise>,
}

impl Sandbox {
    fn new(stack_toml: &str) -> Self {
        let sb = Self { tmp: TempDir::new().unwrap(), mise: FakeMise::new() };
        fs::create_dir_all(sb.root()).unwrap();
        sb.write(stack_toml);
        sb
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("app")
    }

    fn write(&self, stack_toml: &str) {
        fs::write(self.root().join("stack.toml"), stack_toml).unwrap();
    }

    fn compile(&self, mode: Mode) -> Result<Report, StackError> {
        self.run(mode, true)
    }

    fn run(&self, mode: Mode, write: bool) -> Result<Report, StackError> {
        compile(&Options {
            root: self.root(),
            mode,
            write,
            cache: self.tmp.path().join("cache"),
            state: self.tmp.path().join("state"),
            reassign_ports: false,
            resolver: Some(Arc::new(Upstream)),
            locker: Some(self.mise.clone()),
        })
    }

    fn lock(&self) -> lock::Lockfile {
        lock::read(&self.root()).unwrap().unwrap()
    }

    fn embedded(&self) -> Table {
        self.lock().provider_lock.unwrap()
    }

    fn rendered(&self) -> String {
        fs::read_to_string(self.root().join(".config/mise/mise.lock")).unwrap()
    }

    /// stack.lock, the generated config and the rendered lock, byte for byte (absent as None).
    fn files(&self) -> Vec<Option<Vec<u8>>> {
        ["stack.lock", ".config/mise/conf.d/stack.toml", ".config/mise/mise.lock"].iter().map(|f| fs::read(self.root().join(f)).ok()).collect()
    }
}

fn checksum(embedded: &Table, tool: &str, platform: &str) -> String {
    embedded["tools"][tool][0][format!("platforms.{platform}").as_str()]["checksum"].as_str().unwrap().into()
}

fn artifacts<'a>(report: &'a Report, name: &str) -> &'a indexmap::IndexMap<String, stack::artifacts::PlatformReport> {
    report.versions.iter().find(|v| v.name == name).unwrap().artifacts.as_ref().unwrap()
}

const TOOLS: &str = r#"
[tools]
jq = "1.7.1"
fnox = "1.39.0"

[env]
HOOK = "{{ exec(command='touch hook-ran') }}"
"#;

#[test]
fn ordinary_compile_locks_needed_pins_in_a_unique_tools_only_scratch_root_and_then_stops() {
    let sb = Sandbox::new(&format!("{TOOLS}\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n"));
    let report = sb.compile(Mode::UseLock).unwrap();
    let calls = sb.mise.calls();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.tools, ["fnox", "jq", "pitchfork", "postgres"]);
    assert_eq!(call.platforms, ["macos-arm64", "linux-x64", "linux-arm64"]);
    let config: Table = toml::from_str(&call.config).unwrap();
    assert_eq!(config.keys().collect::<Vec<_>>(), ["tools"], "only [tools]: {}", call.config);
    assert_eq!(config["tools"]["postgres"].as_str(), Some("17.6"), "a service pin under its preset tool");
    assert!(!call.config.contains("exec") && !call.config.contains("HOOK"));
    let seed: Table = toml::from_str(&call.seed).unwrap();
    assert_eq!(seed.keys().collect::<Vec<_>>(), ["lockfile_version"], "pins being locked are seeded with nothing");
    assert!(call.root.starts_with(sb.tmp.path().join("cache/lock")) || call.root.to_string_lossy().contains("/cache/lock/"));
    assert!(!call.root.exists(), "the scratch root is removed");
    assert!(!sb.root().join("hook-ran").exists());

    let lock = sb.lock();
    assert_eq!(lock.version, 3);
    assert_eq!(lock.tool("jq").unwrap().resolved_on.as_deref(), Some(stack::artifacts::current_platform().as_str()));
    let embedded = sb.embedded();
    assert_eq!(embedded["provider"].as_str(), Some("mise"));
    assert_eq!(checksum(&embedded, "jq", "linux-arm64"), "sha256:jq-1.7.1-linux-arm64-g1");
    // The rendered lock is the embedded one without stack's keys.
    let rendered: Table = toml::from_str(&sb.rendered()).unwrap();
    assert_eq!(rendered, stack::artifacts::to_mise(&embedded));
    // Coverage and backend in the report.
    let fnox = report.versions.iter().find(|v| v.name == "fnox").unwrap();
    assert_eq!(fnox.backend.as_deref(), Some("packslip:github.com/jdx/fnox"));
    let a = artifacts(&report, "fnox");
    assert_eq!(a["macos-arm64"].state, stack::artifacts::State::Verified);
    assert_eq!(a["macos-arm64"].change, Some("added"));
    assert!(a["macos-arm64"].signer.is_some());
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["versions"][0]["artifacts"]["linux-x64"]["state"], "verified", "{json}");

    // Full coverage: the next compile asks mise for nothing and changes nothing.
    let before = sb.files();
    let again = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().len(), 1, "no second `mise lock`");
    assert!(!again.lock_changed);
    assert_eq!(sb.files(), before);
    assert!(artifacts(&again, "jq")["linux-x64"].change.is_none());
    // Two compiles never share a root.
    sb.compile(Mode::Update).unwrap();
    let roots: BTreeSet<PathBuf> = sb.mise.calls().iter().map(|c| c.root.clone()).collect();
    assert_eq!(roots.len(), 2);
}

#[test]
fn locked_mode_and_inspect_never_lock_and_compile_locked_renders_the_committed_lock() {
    let sb = Sandbox::new(TOOLS);
    sb.compile(Mode::UseLock).unwrap();
    fs::remove_file(sb.root().join(".config/mise/mise.lock")).unwrap();
    let n = sb.mise.calls().len();
    sb.compile(Mode::Frozen).unwrap();
    assert!(sb.root().join(".config/mise/mise.lock").exists(), "compile --locked renders it");
    let report = sb.run(Mode::Frozen, false).unwrap();
    assert_eq!(sb.mise.calls().len(), n, "no `mise lock` in locked mode or inspect");
    assert_eq!(artifacts(&report, "jq")["macos-arm64"].state, stack::artifacts::State::Verified);
}

#[test]
fn a_different_upstream_checksum_is_kept_and_warned_about_and_only_update_accepts_it() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    *sb.mise.generation.lock().unwrap() = 2;
    // A newly listed platform makes jq a target; its committed linux-x64 value must survive.
    sb.write("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"linux-x64\", \"linux-arm64\"]\n");
    let report = sb.compile(Mode::UseLock).unwrap();
    let embedded = sb.embedded();
    assert_eq!(checksum(&embedded, "jq", "linux-x64"), "sha256:jq-1.7.1-linux-x64-g1", "committed value kept");
    assert_eq!(checksum(&embedded, "jq", "linux-arm64"), "sha256:jq-1.7.1-linux-arm64-g2", "new platform added");
    assert!(report.warnings.iter().any(|w| w == "artifacts.jq@1.7.1.linux-x64 differs upstream; run `stack compile --update` to accept"), "{:?}", report.warnings);
    assert_eq!(artifacts(&report, "jq")["linux-x64"].change, Some("differs_upstream"));

    let report = sb.compile(Mode::Update).unwrap();
    assert_eq!(checksum(&sb.embedded(), "jq", "linux-x64"), "sha256:jq-1.7.1-linux-x64-g2");
    let a = &artifacts(&report, "jq")["linux-x64"];
    assert_eq!(a.change, Some("artifact_changed"));
    assert_eq!(a.checksum_was.as_deref(), Some("sha256:jq-1.7.1-linux-x64-g1"));
    assert!(a.url_was.is_some());
}

#[test]
fn an_update_that_mise_skips_is_retained_never_refreshed() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    let before = sb.embedded();
    sb.mise.set(Behaviour::Offline);
    let report = sb.compile(Mode::Update).unwrap();
    assert_eq!(sb.embedded(), before, "the committed entry is kept");
    let a = &artifacts(&report, "jq")["linux-x64"];
    assert_eq!(a.change, Some("retained"));
    assert_eq!(a.state, stack::artifacts::State::Verified);
    let seed: Table = toml::from_str(&sb.mise.calls().last().unwrap().seed).unwrap();
    assert_eq!(seed.keys().collect::<Vec<_>>(), ["lockfile_version"], "nothing seeded for jq");
}

#[test]
fn shared_conda_records_are_kept_without_update_reported_with_it_and_pruned_when_unreferenced() {
    let toml = "[tools]\n\"conda:pgcli\" = \"4.0.0\"\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n[lock]\nplatforms = [\"linux-x64\"]\n";
    let sb = Sandbox::new(toml);
    sb.compile(Mode::UseLock).unwrap();
    let record = |e: &Table| e["conda-packages"]["linux-x64"]["openssl-3.6.5-h0"]["checksum"].as_str().unwrap().to_string();
    assert_eq!(record(&sb.embedded()), "sha256:openssl-3.6.5-h0-linux-x64-d1");
    *sb.mise.dep_generation.lock().unwrap() = 2;
    // Re-lock pgcli only (a new platform for it would do; here: a changed pin).
    sb.write(&toml.replace("4.0.0", "4.1.0"));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().last().unwrap().tools, ["conda:pgcli"]);
    assert_eq!(record(&sb.embedded()), "sha256:openssl-3.6.5-h0-linux-x64-d1", "shared record kept for postgres and pgcli");
    assert!(report.warnings.iter().any(|w| w.contains("conda-packages.linux-x64.openssl-3.6.5-h0 differs upstream")), "{:?}", report.warnings);
    // pgcli 4.0.0's own record went with its entry.
    let records = sb.embedded()["conda-packages"]["linux-x64"].as_table().unwrap().clone();
    assert!(records.contains_key("pgcli-4.1.0-h0") && !records.contains_key("pgcli-4.0.0-h0"), "{records:?}");

    let report = sb.compile(Mode::Update).unwrap();
    assert_eq!(record(&sb.embedded()), "sha256:openssl-3.6.5-h0-linux-x64-d2");
    let deps = &artifacts(&report, "db")["linux-x64"].deps;
    assert!(deps.iter().any(|d| d["dep"] == "openssl-3.6.5-h0" && d["change"] == "artifact_changed" && d["checksum_was"] == "sha256:openssl-3.6.5-h0-linux-x64-d1"), "{deps:?}");

    // A dependency without its record is lock_invalid, and nothing is written.
    let before = sb.files();
    *sb.mise.dangling.lock().unwrap() = true;
    let e = sb.compile(Mode::Update).unwrap_err();
    assert_eq!(e.code, "lock_invalid", "{e:?}");
    assert!(e.message.contains("ghost-1-h0"));
    assert_eq!(sb.files(), before);
}

#[test]
fn removed_pins_and_unlisted_platforms_are_dropped_without_asking_mise() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\nfnox = \"1.39.0\"\n[lock]\nplatforms = [\"linux-x64\", \"macos-arm64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    sb.write("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().len(), 1);
    let embedded = sb.embedded();
    assert!(embedded["tools"].get("fnox").is_none());
    assert!(embedded["tools"]["jq"][0].get("platforms.macos-arm64").is_none());
    assert!(!sb.rendered().contains("fnox"));
}

#[test]
fn changed_options_on_the_same_release_lock_it_again_and_keep_the_commitment_until_update() {
    let toml = |identity: &str| format!("[tools]\njq = \"1.7.1\"\nfnox = {{ version = \"1.39.0\", identity = \"{identity}\" }}\n[lock]\nplatforms = [\"linux-x64\"]\n");
    let sb = Sandbox::new(&toml("https://ci/old"));
    sb.compile(Mode::UseLock).unwrap();
    let committed = checksum(&sb.embedded(), "fnox", "linux-x64");
    let signer = sb.embedded()["tools"]["fnox"][0]["platforms.linux-x64"]["signer"].clone();

    // Same release, same upstream answer: locked again (fnox only), nothing to report.
    sb.write(&toml("https://ci/new"));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().len(), 2, "the changed pin is locked again");
    let call = sb.mise.calls().last().unwrap().clone();
    assert_eq!(call.tools, ["fnox"]);
    assert!(call.config.contains("https://ci/new"), "locked with the new options: {}", call.config);
    let seed: Table = toml::from_str(&call.seed).unwrap();
    assert!(seed["tools"].get("fnox").is_none() && seed["tools"].get("jq").is_some(), "only the changed pin is unseeded: {seed:?}");
    assert!(report.warnings.iter().all(|w| !w.contains("artifacts.")), "{:?}", report.warnings);
    assert!(artifacts(&report, "fnox")["linux-x64"].change.is_none());
    assert_eq!(sb.lock().tool("fnox").unwrap().options["identity"], stack::tool::OptionValue::String("https://ci/new".into()));

    // A different upstream answer for the new request is kept out and reported, as for any pin.
    *sb.mise.generation.lock().unwrap() = 2;
    sb.write(&toml("https://ci/newer"));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(checksum(&sb.embedded(), "fnox", "linux-x64"), committed, "the commitment is kept");
    assert_eq!(sb.embedded()["tools"]["fnox"][0]["platforms.linux-x64"]["signer"], signer);
    assert!(report.warnings.iter().any(|w| w == "artifacts.fnox@1.39.0.linux-x64 differs upstream; run `stack compile --update` to accept"), "{:?}", report.warnings);
    assert_eq!(artifacts(&report, "fnox")["linux-x64"].change, Some("differs_upstream"));
    assert_eq!(checksum(&sb.embedded(), "jq", "linux-x64"), "sha256:jq-1.7.1-linux-x64-g1", "unchanged pins are not locked again");

    // No fresh answer (offline): kept, and reported as retained rather than compared.
    sb.mise.set(Behaviour::Offline);
    sb.write(&toml("https://ci/offline"));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(checksum(&sb.embedded(), "fnox", "linux-x64"), committed);
    let a = &artifacts(&report, "fnox")["linux-x64"];
    assert_eq!((a.change, a.state), (Some("retained"), stack::artifacts::State::Verified));
    assert!(report.warnings.iter().any(|w| w.starts_with("artifacts.fnox@1.39.0.linux-x64: its request changed")), "{:?}", report.warnings);

    // Unchanged since: nothing to lock. `--update` accepts the difference and reports it.
    sb.mise.set(Behaviour::Online);
    let n = sb.mise.calls().len();
    sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().len(), n);
    let report = sb.compile(Mode::Update).unwrap();
    assert_eq!(checksum(&sb.embedded(), "fnox", "linux-x64"), "sha256:fnox-1.39.0-linux-x64-g2");
    assert_eq!(artifacts(&report, "fnox")["linux-x64"].checksum_was.as_deref(), Some(committed.as_str()));
    // A changed option is still a stale pin for locked operations.
    sb.write(&toml("https://ci/unlocked"));
    assert_eq!(sb.compile(Mode::Frozen).unwrap_err().code, "lock_outdated");
    // Locking the changed pin fails: stack.lock, the config and the rendered lock stay as they were.
    let before = sb.files();
    sb.mise.set(Behaviour::Garbage);
    assert_eq!(sb.compile(Mode::UseLock).unwrap_err().code, "artifact_lock_failed");
    assert_eq!(sb.files(), before);
}

#[test]
fn requests_that_name_a_pinned_release_again_and_duplicate_pins_need_no_locking() {
    let svc = |version: &str| format!("[services.db]\npreset = \"postgres\"\nversion = \"{version}\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    let sb = Sandbox::new(&svc("17"));
    sb.compile(Mode::UseLock).unwrap();
    let n = sb.mise.calls().len();
    let before = sb.embedded();
    // A different request for the release already pinned: the same question for mise.
    sb.write(&svc("17.6"));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert!(report.lock_changed, "the request is recorded");
    assert_eq!(sb.mise.calls().len(), n);
    assert_eq!(sb.embedded(), before);
    assert_eq!(artifacts(&report, "db")["linux-x64"].state, stack::artifacts::State::Verified);
    // A tool that pins the release a service already pins shares its entry.
    sb.write(&format!("[tools]\npostgres = \"17.6\"\n{}", svc("17.6")));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().len(), n);
    assert_eq!(sb.embedded(), before);
    assert_eq!(artifacts(&report, "postgres")["linux-x64"].state, stack::artifacts::State::Verified);
    // A new release for the service is a new pin, locked on its own.
    sb.write(&format!("[tools]\npostgres = \"17.6\"\n{}", svc("16.4")));
    sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.mise.calls().last().unwrap().tools, ["postgres"]);
    let versions: Vec<String> = sb.embedded()["tools"]["postgres"].as_array().unwrap().iter().map(|e| e["version"].as_str().unwrap().to_string()).collect();
    assert_eq!(versions, ["17.6", "16.4"]);
}

#[test]
fn tools_mise_cannot_lock_are_missing_with_its_reason_and_the_rest_are_written() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[services.cache]\npreset = \"redis\"\nversion = \"8\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.mise.unpublished.lock().unwrap().push(("pitchfork".into(), "linux-x64".into()));
    let report = sb.compile(Mode::UseLock).unwrap();
    let a = &artifacts(&report, "cache")["linux-x64"];
    assert_eq!(a.state, stack::artifacts::State::Missing);
    assert_eq!(a.reason.as_deref(), Some("failed to resolve redis for linux-x64: failed to solve redis-server for linux-x64"));
    let p = &artifacts(&report, "pitchfork")["linux-x64"];
    assert_eq!((p.state, p.reason.as_deref()), (stack::artifacts::State::Missing, None), "skipped without a reason");
    assert_eq!(artifacts(&report, "jq")["linux-x64"].state, stack::artifacts::State::Verified);
    assert!(report.warnings.iter().any(|w| w.contains("`mise lock` exited with status 1")), "{:?}", report.warnings);
}

#[test]
fn required_coverage_fails_before_anything_is_written() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[services.cache]\npreset = \"redis\"\nversion = \"8\"\n[lock]\nplatforms = [\"linux-x64\"]\nartifacts = \"required\"\n");
    let e = sb.compile(Mode::UseLock).unwrap_err();
    assert_eq!(e.code, "artifact_unlocked");
    assert!(e.details.iter().any(|d| d["name"] == "redis@8.2.1" && d["platform"] == "linux-x64" && d["state"] == "missing" && d["reason"].as_str().unwrap().contains("failed to solve")), "{e:?}");
    assert_eq!(sb.files(), vec![None, None, None]);
    // What mise just could not lock is not fixed by compiling again.
    let hint = e.hint.as_deref().unwrap();
    assert!(hint.contains("publishes no artifact") && hint.contains("Remove the platform") && !hint.contains("`stack compile`"), "{hint}");
    // Unsupported pins fail too.
    sb.write("[tools]\n\"npm:prettier\" = \"3.6.2\"\n[lock]\nplatforms = [\"linux-x64\"]\nartifacts = \"required\"\n");
    let e = sb.compile(Mode::UseLock).unwrap_err();
    assert_eq!((e.code, e.details[0]["state"].as_str()), ("artifact_unlocked", Some("unsupported")));
    assert!(e.hint.as_deref().unwrap().contains("never locked"), "{e:?}");
}

#[test]
fn checksums_of_a_platform_no_longer_listed_are_dropped_with_a_warning() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"macos-arm64\", \"macos-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    assert!(sb.embedded()["tools"]["jq"][0].get("platforms.macos-x64").is_some());
    // As a project relying on the default that once listed macos-x64.
    sb.write("[tools]\njq = \"1.7.1\"\n");
    let report = sb.compile(Mode::UseLock).unwrap();
    assert!(sb.embedded()["tools"]["jq"][0].get("platforms.macos-x64").is_none());
    assert!(report.warnings.iter().any(|w| w.contains("checksums for macos-x64 were dropped") && w.contains("lists it to keep them")), "{:?}", report.warnings);
    // Nothing more to say once they are gone.
    let report = sb.compile(Mode::UseLock).unwrap();
    assert!(!report.warnings.iter().any(|w| w.contains("dropped")), "{:?}", report.warnings);
}

#[test]
fn required_artifacts_under_the_default_platforms_cover_a_stack_with_a_service() {
    // Pitchfork publishes no Intel macOS build, as for its real 2.29.0 release.
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n[lock]\nartifacts = \"required\"\n");
    sb.mise.unpublished.lock().unwrap().push(("pitchfork".into(), "macos-x64".into()));
    let report = sb.compile(Mode::UseLock).unwrap();
    for name in ["jq", "pitchfork", "db"] {
        let covered = artifacts(&report, name);
        assert_eq!(covered.keys().collect::<Vec<_>>(), ["macos-arm64", "linux-x64", "linux-arm64"], "{name}");
        assert!(covered.values().all(|a| a.state == stack::artifacts::State::Verified), "{name}: {covered:?}");
    }
    sb.compile(Mode::Frozen).unwrap();
    // A tools-only project may still list Intel macOS itself.
    sb.write("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"macos-arm64\", \"macos-x64\"]\nartifacts = \"required\"\n");
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(artifacts(&report, "jq")["macos-x64"].state, stack::artifacts::State::Verified);
    // A service stack that lists it fails on Pitchfork, with a hint that does not repeat the compile.
    sb.write("[tools]\njq = \"1.7.1\"\n[services.db]\npreset = \"postgres\"\nversion = \"17\"\n[lock]\nplatforms = [\"macos-arm64\", \"macos-x64\"]\nartifacts = \"required\"\n");
    let e = sb.compile(Mode::UseLock).unwrap_err();
    assert_eq!(e.code, "artifact_unlocked");
    assert_eq!(e.details.iter().map(|d| (d["name"].as_str().unwrap(), d["platform"].as_str().unwrap())).collect::<Vec<_>>(), [("pitchfork@2.29.0", "macos-x64")]);
}

#[test]
fn failed_or_unreadable_locking_leaves_every_file_byte_identical() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n");
    sb.compile(Mode::UseLock).unwrap();
    sb.write("[tools]\njq = \"1.7.1\"\nfnox = \"1.39.0\"\n");
    let before = sb.files();
    for behaviour in [Behaviour::Garbage, Behaviour::Unrunnable] {
        sb.mise.set(behaviour);
        let e = sb.compile(Mode::UseLock).unwrap_err();
        assert_eq!(e.code, "artifact_lock_failed", "{behaviour:?}: {e:?}");
        assert!(e.hint.unwrap().contains("not changed"));
        assert_eq!(sb.files(), before, "{behaviour:?}");
    }
}

#[test]
fn an_old_mise_fails_provider_outdated_before_anything_is_written() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n");
    *sb.mise.version.lock().unwrap() = CalVer(2026, 9, 15);
    let e = sb.compile(Mode::UseLock).unwrap_err();
    assert_eq!(e.code, "provider_outdated");
    assert!(e.message.contains("2026.9.16"), "{e:?}");
    assert!(sb.mise.calls().is_empty());
    assert_eq!(sb.files(), vec![None, None, None]);
}

#[test]
fn version_2_locks_stay_usable_under_best_effort_and_migrate_on_compile() {
    let sb = Sandbox::new("[tools]\njq = \"1.7\"\n");
    fs::write(sb.root().join("stack.lock"), "version = 2\n[[tool]]\nname = \"jq\"\nrequested = \"1.7\"\nresolved = \"1.7.1\"\nresolved_on = \"macos-aarch64\"\n").unwrap();
    let report = sb.compile(Mode::Frozen).unwrap();
    assert!(artifacts(&report, "jq").values().all(|a| a.state == stack::artifacts::State::Missing));
    assert!(!sb.root().join(".config/mise/mise.lock").exists());
    assert_eq!(sb.lock().version, 2, "locked mode does not rewrite it");
    // Required refuses it.
    sb.write("[tools]\njq = \"1.7\"\n[lock]\nartifacts = \"required\"\n");
    assert_eq!(sb.compile(Mode::Frozen).unwrap_err().code, "lock_outdated");
    // Ordinary compile writes version 3 with mise's platform names.
    sb.write("[tools]\njq = \"1.7\"\n");
    sb.compile(Mode::UseLock).unwrap();
    let lock = sb.lock();
    assert_eq!(lock.version, 3);
    assert_eq!(lock.tool("jq").unwrap().resolved_on.as_deref(), Some("macos-arm64"));
    assert!(lock.provider_lock.is_some());
}

#[test]
fn embedded_entries_that_disagree_with_pins_are_lock_invalid_until_update() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    let text = fs::read_to_string(sb.root().join("stack.lock")).unwrap();
    let tampered = text.replacen("version = \"1.7.1\"", "version = \"1.6.0\"", 1);
    assert_ne!(text, tampered);
    fs::write(sb.root().join("stack.lock"), &tampered).unwrap();
    for mode in [Mode::Frozen, Mode::UseLock] {
        let e = sb.compile(mode).unwrap_err();
        assert_eq!(e.code, "lock_invalid", "{mode:?}: {e:?}");
        assert!(e.message.contains("tools.jq"), "{e:?}");
    }
    sb.compile(Mode::Update).unwrap();
    assert_eq!(checksum(&sb.embedded(), "jq", "linux-x64"), "sha256:jq-1.7.1-linux-x64-g1");
}

#[test]
fn sidecars_are_stripped_unsupported_and_never_rendered() {
    let sb = Sandbox::new("[tools]\n\"npm:prettier\" = \"3.6.2\"\nrust = \"1.93.1\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    let report = sb.compile(Mode::UseLock).unwrap();
    // npm is unsupported by name, so only rust needed locking.
    assert_eq!(sb.mise.calls()[0].tools, ["rust"]);
    assert_eq!(artifacts(&report, "npm:prettier")["linux-x64"].state, stack::artifacts::State::Unsupported);
    assert_eq!(artifacts(&report, "rust")["linux-x64"].state, stack::artifacts::State::Exempt);
    sb.compile(Mode::Update).unwrap();
    let embedded = sb.embedded();
    assert!(embedded["tools"]["npm:prettier"][0].get("aube").is_none());
    assert_eq!(embedded["stack_sidecars"][0]["tool"].as_str(), Some("npm:prettier"));
    assert!(!sb.rendered().contains("aube") && !sb.rendered().contains("stack_sidecars"));
}

#[test]
fn several_releases_of_one_tool_and_unknown_provider_fields_survive() {
    let sb = Sandbox::new("[services.a]\npreset = \"postgres\"\nversion = \"17.6\"\n[services.b]\npreset = \"postgres\"\nversion = \"16.4\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    let embedded = sb.embedded();
    let versions: Vec<&str> = embedded["tools"]["postgres"].as_array().unwrap().iter().map(|e| e["version"].as_str().unwrap()).collect();
    assert_eq!(versions, ["17.6", "16.4"]);
    let config: Table = toml::from_str(&sb.mise.calls()[0].config).unwrap();
    assert_eq!(config["tools"]["postgres"].as_array().unwrap().len(), 2);
    // A field stack does not know, on a committed entry, survives an ordinary compile that
    // re-locks the tool and is written to the rendered lock.
    let text = fs::read_to_string(sb.root().join("stack.lock")).unwrap();
    let text = text.replacen("backend = \"conda:postgresql\"", "backend = \"conda:postgresql\"\nfuture_field = \"kept\"", 1);
    fs::write(sb.root().join("stack.lock"), text).unwrap();
    sb.write("[services.a]\npreset = \"postgres\"\nversion = \"17.6\"\n[services.b]\npreset = \"postgres\"\nversion = \"16.4\"\n[lock]\nplatforms = [\"linux-x64\", \"linux-arm64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    assert_eq!(sb.embedded()["tools"]["postgres"][0]["future_field"].as_str(), Some("kept"));
    assert!(sb.rendered().contains("future_field = \"kept\""));
}

#[test]
fn template_syntax_never_reaches_a_scratch_config_or_the_rendered_lock() {
    // An option value mise would evaluate as a template.
    let sb = Sandbox::new("[tools]\nfnox = { version = \"1.39.0\", identity = \"{{ exec(command='touch sentinel') }}\" }\n");
    let e = sb.compile(Mode::UseLock).unwrap_err();
    assert_eq!(e.code, "invalid_tool", "{e:?}");
    assert!(e.message.contains("template syntax"), "{e:?}");
    assert!(sb.mise.calls().is_empty());
    assert!(!sb.root().join("sentinel").exists());
    assert_eq!(sb.files(), vec![None, None, None]);

    // A committed provider lock carrying a template is refused before anything renders it.
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"linux-x64\"]\n");
    sb.compile(Mode::UseLock).unwrap();
    let text = fs::read_to_string(sb.root().join("stack.lock")).unwrap();
    let text = text.replacen("url = \"https://example.invalid/jq/1.7.1/linux-x64\"", "url = \"{{ exec(command='touch sentinel') }}\"", 1);
    fs::write(sb.root().join("stack.lock"), &text).unwrap();
    fs::remove_file(sb.root().join(".config/mise/mise.lock")).unwrap();
    assert!(text.contains("touch sentinel"), "{text}");
    for mode in [Mode::Frozen, Mode::UseLock] {
        assert_eq!(sb.compile(mode).unwrap_err().code, "lock_invalid", "{mode:?}");
    }
    assert!(!sb.root().join(".config/mise/mise.lock").exists());
    // `--update` replaces the bad commitment instead of carrying it.
    sb.compile(Mode::Update).unwrap();
    assert!(!sb.rendered().contains("sentinel") && !fs::read_to_string(sb.root().join("stack.lock")).unwrap().contains("sentinel"));
    assert!(!sb.root().join("sentinel").exists());
}

#[test]
fn compile_reports_inspect_artifacts_without_a_lock_as_unresolved() {
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n");
    let report = sb.run(mise_inspect_mode(&sb), false).unwrap();
    assert!(report.versions[0].artifacts.is_none(), "nothing is resolved by inspect without a lock");
    assert!(sb.mise.calls().is_empty());
}

fn mise_inspect_mode(sb: &Sandbox) -> Mode {
    stack::project::inspect_mode(&sb.root())
}

#[test]
fn only_the_project_sets_the_artifact_policy() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("bundle.toml"), "[bundle]\nname = \"b\"\n[lock]\nartifacts = \"best-effort\"\n").unwrap();
    let e = stack::manifest::read_bundle(dir.path(), "path:b").unwrap_err();
    assert_eq!(e.code, "bundle_invalid");
    assert!(e.message.contains("lock"), "{e:?}");
    let sb = Sandbox::new("[tools]\njq = \"1.7.1\"\n[lock]\nplatforms = [\"macos-aarch64\"]\n");
    let e = sb.compile(Mode::UseLock).unwrap_err();
    assert_eq!(e.code, "manifest_invalid");
    assert_eq!(sb.files(), vec![None, None, None]);
}

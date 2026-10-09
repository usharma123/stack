//! Coverage on a machine is coverage under the key mise looks each pin up by there: Bun's build
//! variant (`-baseline` without AVX2, `-musl`, `-musl-baseline`) and `-musl` for every other
//! backend on a musl Linux. The machines here are SIMULATED (`Host` values and a locker that
//! reports one); no Alpine or non-AVX2 machine runs these tests. The lock under test holds the
//! tables real mise 2026.10.3 wrote for bun 1.3.0 and node 24.13.0.

#[path = "support/mise_variants.rs"]
mod support;

use stack::artifacts::{self, Host, Libc, PinKey};
use stack::error::StackError;
use stack::provider::mise::{CalVer, LockRun, Locker, ScratchRoot};
use stack::source::Mode;
use std::path::Path;
use std::sync::Arc;
use support::{real, FakeMise, Sandbox, REAL_BUN, REAL_NODE};
use toml::Table;

fn host(os: &str, arch: &str, libc: Option<Libc>, avx2: bool) -> Host {
    Host { os: os.into(), arch: arch.into(), libc, avx2 }
}

fn glibc(avx2: bool) -> Host {
    host("linux", "x64", Some(Libc::Gnu), avx2)
}

fn alpine(avx2: bool) -> Host {
    host("linux", "x64", Some(Libc::Musl), avx2)
}

fn pin(tool: &str, version: &str) -> PinKey {
    PinKey { tool: tool.into(), version: version.into() }
}

/// The lock real mise wrote for both releases, embedded as stack.lock does.
fn real_lock() -> Table {
    let mut lock: Table = toml::from_str(REAL_BUN).unwrap();
    let node: Table = toml::from_str(REAL_NODE).unwrap();
    lock["tools"].as_table_mut().unwrap().insert("node".into(), node["tools"]["node"].clone());
    artifacts::embed(lock)
}

/// The same lock without one table, as a lock mise could not complete leaves it.
fn without(mut lock: Table, tool: &str, key: &str) -> Table {
    lock["tools"][tool][0].as_table_mut().unwrap().remove(&format!("platforms.{key}"));
    lock
}

#[test]
fn each_simulated_machine_resolves_bun_to_its_build_variant_and_other_backends_to_mises_name() {
    let lock = real_lock();
    let bun = pin("bun", "1.3.0");
    let node = pin("node", "24.13.0");
    let cases = [
        (glibc(true), "linux-x64", "linux-x64"),
        (glibc(false), "linux-x64-baseline", "linux-x64"),
        (alpine(true), "linux-x64-musl", "linux-x64-musl"),
        (alpine(false), "linux-x64-musl-baseline", "linux-x64-musl"),
        (host("macos", "x64", None, false), "macos-x64-baseline", "macos-x64"),
        (host("macos", "arm64", None, false), "macos-arm64", "macos-arm64"),
        (host("linux", "arm64", Some(Libc::Musl), false), "linux-arm64-musl", "linux-arm64-musl"),
    ];
    for (machine, bun_key, node_key) in cases {
        let at = artifacts::coverage_at(Some(&lock), &bun, &machine);
        assert_eq!(at.key, bun_key, "{machine:?}");
        assert_eq!(machine.key(Some("core:node"), "node").unwrap(), node_key, "{machine:?}");
        if REAL_BUN.contains(&format!("platforms.{bun_key}\"")) {
            assert_eq!(at.coverage.state.name(), "verified", "{machine:?}");
            assert_eq!(at.coverage.checksum.unwrap(), real(REAL_BUN, "bun", bun_key), "{machine:?}: checked against its own variant");
        }
    }
    // Node on Alpine: the lock holds linux-x64-musl here, so it is verified with that value.
    let at = artifacts::coverage_at(Some(&lock), &node, &alpine(true));
    assert_eq!((at.key.as_str(), at.coverage.state.name()), ("linux-x64-musl", "verified"));
    assert_eq!(at.coverage.checksum.unwrap(), real(REAL_NODE, "node", "linux-x64-musl"));
}

#[test]
fn a_linux_x64_entry_is_never_called_verified_on_a_machine_that_needs_another_key() {
    let bun = pin("bun", "1.3.0");
    let node = pin("node", "24.13.0");
    let pins = [bun.clone(), node.clone()];
    // What the reviewer's lock committed: linux-x64 alone for both.
    let mut lock = real_lock();
    for key in ["linux-x64-baseline", "linux-x64-musl", "linux-x64-musl-baseline"] {
        lock = without(lock, "bun", key);
    }
    lock = without(lock, "node", "linux-x64-musl");
    for machine in [alpine(true), alpine(false), glibc(false)] {
        let summary = artifacts::summary(Some(&lock), &pins, &machine);
        let partition = artifacts::partition(Some(&lock), &pins, &machine);
        assert!(summary.missing.contains(&"bun@1.3.0".to_string()), "{machine:?}: {summary:?}");
        assert!(partition.plain.contains(&"bun".to_string()), "{machine:?}: never `--locked` without its entry");
        let details = artifacts::unchecked(Some(&lock), std::slice::from_ref(&bun), std::slice::from_ref(&machine), &Default::default());
        assert_eq!(details[0]["platform"], machine.key(Some("core:bun"), "bun").unwrap(), "the error names the key mise needs");
    }
    let summary = artifacts::summary(Some(&lock), &pins, &alpine(true));
    assert_eq!(summary.platform, "linux-x64-musl");
    assert_eq!(summary.missing, ["bun@1.3.0", "node@24.13.0"]);
    // A glibc AVX2 machine is what linux-x64 was locked for.
    let summary = artifacts::summary(Some(&lock), &pins, &glibc(true));
    assert_eq!(summary.verified, ["bun@1.3.0", "node@24.13.0"]);
    assert!(summary.keys.is_empty(), "{summary:?}");
    assert_eq!(artifacts::partition(Some(&lock), &pins, &glibc(true)).locked, ["bun", "node"]);
}

#[test]
fn the_summary_names_the_keys_that_differ_from_the_machines_name() {
    let pins = [pin("bun", "1.3.0"), pin("node", "24.13.0")];
    let summary = artifacts::summary(Some(&real_lock()), &pins, &glibc(false));
    assert_eq!(summary.platform, "linux-x64");
    assert_eq!(summary.verified, ["bun@1.3.0", "node@24.13.0"]);
    let json = serde_json::to_value(&summary).unwrap();
    assert_eq!(json["keys"], serde_json::json!({ "bun@1.3.0": "linux-x64-baseline" }));
    assert!(json.get("reason").is_none());
}

#[test]
fn a_linux_whose_c_library_cannot_be_told_has_nothing_verified_and_says_why() {
    let pins = [pin("bun", "1.3.0"), pin("node", "24.13.0"), pin("rust", "1.90.0")];
    let mut lock = real_lock();
    let rust: Table = toml::from_str("[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"core:rust\"\n").unwrap();
    lock["tools"].as_table_mut().unwrap().insert("rust".into(), rust["tools"]["rust"].clone());
    let machine = host("linux", "x64", Some(Libc::Undetected), true);
    let summary = artifacts::summary(Some(&lock), &pins, &machine);
    assert_eq!(summary.missing, ["bun@1.3.0", "node@24.13.0"]);
    assert_eq!(summary.exempt, ["rust@1.90.0"], "a backend mise never checks does not depend on the key");
    assert!(summary.reason.as_deref().is_some_and(|r| r.contains("glibc or musl")), "{summary:?}");
    let details = artifacts::unchecked(Some(&lock), &pins[..1], std::slice::from_ref(&machine), &Default::default());
    assert!(details[0]["reason"].as_str().unwrap().contains("glibc or musl"), "{details:?}");
}

/// The fake locker, on a simulated machine.
struct On(Arc<FakeMise>, Host);

impl Locker for On {
    fn version(&self, root: &Path) -> Result<CalVer, StackError> {
        self.0.version(root)
    }

    fn lock(&self, scratch: &ScratchRoot, platforms: &[String], tools: &[String]) -> Result<LockRun, StackError> {
        self.0.lock(scratch, platforms, tools)
    }

    fn host(&self) -> Host {
        self.1.clone()
    }
}

fn sandbox(tools: &str, platforms: &str, required: bool, machine: Host) -> Sandbox {
    let policy = if required { "artifacts = \"required\"\n" } else { "" };
    let toml = format!("[tools]\n{tools}\n[lock]\nplatforms = [{platforms}]\n{policy}");
    Sandbox::new(&toml, Arc::new(On(FakeMise::new(), machine)))
}

#[test]
fn required_locked_operations_on_a_simulated_alpine_need_the_entries_mise_needs_there() {
    // Listing the qualified platform lists the machine; locked operations run.
    let sb = sandbox("bun = \"1.3.0\"\nnode = \"24.13.0\"", "\"linux-x64-musl\"", true, alpine(true));
    sb.compile(Mode::UseLock).unwrap();
    sb.compile(Mode::Frozen).unwrap();

    // Listing linux-x64 lists it too (Bun generates musl variants for it), but Node has no
    // musl entry from linux-x64, so a locked operation refuses naming the key mise needs.
    let sb = sandbox("bun = \"1.3.0\"\nnode = \"24.13.0\"", "\"linux-x64\"", true, alpine(true));
    sb.compile(Mode::UseLock).unwrap();
    let e = sb.compile(Mode::Frozen).unwrap_err();
    assert_eq!(e.code, "artifact_unlocked", "{e:?}");
    assert_eq!(e.details, vec![serde_json::json!({ "name": "node@24.13.0", "platform": "linux-x64-musl", "state": "missing" })]);

    // Bun alone is covered there by the variant linux-x64 generated.
    let sb = sandbox("bun = \"1.3.0\"", "\"linux-x64\"", true, alpine(false));
    sb.compile(Mode::UseLock).unwrap();
    sb.compile(Mode::Frozen).unwrap();

    // A glibc machine is not listed by linux-x64-musl.
    let sb = sandbox("bun = \"1.3.0\"", "\"linux-x64-musl\"", true, glibc(true));
    sb.compile(Mode::UseLock).unwrap();
    let e = sb.compile(Mode::Frozen).unwrap_err();
    assert_eq!(e.code, "artifact_unlocked");
    assert_eq!(e.details, vec![serde_json::json!({ "platform": "linux-x64", "state": "unlisted", "platforms": ["linux-x64-musl"] })]);

    // Best effort never refuses; the gap is reported instead.
    let sb = sandbox("node = \"24.13.0\"", "\"linux-x64\"", false, alpine(true));
    sb.compile(Mode::UseLock).unwrap();
    sb.compile(Mode::Frozen).unwrap();
}

#[test]
fn a_bun_variant_mise_did_not_lock_is_refused_on_the_machine_that_needs_it() {
    let fake = FakeMise::new();
    fake.unpublished.lock().unwrap().push("linux-x64-musl-baseline".into());
    let sb = Sandbox::new("[tools]\nbun = \"1.3.0\"\n[lock]\nplatforms = [\"linux-x64\"]\nartifacts = \"required\"\n", Arc::new(On(fake, alpine(false))));
    // Compile requires the listed platform, which is locked; the variant is reported missing.
    let report = sb.compile(Mode::UseLock).unwrap();
    let artifacts = report.versions[0].artifacts.as_ref().unwrap();
    assert_eq!(artifacts["linux-x64"].state.name(), "verified");
    assert_eq!(artifacts["linux-x64-musl-baseline"].state.name(), "missing");
    assert!(artifacts["linux-x64-musl-baseline"].reason.as_deref().is_some_and(|r| r.contains("not available")), "{artifacts:?}");
    let e = sb.compile(Mode::Frozen).unwrap_err();
    assert_eq!(e.code, "artifact_unlocked");
    assert_eq!(e.details, vec![serde_json::json!({ "name": "bun@1.3.0", "platform": "linux-x64-musl-baseline", "state": "missing" })]);
}

#[test]
fn current_names_the_simulated_machine_as_mise_does() {
    let sb = sandbox("node = \"24.13.0\"", "\"current\"", true, alpine(true));
    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(report.artifact_policy.platforms, ["linux-x64-musl"]);
    assert_eq!(support::keys(&sb.embedded(), "node"), ["linux-x64-musl"]);
    sb.compile(Mode::Frozen).unwrap();
}

#[test]
fn required_policies_list_machines_by_the_keys_they_need() {
    use stack::manifest::LockSettings;
    let policy = |platforms: &[&str]| {
        artifacts::Policy::from_settings(&LockSettings { platforms: Some(platforms.iter().map(|p| p.to_string()).collect()), artifacts: Some("required".into()) }).unwrap()
    };
    let listed = |p: &artifacts::Policy, machine: &Host| p.check_host(machine).is_ok();
    assert!(listed(&policy(&["linux-x64"]), &alpine(true)));
    assert!(listed(&policy(&["linux-x64"]), &glibc(false)));
    assert!(listed(&policy(&["linux-x64-musl"]), &alpine(true)));
    assert!(listed(&policy(&["linux-x64-musl-baseline"]), &alpine(false)));
    assert!(listed(&policy(&["linux-arm64"]), &host("linux", "arm64", Some(Libc::Musl), false)));
    assert!(!listed(&policy(&["linux-x64-musl"]), &glibc(true)));
    assert!(!listed(&policy(&["linux-x64-baseline"]), &alpine(true)));
    assert!(!listed(&policy(&["linux-arm64"]), &alpine(true)));
    assert!(!listed(&policy(&["macos-x64"]), &host("macos", "arm64", None, false)));
    // A Linux of unknown C library is listed by its OS and architecture; coverage decides.
    assert!(listed(&policy(&["linux-x64-musl"]), &host("linux", "x64", Some(Libc::Undetected), true)));
    let e = policy(&["linux-x64-musl"]).check_host(&glibc(true)).unwrap_err();
    assert!(e.message.contains("linux-x64 is not in [lock] platforms (linux-x64-musl)"), "{e:?}");
}

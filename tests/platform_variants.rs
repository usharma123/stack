//! Lock entries mise generates beyond the requested platform (Bun's build variants) are kept,
//! compared and reported like any other entry. Uses only the public API that predates host
//! resolution, so the same file shows the old behaviour (variants dropped) when run against it.

#[path = "support/mise_variants.rs"]
mod support;

use stack::source::Mode;
use support::{checksum, keys, real, FakeMise, Sandbox, REAL_BUN, REAL_NODE};

const BUN_ALL: &[&str] = &[
    "linux-x64",
    "linux-x64-baseline",
    "linux-x64-musl",
    "linux-x64-musl-baseline",
    "macos-arm64",
    "macos-x64",
    "macos-x64-baseline",
];

fn project(tools: &str, platforms: &str) -> String {
    format!("[tools]\n{tools}\n[lock]\nplatforms = [{platforms}]\n")
}

#[test]
fn the_variants_mise_generates_for_a_listed_platform_are_committed_rendered_and_reported() {
    let mise = FakeMise::new();
    let sb = Sandbox::new(&project("bun = \"1.3.0\"", "\"linux-x64\", \"macos-arm64\", \"macos-x64\""), mise.clone());
    let report = sb.compile(Mode::UseLock).unwrap();
    let embedded = sb.embedded();
    assert_eq!(keys(&embedded, "bun"), BUN_ALL, "every table real mise wrote is committed");
    for key in BUN_ALL {
        assert_eq!(checksum(&embedded, "bun", key), real(REAL_BUN, "bun", key), "{key} is mise's value, not another key's");
    }
    let rendered = sb.rendered();
    for key in BUN_ALL {
        assert!(rendered.contains(&format!("platforms.{key}")), "{key} reaches the rendered mise.lock");
    }
    let artifacts = report.versions.iter().find(|v| v.name == "bun").unwrap().artifacts.clone().unwrap();
    let reported: Vec<&str> = artifacts.keys().map(String::as_str).collect();
    assert_eq!(reported, ["linux-x64", "linux-x64-baseline", "linux-x64-musl", "linux-x64-musl-baseline", "macos-arm64", "macos-x64", "macos-x64-baseline"]);
    for (key, r) in &artifacts {
        assert_eq!(r.state.name(), "verified", "{key}");
        assert_eq!(r.change, Some("added"), "{key}");
        assert_eq!(r.checksum.as_deref(), Some(real(REAL_BUN, "bun", key).as_str()), "{key}");
    }

    // A second compile asks mise nothing and leaves every variant byte-identical.
    let before = std::fs::read(sb.root().join("stack.lock")).unwrap();
    sb.compile(Mode::UseLock).unwrap();
    assert_eq!(mise.calls().len(), 1);
    assert_eq!(std::fs::read(sb.root().join("stack.lock")).unwrap(), before);
}

#[test]
fn backends_without_variants_keep_only_the_listed_key_and_a_qualified_target_is_its_own() {
    let mise = FakeMise::new();
    let sb = Sandbox::new(&project("bun = \"1.3.0\"\nnode = \"24.13.0\"", "\"linux-x64\""), mise.clone());
    sb.compile(Mode::UseLock).unwrap();
    let embedded = sb.embedded();
    assert_eq!(keys(&embedded, "node"), ["linux-x64"], "mise writes no musl table for Node from linux-x64");
    assert_eq!(keys(&embedded, "bun"), ["linux-x64", "linux-x64-baseline", "linux-x64-musl", "linux-x64-musl-baseline"]);

    // Listing the qualified platform locks it for every backend, and Bun writes it alone.
    let sb = Sandbox::new(&project("bun = \"1.3.0\"\nnode = \"24.13.0\"", "\"linux-x64-musl\""), FakeMise::new());
    sb.compile(Mode::UseLock).unwrap();
    let embedded = sb.embedded();
    assert_eq!(keys(&embedded, "node"), ["linux-x64-musl"]);
    assert_eq!(checksum(&embedded, "node", "linux-x64-musl"), real(REAL_NODE, "node", "linux-x64-musl"));
    assert_eq!(keys(&embedded, "bun"), ["linux-x64-musl"]);
}

#[test]
fn removing_the_listed_platform_drops_its_variants_and_nothing_else() {
    let mise = FakeMise::new();
    let sb = Sandbox::new(&project("bun = \"1.3.0\"", "\"linux-x64\", \"macos-x64\""), mise.clone());
    sb.compile(Mode::UseLock).unwrap();
    sb.write(&project("bun = \"1.3.0\"", "\"macos-x64\""));
    sb.compile(Mode::UseLock).unwrap();
    assert_eq!(mise.calls().len(), 1, "dropping platforms needs no locking");
    assert_eq!(keys(&sb.embedded(), "bun"), ["macos-x64", "macos-x64-baseline"]);
}

#[test]
fn a_variant_that_differs_upstream_is_kept_and_warned_about_and_only_update_accepts_it() {
    let mise = FakeMise::new();
    let sb = Sandbox::new(&project("bun = \"1.3.0\"", "\"linux-x64\""), mise.clone());
    sb.compile(Mode::UseLock).unwrap();
    let committed = real(REAL_BUN, "bun", "linux-x64-musl");
    // One variant gone from the commitment makes ordinary compile lock bun again.
    drop_keys(&sb, "bun", |k| k == "platforms.linux-x64-baseline");
    mise.changed.lock().unwrap().push("linux-x64-musl".into());

    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(mise.calls().len(), 2);
    assert_eq!(checksum(&sb.embedded(), "bun", "linux-x64-musl"), committed, "never taken without --update");
    assert!(report.warnings.iter().any(|w| w == "artifacts.bun@1.3.0.linux-x64-musl differs upstream; run `stack compile --update` to accept"), "{:?}", report.warnings);
    let artifacts = report.versions.iter().find(|v| v.name == "bun").unwrap().artifacts.clone().unwrap();
    assert_eq!(artifacts["linux-x64-musl"].change, Some("differs_upstream"));
    assert_eq!(artifacts["linux-x64-baseline"].change, Some("added"));

    let report = sb.compile(Mode::Update).unwrap();
    let embedded = sb.embedded();
    assert_eq!(checksum(&embedded, "bun", "linux-x64-musl"), "sha256:upstream-changed-linux-x64-musl");
    assert_eq!(checksum(&embedded, "bun", "linux-x64"), real(REAL_BUN, "bun", "linux-x64"), "the base key is not rebased onto the variant");
    let musl = &report.versions.iter().find(|v| v.name == "bun").unwrap().artifacts.as_ref().unwrap()["linux-x64-musl"];
    assert_eq!(musl.change, Some("artifact_changed"));
    assert_eq!(musl.checksum_was.as_deref(), Some(committed.as_str()));
}

/// Remove platform tables from the committed entry, as an older build or a hand edit leaves it.
fn drop_keys(sb: &Sandbox, tool: &str, drop: impl Fn(&str) -> bool) {
    let path = sb.root().join("stack.lock");
    let mut doc: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = doc["provider_lock"]["tools"][tool][0].as_table().unwrap().clone();
    let kept: toml::Table = entry.into_iter().filter(|(k, _)| !drop(k)).collect();
    doc["provider_lock"]["tools"][tool] = toml::Value::Array(vec![kept.into()]);
    std::fs::write(&path, toml::to_string_pretty(&doc).unwrap()).unwrap();
}

#[test]
fn an_update_that_produces_no_fresh_variant_retains_the_committed_one() {
    let mise = FakeMise::new();
    let sb = Sandbox::new(&project("bun = \"1.3.0\"", "\"linux-x64\""), mise.clone());
    sb.compile(Mode::UseLock).unwrap();
    mise.unpublished.lock().unwrap().push("linux-x64-musl-baseline".into());
    let report = sb.compile(Mode::Update).unwrap();
    assert_eq!(checksum(&sb.embedded(), "bun", "linux-x64-musl-baseline"), real(REAL_BUN, "bun", "linux-x64-musl-baseline"));
    let r = &report.versions.iter().find(|v| v.name == "bun").unwrap().artifacts.as_ref().unwrap()["linux-x64-musl-baseline"];
    assert_eq!((r.state.name(), r.change), ("verified", Some("retained")), "kept and said to be kept, never refreshed");
}

#[test]
fn a_lock_without_the_generated_variants_gets_them_added_without_touching_the_listed_key() {
    // What a build that dropped variants committed: the listed key only.
    let mise = FakeMise::new();
    let sb = Sandbox::new(&project("bun = \"1.3.0\"", "\"linux-x64\""), mise.clone());
    sb.compile(Mode::UseLock).unwrap();
    drop_keys(&sb, "bun", |k| k.starts_with("platforms.linux-x64-"));
    mise.changed.lock().unwrap().push("linux-x64".into());

    let report = sb.compile(Mode::UseLock).unwrap();
    assert_eq!(mise.calls().len(), 2, "missing variants are locked");
    let embedded = sb.embedded();
    assert_eq!(keys(&embedded, "bun"), ["linux-x64", "linux-x64-baseline", "linux-x64-musl", "linux-x64-musl-baseline"]);
    assert_eq!(checksum(&embedded, "bun", "linux-x64"), real(REAL_BUN, "bun", "linux-x64"), "the committed key is kept");
    let artifacts = report.versions.iter().find(|v| v.name == "bun").unwrap().artifacts.clone().unwrap();
    assert_eq!(artifacts["linux-x64"].change, Some("differs_upstream"));
    assert_eq!(artifacts["linux-x64-musl"].change, Some("added"));
}

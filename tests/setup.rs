#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn fake_mise(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let mise = dir.join("mise");
    fs::write(&mise, "#!/bin/sh\necho '2026.9.18 fake'\n").unwrap();
    fs::set_permissions(&mise, fs::Permissions::from_mode(0o755)).unwrap();
}

fn stack(data: &Path, path: &str, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_stack"))
        .args(args)
        .env("STACK_DATA_DIR", data)
        .env("PATH", path)
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn a_mise_installed_by_setup_is_found_without_being_on_path() {
    let data = tempfile::tempdir().unwrap();
    fake_mise(&data.path().join("bin"));
    let out = stack(data.path(), "/usr/bin:/bin", &["setup", "--json"]);
    assert_eq!(out["data"]["installed"], false, "{out}");
    assert_eq!(out["data"]["mise"], data.path().join("bin/mise").to_str().unwrap(), "{out}");
}

#[test]
fn a_mise_the_user_installed_takes_precedence() {
    let data = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    fake_mise(&data.path().join("bin"));
    fake_mise(user.path());
    let out = stack(data.path(), &format!("{}:/usr/bin:/bin", user.path().display()), &["setup", "--json"]);
    assert_eq!(out["data"]["mise"], user.path().join("mise").to_str().unwrap(), "{out}");
}

#[test]
fn a_missing_mise_points_to_setup() {
    let data = tempfile::tempdir().unwrap();
    let out = stack(data.path(), "/usr/bin:/bin", &["doctor", "--json"]);
    let checks = out["error"]["details"].as_array().unwrap();
    let mise = checks.iter().find(|c| c["name"] == "mise").unwrap();
    assert_eq!(mise["ok"], false, "{out}");
    assert!(mise["hint"].as_str().unwrap().contains("stack setup"), "{out}");
}

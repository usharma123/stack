//! `stack doctor`: check the providers stack relies on before the first `up` fails on them.

use crate::error::{Result, StackError};
use crate::project::{self, Options};
use serde::Serialize;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

/// Every check, or `doctor_failed` listing them all when any fails.
pub fn run(root: &Path, cache: &Path, state: &Path) -> Result<Vec<Check>> {
    let mut checks = vec![
        tool("mise", "mise", &["--version"], crate::setup::MISE_INSTALL_HINT),
        tool("git", "git", &["--version"], "install git; it fetches git+ bundles"),
        tool("tar", "tar", &["--version"], "install tar; it extracts git bundles"),
    ];
    if checks[0].ok {
        // Services run through `mise daemons`, which older mise releases lack.
        checks.push(tool(
            "mise_daemons",
            "mise",
            &["daemons", "--help"],
            "upgrade mise (`mise self-update`); services need `mise daemons`",
        ));
    }
    checks.push(writable(state));
    checks.push(stack_release(root));
    if root.join(crate::manifest::PROJECT_FILE).exists() {
        let compiled = project::compile(&Options {
            root: root.to_path_buf(),
            mode: project::inspect_mode(root),
            write: false,
            cache: cache.to_path_buf(),
            state: state.to_path_buf(),
            reassign_ports: false,
            resolver: None,
            locker: None,
        });
        if let Ok(r) = &compiled {
            if let Some(check) = provider_release(root, &project::install_requirements(r)) {
                checks.push(check);
            }
            let fnox = r.versions.iter().find(|v| v.kind == "tool" && v.name == crate::secrets::FNOX).and_then(|v| v.resolved.as_deref());
            if let Some(f) = crate::secrets::doctor(root, cache, &r.stack, fnox) {
                let hint = (!f.ok).then_some("run `fnox env --json --describe` in the project to see fnox's own message; stack never shows it");
                checks.push(Check { name: "fnox", ok: f.ok, detail: f.detail, hint });
            }
        }
        checks.push(match compiled {
            Ok(r) => Check {
                name: "project",
                ok: true,
                detail: format!(
                    "{} bundle(s), {} service(s){}",
                    r.bundles.len(),
                    r.stack.services.len(),
                    if r.lock_changed { "; stack.lock needs `stack compile`" } else { "" }
                ),
                hint: None,
            },
            Err(e) => Check { name: "project", ok: false, detail: e.to_string(), hint: None },
        });
    }
    let failed = checks.iter().filter(|c| !c.ok).count();
    if failed == 0 {
        return Ok(checks);
    }
    Err(StackError::new("doctor_failed", format!("{failed} check(s) failed"))
        .details(checks.iter().map(|c| serde_json::to_value(c).expect("check serializes")).collect()))
}

fn tool(name: &'static str, program: &str, args: &[&str], hint: &'static str) -> Check {
    let out = Command::new(program).args(args).stdin(Stdio::null()).output();
    match out {
        Ok(o) if o.status.success() => Check {
            name,
            ok: true,
            detail: String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default().trim().to_string(),
            hint: None,
        },
        Ok(o) => Check {
            name,
            ok: false,
            detail: format!("`{program} {}` exited with {}", args.join(" "), o.status),
            hint: Some(hint),
        },
        Err(e) => Check { name, ok: false, detail: format!("cannot run {program}: {e}"), hint: Some(hint) },
    }
}

/// Whether mise is new enough for what this project's configuration asks (`mr_boxington`).
/// Nothing to check when the configuration needs nothing beyond any stack.
fn provider_release(root: &Path, requirements: &[crate::provider::mise::Requirement]) -> Option<Check> {
    use crate::provider::mise;
    let version = match mise::require(root, requirements) {
        Ok(version) => version?,
        Err(e) => return Some(Check { name: "mise_release", ok: false, detail: e.message, hint: Some("upgrade mise (`mise self-update`)") }),
    };
    let needs: Vec<String> = requirements.iter().map(|r| format!("{} ({})", r.minimum, r.reason)).collect();
    Some(Check { name: "mise_release", ok: true, detail: format!("mise {version}; needs {}", needs.join(", ")), hint: None })
}

/// This Stack's release and binary, and anything that disagrees with it: another `stack` first
/// on PATH (which agents, hooks and `stack mcp` registrations run), or a stack.lock another
/// release last wrote. A notice, not a failure: running a build by its path is legitimate.
fn stack_release(root: &Path) -> Check {
    let me = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let mut notes = vec![format!("{} at {}", crate::lock::writer(), me.as_ref().map_or("?".into(), |p| p.display().to_string()))];
    let on_path = std::env::var("PATH").ok().and_then(|path| crate::session::which_in(Some(&path), "stack")).and_then(|p| p.canonicalize().ok());
    match on_path {
        Some(found) if !same_install(&found, me.as_deref()) => {
            let mut command = Command::new(&found);
            command.arg("--version");
            let version = crate::process::capture(&mut command, std::time::Duration::from_secs(5), 4096)
                .ok()
                .map(|o| o.stdout.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "an unknown release".into());
            notes.push(format!("warning: `stack` on PATH is {} ({version}), not this one", found.display()));
        }
        Some(_) => {}
        None => notes.push("warning: no `stack` on PATH; hooks and MCP clients that run `stack` will not find this one".into()),
    }
    if let Ok(Some(lock)) = crate::lock::read(root) {
        if let Some(other) = lock.written_by.filter(|w| *w != crate::lock::writer()) {
            notes.push(format!("note: stack.lock was last written by {other}"));
        }
    }
    let warned = notes.iter().any(|n| n.starts_with("warning:") || n.starts_with("note:"));
    Check {
        name: "stack",
        ok: true,
        detail: notes.join("; "),
        hint: warned.then_some("run the same Stack everywhere: put it first on PATH, or register MCP and hooks with its full path"),
    }
}

/// Whether `on_path` launches `me`: either the exact same path, or the npm package wrapper
/// `<pkg>/bin/stack.cjs` launching `<pkg>/binaries/<platform>/stack` in the same package.
fn same_install(on_path: &Path, me: Option<&Path>) -> bool {
    let Some(me) = me else { return false };
    if on_path == me {
        return true;
    }
    if on_path.file_name().and_then(|n| n.to_str()) != Some("stack.cjs") {
        return false;
    }
    let Some(bin_dir) = on_path.parent() else { return false };
    if bin_dir.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return false;
    }
    let Some(pkg_root) = bin_dir.parent() else { return false };
    let Ok(rel) = me.strip_prefix(pkg_root) else { return false };
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    matches!(
        parts.as_slice(),
        [
            Some("binaries"),
            Some("darwin-x64" | "darwin-arm64" | "linux-x64" | "linux-arm64"),
            Some("stack")
        ]
    )
}

fn writable(state: &Path) -> Check {
    let probe = std::fs::create_dir_all(state).and_then(|_| tempfile::tempfile_in(state).map(drop));
    let ok = probe.is_ok();
    Check {
        name: "state_dir",
        ok,
        detail: match probe {
            Ok(()) => state.display().to_string(),
            Err(e) => format!("{}: {e}", state.display()),
        },
        hint: (!ok).then_some("set STACK_STATE_DIR to a writable directory"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_wrapper_matches_its_packaged_binary_only() {
        let pkg = Path::new("/opt/homebrew/lib/node_modules/@ushawarma/stack");
        let wrapper = pkg.join("bin/stack.cjs");
        let packaged = pkg.join("binaries/darwin-arm64/stack");
        let other = Path::new("/Users/dev/stack/target/release/stack");
        let other_pkg = Path::new("/tmp/other/binaries/darwin-arm64/stack");

        assert!(same_install(&packaged, Some(&packaged)));
        assert!(same_install(&wrapper, Some(&packaged)));
        assert!(!same_install(&wrapper, Some(other)));
        assert!(!same_install(&wrapper, Some(other_pkg)));
        assert!(!same_install(&wrapper, None));
    }
}

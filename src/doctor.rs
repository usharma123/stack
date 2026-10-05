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
        tool("mise", "mise", &["--version"], "install mise: https://mise.jdx.dev/getting-started.html"),
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
    if root.join(crate::manifest::PROJECT_FILE).exists() {
        let compiled = project::compile(&Options {
            root: root.to_path_buf(),
            mode: project::inspect_mode(root),
            write: false,
            cache: cache.to_path_buf(),
            state: state.to_path_buf(),
            reassign_ports: false,
        });
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

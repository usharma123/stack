//! `stack env` and `stack hook direnv`: the environment `stack exec` would give a command, for
//! a shell or direnv to load. Planned exactly as `exec` plans it (same verification, same
//! withholding), so an unverified service's address is poisoned or removed here too. Unlike
//! `exec`, the result is a snapshot: a shell keeps it after the service stops verifying.

use crate::error::{Result, StackError};
use crate::session::{self, Ctx, Require};
use indexmap::IndexMap;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, Serialize)]
pub struct Withheld {
    pub service: String,
    pub reason: Option<String>,
    /// Variables poisoned (set to an unresolvable host) or removed for this service.
    pub vars: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct EnvReport {
    /// Variables to set, as `exec` sets them (withheld endpoints already poisoned).
    pub env: IndexMap<String, String>,
    /// Variables to unset.
    pub unset: Vec<String>,
    /// Services that did not verify, and what was withheld for each.
    pub withheld: Vec<Withheld>,
    /// Variables left out: names a shell cannot export, and Stack's own provider selection
    /// (`MISE_*`), which would change how your shell's mise reads its configuration.
    pub skipped: Vec<String>,
}

/// Provider plumbing `exec` gives a command but a login shell should not inherit.
fn provider_key(key: &str) -> bool {
    key.starts_with("MISE_") || key.starts_with("__MISE")
}

fn exportable(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Plan as `stack exec` does, then keep only the environment.
pub fn plan(ctx: &Ctx) -> Result<EnvReport> {
    // `sh` stands in for the command: planning needs one on the stack's PATH. The execution
    // the plan reserves is released when it drops, below.
    let plan = session::plan_exec(ctx, &["sh".to_string()], &Require::Nothing)?;
    let mut env = IndexMap::new();
    let mut skipped = Vec::new();
    for (key, value) in &plan.env {
        if provider_key(key) || !exportable(key) {
            skipped.push(key.clone());
        } else {
            env.insert(key.clone(), value.clone());
        }
    }
    let mut unset: Vec<String> = Vec::new();
    for key in &plan.removed {
        if !provider_key(key) && exportable(key) && !unset.contains(key) && !env.contains_key(key) {
            unset.push(key.clone());
        }
    }
    let withheld = plan
        .checks
        .iter()
        .filter(|c| !c.ready)
        .map(|c| Withheld { service: c.service.clone(), reason: c.reason.clone(), vars: c.withheld.clone() })
        .collect();
    Ok(EnvReport { env, unset, withheld, skipped })
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn fish_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', r"\\").replace('\'', r"\'"))
}

/// Comment lines are safe in every supported shell; a newline in a reason must not end one.
fn comment(text: &str) -> String {
    format!("# {}\n", text.replace(['\n', '\r'], " "))
}

/// Lines a shell can `eval`.
pub fn render(report: &EnvReport, shell: Shell) -> String {
    let mut out = comment("stack env: the environment `stack exec` would give a command now; re-run after `stack up`/`down`");
    for w in &report.withheld {
        let vars = if w.vars.is_empty() { "nothing".to_string() } else { w.vars.join(", ") };
        out.push_str(&comment(&format!(
            "stack: {} not verified ({}); withheld: {vars}",
            w.service,
            w.reason.as_deref().unwrap_or("unknown")
        )));
    }
    for (key, value) in &report.env {
        out.push_str(&match shell {
            Shell::Bash | Shell::Zsh => format!("export {key}={}\n", sh_quote(value)),
            // fish keeps *PATH variables as lists; split them so each entry is one element.
            Shell::Fish if key.ends_with("PATH") => format!("set -gx {key} (string split : -- {})\n", fish_quote(value)),
            Shell::Fish => format!("set -gx {key} {}\n", fish_quote(value)),
        });
    }
    for key in &report.unset {
        out.push_str(&match shell {
            Shell::Bash | Shell::Zsh => format!("unset {key}\n"),
            Shell::Fish => format!("set -e {key}\n"),
        });
    }
    out
}

/// The shell named by `$SHELL`, else bash.
pub fn default_shell() -> Shell {
    let shell = std::env::var("SHELL").unwrap_or_default();
    match shell.rsplit('/').next().unwrap_or_default() {
        "fish" => Shell::Fish,
        "zsh" => Shell::Zsh,
        _ => Shell::Bash,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Hook {
    Direnv,
}

/// An `.envrc` snippet. direnv evaluates `.envrc` with bash.
pub fn hook(hook: Hook) -> String {
    match hook {
        Hook::Direnv => "\
# Load this checkout's Stack environment (https://github.com/usharma123/stack/blob/main/docs/user/worktrees.md).
# Services that do not verify have their addresses withheld, as `stack exec` does, but only as of
# when direnv last loaded this file: run `direnv reload` after `stack up`, `restart` or `down`.
watch_file stack.toml stack.lock .stack/session.json
if command -v stack >/dev/null 2>&1; then
  eval \"$(stack env --shell bash)\"
else
  log_error 'stack is not on PATH; see https://github.com/usharma123/stack/blob/main/docs/user/install.md'
fi
"
        .to_string(),
    }
}

/// `usage` for `--json` with output meant for a shell.
pub fn json_conflict() -> StackError {
    StackError::new("usage", "--shell and --json cannot be given together").hint("use `stack env --json` or `stack env --shell <shell>`")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> EnvReport {
        let mut env = IndexMap::new();
        env.insert("DATABASE_URL".into(), "postgresql://unverified.stack.invalid:5432/app".into());
        env.insert("PATH".into(), "/a:/b".into());
        env.insert("QUOTE".into(), "it's".into());
        EnvReport {
            env,
            unset: vec!["PGPORT".into()],
            withheld: vec![Withheld { service: "postgres".into(), reason: Some("not running\nat all".into()), vars: vec!["DATABASE_URL".into(), "PGPORT".into()] }],
            skipped: vec![],
        }
    }

    #[test]
    fn posix_output_quotes_values_and_says_what_was_withheld() {
        let out = render(&report(), Shell::Bash);
        assert!(out.contains("# stack: postgres not verified (not running at all); withheld: DATABASE_URL, PGPORT\n"), "{out}");
        assert!(out.contains("export QUOTE='it'\\''s'\n"), "{out}");
        assert!(out.contains("export DATABASE_URL='postgresql://unverified.stack.invalid:5432/app'\n"));
        assert!(out.contains("unset PGPORT\n"));
    }

    #[test]
    fn fish_output_splits_paths_and_erases() {
        let out = render(&report(), Shell::Fish);
        assert!(out.contains("set -gx PATH (string split : -- '/a:/b')\n"), "{out}");
        assert!(out.contains("set -gx QUOTE 'it\\'s'\n"), "{out}");
        assert!(out.contains("set -e PGPORT\n"));
    }

    #[test]
    fn provider_selection_and_unexportable_names_are_not_exported() {
        assert!(provider_key("MISE_CONFIG_DIR") && !provider_key("DATABASE_URL"));
        assert!(exportable("A_1") && !exportable("1A") && !exportable("A-B") && !exportable(""));
    }
}

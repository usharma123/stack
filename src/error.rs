use serde::ser::{Serialize, SerializeStruct, Serializer};
use std::fmt;
use std::path::Path;

/// A failure an agent can act on: a stable `code`, a human message, and an optional fix.
///
/// Every code is listed in [`REGISTRY`]. A site that gives no hint of its own is reported with
/// the registry's default hint, and the serialized error also carries `retryable`.
#[derive(Debug)]
pub struct StackError {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
    pub details: Vec<serde_json::Value>,
}

impl StackError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details: Vec::new(),
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn details(mut self, details: Vec<serde_json::Value>) -> Self {
        self.details = details;
        self
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.details.push(detail);
        self
    }

    /// The registry entry for this error's code, if it has one.
    pub fn info(&self) -> Option<&'static ErrorInfo> {
        lookup(self.code)
    }

    /// The hint this site gave, or the registry's default for the code.
    pub fn effective_hint(&self) -> Option<&str> {
        self.hint.as_deref().or_else(|| self.info().map(|i| i.hint))
    }

    /// Whether repeating the same call, unchanged, can succeed.
    pub fn retryable(&self) -> bool {
        self.info().is_some_and(|i| i.retryable)
    }

    /// The registry's next steps for this code, as commands and MCP tool calls naming `dir`.
    pub fn next(&self, dir: Option<&Path>) -> Vec<serde_json::Value> {
        self.info().map(|i| i.next.iter().map(|n| n.render(dir)).collect()).unwrap_or_default()
    }
}

impl Serialize for StackError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let hint = self.effective_hint();
        let fields = 3 + usize::from(hint.is_some()) + usize::from(!self.details.is_empty());
        let mut s = serializer.serialize_struct("StackError", fields)?;
        s.serialize_field("code", self.code)?;
        s.serialize_field("message", &self.message)?;
        if let Some(hint) = hint {
            s.serialize_field("hint", hint)?;
        }
        s.serialize_field("retryable", &self.retryable())?;
        if !self.details.is_empty() {
            s.serialize_field("details", &self.details)?;
        }
        s.end()
    }
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(hint) = self.effective_hint() {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for StackError {}

pub type Result<T> = std::result::Result<T, StackError>;

pub fn io_error(context: impl fmt::Display, err: std::io::Error) -> StackError {
    StackError::new("io", format!("{context}: {err}"))
}

// ---- registry ------------------------------------------------------------------------------

/// One error code: what it means, what to do about it, and whether retrying can help.
#[derive(Debug)]
pub struct ErrorInfo {
    pub code: &'static str,
    /// One line: what happened.
    pub meaning: &'static str,
    /// The next action, used when the site that raised the error gives no hint of its own.
    pub hint: &'static str,
    /// Repeating the same call unchanged can succeed (the cause may be transient).
    pub retryable: bool,
    /// Steps that fix the cause, when they are unambiguous. Rendered into `error.next`.
    pub next: &'static [Next],
}

/// A Stack operation to run next: `stack -C <dir> <op> <flags...>`, or the MCP tool
/// `stack_<op>` with `dir` and `args`.
#[derive(Debug)]
pub struct Next {
    pub op: &'static str,
    pub flags: &'static [&'static str],
    /// MCP arguments besides `dir` (all booleans so far).
    pub args: &'static [(&'static str, bool)],
    /// Whether `stack mcp` serves this operation as a tool.
    pub tool: bool,
}

impl Next {
    const fn op(op: &'static str) -> Self {
        Self { op, flags: &[], args: &[], tool: true }
    }

    /// `{command, tool, args}`; `command` is argv, `tool` is null for CLI-only operations.
    pub fn render(&self, dir: Option<&Path>) -> serde_json::Value {
        let mut command: Vec<String> = vec!["stack".into()];
        let mut args = serde_json::Map::new();
        if let Some(dir) = dir {
            command.extend(["-C".into(), dir.display().to_string()]);
            args.insert("dir".into(), dir.display().to_string().into());
        }
        command.push(self.op.into());
        command.extend(self.flags.iter().map(|f| f.to_string()));
        for (key, value) in self.args {
            args.insert((*key).into(), (*value).into());
        }
        serde_json::json!({
            "command": command,
            "tool": self.tool.then(|| format!("stack_{}", self.op)),
            "args": args,
        })
    }
}

const UP: Next = Next::op("up");
const STATUS: Next = Next::op("status");
const COMPILE: Next = Next::op("compile");
const INSPECT: Next = Next::op("inspect");
const INSTALL: Next = Next::op("install");
const DOCTOR: Next = Next::op("doctor");
const SETUP: Next = Next { op: "setup", flags: &[], args: &[], tool: false };
const COMPILE_UPDATE: Next = Next { op: "compile", flags: &["--update"], args: &[("update", true)], tool: true };
const REASSIGN_PORTS: Next = Next { op: "compile", flags: &["--reassign-ports"], args: &[("reassign_ports", true)], tool: true };

macro_rules! codes {
    ($($code:literal, $retry:literal, [$($next:expr),*], $meaning:literal, $hint:literal;)*) => {
        /// Every error code Stack reports, sorted by code.
        pub const REGISTRY: &[ErrorInfo] = &[
            $(ErrorInfo { code: $code, meaning: $meaning, hint: $hint, retryable: $retry, next: &[$($next),*] },)*
        ];
    };
}

codes! {
    "agent_command_failed", true, [], "The command that loads or unloads the gc agent failed; the unit file is in place.",
        "Run the command the message shows yourself to see why, then retry `stack setup --gc-agent --enable`.";
    "artifact_lock_failed", true, [], "`mise lock` could not run or returned something unreadable; nothing changed.",
        "Check that mise runs (`stack doctor`) and the network is reachable, then retry `stack compile`.";
    "artifact_mismatch", false, [], "A download differs from the checksum stack.lock records.",
        "Do not work around it: check upstream, and only if the change is expected run `stack compile --update` and review the diff.";
    "artifact_unlocked", false, [], "`[lock] artifacts = \"required\"` but a pin has no checksum for a listed platform.",
        "Drop the platform from `[lock] platforms`, pin another release, or set `artifacts = \"best-effort\"`, then run `stack compile`.";
    "bundle_fixed_port", false, [], "A bundle pins a service to a fixed port.",
        "Use `port = \"auto\"` in the bundle; put fixed ports in the project's `[override.services]`.";
    "bundle_invalid", false, [], "A bundle.toml is malformed or breaks a bundle rule.",
        "Fix bundle.toml at the file, line and column the message names, then retry.";
    "bundle_not_found", false, [], "The bundle directory given to `stack publish` does not exist.",
        "Pass the path of a directory that contains bundle.toml.";
    "bundle_too_large", false, [], "A bundle exceeds 256 MiB of files or 10,000 entries.",
        "Remove large or generated files from the bundle directory and publish again.";
    "cancelled", true, [], "A signal sent to Stack cancelled the start; anything launched before stays recorded.",
        "Nothing more was started: `stack status` shows what was launched, `stack down` stops it, `stack up` starts again.";
    "command_not_found", false, [], "The program given to `exec` is not on the stack's PATH.",
        "Add the tool to `[tools]` in stack.toml and run `stack compile`, or check the command name.";
    "conflict", false, [], "Two layers define the same key differently.",
        "Make the layers agree, or resolve it explicitly with an `[override.*]` entry in stack.toml; details list every conflict.";
    "content_hash_mismatch", false, [], "Fetched bundle content does not match the hash stack.lock records.",
        "Check the source for tampering or a moved ref; if the change is expected run `stack compile --update` and review the diff.";
    "dependency_cycle", false, [], "A service depends on itself, or services depend on each other in a cycle.",
        "Remove one of the `depends_on` entries the message names.";
    "dependency_not_ready", false, [], "A service `restart` would start depends on one that is not ready; nothing was stopped, set up or started.",
        "Start the dependency first: `stack restart <dependency>` restarts it and what depends on it, `stack up` starts every service that is not running.";
    "dir_not_found", false, [], "The project directory (`-C` or MCP `dir`) does not exist.",
        "Pass an existing project directory with `-C <dir>` (MCP: `dir`).";
    "doctor_failed", false, [], "One or more `stack doctor` checks failed; details list every check.",
        "Fix each failing check as its hint in details says, then run `stack doctor` again.";
    "duplicate_bundle", false, [], "Two `[[use]]` entries resolve to bundles with the same name.",
        "Remove one of the `[[use]]` entries in stack.toml.";
    "exec_failed", false, [], "The command could not be started at all.",
        "Check that the program exists and is executable (`stack exec -- which <program>`).";
    "gc_incomplete", true, [], "Some sessions of deleted checkouts could not be confirmed stopped; their records are kept.",
        "For each service in details, run `recovery.inspect`, stop it only if the PID still matches, then run `stack gc` again.";
    "git_failed", true, [], "A git command failed while fetching a bundle.",
        "Check the bundle's git URL and your network or credentials, then retry; the message carries git's output.";
    "git_unavailable", false, [DOCTOR], "git is not installed or cannot run.",
        "Install git (git bundle sources need it), then retry; `stack doctor` checks it.";
    "init_failed", true, [UP], "An init task of a service failed, or its data directory vanished; services are still running.",
        "Read the task's output in details, fix the cause and run `stack up` again; it resumes from the failed task.";
    "init_invalid", false, [], "A service's `init` list names an unknown or repeated task, or one that excludes the service.",
        "Fix the `init` list in stack.toml (details name the service and task), then run `stack compile`.";
    "install_failed", true, [], "mise could not install a pinned tool or service binary.",
        "Read mise's output in details; fix network or platform support, then run `stack install` again.";
    "invalid_env", false, [], "`[env]` sets a variable Stack reserves.",
        "Remove the reserved variable (such as STACK_SESSION or MISE_*) from `[env]`.";
    "invalid_path", false, [], "A configured path is not a directory inside the project.",
        "Use a relative directory inside the project root, such as \".claude/skills\".";
    "invalid_secret", false, [], "A secret name cannot be granted.",
        "Use an upper-case variable name that no service, `[env]` entry or Stack/mise variable uses.";
    "invalid_service", false, [], "A service definition is malformed.",
        "Set exactly one of `preset` or `run` for the service in stack.toml or the bundle.";
    "invalid_tool", false, [], "A tool option or name is invalid.",
        "Fix the `[tools]` entry the message names, then run `stack compile`.";
    "io", true, [], "A file or directory could not be read or written.",
        "Check the path and permissions in the message (and free disk space), then retry.";
    "lock_busy", true, [], "Another Stack command holds the lock this one needs.",
        "Wait for the other Stack command in this checkout to finish, then retry.";
    "lock_invalid", false, [COMPILE_UPDATE], "stack.lock is malformed, inconsistent, or written by a newer Stack.",
        "Run `stack compile --update` to regenerate it, or upgrade Stack if a newer version wrote it.";
    "lock_outdated", false, [COMPILE], "stack.lock is missing or does not match stack.toml.",
        "Run `stack compile` to update stack.lock, then retry.";
    "locked_commit_missing", false, [COMPILE_UPDATE], "A bundle commit stack.lock pins no longer exists upstream.",
        "Run `stack compile --update` to re-resolve, then review the change.";
    "logs_failed", true, [STATUS], "The supervisor could not return the service's output.",
        "Run `stack status` to see whether the service was launched; start it with `stack up`.";
    "manifest_invalid", false, [], "stack.toml is malformed or breaks a project rule.",
        "Fix stack.toml at the file, line and column the message names, then retry.";
    "manifest_missing", false, [], "There is no readable stack.toml in the project directory.",
        "Create a stack.toml in the project directory, or point `-C` (MCP: `dir`) at one that has it.";
    "materialize_failed", true, [], "A fetched bundle commit could not be extracted.",
        "Retry; if it persists, delete the bundle's entry in Stack's cache directory and run `stack compile`.";
    "missing_path", false, [], "A bundle declares a path that is not in the bundle.",
        "Add the directory to the bundle or remove it from `paths` in bundle.toml.";
    "mv_data_unmovable", false, [], "`stack mv` could not move a service's data directory to the new path's location.",
        "Check that MISE_STATE_DIR is what it was when the services started, then retry `stack mv <old> <new>`.";
    "mv_data_unplaced", false, [], "The provider names no data directory for a service at the new path.",
        "Run `stack compile` in the new directory so its provider configuration is current, then retry `stack mv <old> <new>`.";
    "mv_nothing_recorded", false, [], "Stack has no records for the old path.",
        "Run `stack ps` to list the paths Stack knows; if the checkout is not there, just run `stack up` in the new directory.";
    "mv_services_running", false, [], "Services recorded for the old path may still be running, so its records cannot move.",
        "Move the directory back, run `stack down` there, move it again and rerun `stack mv <old> <new>`.";
    "mv_source_exists", false, [], "The old path still exists, so the checkout was not moved.",
        "Move the directory first (`git worktree move` or `mv`), then run `stack mv <old> <new>`.";
    "mv_target_has_state", false, [], "The new path already has a session of its own.",
        "Run `stack down` in the new directory, then rerun `stack mv <old> <new>`.";
    "mv_target_missing", false, [], "The new path is not an existing directory.",
        "Move the checkout there first, then run `stack mv <old> <new>`.";
    "no_lease", false, [], "`renew` was called but the session has no lease to renew.",
        "Nothing to renew: start the session with `stack up --ttl <duration>` or `--owner-pid` if it should expire.";
    "no_session", false, [UP], "No session is running in this checkout.",
        "Run `stack up` first.";
    "not_ready", true, [STATUS], "Services started but failed verification.",
        "Read each service's reason in details, check `stack logs <service>`, fix the cause and run `stack up` again.";
    "oci_auth_untrusted", false, [], "A registry redirected authentication to another origin; Stack refused.",
        "Check the registry URL; credentials are only sent to the registry's own origin.";
    "oci_failed", true, [], "An OCI registry request failed.",
        "Read the HTTP status in the message, check the registry, then retry.";
    "oci_not_found", false, [], "The OCI reference does not exist in the registry.",
        "Check the repository, tag or digest in the reference.";
    "oci_unauthorized", false, [], "The OCI registry refused the credentials (HTTP 401/403).",
        "Set STACK_OCI_USERNAME and STACK_OCI_PASSWORD for this registry, then retry.";
    "oci_unreachable", true, [], "The OCI registry could not be reached.",
        "Check the network and the registry host name, then retry.";
    "owner_not_found", false, [], "No ancestor process of Stack has the name given to `--owner-pid ancestor:<name>`.",
        "Pick a name from the ancestor chain in details, pass a process ID with `--owner-pid <pid>`, or use `--ttl 30m` instead.";
    "port_conflict", false, [REASSIGN_PORTS, UP], "Another program holds a port this checkout reserved.",
        "Run `stack compile --reassign-ports`, then `stack up`; Stack never kills the other program.";
    "ports_exhausted", false, [], "No free port is left in Stack's range (40000-49999).",
        "Run `stack gc` and `stack down` in checkouts you no longer use to release their ports, then retry.";
    "project_exists", false, [], "`stack init` found a stack.toml already there.",
        "Edit it, or run `stack init` in a directory without one.";
    "provider_failed", true, [DOCTOR], "mise or the supervisor returned an error.",
        "Run `stack doctor`; details carry mise's output.";
    "provider_outdated", false, [], "mise is older than this project needs.",
        "Run `mise self-update`, or `stack setup --force` for the release Stack is tested against.";
    "provider_unavailable", false, [SETUP], "mise cannot be found or run.",
        "Run `stack setup` to install mise, or put mise on PATH.";
    "ref_not_found", false, [], "A git bundle's `ref=` names no branch, tag or commit.",
        "Fix the `?ref=` value in the bundle source.";
    "ref_required", false, [], "An OCI reference has no tag or digest.",
        "Pin a tag or digest, such as oci:ghcr.io/acme/pybase:1.0.0.";
    "resolve_failed", true, [], "A version request could not be resolved; stack.lock and the provider config were not changed.",
        "Check the version requests the message names exist, and the network, then run `stack compile` again.";
    "root_unsupported", false, [], "PostgreSQL refuses to run as root, and Stack is running as root.",
        "Run Stack as an unprivileged user that owns the checkout (e.g. `su dev -c 'stack up'`); installs and tools-only commands work as root.";
    "secret_missing", false, [], "fnox does not know a granted secret.",
        "Define the key in fnox.toml for the active profile (FNOX_PROFILE), or remove the grant.";
    "secret_unavailable", true, [], "fnox could not be run or could not resolve the secrets.",
        "Run `stack doctor`, then `fnox env --json --describe` in the project to see fnox's own message.";
    "secret_unsupported", false, [], "A granted secret cannot be injected or redacted safely.",
        "Use `stack exec -- fnox exec -- <command>` for file secrets and leases, or a longer secret value.";
    "service_unavailable", false, [UP], "A required service did not verify, so the command was not run.",
        "Run `stack up` (or `stack restart <service>`), then retry.";
    "services_unsupported", false, [], "This platform cannot run services (no Pitchfork build).",
        "Run services on Apple silicon or Linux; on this machine use a tools-only project.";
    "session_busy", true, [], "Commands are still running in the session.",
        "Wait for the running `stack exec`/`stack run` commands to finish, then retry.";
    "session_conflict", false, [], "The session record belongs to a checkout at another path (it may have been moved).",
        "Run `stack down` in the original directory, or remove the session record the message names, then `stack up`.";
    "session_stale", false, [UP], "The compiled configuration changed since the session started.",
        "Run `stack up` to restart and verify the services with the new configuration.";
    "setup_failed", true, [], "mise could not be downloaded or does not run after installation.",
        "Retry `stack setup`, or install mise yourself and put it on PATH.";
    "setup_unsupported", false, [], "Stack has no mise build for this platform.",
        "Install mise yourself (see mise's installation docs) and put it on PATH.";
    "skill_not_found", false, [INSPECT], "No available skill matches the tool and name.",
        "Run `stack inspect` and pick a tool and skill whose status is `available`.";
    "skill_too_large", false, [], "The skill is larger than the 64 KiB Stack returns.",
        "Read the skill file directly at the path in details.";
    "skill_unreadable", false, [INSPECT], "The skill file could not be read.",
        "Check the path in details; run `stack install` if the tool is not installed, then `stack inspect`.";
    "snapshot_exists", false, [], "A snapshot of that name already exists for this project.",
        "Choose another name, or run `stack snapshot save <name> --force` to replace it.";
    "snapshot_failed", true, [UP], "A snapshot could not be saved or restored; the checkout's data is unchanged.",
        "Run `stack up` once so the service is configured and check free disk space, then retry the snapshot command.";
    "snapshot_incompatible", false, [], "The snapshot holds a different service kind or major version than this checkout pins.",
        "Restore it in a checkout that pins the same version, or save a new snapshot with `stack snapshot save <name>`.";
    "snapshot_invalid", false, [], "A snapshot's metadata cannot be read.",
        "Remove it with `stack snapshot rm <name>` and save it again.";
    "snapshot_not_found", false, [], "This project has no snapshot of that name.",
        "Run `stack snapshot list` to see the names, or save one with `stack snapshot save <name>`.";
    "snapshot_unsupported", false, [], "The service or its data directory cannot be snapshotted (only postgres and redis data in real directories).",
        "Use the service's own dump and restore tools through `stack exec` instead.";
    "socket_path_too_long", false, [], "Pitchfork's supervisor socket path is longer than this platform allows.",
        "Set PITCHFORK_STATE_DIR to a short absolute directory, such as /tmp/pitchfork-$USER, then retry.";
    "source_invalid", false, [], "A bundle source or publish target is malformed.",
        "Use a valid source, such as path:../b, git+https://...?ref=v1 or oci:<registry>/<repo>:<tag>.";
    "start_failed", true, [STATUS], "The supervisor could not start a service.",
        "Read the output in details and `stack logs <service>`, fix the cause, then run `stack up` again.";
    "state_invalid", false, [], "A Stack state file is unreadable or corrupt.",
        "Delete the state file the message names to reset it, then retry.";
    "stop_failed", true, [], "The supervisor could not stop a service.",
        "Retry `stack down`; details carry the supervisor's output.";
    "stop_unconfirmed", true, [], "Services did not stop in time.",
        "Retry `stack down` (or `stack restart`); details list the processes still alive.";
    "supervisor_changed", true, [], "The Pitchfork supervisor the project's environment names changed while the command ran; nothing more was started or stopped.",
        "Run the command again: it uses the supervisor the environment names when it begins; `stack status` shows what was already started.";
    "supervisor_unbound", false, [COMPILE], "Stack cannot bind a request to the supervisor whose lock it holds; nothing was asked of it.",
        "Run `stack compile` to write the configuration again, then retry.";
    "supervisor_unknown", false, [], "Stack cannot tell which Pitchfork supervisor a request would reach; nothing was asked of it.",
        "Set PITCHFORK_STATE_DIR to an absolute directory.";
    "tag_exists", false, [], "The OCI tag already points to different content.",
        "Publish a new tag, or pass --force to move this one.";
    "tar_unavailable", false, [DOCTOR], "tar is not installed or cannot run.",
        "Install tar (git bundle sources need it), then retry; `stack doctor` checks it.";
    "timed_out", true, [STATUS], "The call did not finish within its deadline.",
        "Raise the timeout if the work needs longer; for `up`/`restart` check `stack status` first, since services may still be starting.";
    "tools_not_installed", false, [INSTALL], "A pinned tool release is not installed on this machine.",
        "Run `stack install`, then retry.";
    "unknown_service", false, [INSPECT], "No service has that name in the composed stack.",
        "Use a service name `stack inspect` lists.";
    "unknown_task", false, [INSPECT], "No task has that name in stack.toml.",
        "Use a task name the hint or `stack inspect` lists, or add `[tasks.<name>]` to stack.toml.";
    "unknown_tool", false, [], "The MCP server has no tool with that name.",
        "Call `tools/list` and use one of the tools it returns.";
    "unlocked_service", false, [], "A service uses a preset Stack cannot lock.",
        "Use a supported preset (postgres or redis) or a custom `run` service.";
    "unsupported_platform", false, [], "`stack setup --gc-agent` knows no user service manager on this platform.",
        "Run `stack gc --watch` under your own supervisor instead.";
    "usage", false, [], "The arguments are invalid.",
        "Fix the arguments as the message says; `stack --help` (MCP: the tool's inputSchema) lists them.";
}

/// The registry entry for `code`.
pub fn lookup(code: &str) -> Option<&'static ErrorInfo> {
    REGISTRY.iter().find(|i| i.code == code)
}

/// The registry as data, for `stack errors`.
pub fn registry_json() -> serde_json::Value {
    serde_json::Value::Array(
        REGISTRY
            .iter()
            .map(|i| {
                serde_json::json!({
                    "code": i.code,
                    "meaning": i.meaning,
                    "hint": i.hint,
                    "retryable": i.retryable,
                    "next": i.next.iter().map(|n| n.render(None)).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// docs/user/errors.md, generated from [`REGISTRY`]. A test keeps the file in step.
pub fn registry_markdown() -> String {
    let mut out = String::from(
        "# Error codes\n\n\
         <!-- Generated from src/error.rs by `stack errors --markdown`; do not edit by hand. -->\n\n\
         Every failure (`ok: false`) carries `error.code`, one of the codes below, plus `message`,\n\
         `hint` (the site's own, or the default listed here), `retryable` (whether repeating the same\n\
         call unchanged can succeed) and, where the fix is unambiguous, `next`: a list of\n\
         `{command, tool, args}` naming the Stack operation to run, with this checkout's directory\n\
         filled in. `stack errors --json` (MCP: `stack_errors`) returns this table.\n\n\
         | Code | Meaning | Default hint | Retryable | Next |\n|---|---|---|---|---|\n",
    );
    for i in REGISTRY {
        let next: Vec<String> = i
            .next
            .iter()
            .map(|n| {
                let mut words = vec![n.op];
                words.extend(n.flags);
                format!("`stack {}`", words.join(" "))
            })
            .collect();
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            i.code,
            i.meaning.replace('|', "\\|"),
            i.hint.replace('|', "\\|"),
            if i.retryable { "yes" } else { "no" },
            if next.is_empty() { "-".to_string() } else { next.join(", then ") }
        ));
    }
    out.push_str("\n[All docs](../README.md)\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every code literal under src/: the first argument of `StackError::new`, the code argument
    /// of helpers that take one, codes chosen by `let code... = if`, and codes compared with
    /// `.code == "..."`.
    fn codes_in_source() -> BTreeSet<String> {
        fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        fn literal(s: &str) -> Option<&str> {
            let rest = s.trim_start().strip_prefix('"')?;
            let end = rest.find('"')?;
            let word = &rest[..end];
            let code_like = !word.is_empty()
                && word.starts_with(|c: char| c.is_ascii_lowercase())
                && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            code_like.then_some(word)
        }
        fn literals(s: &str) -> Vec<&str> {
            s.split('"').skip(1).step_by(2).filter(|w| literal(&format!("\"{w}\"")).is_some()).collect()
        }
        // The text of a call's arguments: from after `(` to the matching `)`.
        fn call_args(s: &str) -> &str {
            let mut depth = 1;
            for (i, c) in s.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return &s[..i];
                        }
                    }
                    _ => {}
                }
            }
            s
        }
        let mut sources = Vec::new();
        files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut sources);
        let mut found = BTreeSet::new();
        for path in sources {
            let text = std::fs::read_to_string(&path).unwrap();
            let after = |pat: &str| text.match_indices(pat).map(|(i, _)| &text[i + pat.len()..]).collect::<Vec<_>>();
            // A call of exactly this function, not of one whose name ends the same way.
            let calls = |name: &str| {
                text.match_indices(name)
                    .filter(|(i, _)| !text[..*i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
                    .map(|(i, _)| &text[i + name.len()..])
                    .collect::<Vec<_>>()
            };
            for rest in after("StackError::new(") {
                found.extend(literal(rest).map(String::from));
            }
            // Helpers given the code as an argument: their last code-like literal.
            for helper in ["into_error(", "checked(", "request(", "detach_supervisor("] {
                for rest in calls(helper) {
                    if let Some(last) = literals(call_args(rest)).last() {
                        found.insert(last.to_string());
                    }
                }
            }
            for rest in after("let code") {
                let stmt = &rest[..rest.find(';').unwrap_or(rest.len())];
                if stmt.split_once('=').is_some_and(|(_, v)| v.trim_start().starts_with("if ")) {
                    found.extend(literals(stmt).into_iter().map(String::from));
                }
            }
            for rest in after(".code == ") {
                found.extend(literal(rest).map(String::from));
            }
        }
        found
    }

    #[test]
    fn the_registry_lists_exactly_the_codes_the_source_uses() {
        let used = codes_in_source();
        let listed: BTreeSet<String> = REGISTRY.iter().map(|i| i.code.to_string()).collect();
        let missing: Vec<_> = used.difference(&listed).collect();
        let unused: Vec<_> = listed.difference(&used).collect();
        assert!(missing.is_empty(), "codes used under src/ but missing from REGISTRY in src/error.rs: {missing:?}");
        assert!(unused.is_empty(), "REGISTRY entries no code under src/ uses: {unused:?}");
        assert!(used.contains("socket_path_too_long") && used.contains("oci_not_found"), "{used:?}");
    }

    #[test]
    fn the_registry_is_sorted_and_every_entry_says_what_to_do() {
        for pair in REGISTRY.windows(2) {
            assert!(pair[0].code < pair[1].code, "{} is out of order", pair[1].code);
        }
        for i in REGISTRY {
            assert!(i.meaning.ends_with('.') && i.hint.ends_with('.'), "{}: end meaning and hint with a period", i.code);
            assert!(!i.meaning.contains('\n') && !i.hint.contains('\n'), "{}", i.code);
            // A default hint names an action: a command to run, or something to change.
            let actionable = i.hint.contains('`')
                || ["Set ", "Use ", "Pass ", "Add ", "Remove ", "Fix ", "Check ", "Install ", "Delete ", "Define ", "Wait ", "Retry", "Read ", "Make ", "Publish ", "Pin ", "Drop ", "Do not", "Call ", "Create ", "Run ", "For each"]
                    .iter()
                    .any(|verb| i.hint.starts_with(verb));
            assert!(actionable, "{}: the default hint must name a next action: {}", i.code, i.hint);
        }
    }

    #[test]
    fn errors_without_a_hint_use_the_registry_default_and_say_whether_to_retry() {
        let e = StackError::new("ports_exhausted", "no free port");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["hint"], lookup("ports_exhausted").unwrap().hint);
        assert_eq!(v["retryable"], false);
        assert!(v.get("details").is_none());
        let e = StackError::new("lock_busy", "held").hint("mine");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!((v["hint"].as_str(), v["retryable"].as_bool()), (Some("mine"), Some(true)));
        assert!(e.to_string().contains("hint: mine"));
    }

    #[test]
    fn next_steps_name_the_project_directory_for_both_cli_and_mcp() {
        let dir = Path::new("/work/app");
        let next = StackError::new("port_conflict", "taken").next(Some(dir));
        assert_eq!(next.len(), 2);
        assert_eq!(next[0]["command"], serde_json::json!(["stack", "-C", "/work/app", "compile", "--reassign-ports"]));
        assert_eq!(next[0]["tool"], "stack_compile");
        assert_eq!(next[0]["args"], serde_json::json!({ "dir": "/work/app", "reassign_ports": true }));
        assert_eq!(next[1]["tool"], "stack_up");
        let setup = StackError::new("provider_unavailable", "no mise").next(Some(dir));
        assert!(setup[0]["tool"].is_null(), "setup has no MCP tool");
        assert!(StackError::new("usage", "bad").next(Some(dir)).is_empty());
    }

    #[test]
    fn errors_md_is_generated_from_the_registry() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/user/errors.md");
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if std::env::var_os("STACK_UPDATE_DOCS").is_some() {
            std::fs::write(&path, registry_markdown()).unwrap();
            return;
        }
        assert!(
            current == registry_markdown(),
            "docs/user/errors.md is out of date; run `STACK_UPDATE_DOCS=1 cargo test --lib errors_md` or `stack errors --markdown > docs/user/errors.md`"
        );
    }
}

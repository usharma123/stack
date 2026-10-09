//! The provider configuration one task invocation runs from, fixed when it is planned.
//!
//! `stack run` plans a task under the project lock and releases the lock before `mise run`
//! starts, so a long task does not block the project. `mise run` reads the task's body and env
//! from the generated configuration when it starts, and an ordinary compile may rewrite that
//! file in between. Without a copy, a saved plan would run whatever definition is current then,
//! with the grant and service checks of the one it was planned from.
//!
//! [`TaskConfig::capture`] copies the generated configuration under the planning lock to
//! `<cache>/task-config/<pid>-<random>/` (a directory only this user can read, removed when the
//! [`TaskConfig`] is dropped: at command completion, timeout or error, or when planning fails
//! after the copy), with the provider lock beside it rendered from the stack.lock the same
//! compile validated. The project's own `.config/mise/mise.lock` is never read: it may be
//! missing (a fresh checkout) or stale (written from an earlier stack.lock), and mise checks
//! downloads against whatever lock it finds. The copy keeps the layout of the project's files
//! (`.config/mise/conf.d/stack.toml`, `.config/mise/mise.lock`), so mise finds the lock beside
//! it as it would in the project. Granted values are never in either file.
//!
//! The copy keeps every plain `[env]` variable the plan holds, but declares it as the value
//! the command inherits (`{{ env["KEY"] }}`), never as the project's template. Planning read
//! these values from the project's own file, so a template such as `{{config_source}}` named
//! that file; evaluated again against the copy it would name the copy. Still declared, each
//! keeps its precedence over the environment a tool sets (mise applies `[env]` after a tool's
//! `JAVA_HOME` or `GOROOT`), in the task's shell and in `{{env.X}}` task templates. Where mise
//! expands `$VAR` in rendered values (its `env_shell_expand` setting), a `$` in the value is
//! doubled first, so the expansion gives it back unchanged. `PATH`, which mise builds from the
//! inherited `PATH` and its own directives, and a name the template cannot spell (containing
//! anything but ASCII letters, digits, `_`, `-` and `.`) are left out, as values the command
//! inherits. Everything else is copied unchanged: tools, tasks, services and the generated `_`
//! directives (such as `_.path`), which mise still applies. A value is never written into the
//! copy, only its name.
//!
//! [`TaskConfig::env`] points mise at the copy as its global configuration, with
//! `MISE_GLOBAL_CONFIG_ROOT` set to the project root, and at a project configuration that
//! never exists. The copy is then the only configuration mise loads, while everything mise
//! resolves against the configuration's root (a task's working directory, relative `_.path`
//! entries, `{{config_root}}`) is the project root, as it is for the project's own file.
//! mise trusts its global configuration, so the copy is never registered as trusted.
//!
//! mise links every configuration it loads under `$MISE_STATE_DIR/tracked-configs`, and only
//! `mise prune` removes links whose file is gone. Once [`TaskConfig::track_with`] has named the
//! command's environment, dropping the copy removes the links that point into it, and links
//! into copies of runs killed before they could are removed by the next run.

use crate::error::{io_error, Result, StackError};
use std::path::{Path, PathBuf};

/// Where each copy is made, under the cache.
const PURPOSE: &str = "task-config";
/// The copied configuration, relative to the copy's directory: the project's own layout.
const CONFIG: &str = ".config/mise/conf.d/stack.toml";
/// The provider lock, relative to the copy's directory.
const LOCK: &str = ".config/mise/mise.lock";
/// A project configuration name the copy's directory never contains, so mise finds none.
const NO_PROJECT_CONFIG: &str = "no-project-config.toml";

/// One invocation's copy of the generated configuration, removed when dropped.
#[derive(Debug)]
pub struct TaskConfig {
    dir: tempfile::TempDir,
    root: PathBuf,
    /// `<cache>/task-config`, where every copy is made.
    parent: PathBuf,
    /// mise's `tracked-configs` directory, as the command's environment names it.
    tracked: Option<PathBuf>,
}

impl TaskConfig {
    /// Copy `root`'s generated configuration, with `lock` (the provider lock rendered from the
    /// validated stack.lock, `None` when it embeds nothing) beside it. Call it under the project
    /// lock, after the compile that planned the task and the environment read from what it
    /// wrote, so the copy is what that compile wrote and validated. `planned` gives the value the plan holds (and the
    /// command inherits) for an `[env]` variable mise evaluated; such a variable is declared as
    /// that inherited value. `shell_expands` answers whether mise expands `$VAR` in `[env]`
    /// values; it is asked only when a planned value contains a `$`.
    pub fn capture(
        root: &Path,
        cache: &Path,
        lock: Option<&str>,
        planned: impl Fn(&str) -> Option<String>,
        shell_expands: impl FnOnce() -> Result<bool>,
    ) -> Result<Self> {
        let path = super::mise::output_path(root);
        let config = std::fs::read(&path).map_err(|e| io_error(path.display(), e))?;
        let config = inheriting_planned_env(&config, planned, shell_expands, &path)?;
        let parent = cache.join(PURPOSE);
        std::fs::create_dir_all(&parent).map_err(|e| io_error(parent.display(), e))?;
        // Absolute: the command runs from the project root, not from where stack started.
        let parent = parent.canonicalize().map_err(|e| io_error(parent.display(), e))?;
        sweep(&parent);
        // mise reads its project configuration names as a `:`-separated list.
        if parent.to_string_lossy().contains(':') {
            return Err(StackError::new(
                "io",
                format!("cannot copy a task's configuration under {}: mise cannot name a path containing ':'", parent.display()),
            )
            .hint("set STACK_CACHE_DIR to a directory whose path has no ':'"));
        }
        // Owner-only (0700) and new: an existing directory is never reused or written into.
        let prefix = format!("{}-", std::process::id());
        let mut builder = tempfile::Builder::new();
        builder.prefix(&prefix);
        #[cfg(unix)]
        builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let dir = builder
            .tempdir_in(&parent)
            .map_err(|e| io_error(parent.display(), e))?;
        write_new(&dir.path().join(CONFIG), &config)?;
        if let Some(lock) = lock {
            write_new(&dir.path().join(LOCK), lock.as_bytes())?;
        }
        Ok(Self { dir, root: root.to_path_buf(), parent, tracked: None })
    }

    /// The copied configuration.
    pub fn config(&self) -> PathBuf {
        self.dir.path().join(CONFIG)
    }

    /// The directory holding the copy, removed on drop.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Provider variables that make `mise run` load the copy and nothing else, resolving it
    /// against the project root. They replace the ones that name the project's configuration.
    pub fn env(&self) -> [(String, String); 3] {
        [
            ("MISE_GLOBAL_CONFIG_FILE", self.config()),
            ("MISE_GLOBAL_CONFIG_ROOT", self.root.clone()),
            ("MISE_OVERRIDE_CONFIG_FILENAMES", self.dir.path().join(NO_PROJECT_CONFIG)),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string_lossy().into_owned()))
    }

    /// Name the environment the command runs with (`var` answers what it sees for a variable),
    /// so the links mise makes to the copy can be removed with it. Links to copies that no
    /// longer exist, left by runs killed before they cleaned up, are removed now.
    pub fn track_with(&mut self, var: impl Fn(&str) -> Option<String>) {
        // As mise finds it, run from the root: the same rules as a scratch root's commands.
        self.tracked = super::scratch::tracked_configs(&self.root, var);
        self.unlink(|target| target.starts_with(&self.parent) && !target.exists());
    }

    /// Remove mise's tracking links whose target `matches`. Only links are touched.
    fn unlink(&self, matches: impl Fn(&Path) -> bool) {
        let Some(entries) = self.tracked.as_ref().and_then(|t| std::fs::read_dir(t).ok()) else {
            return;
        };
        for entry in entries.flatten() {
            if std::fs::read_link(entry.path()).is_ok_and(|target| matches(&target)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

impl Drop for TaskConfig {
    fn drop(&mut self) {
        // The directory itself is removed after this, when `dir` is dropped.
        self.unlink(|target| target.starts_with(self.dir.path()));
    }
}

/// `config` with each plain `[env]` value `planned` gives declared as the inherited value (see
/// the module documentation); unchanged if it gives none. Directives (the `_` table) and every
/// other table are kept. `path` names the generated configuration in errors.
fn inheriting_planned_env(
    config: &[u8],
    planned: impl Fn(&str) -> Option<String>,
    shell_expands: impl FnOnce() -> Result<bool>,
    path: &Path,
) -> Result<Vec<u8>> {
    let unreadable = |e: String| {
        StackError::new("provider_failed", format!("cannot read the generated configuration {}: {e}", path.display()))
            .hint("run `stack compile` to regenerate it")
    };
    let text = std::str::from_utf8(config).map_err(|e| unreadable(e.to_string()))?;
    let mut doc: toml::Table = toml::from_str(text).map_err(|e| unreadable(e.message().trim().to_string()))?;
    let Some(toml::Value::Table(env)) = doc.get_mut("env") else {
        return Ok(config.to_vec());
    };
    let held: Vec<(String, String)> = env
        .iter()
        .filter(|(_, value)| value.is_str())
        .filter_map(|(key, _)| Some((key.clone(), planned(key)?)))
        .collect();
    if held.is_empty() {
        return Ok(config.to_vec());
    }
    let expands = match held.iter().any(|(_, value)| value.contains('$')) {
        true => shell_expands()?,
        false => false,
    };
    for (key, value) in &held {
        match inherited(key, value, expands) {
            Some(declared) => env.insert(key.clone(), toml::Value::String(declared)),
            None => env.remove(key),
        };
    }
    if env.is_empty() {
        doc.remove("env");
    }
    let body = toml::to_string_pretty(&doc).map_err(|e| unreadable(e.to_string()))?;
    Ok(format!("# Copied by stack for one task run, declaring the [env] values the run inherits.\n{body}").into_bytes())
}

/// The declaration that gives `key` the `value` the command inherits, or `None` to leave it
/// out: `PATH`, and a name the template cannot spell. With `expands`, a `$` is doubled so
/// mise's `$VAR` expansion gives it back.
fn inherited(key: &str, value: &str, expands: bool) -> Option<String> {
    let spelled = key.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b));
    if key == "PATH" || key.is_empty() || !spelled {
        return None;
    }
    let escape = if expands && value.contains('$') { r#" | replace(from="$", to="$$")"# } else { "" };
    Some(format!(r#"{{{{ env["{key}"]{escape} }}}}"#))
}

/// Write a file that must not exist yet, readable by its owner only.
fn write_new(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().expect("copied files have a parent");
    std::fs::create_dir_all(parent).map_err(|e| io_error(parent.display(), e))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|e| io_error(path.display(), e))?;
    file.write_all(contents).map_err(|e| io_error(path.display(), e))
}

/// Remove copies left by stack processes that exited without dropping them (killed by a
/// signal). A copy is named after the process that made it; a live one is never touched.
fn sweep(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.split_once('-')).and_then(|(pid, _)| pid.parse::<u32>().ok()) else {
            continue;
        };
        if pid != std::process::id() && !crate::state::pid_alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// For a copy whose planned values hold no `$`: mise's setting must not be asked.
    fn never() -> Result<bool> {
        panic!("the shell expansion setting was asked without a `$` in a planned value")
    }

    fn project(config: &str, lock: Option<&str>) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".config/mise/conf.d")).unwrap();
        std::fs::write(root.path().join(CONFIG), config).unwrap();
        if let Some(lock) = lock {
            std::fs::write(root.path().join(LOCK), lock).unwrap();
        }
        root
    }

    #[test]
    fn the_copy_is_private_unchanged_by_later_compiles_and_removed_on_drop() {
        let root = project("[tasks.a]\nrun = 'version-one'\n", Some("lockfile_version = 3\n"));
        let cache = tempfile::tempdir().unwrap();
        let copy = TaskConfig::capture(root.path(), cache.path(), Some("lockfile_version = 3\n"), |_| None, never).unwrap();
        std::fs::write(root.path().join(CONFIG), "[tasks.a]\nrun = 'version-two'\n").unwrap();
        std::fs::remove_file(root.path().join(LOCK)).unwrap();
        assert_eq!(std::fs::read_to_string(copy.config()).unwrap(), "[tasks.a]\nrun = 'version-one'\n");
        assert_eq!(std::fs::read_to_string(copy.dir().join(LOCK)).unwrap(), "lockfile_version = 3\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(copy.dir()).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(copy.config()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let env = copy.env();
        assert_eq!(env[0], ("MISE_GLOBAL_CONFIG_FILE".into(), copy.config().to_string_lossy().into_owned()));
        assert_eq!(env[1], ("MISE_GLOBAL_CONFIG_ROOT".into(), root.path().to_string_lossy().into_owned()));
        assert!(!Path::new(&env[2].1).exists() && Path::new(&env[2].1).is_absolute());
        let dir = copy.dir().to_path_buf();
        drop(copy);
        assert!(!dir.exists());
    }

    #[test]
    fn the_lock_beside_the_copy_is_the_one_given_never_the_projects_rendered_file() {
        // A stale rendered lock in the project (an earlier stack.lock's checksum) is not copied.
        let root = project("[tasks.a]\nrun = 'x'\n", Some("stale = 'project file'\n"));
        let cache = tempfile::tempdir().unwrap();
        let copy = TaskConfig::capture(root.path(), cache.path(), Some("validated = 'stack.lock'\n"), |_| None, never).unwrap();
        assert_eq!(std::fs::read_to_string(copy.dir().join(LOCK)).unwrap(), "validated = 'stack.lock'\n");
        // A stack.lock that embeds nothing gives no lock, whatever the project holds.
        let copy = TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap();
        assert!(!copy.dir().join(LOCK).exists());
        // None rendered in the project (a fresh checkout): the given lock is still beside the copy.
        std::fs::remove_file(root.path().join(LOCK)).unwrap();
        let copy = TaskConfig::capture(root.path(), cache.path(), Some("validated = 'stack.lock'\n"), |_| None, never).unwrap();
        assert_eq!(std::fs::read_to_string(copy.dir().join(LOCK)).unwrap(), "validated = 'stack.lock'\n");
    }

    #[test]
    fn concurrent_copies_are_distinct_and_a_missing_lock_is_not_invented() {
        let root = project("[tasks.a]\nrun = 'x'\n", None);
        let cache = tempfile::tempdir().unwrap();
        let (a, b) = (TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap(), TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap());
        assert_ne!(a.dir(), b.dir());
        assert!(!a.dir().join(LOCK).exists());
        drop(a);
        assert!(b.config().exists(), "dropping one copy must not remove another");
    }

    #[test]
    fn copies_of_exited_processes_are_swept_and_live_ones_kept() {
        let root = project("[tasks.a]\nrun = 'x'\n", None);
        let cache = tempfile::tempdir().unwrap();
        let parent = cache.path().join(PURPOSE);
        // A process that has exited: spawned and waited for.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        std::fs::create_dir_all(parent.join(format!("{dead}-left/.config"))).unwrap();
        let mut live = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::create_dir_all(parent.join(format!("{}-running", live.id()))).unwrap();
        std::fs::create_dir_all(parent.join("unrelated")).unwrap();
        let _copy = TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap();
        let kept = parent.join(format!("{}-running", live.id())).exists();
        let _ = live.kill();
        let _ = live.wait();
        assert!(!parent.join(format!("{dead}-left")).exists());
        assert!(kept, "a live process's copy is kept");
        assert!(parent.join("unrelated").exists());
    }

    #[test]
    fn links_mise_tracks_to_copies_are_removed_with_them_and_others_kept() {
        let root = project("[tasks.a]\nrun = 'x'\n", None);
        let (cache, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let tracked = state.path().join("tracked-configs");
        std::fs::create_dir_all(&tracked).unwrap();
        let parent = cache.path().join(PURPOSE);
        std::fs::create_dir_all(&parent).unwrap();
        let parent = parent.canonicalize().unwrap();
        let link = |name: &str, target: &Path| std::os::unix::fs::symlink(target, tracked.join(name)).unwrap();
        // Left by a killed run, a live project's config, and a missing file stack never made.
        link("killed", &parent.join("1-gone/.config/mise/conf.d/stack.toml"));
        link("project", &root.path().join(CONFIG));
        link("elsewhere", &state.path().join("missing.toml"));
        let mut copy = TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap();
        let state_dir = state.path().to_string_lossy().into_owned();
        copy.track_with(|key| (key == "MISE_STATE_DIR").then(|| state_dir.clone()));
        assert!(!tracked.join("killed").exists() && tracked.join("killed").symlink_metadata().is_err());
        link("copy", &copy.config());
        drop(copy);
        let mut left: Vec<String> = std::fs::read_dir(&tracked).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["elsewhere", "project"]);
    }

    #[test]
    fn planned_env_values_are_declared_as_inherited_and_everything_else_is_kept() {
        let config = "[tools]\nnode = \"24.13.0\"\n\n[env]\nASSET = \"{{ config_source }}/x\"\nKEPT = \"{{ config_root }}\"\nPATH = \"/scalar-bin:{{ env.PATH }}\"\n\"x\\\"y\" = \"odd\"\n\n[env._]\npath = [\"bin\"]\n\n[tasks.a]\nrun = \"\"\"\necho {{env.ASSET}}\n\"\"\"\n";
        let root = project(config, None);
        let cache = tempfile::tempdir().unwrap();
        let planned = |key: &str| ["ASSET", "PATH", "x\"y", "_", "MISSING"].contains(&key).then(|| format!("{key}-secret-planned-value"));
        let copy = TaskConfig::capture(root.path(), cache.path(), None, planned, never).unwrap();
        let text = std::fs::read_to_string(copy.config()).unwrap();
        assert!(!text.contains("planned-value"), "a value is never written into the copy: {text}");
        let held: toml::Table = toml::from_str(&text).unwrap();
        let mut expected: toml::Table = toml::from_str(config).unwrap();
        let env = expected["env"].as_table_mut().unwrap();
        env.insert("ASSET".into(), toml::Value::String(r#"{{ env["ASSET"] }}"#.into()));
        // PATH and a name the template cannot spell are left out; the `_` directives and an
        // unplanned value stay as compile wrote them, in their order.
        env.remove("PATH");
        env.remove("x\"y");
        assert_eq!(held, expected);
        let keys: Vec<&String> = held["env"].as_table().unwrap().keys().collect();
        assert_eq!(keys, ["ASSET", "KEPT", "_"]);

        // Nothing planned: the bytes compile wrote. Only PATH planned: no `[env]` at all.
        let copy = TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap();
        assert_eq!(std::fs::read_to_string(copy.config()).unwrap(), config);
        let root = project("[env]\nPATH = \"a\"\n[tasks.a]\nrun = \"x\"\n", None);
        let copy = TaskConfig::capture(root.path(), cache.path(), None, |_| Some("a".into()), never).unwrap();
        let held: toml::Table = toml::from_str(&std::fs::read_to_string(copy.config()).unwrap()).unwrap();
        assert!(!held.contains_key("env") && held["tasks"]["a"]["run"].as_str() == Some("x"));
    }

    #[test]
    fn a_dollar_is_escaped_only_where_mise_expands_and_the_setting_is_asked_only_then() {
        let root = project("[env]\nA = \"$${HOME}\"\nB = \"plain\"\n", None);
        let cache = tempfile::tempdir().unwrap();
        let planned = |key: &str| Some(if key == "A" { "${HOME}".to_string() } else { "plain".into() });
        let declared = |expands: bool| {
            let copy = TaskConfig::capture(root.path(), cache.path(), None, planned, || Ok(expands)).unwrap();
            let held: toml::Table = toml::from_str(&std::fs::read_to_string(copy.config()).unwrap()).unwrap();
            let env = held["env"].as_table().unwrap();
            (env["A"].as_str().unwrap().to_string(), env["B"].as_str().unwrap().to_string())
        };
        assert_eq!(declared(true), (r#"{{ env["A"] | replace(from="$", to="$$") }}"#.into(), r#"{{ env["B"] }}"#.into()));
        assert_eq!(declared(false), (r#"{{ env["A"] }}"#.into(), r#"{{ env["B"] }}"#.into()));
        // No `$` in any planned value: the setting is not asked. A failure to ask is the error.
        assert!(TaskConfig::capture(root.path(), cache.path(), None, |_| Some("plain".into()), never).is_ok());
        let err = TaskConfig::capture(root.path(), cache.path(), None, planned, || Err(StackError::new("provider_failed", "asked"))).unwrap_err();
        assert_eq!((err.code, err.message.as_str()), ("provider_failed", "asked"));
        assert!(std::fs::read_dir(cache.path().join(PURPOSE)).unwrap().next().is_none());
    }

    #[test]
    fn an_unreadable_generated_configuration_is_refused_before_anything_is_copied() {
        let root = project("[env\n", None);
        let cache = tempfile::tempdir().unwrap();
        let err = TaskConfig::capture(root.path(), cache.path(), None, |_| Some("v".into()), never).unwrap_err();
        assert_eq!(err.code, "provider_failed");
        assert!(!cache.path().join(PURPOSE).exists());
    }

    #[test]
    fn tracking_links_are_found_where_mise_keeps_them_for_a_home_relative_state_directory() {
        let root = project("[tasks.a]\nrun = 'x'\n", None);
        let (cache, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let home = home.path().canonicalize().unwrap();
        let tracked = home.join("mise-state/tracked-configs");
        std::fs::create_dir_all(&tracked).unwrap();
        let mut copy = TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap();
        let home_dir = home.to_string_lossy().into_owned();
        // A literal `~`, as mise reads it: beneath HOME, not beneath the project root.
        copy.track_with(|key| match key {
            "MISE_STATE_DIR" => Some("~/mise-state".into()),
            "HOME" => Some(home_dir.clone()),
            _ => None,
        });
        std::os::unix::fs::symlink(copy.config(), tracked.join("copy")).unwrap();
        std::os::unix::fs::symlink(root.path().join(CONFIG), tracked.join("project")).unwrap();
        drop(copy);
        let left: Vec<String> = std::fs::read_dir(&tracked).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["project"]);
        assert!(!root.path().join("~").exists());
    }

    #[test]
    fn a_missing_generated_configuration_is_an_error_and_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let err = TaskConfig::capture(root.path(), cache.path(), None, |_| None, never).unwrap_err();
        assert_eq!(err.code, "io");
        assert!(!cache.path().join(PURPOSE).exists());
    }
}

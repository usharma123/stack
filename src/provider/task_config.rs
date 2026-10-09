//! The provider configuration one task invocation runs from, fixed when it is planned.
//!
//! `stack run` plans a task under the project lock and releases the lock before `mise run`
//! starts, so a long task does not block the project. `mise run` reads the task's body and env
//! from the generated configuration when it starts, and an ordinary compile may rewrite that
//! file in between. Without a copy, a saved plan would run whatever definition is current then,
//! with the grant and service checks of the one it was planned from.
//!
//! [`TaskConfig::capture`] copies the generated configuration, and the provider lock rendered
//! next to it, under the planning lock, to `<cache>/task-config/<pid>-<random>/` (a directory
//! only this user can read, removed when the [`TaskConfig`] is dropped: at command completion,
//! timeout or error, or when planning fails after the copy). The copy keeps the layout of the
//! project's files (`.config/mise/conf.d/stack.toml`, `.config/mise/mise.lock`), so mise finds
//! the lock beside it as it would in the project. Only what compile already wrote is copied:
//! granted values are never in either file.
//!
//! The copy leaves out the plain `[env]` values the plan already holds. Planning read them
//! from the project's own file, so a template such as `{{config_source}}` named that file;
//! `mise run` would evaluate the copy's declarations again, against the copy, and replace
//! them. Left out, the planned value the command inherits is the one the task sees, in its
//! shell and in `{{env.X}}` task templates. Everything else is copied unchanged: tools, tasks,
//! services and the generated `_` directives (such as `_.path`), which mise still applies.
//! A value is never written into the copy, only left out of it.
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
    /// Copy `root`'s generated configuration and rendered lock. Call it under the project lock,
    /// after the compile that planned the task and the environment read from what it wrote, so
    /// the copy is what that compile wrote. `planned` answers whether the plan holds the value
    /// mise evaluated for an `[env]` variable; such a variable is left out of the copy.
    pub fn capture(root: &Path, cache: &Path, planned: impl Fn(&str) -> bool) -> Result<Self> {
        let path = super::mise::output_path(root);
        let config = std::fs::read(&path).map_err(|e| io_error(path.display(), e))?;
        let config = without_planned_env(&config, planned).map_err(|e| {
            StackError::new("provider_failed", format!("cannot read the generated configuration {}: {e}", path.display()))
                .hint("run `stack compile` to regenerate it")
        })?;
        let lock = match std::fs::read(crate::artifacts::rendered_path(root)) {
            Ok(lock) => Some(lock),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error(crate::artifacts::rendered_path(root).display(), e)),
        };
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
            write_new(&dir.path().join(LOCK), &lock)?;
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
        // A relative value is read from the working directory, the root; an absolute one stands.
        let path = |key: &str| var(key).filter(|v| !v.is_empty()).map(|v| self.root.join(v));
        let state = path("MISE_STATE_DIR")
            .or_else(|| path("XDG_STATE_HOME").map(|d| d.join("mise")))
            .or_else(|| path("HOME").map(|h| h.join(".local/state/mise")));
        self.tracked = state.map(|s| s.join("tracked-configs"));
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

/// `config` without the plain `[env]` values `planned` names; unchanged if it names none.
/// Directives (the `_` table) and every other table are kept.
fn without_planned_env(config: &[u8], planned: impl Fn(&str) -> bool) -> std::result::Result<Vec<u8>, String> {
    let text = std::str::from_utf8(config).map_err(|e| e.to_string())?;
    let mut doc: toml::Table = toml::from_str(text).map_err(|e| e.message().trim().to_string())?;
    let Some(toml::Value::Table(env)) = doc.get_mut("env") else {
        return Ok(config.to_vec());
    };
    let before = env.len();
    env.retain(|key, value| !(value.is_str() && planned(key)));
    if env.len() == before {
        return Ok(config.to_vec());
    }
    if env.is_empty() {
        doc.remove("env");
    }
    let body = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    Ok(format!("# Copied by stack for one task run, without the [env] values the run inherits.\n{body}").into_bytes())
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
        let copy = TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap();
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
    fn concurrent_copies_are_distinct_and_a_missing_lock_is_not_invented() {
        let root = project("[tasks.a]\nrun = 'x'\n", None);
        let cache = tempfile::tempdir().unwrap();
        let (a, b) = (TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap(), TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap());
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
        let _copy = TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap();
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
        let mut copy = TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap();
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
    fn planned_env_values_are_left_out_and_everything_else_is_kept() {
        let config = "[tools]\nnode = \"24.13.0\"\n\n[env]\nASSET = \"{{ config_source }}/x\"\nKEPT = \"{{ config_root }}\"\n\n[env._]\npath = [\"bin\"]\n\n[tasks.a]\nrun = \"\"\"\necho {{env.ASSET}}\n\"\"\"\n";
        let root = project(config, None);
        let cache = tempfile::tempdir().unwrap();
        let copy = TaskConfig::capture(root.path(), cache.path(), |key| ["ASSET", "_", "MISSING"].contains(&key)).unwrap();
        let held: toml::Table = toml::from_str(&std::fs::read_to_string(copy.config()).unwrap()).unwrap();
        let mut expected: toml::Table = toml::from_str(config).unwrap();
        expected["env"].as_table_mut().unwrap().remove("ASSET");
        // Only a plain value is left out: the `_` directives stay even when asked about.
        assert_eq!(held, expected);

        // Nothing planned: the bytes compile wrote. Every value planned: no `[env]` at all.
        let copy = TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap();
        assert_eq!(std::fs::read_to_string(copy.config()).unwrap(), config);
        let root = project("[env]\nA = \"a\"\n[tasks.a]\nrun = \"x\"\n", None);
        let copy = TaskConfig::capture(root.path(), cache.path(), |_| true).unwrap();
        let held: toml::Table = toml::from_str(&std::fs::read_to_string(copy.config()).unwrap()).unwrap();
        assert!(!held.contains_key("env") && held["tasks"]["a"]["run"].as_str() == Some("x"));
    }

    #[test]
    fn an_unreadable_generated_configuration_is_refused_before_anything_is_copied() {
        let root = project("[env\n", None);
        let cache = tempfile::tempdir().unwrap();
        let err = TaskConfig::capture(root.path(), cache.path(), |_| true).unwrap_err();
        assert_eq!(err.code, "provider_failed");
        assert!(!cache.path().join(PURPOSE).exists());
    }

    #[test]
    fn a_missing_generated_configuration_is_an_error_and_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let err = TaskConfig::capture(root.path(), cache.path(), |_| false).unwrap_err();
        assert_eq!(err.code, "io");
        assert!(!cache.path().join(PURPOSE).exists());
    }
}

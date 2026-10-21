//! `stack init`: a starting `stack.toml`, so a new project need not be written from the docs.

use crate::error::{io_error, Result, StackError};
use crate::manifest::PROJECT_FILE;
use serde::Serialize;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

/// What `init` wrote.
#[derive(Debug, Serialize)]
pub struct InitReport {
    pub path: PathBuf,
    pub tools: Vec<String>,
    pub services: Vec<String>,
    pub next: Vec<&'static str>,
}

/// Presets `--service` accepts, as `NAME`, `NAME=PRESET` or either with `@VERSION`.
const PRESETS: &[&str] = &["postgres", "redis", "cockroachdb", "nats", "spicedb"];

/// Write `root/stack.toml` with `tools` (`NAME` or `NAME=VERSION`, `latest` when no version is
/// given) and preset `services` (`NAME[=PRESET][@VERSION]`). The file is parsed back as a
/// project before anything is written, then published whole, by an atomic rename that never
/// replaces whatever has the name: a file, a link (dangling or not, never followed), or what
/// another `init` published meanwhile. Any of those is `project_exists`, and is left as it was.
/// An interrupted write leaves at most a hidden temporary file, never a partial `stack.toml`.
pub fn init(root: &Path, tools: &[String], services: &[String]) -> Result<InitReport> {
    let path = root.join(PROJECT_FILE);
    let exists = || {
        StackError::new("project_exists", format!("{} already exists", path.display()))
            .hint("edit it, or run `stack init` in a directory without one")
    };
    // Settles nothing (another `init` may publish next); spares the work when one is there.
    if path.symlink_metadata().is_ok() {
        return Err(exists());
    }
    let usage = |what: &str, value: &str, why: String| {
        StackError::new("usage", format!("{what} {value:?}: {why}")).hint("--tool NAME[=VERSION], --service NAME[=PRESET][@VERSION]; see `stack init --help`")
    };
    let mut tool_table = Table::new();
    for spec in tools {
        let (name, version) = spec.split_once('=').unwrap_or((spec, "latest"));
        if name.trim().is_empty() || version.trim().is_empty() {
            return Err(usage("--tool", spec, "needs a name, and a version after `=` when one is given".into()));
        }
        tool_table.insert(name.trim().into(), Value::String(version.trim().into()));
    }
    let mut service_table = Table::new();
    for spec in services {
        let (named, version) = match spec.split_once('@') {
            Some((named, version)) => (named, Some(version.trim())),
            None => (spec.as_str(), None),
        };
        let (name, preset) = named.split_once('=').unwrap_or((named, named));
        let (name, preset) = (name.trim(), preset.trim());
        if !PRESETS.contains(&preset) {
            return Err(usage("--service", spec, format!("presets are {}; write a service with its own `run` in stack.toml", PRESETS.join(", "))));
        }
        if name.is_empty() || version.is_some_and(str::is_empty) {
            return Err(usage("--service", spec, "needs a name, and a version after `@` when one is given".into()));
        }
        let mut service = Table::new();
        service.insert("preset".into(), Value::String(preset.into()));
        if let Some(version) = version {
            service.insert("version".into(), Value::String(version.into()));
        }
        service_table.insert(name.into(), Value::Table(service));
    }
    let mut doc = Table::new();
    doc.insert("tools".into(), Value::Table(tool_table.clone()));
    if !service_table.is_empty() {
        doc.insert("services".into(), Value::Table(service_table.clone()));
    }
    let body = toml::to_string_pretty(&doc).map_err(|e| StackError::new("usage", e.to_string()))?;
    let task_services: Vec<String> = service_table.keys().map(|s| format!("{s:?}")).collect();
    let text = format!(
        "# Stack project: tools, services and tasks. Commit this file and stack.lock.\n\
         # Reference: https://github.com/usharma123/stack/blob/main/docs/user/bundles.md\n\
         {body}\n\
         # A service of your own; it gets $PORT, and the app gets $<NAME>_PORT and $<NAME>_URL.\n\
         # [services.api]\n\
         # run = \"exec python3 -m http.server $PORT --bind 127.0.0.1\"\n\
         # ready_cmd = \"curl -fsS http://127.0.0.1:$PORT/ >/dev/null\"\n\
         # depends_on = [{}]\n\
         # watch = [\"app.py\"]\n\
         \n\
         # [tasks.test]\n\
         # run = \"pytest -q\"\n",
        task_services.join(", ")
    );
    // Whatever was asked for must make a project Stack reads.
    let check = tempfile::tempdir().map_err(|e| io_error("temporary directory", e))?;
    std::fs::write(check.path().join(PROJECT_FILE), &text).map_err(|e| io_error("temporary directory", e))?;
    crate::manifest::read_project(check.path())?;
    let mut staged = tempfile::Builder::new();
    staged.prefix(".stack.toml.");
    // Readable as a file `fs::write` creates: 0666 less the umask, not a temporary file's 0600.
    #[cfg(unix)]
    staged.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let mut staged = staged
        .tempfile_in(root)
        .map_err(|e| io_error(root.display(), e))?;
    std::io::Write::write_all(&mut staged, text.as_bytes())
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|e| io_error(staged.path().display(), e))?;
    // Dropping the staged file on failure removes it.
    staged.persist_noclobber(&path).map_err(|e| match e.error.kind() {
        std::io::ErrorKind::AlreadyExists => exists(),
        _ => io_error(path.display(), e.error),
    })?;
    Ok(InitReport {
        path,
        tools: tool_table.keys().cloned().collect(),
        services: service_table.keys().cloned().collect(),
        next: vec!["stack compile", "stack up --ttl 30m", "stack exec -- <command>", "stack down"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_a_project_stack_reads_and_never_replaces_one() {
        let dir = tempfile::tempdir().unwrap();
        let report = init(dir.path(), &["python=3.13".into(), "uv".into()], &["postgres@17".into(), "cache=redis".into()]).unwrap();
        assert_eq!((report.tools, report.services), (vec!["python".to_string(), "uv".into()], vec!["postgres".to_string(), "cache".into()]));
        let project = crate::manifest::read_project(dir.path()).unwrap();
        assert_eq!(project.tools["python"].as_str(), Some("3.13"));
        assert_eq!(project.tools["uv"].as_str(), Some("latest"));
        assert_eq!(project.services["postgres"].version.as_deref(), Some("17"));
        assert_eq!(project.services["cache"].preset.as_deref(), Some("redis"));
        let e = init(dir.path(), &[], &[]).unwrap_err();
        assert_eq!(e.code, "project_exists");

        let empty = tempfile::tempdir().unwrap();
        init(empty.path(), &[], &[]).unwrap();
        assert!(crate::manifest::read_project(empty.path()).unwrap().services.is_empty());
        for (tools, services) in [(vec!["=1".to_string()], vec![]), (vec![], vec!["web=nginx".to_string()]), (vec![], vec!["db@".to_string()])] {
            let other = tempfile::tempdir().unwrap();
            assert_eq!(init(other.path(), &tools, &services).unwrap_err().code, "usage");
            assert!(!other.path().join(PROJECT_FILE).exists());
        }
    }

    /// What the directory holds, by name.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn concurrent_inits_leave_exactly_one_whole_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let writers = 12;
        let barrier = std::sync::Barrier::new(writers);
        let results: Vec<Result<InitReport>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..writers)
                .map(|i| {
                    let (dir, barrier) = (dir.path(), &barrier);
                    scope.spawn(move || {
                        let tools: Vec<String> = (0..250).map(|k| format!("probe{k}=1.{i}")).collect();
                        barrier.wait();
                        init(dir, &tools, &[])
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let won: Vec<usize> = results.iter().enumerate().filter(|(_, r)| r.is_ok()).map(|(i, _)| i).collect();
        assert_eq!(won.len(), 1, "one writer wins: {won:?}");
        for r in &results {
            if let Err(e) = r {
                assert_eq!(e.code, "project_exists", "{e}");
            }
        }
        // The file is the winner's, whole, and nothing else was left beside it.
        let project = crate::manifest::read_project(dir.path()).unwrap();
        assert_eq!(project.tools.len(), 250);
        assert_eq!(project.tools["probe249"].as_str(), Some(format!("1.{}", won[0]).as_str()));
        assert_eq!(names(dir.path()), [PROJECT_FILE]);
        // Created like any file the user writes there, not private like a temporary file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let plain = dir.path().join("plain");
            std::fs::write(&plain, "").unwrap();
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.path().join(PROJECT_FILE)), mode(&plain));
        }
    }

    #[cfg(unix)]
    #[test]
    fn init_never_writes_through_a_link_in_the_projects_place() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // A dangling link: `exists()` says no file is there.
        let target = outside.path().join("elsewhere.toml");
        std::os::unix::fs::symlink(&target, dir.path().join(PROJECT_FILE)).unwrap();
        assert_eq!(init(dir.path(), &[], &[]).unwrap_err().code, "project_exists");
        assert!(!target.exists(), "written through the link");
        assert_eq!(std::fs::read_link(dir.path().join(PROJECT_FILE)).unwrap(), target);
        // A link to a file someone has not saved elsewhere: kept as it is.
        std::fs::write(&target, "unsaved = true\n").unwrap();
        assert_eq!(init(dir.path(), &["uv".into()], &[]).unwrap_err().code, "project_exists");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unsaved = true\n");
        assert_eq!(names(dir.path()), [PROJECT_FILE]);
    }
}

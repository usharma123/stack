//! Merge bundles and the project into one stack.
//!
//! Rule: two layers may define the same key only if they agree. Any disagreement is a conflict
//! unless the project resolves it in `[override.*]`. Nothing is silently "last one wins".

use crate::error::{Result, StackError};
use crate::manifest::{BundleManifest, Overrides, ProjectManifest, Service, Task};
use crate::tool::{self, ToolSpec};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const BUNDLE_DIR_VAR: &str = "{{bundle_dir}}";

#[derive(Debug, Clone, Serialize)]
pub struct Entry<T> {
    pub value: T,
    /// `bundle:<name>`, `project` or `override`.
    pub origin: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverrideRecord {
    pub kind: &'static str,
    pub key: String,
    /// Layers whose value the override replaced. Empty if the override only adds the key.
    pub replaced: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Composed {
    pub tools: IndexMap<String, Entry<ToolSpec>>,
    pub env: IndexMap<String, Entry<String>>,
    pub services: IndexMap<String, Entry<Service>>,
    pub tasks: IndexMap<String, Entry<Task>>,
    pub bin_paths: Vec<PathBuf>,
    pub overrides: Vec<OverrideRecord>,
    /// `services.<name>.init` of every layer, composed as its own key (see `init_tasks`).
    /// Not serialized with the stack, so the session fingerprint does not depend on it;
    /// reports show it beside the stack.
    #[serde(skip)]
    pub init: IndexMap<String, Entry<Vec<String>>>,
}

#[derive(Debug)]
struct Conflict {
    kind: &'static str,
    key: String,
    origins: Vec<String>,
}

/// A bundle after fetching, with `{{bundle_dir}}` already resolved.
pub struct LoadedBundle {
    pub manifest: BundleManifest,
    pub dir: PathBuf,
}

impl LoadedBundle {
    pub fn new(mut manifest: BundleManifest, dir: PathBuf) -> Result<Self> {
        let origin = format!("bundle:{}", manifest.bundle.name);
        let dir_str = dir.to_string_lossy().to_string();
        let expand = |s: &mut String| *s = s.replace(BUNDLE_DIR_VAR, &dir_str);

        manifest.env.values_mut().for_each(expand);
        for task in manifest.tasks.values_mut() {
            expand(&mut task.run);
        }
        for (name, service) in manifest.services.iter_mut() {
            service.validate(name, &origin)?;
            if let Some(toml::Value::Integer(port)) = &service.port {
                return Err(StackError::new(
                    "bundle_fixed_port",
                    format!("{origin} pins service '{name}' to port {port}"),
                )
                .hint("bundles must use port = \"auto\"; fixed ports belong in a project's [override.services]"));
            }
            service.run.as_mut().map(expand);
            service.ready_cmd.as_mut().map(expand);
            if let Some(probe) = service.identity.as_mut() {
                expand(&mut probe.command);
            }
            service.watch.iter_mut().for_each(expand);
        }
        for bin in &manifest.paths.bin {
            bin_dir(&dir, bin, &origin)?;
        }
        Ok(Self { manifest, dir })
    }
}

/// `paths.bin` entry `bin` as a directory of the bundle at `dir`, whose content the bundle's hash
/// covers: a relative path of plain names, none of them `..` or a directory the hash excludes
/// (`.git`, `.stack`), each a real directory rather than a symbolic link. Checked on the tree as
/// it is, so neither spelling nor a link can put a directory outside the bundle on PATH.
fn bin_dir(dir: &Path, bin: &str, origin: &str) -> Result<()> {
    let escapes = |why: &str| {
        StackError::new("bundle_invalid", format!("{origin} declares paths.bin '{bin}', which {why}"))
            .hint("name a directory inside the bundle by a relative path, e.g. paths.bin = [\"bin\"]")
    };
    let mut path = dir.to_path_buf();
    let mut named = false;
    for part in Path::new(bin).components() {
        match part {
            Component::CurDir => continue,
            Component::Normal(name) if crate::hash::excluded(name) => {
                return Err(escapes(&format!("is inside {}, which is not bundle content", name.to_string_lossy())))
            }
            Component::Normal(name) => path.push(name),
            Component::ParentDir => return Err(escapes("leaves the bundle through '..'")),
            Component::RootDir | Component::Prefix(_) => return Err(escapes("is not a relative path")),
        }
        named = true;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(escapes(&format!("passes through the symbolic link {}", path.display())))
            }
            Ok(meta) if meta.is_dir() => {}
            _ => {
                return Err(StackError::new(
                    "missing_path",
                    format!("{origin} declares paths.bin '{bin}' but it is not a directory in the bundle"),
                ))
            }
        }
    }
    if !named {
        return Err(escapes("names the bundle root rather than a directory in it"));
    }
    Ok(())
}

pub fn compose(bundles: &[LoadedBundle], project: &ProjectManifest) -> Result<Composed> {
    let mut out = Composed::default();
    let mut conflicts = Vec::new();

    for b in bundles {
        let origin = format!("bundle:{}", b.manifest.bundle.name);
        let tools = tool_specs(&b.manifest.tools, &origin)?;
        merge("tools", &mut out.tools, &tools, &origin, &mut conflicts);
        merge("env", &mut out.env, &b.manifest.env, &origin, &mut conflicts);
        let (services, init) = crate::init_tasks::split(&b.manifest.services);
        merge("services", &mut out.services, &services, &origin, &mut conflicts);
        merge("init", &mut out.init, &init, &origin, &mut conflicts);
        merge("tasks", &mut out.tasks, &b.manifest.tasks, &origin, &mut conflicts);
        for bin in &b.manifest.paths.bin {
            // Checked again here: compose may run long after the bundle was loaded.
            bin_dir(&b.dir, bin, &origin)?;
            out.bin_paths.push(b.dir.join(bin));
        }
    }

    for (name, service) in &project.services {
        service.validate(name, "project")?;
    }
    let tools = tool_specs(&project.tools, "project")?;
    merge("tools", &mut out.tools, &tools, "project", &mut conflicts);
    merge("env", &mut out.env, &project.env, "project", &mut conflicts);
    let (services, init) = crate::init_tasks::split(&project.services);
    merge("services", &mut out.services, &services, "project", &mut conflicts);
    merge("init", &mut out.init, &init, "project", &mut conflicts);
    merge("tasks", &mut out.tasks, &project.tasks, "project", &mut conflicts);

    apply_overrides(&mut out, &project.overrides, &mut conflicts)?;

    if !conflicts.is_empty() {
        let details = conflicts
            .iter()
            .map(|c| json!({ "kind": c.kind, "key": c.key, "defined_by": c.origins }))
            .collect::<Vec<_>>();
        let first = &conflicts[0];
        return Err(StackError::new(
            "conflict",
            format!(
                "{} conflicting definition(s); first: {}.{} defined differently by {}",
                conflicts.len(),
                first.kind,
                first.key,
                first.origins.join(" and ")
            ),
        )
        .hint(if first.kind == "init" {
            format!("resolve explicitly in stack.toml, e.g. [override.services.{}] init = [...]", first.key)
        } else {
            format!("resolve explicitly in stack.toml, e.g. [override.{}] {} = ...", first.kind, first.key)
        })
        .details(details));
    }

    validate_task_services(&out)?;
    validate_service_dependencies(&out)?;
    crate::init_tasks::validate(&out)?;
    validate_tool_options(&out)?;
    crate::secrets::validate_tasks(&out)?;
    Ok(out)
}

/// Each `tools.<name>` of one layer in canonical form; a value outside the allowlist is
/// `invalid_tool` naming the layer.
fn tool_specs(tools: &IndexMap<String, toml::Value>, origin: &str) -> Result<IndexMap<String, ToolSpec>> {
    tools.iter().map(|(name, value)| Ok((name.clone(), ToolSpec::parse(name, value, origin)?))).collect()
}

/// Options that only take effect together with another tool. mise ignores `mr_boxington`
/// unless Mr Boxington is among the active tools, and the user asked for caching.
fn validate_tool_options(out: &Composed) -> Result<()> {
    let has_mbx = out.tools.keys().any(|name| tool::is_mr_boxington(name));
    if let Some((name, e)) = out.tools.iter().find(|(_, e)| e.value.mr_boxington()) {
        if !has_mbx {
            return Err(StackError::new(
                "invalid_tool",
                format!("tools.{name} ({}) sets mr_boxington, but no layer adds Mr Boxington; mise requires mr-boxington in the active tools", e.origin),
            )
            .hint("add `mbx = \"<version>\"` to [tools], or use the rust-mbx bundle")
            .with_detail(json!({ "tool": name, "origin": e.origin, "option": tool::MR_BOXINGTON })));
        }
    }
    Ok(())
}

fn merge<T: Clone + PartialEq>(
    kind: &'static str,
    acc: &mut IndexMap<String, Entry<T>>,
    items: &IndexMap<String, T>,
    origin: &str,
    conflicts: &mut Vec<Conflict>,
) {
    for (key, value) in items {
        match acc.get(key) {
            None => {
                acc.insert(key.clone(), Entry { value: value.clone(), origin: origin.to_string() });
            }
            Some(existing) if existing.value == *value => {}
            Some(existing) => {
                if let Some(c) = conflicts.iter_mut().find(|c| c.kind == kind && c.key == *key) {
                    c.origins.push(origin.to_string());
                } else {
                    conflicts.push(Conflict {
                        kind,
                        key: key.clone(),
                        origins: vec![existing.origin.clone(), origin.to_string()],
                    });
                }
            }
        }
    }
}

fn apply_overrides(out: &mut Composed, o: &Overrides, conflicts: &mut Vec<Conflict>) -> Result<()> {
    for (name, service) in &o.services {
        service.validate(name, "[override.services]")?;
    }
    let tools = tool_specs(&o.tools, "[override.tools]")?;
    override_kind("tools", &mut out.tools, &tools, conflicts, &mut out.overrides);
    override_kind("env", &mut out.env, &o.env, conflicts, &mut out.overrides);
    let (services, init) = crate::init_tasks::split(&o.services);
    override_kind("services", &mut out.services, &services, conflicts, &mut out.overrides);
    override_kind("init", &mut out.init, &init, conflicts, &mut out.overrides);
    override_kind("tasks", &mut out.tasks, &o.tasks, conflicts, &mut out.overrides);
    Ok(())
}

fn override_kind<T: Clone>(
    kind: &'static str,
    acc: &mut IndexMap<String, Entry<T>>,
    items: &IndexMap<String, T>,
    conflicts: &mut Vec<Conflict>,
    records: &mut Vec<OverrideRecord>,
) {
    for (key, value) in items {
        let mut replaced: Vec<String> = acc.get(key).map(|e| vec![e.origin.clone()]).unwrap_or_default();
        if let Some(pos) = conflicts.iter().position(|c| c.kind == kind && c.key == *key) {
            replaced = conflicts.remove(pos).origins;
        }
        acc.insert(key.clone(), Entry { value: value.clone(), origin: "override".into() });
        records.push(OverrideRecord { kind, key: key.clone(), replaced });
    }
}

/// Every `depends_on` names another service, and no service depends on itself through others.
fn validate_service_dependencies(out: &Composed) -> Result<()> {
    for (name, service) in &out.services {
        for dependency in &service.value.depends_on {
            if dependency == name {
                return Err(StackError::new("dependency_cycle", format!("service '{name}' ({}) depends on itself", service.origin))
                    .hint("remove it from its own depends_on"));
            }
            if !out.services.contains_key(dependency) {
                let known: Vec<&str> = out.services.keys().map(String::as_str).collect();
                return Err(StackError::new(
                    "unknown_service",
                    format!("service '{name}' ({}) depends on '{dependency}', which no layer defines", service.origin),
                )
                .hint(format!("services: {}", known.join(", "))));
            }
        }
    }
    // Depth-first, in declaration order: a service met again while its own dependencies are
    // being walked closes a cycle.
    fn walk<'a>(out: &'a Composed, name: &'a str, path: &mut Vec<&'a str>, done: &mut Vec<&'a str>) -> Result<()> {
        if done.contains(&name) {
            return Ok(());
        }
        if let Some(start) = path.iter().position(|n| *n == name) {
            let cycle: Vec<&str> = path[start..].iter().copied().chain([name]).collect();
            return Err(StackError::new("dependency_cycle", format!("services depend on each other in a cycle: {}", cycle.join(" -> ")))
                .hint("remove one of these depends_on entries"));
        }
        path.push(name);
        for dependency in &out.services[name].value.depends_on {
            walk(out, dependency, path, done)?;
        }
        path.pop();
        done.push(name);
        Ok(())
    }
    let mut done = Vec::new();
    for name in out.services.keys() {
        walk(out, name, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

fn validate_task_services(out: &Composed) -> Result<()> {
    for (name, task) in &out.tasks {
        for service in &task.value.services {
            if !out.services.contains_key(service) {
                return Err(StackError::new(
                    "unknown_service",
                    format!("task '{name}' ({}) requires service '{service}', which no layer defines", task.origin),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn manifest(bin: &[&str]) -> BundleManifest {
        let bin: Vec<String> = bin.iter().map(|b| format!("{b:?}")).collect();
        toml::from_str(&format!("[bundle]\nname = 'b'\n[paths]\nbin = [{}]\n", bin.join(", "))).unwrap()
    }

    /// A bundle directory (canonical, as fetched) with `bin/`, `tools/bin/` and a file, beside a
    /// directory outside it.
    fn layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().canonicalize().unwrap().join("bundle");
        let outside = root.path().canonicalize().unwrap().join("outside");
        for d in [bundle.join("bin"), bundle.join("tools/bin"), outside.clone(), bundle.join(".stack/bin"), bundle.join(".git/hooks")] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(bundle.join("file"), "x").unwrap();
        symlink(&outside, bundle.join("out")).unwrap();
        symlink("tools", bundle.join("alias")).unwrap();
        (root, bundle, outside)
    }

    #[test]
    fn bin_paths_inside_the_bundle_are_kept() {
        let (_root, bundle, _) = layout();
        let loaded = LoadedBundle::new(manifest(&["bin", "./tools/bin", "tools/./bin"]), bundle.clone()).unwrap();
        let stack = compose(&[loaded], &ProjectManifest::default()).unwrap();
        assert_eq!(stack.bin_paths, [bundle.join("bin"), bundle.join("./tools/bin"), bundle.join("tools/./bin")]);
    }

    #[test]
    fn bin_paths_cannot_leave_the_bundle_or_reach_unhashed_content() {
        let (_root, bundle, outside) = layout();
        let outside_name = outside.file_name().unwrap().to_string_lossy().into_owned();
        for (bin, code, why) in [
            (format!("../{outside_name}"), "bundle_invalid", "'..'"),
            (format!("bin/../../{outside_name}"), "bundle_invalid", "'..'"),
            ("bin/..".to_string(), "bundle_invalid", "'..'"),
            (outside.display().to_string(), "bundle_invalid", "not a relative path"),
            ("out".to_string(), "bundle_invalid", "symbolic link"),
            ("alias/bin".to_string(), "bundle_invalid", "symbolic link"),
            (".stack/bin".to_string(), "bundle_invalid", "not bundle content"),
            ("tools/.git".to_string(), "bundle_invalid", "not bundle content"),
            (".git/hooks".to_string(), "bundle_invalid", "not bundle content"),
            (".".to_string(), "bundle_invalid", "bundle root"),
            ("".to_string(), "bundle_invalid", "bundle root"),
            ("missing".to_string(), "missing_path", "not a directory"),
            ("file".to_string(), "missing_path", "not a directory"),
        ] {
            let err = LoadedBundle::new(manifest(&[&bin]), bundle.clone()).err().unwrap_or_else(|| panic!("{bin:?} was accepted"));
            assert_eq!(err.code, code, "{bin:?}: {}", err.message);
            assert!(err.message.contains(why), "{bin:?}: {}", err.message);
        }
    }

    #[test]
    fn compose_rechecks_bin_paths_that_changed_after_loading() {
        let (_root, bundle, outside) = layout();
        let loaded = LoadedBundle::new(manifest(&["bin"]), bundle.clone()).unwrap();
        fs::remove_dir(bundle.join("bin")).unwrap();
        symlink(&outside, bundle.join("bin")).unwrap();
        let err = compose(&[loaded], &ProjectManifest::default()).unwrap_err();
        assert_eq!(err.code, "bundle_invalid", "{}", err.message);
    }
}

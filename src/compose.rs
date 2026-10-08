//! Merge bundles and the project into one stack.
//!
//! Rule: two layers may define the same key only if they agree. Any disagreement is a conflict
//! unless the project resolves it in `[override.*]`. Nothing is silently "last one wins".

use crate::error::{Result, StackError};
use crate::manifest::{BundleManifest, Overrides, ProjectManifest, Service, Task};
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;

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
    pub tools: IndexMap<String, Entry<String>>,
    pub env: IndexMap<String, Entry<String>>,
    pub services: IndexMap<String, Entry<Service>>,
    pub tasks: IndexMap<String, Entry<Task>>,
    pub bin_paths: Vec<PathBuf>,
    pub overrides: Vec<OverrideRecord>,
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
            let path = dir.join(bin);
            if !path.is_dir() || !path.starts_with(&dir) {
                return Err(StackError::new(
                    "missing_path",
                    format!("{origin} declares paths.bin '{bin}' but it is not a directory in the bundle"),
                ));
            }
        }
        Ok(Self { manifest, dir })
    }
}

pub fn compose(bundles: &[LoadedBundle], project: &ProjectManifest) -> Result<Composed> {
    let mut out = Composed::default();
    let mut conflicts = Vec::new();

    for b in bundles {
        let origin = format!("bundle:{}", b.manifest.bundle.name);
        merge("tools", &mut out.tools, &b.manifest.tools, &origin, &mut conflicts);
        merge("env", &mut out.env, &b.manifest.env, &origin, &mut conflicts);
        merge("services", &mut out.services, &b.manifest.services, &origin, &mut conflicts);
        merge("tasks", &mut out.tasks, &b.manifest.tasks, &origin, &mut conflicts);
        out.bin_paths
            .extend(b.manifest.paths.bin.iter().map(|p| b.dir.join(p)));
    }

    for (name, service) in &project.services {
        service.validate(name, "project")?;
    }
    merge("tools", &mut out.tools, &project.tools, "project", &mut conflicts);
    merge("env", &mut out.env, &project.env, "project", &mut conflicts);
    merge("services", &mut out.services, &project.services, "project", &mut conflicts);
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
        .hint(format!(
            "resolve explicitly in stack.toml, e.g. [override.{}] {} = ...",
            first.kind, first.key
        ))
        .details(details));
    }

    validate_task_services(&out)?;
    Ok(out)
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
    override_kind("tools", &mut out.tools, &o.tools, conflicts, &mut out.overrides);
    override_kind("env", &mut out.env, &o.env, conflicts, &mut out.overrides);
    override_kind("services", &mut out.services, &o.services, conflicts, &mut out.overrides);
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

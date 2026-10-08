//! `bundle.toml` (a shareable definition) and `stack.toml` (a project that uses bundles).

use crate::error::{Result, StackError};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const BUNDLE_FILE: &str = "bundle.toml";
pub const PROJECT_FILE: &str = "stack.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleManifest {
    pub bundle: BundleMeta,
    #[serde(default)]
    /// Version requests, as written: a string or a table (see `tool::ToolSpec::parse`).
    pub tools: IndexMap<String, toml::Value>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub services: IndexMap<String, Service>,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
    #[serde(default)]
    pub paths: Paths,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleMeta {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Paths {
    /// Directories inside the bundle to put on PATH.
    #[serde(default)]
    pub bin: Vec<String>,
}

/// A service definition. Either a provider `preset` or a custom `run` command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_port: Option<u16>,
    /// `"auto"` (default) or a fixed port. Fixed ports belong to projects, not bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<toml::Value>,
    /// Opt-in instance check for services without a built-in one. Without it a service is
    /// verified for liveness only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<IdentityProbe>,
    /// Files or directories the running process loaded, relative to the project. When any
    /// changes after the service started, checks report it so the service can be restarted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<String>,
}

/// A command that asks the running service, through the app's own connection settings, which
/// instance it is. It must print exactly the instance token stack gave this checkout's service
/// (`$STACK_IDENTITY_<NAME>` in the service's environment), and nothing else.
///
/// Probes are trusted code from the bundle, like `run`; they are bounded, not sandboxed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityProbe {
    /// Run with `sh -c` in the project directory, with the stack's env minus identity tokens.
    pub command: String,
    /// Default 5s, at most 30s. The command's process group is killed at the deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
}

pub const IDENTITY_TIMEOUT_DEFAULT: u64 = 5;
pub const IDENTITY_TIMEOUT_MAX: u64 = 30;

impl IdentityProbe {
    pub fn timeout_secs(&self) -> u64 {
        self.timeout
            .as_deref()
            .and_then(|t| crate::mcp::parse_duration(t).ok())
            .unwrap_or(IDENTITY_TIMEOUT_DEFAULT)
    }
}

/// The variable through which a service learns its instance token.
pub fn identity_var(service: &str) -> String {
    format!(
        "STACK_IDENTITY_{}",
        service
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
            .collect::<String>()
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Services that must be running and ready before the task starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    #[serde(rename = "use", default)]
    pub uses: Vec<UseEntry>,
    #[serde(default)]
    /// Version requests, as written: a string or a table (see `tool::ToolSpec::parse`).
    pub tools: IndexMap<String, toml::Value>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub services: IndexMap<String, Service>,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
    /// Explicit resolutions. The only way to replace a value another layer defines.
    #[serde(rename = "override", default)]
    pub overrides: Overrides,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseEntry {
    /// `git+<url>?ref=<ref>` or `path:<dir>`
    pub bundle: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default)]
    /// Version requests, as written: a string or a table (see `tool::ToolSpec::parse`).
    pub tools: IndexMap<String, toml::Value>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub services: IndexMap<String, Service>,
    #[serde(default)]
    pub tasks: IndexMap<String, Task>,
}

impl Service {
    pub fn validate(&self, name: &str, origin: &str) -> Result<()> {
        if self.preset.is_some() == self.run.is_some() {
            return Err(StackError::new(
                "invalid_service",
                format!("service '{name}' in {origin} must set exactly one of `preset` or `run`"),
            ));
        }
        if let Some(probe) = &self.identity {
            if matches!(self.preset.as_deref(), Some("postgres" | "redis")) {
                return Err(StackError::new(
                    "invalid_service",
                    format!("service '{name}' in {origin} sets `identity`, but its preset already has a built-in instance check"),
                ));
            }
            if probe.command.trim().is_empty() {
                return Err(StackError::new(
                    "invalid_service",
                    format!("service '{name}' in {origin} has an empty identity command"),
                ));
            }
            if let Some(t) = &probe.timeout {
                let secs = crate::mcp::parse_duration(t).map_err(|e| {
                    StackError::new("invalid_service", format!("service '{name}' in {origin}: identity timeout: {}", e.message))
                })?;
                if !(1..=IDENTITY_TIMEOUT_MAX).contains(&secs) {
                    return Err(StackError::new(
                        "invalid_service",
                        format!("service '{name}' in {origin}: identity timeout must be 1s to {IDENTITY_TIMEOUT_MAX}s"),
                    ));
                }
            }
        }
        match &self.port {
            None => {}
            Some(toml::Value::String(s)) if s == "auto" => {}
            Some(toml::Value::Integer(p)) if (1..=65535).contains(p) => {}
            Some(other) => {
                return Err(StackError::new(
                    "invalid_service",
                    format!("service '{name}' in {origin} has port {other}; use \"auto\" or 1-65535"),
                ));
            }
        }
        Ok(())
    }

    /// The pinned port, if any. Only meaningful after `validate`.
    pub fn fixed_port(&self) -> Option<u16> {
        match &self.port {
            Some(toml::Value::Integer(p)) => u16::try_from(*p).ok(),
            _ => None,
        }
    }
}

pub fn read_project(root: &Path) -> Result<ProjectManifest> {
    let path = root.join(PROJECT_FILE);
    let text = fs::read_to_string(&path).map_err(|e| {
        StackError::new("manifest_missing", format!("cannot read {}: {e}", path.display()))
            .hint("create a stack.toml; see README.md for the format")
    })?;
    parse(&text).map_err(|e| e.into_error("manifest_invalid", &path, None))
}

pub fn read_bundle(dir: &Path, source: &str) -> Result<BundleManifest> {
    let path = dir.join(BUNDLE_FILE);
    let text = fs::read_to_string(&path).map_err(|e| {
        StackError::new("bundle_invalid", format!("{source} has no readable {BUNDLE_FILE}: {e}"))
    })?;
    parse(&text).map_err(|e| e.into_error("bundle_invalid", &path, Some(source)))
}

/// A TOML document that failed to parse or to match its schema, located in its text.
#[derive(Debug)]
struct ParseFailure {
    message: String,
    at: Option<Location>,
}

#[derive(Debug, PartialEq)]
struct Location {
    /// 1-based line.
    line: usize,
    /// 1-based, in characters rather than bytes, as editors count.
    column: usize,
    /// The line, then a caret under the column.
    excerpt: String,
}

/// Longest stretch of one line an excerpt shows; longer lines are cut around the column.
const EXCERPT_WIDTH: usize = 100;

fn parse<T: serde::de::DeserializeOwned>(text: &str) -> std::result::Result<T, ParseFailure> {
    toml::from_str(text).map_err(|e| {
        let message = e.message().trim_end().to_string();
        // Only a key missing from the document itself has no place in the text to show; it is
        // reported at an empty span. Every other error, syntax errors included, is located,
        // even one spanning the whole document: a key missing from a table points at the table.
        let whole_document = message.starts_with("missing field ") && e.span().is_some_and(|span| span.is_empty());
        ParseFailure { at: e.span().filter(|_| !whole_document).map(|span| locate(text, span.start)), message }
    })
}

impl ParseFailure {
    /// `<file>:<line>:<column>: <message>` and the excerpt. A bundle names its `use` source
    /// first. The details repeat the location for programs.
    fn into_error(self, code: &'static str, file: &Path, source: Option<&str>) -> StackError {
        let origin = source.map(|s| format!("{s}: ")).unwrap_or_default();
        let mut detail = serde_json::json!({ "file": file });
        if let Some(source) = source {
            detail["source"] = source.into();
        }
        let message = match &self.at {
            Some(at) => {
                detail["line"] = at.line.into();
                detail["column"] = at.column.into();
                format!("{origin}{}:{}:{}: {}\n{}", file.display(), at.line, at.column, self.message, at.excerpt)
            }
            None => format!("{origin}{}: {}", file.display(), self.message),
        };
        StackError::new(code, message).with_detail(detail)
    }
}

/// Where byte `offset` of `text` is. Offsets past the end, or inside a character, are moved
/// back to the nearest character start.
fn locate(text: &str, offset: usize) -> Location {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
    let line = text[..start].matches('\n').count() + 1;
    let before = text[start..offset].chars().count();
    let row: Vec<char> = text[start..end].trim_end_matches('\r').chars().collect();

    // A window of the line that contains the column, marked where it was cut.
    let from = before.saturating_sub(EXCERPT_WIDTH / 2).min(row.len().saturating_sub(EXCERPT_WIDTH));
    let to = (from + EXCERPT_WIDTH).min(row.len());
    let cut_before = if from > 0 { "…" } else { "" };
    let cut_after = if to < row.len() { "…" } else { "" };
    let shown: String = row[from..to].iter().collect();
    // Tabs are kept so the caret lines up however the terminal renders them.
    let pad: String = cut_before
        .chars()
        .chain(row[from..before.min(row.len())].iter().copied())
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let number = line.to_string();
    let gutter = " ".repeat(number.len());
    Location {
        line,
        column: before + 1,
        excerpt: format!("{gutter} |\n{number} | {cut_before}{shown}{cut_after}\n{gutter} | {pad}^"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(text: &str) -> ParseFailure {
        parse::<ProjectManifest>(text).unwrap_err()
    }

    #[test]
    fn syntax_errors_name_the_line_and_column_under_a_caret() {
        let f = failure("[env]\nA = '1'\n[services.api\nrun = 'x'\n");
        let at = f.at.unwrap();
        assert_eq!((at.line, at.column), (3, 14));
        assert_eq!(at.excerpt, "  |\n3 | [services.api\n  |              ^");
        assert!(f.message.contains("invalid table header"), "{}", f.message);
    }

    #[test]
    fn schema_errors_point_at_the_offending_key() {
        let f = failure("[services.w]\nrun = 'x'\nrnu = 'y'\n");
        let at = f.at.unwrap();
        assert_eq!((at.line, at.column), (3, 1), "{}", at.excerpt);
        assert!(f.message.contains("unknown field `rnu`"), "{}", f.message);
    }

    #[test]
    fn errors_about_the_whole_document_have_no_location() {
        let f = parse::<BundleManifest>("[tools]\npython = '3'\n").unwrap_err();
        assert!(f.message.contains("missing field `bundle`") && f.at.is_none(), "{f:?}");
        let e = f.into_error("bundle_invalid", Path::new("/b/bundle.toml"), Some("path:../b"));
        assert_eq!(e.message, "path:../b: /b/bundle.toml: missing field `bundle`");
    }

    #[test]
    fn errors_at_the_start_of_a_short_document_are_located_in_projects_and_bundles() {
        for (text, message, column) in [
            ("@", "invalid key", 1),
            ("@\n", "invalid key", 1),
            ("[", "invalid table header", 1),
            ("x = 1", "unknown field `x`", 1),
        ] {
            for (kind, f) in [("project", failure(text)), ("bundle", parse::<BundleManifest>(text).unwrap_err())] {
                assert!(f.message.contains(message), "{kind} {text:?}: {f:?}");
                let at = f.at.as_ref().unwrap_or_else(|| panic!("{kind} {text:?} lost its location: {f:?}"));
                assert_eq!((at.line, at.column), (1, column), "{kind} {text:?}");
                assert_eq!(at.excerpt, format!("  |\n1 | {}\n  | ^", text.trim_end()), "{kind} {text:?}");
                let path = Path::new("/p/stack.toml");
                let e = f.into_error("manifest_invalid", path, None);
                assert!(e.message.starts_with("/p/stack.toml:1:1: "), "{kind} {text:?}: {}", e.message);
                assert_eq!((e.details[0]["line"].as_u64(), e.details[0]["column"].as_u64()), (Some(1), Some(1)));
            }
        }
    }

    #[test]
    fn a_key_missing_from_a_table_points_at_the_table() {
        for text in ["[tasks.t]\n", "[[use]]\n"] {
            let f = failure(text);
            assert!(f.message.starts_with("missing field"), "{text:?}: {f:?}");
            assert_eq!(f.at.map(|at| (at.line, at.column)), Some((1, 1)), "{text:?}");
        }
        let f = parse::<BundleManifest>("[bundle]\nversion = '1'\n").unwrap_err();
        assert!(f.message.contains("missing field `name`") && f.at.is_some(), "{f:?}");
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let f = failure("[env]\nNAME = \"ünï✓\" x\n");
        let at = f.at.unwrap();
        assert_eq!((at.line, at.column), (2, 15), "{at:?}");
        assert_eq!(at.excerpt, format!("  |\n2 | NAME = \"ünï✓\" x\n  | {}^", " ".repeat(14)));
    }

    #[test]
    fn offsets_inside_a_character_or_past_the_end_still_locate() {
        let text = "a = 1\nb = \"ü\"";
        let inside = text.find('ü').unwrap() + 1;
        assert_eq!(locate(text, inside).column, 6);
        let end = locate(text, text.len() + 10);
        assert_eq!((end.line, end.column), (2, 8));
        assert_eq!(locate("", 0).excerpt, "  |\n1 | \n  | ^");
        assert_eq!(locate("x\r\ny", 1).excerpt, "  |\n1 | x\n  |  ^");
    }

    #[test]
    fn tabs_stay_under_the_caret_and_long_lines_are_cut_around_it() {
        assert_eq!(locate("\tk = ?", 5).excerpt, "  |\n1 | \tk = ?\n  | \t    ^");
        let long = format!("k = \"{}\" ?", "x".repeat(300));
        let at = locate(&long, long.len() - 1);
        let lines: Vec<&str> = at.excerpt.lines().collect();
        assert!(lines[1].starts_with("1 | …x") && lines[1].ends_with("\" ?"), "{}", at.excerpt);
        assert_eq!(lines[1].chars().count(), "1 | …".chars().count() + EXCERPT_WIDTH);
        // The caret is under the last character shown.
        assert_eq!(lines[2].chars().count(), lines[1].chars().count());
        assert_eq!(at.column, long.chars().count());
    }

    #[test]
    fn errors_keep_their_code_and_name_the_file_and_source() {
        let e = failure("[x\n").into_error("bundle_invalid", Path::new("/b/bundle.toml"), Some("path:../b"));
        assert_eq!(e.code, "bundle_invalid");
        assert!(e.message.starts_with("path:../b: /b/bundle.toml:1:3: "), "{}", e.message);
        assert_eq!(e.details[0]["line"], 1);
        assert_eq!(e.details[0]["column"], 3);
        assert_eq!(e.details[0]["source"], "path:../b");
    }
}

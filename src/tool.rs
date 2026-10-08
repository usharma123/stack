//! Tool requests: a version and the provider options stack allows for that tool.
//!
//! `tools.<name>` is a version request (`"1.93"`) or a table with `version` and options from a
//! typed allowlist (`{ version = "1.93", mr_boxington = true }`). Both forms canonicalise to one
//! [`ToolSpec`], so `"1.93"` and `{ version = "1.93" }` are the same value for composition,
//! locking and rendering. The allowlist keeps stack's configuration contract from absorbing
//! every installation and trust setting the provider has; extending it is a code change.

use crate::error::{Result, StackError};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;

/// One option value. Options are scalars; tables and arrays are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OptionValue {
    Bool(bool),
    String(String),
}

impl OptionValue {
    pub fn to_toml(&self) -> toml::Value {
        match self {
            Self::Bool(b) => toml::Value::Boolean(*b),
            Self::String(s) => toml::Value::String(s.clone()),
        }
    }
}

/// Options in a canonical (sorted) order, so equal requests compare and serialise equal.
pub type ToolOptions = BTreeMap<String, OptionValue>;

/// A tool request in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// The version request (`3.13`, `latest`, `system`), or an exact release once pinned.
    pub version: String,
    pub options: ToolOptions,
}

/// A plain string when there are no options, so stacks without options read and digest as
/// they always did; otherwise a table with `version` first.
impl Serialize for ToolSpec {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        if self.options.is_empty() {
            return s.serialize_str(&self.version);
        }
        let mut map = s.serialize_map(Some(self.options.len() + 1))?;
        map.serialize_entry("version", &self.version)?;
        for (key, value) in &self.options {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl ToolSpec {
    pub fn new(version: impl Into<String>) -> Self {
        Self { version: version.into(), options: ToolOptions::new() }
    }

    /// The same options at another version (an exact release in place of a request).
    pub fn at(&self, version: impl Into<String>) -> Self {
        Self { version: version.into(), options: self.options.clone() }
    }

    /// `tools.<name>` as written in `origin` (`project`, `bundle:<name>`, `[override.tools]`).
    pub fn parse(name: &str, value: &toml::Value, origin: &str) -> Result<Self> {
        let invalid = |why: String| {
            StackError::new("invalid_tool", format!("tools.{name} in {origin}: {why}"))
                .hint(accepted(name))
                .with_detail(serde_json::json!({ "tool": name, "origin": origin }))
        };
        let table = match value {
            toml::Value::String(version) if templated(version) => return Err(invalid(TEMPLATE.into())),
            toml::Value::String(version) => return Ok(Self::new(version.clone())),
            toml::Value::Table(table) => table,
            other => {
                return Err(invalid(format!(
                    "expected a version string or a table with `version`, found {}",
                    other.type_str()
                )))
            }
        };
        let version = match table.get("version") {
            Some(toml::Value::String(v)) if templated(v) => return Err(invalid(TEMPLATE.into())),
            Some(toml::Value::String(v)) if !v.trim().is_empty() => v.clone(),
            Some(toml::Value::String(_)) => return Err(invalid("`version` is empty".into())),
            Some(other) => return Err(invalid(format!("`version` must be a string, found {}", other.type_str()))),
            None => return Err(invalid("a table must set `version`".into())),
        };
        let mut options = ToolOptions::new();
        for (key, value) in table.iter().filter(|(k, _)| *k != "version") {
            let Some(allowed) = ALLOWED.iter().find(|a| a.key == key) else {
                return Err(invalid(format!("option `{key}` is not supported")));
            };
            if !(allowed.eligible)(name) {
                return Err(invalid(format!("option `{key}` {}", allowed.applies_to)));
            }
            let value = match (allowed.kind, value) {
                (Kind::Bool, toml::Value::Boolean(b)) => OptionValue::Bool(*b),
                (Kind::String, toml::Value::String(s)) => {
                    if templated(s) {
                        return Err(invalid(format!("option `{key}` {TEMPLATE}")));
                    }
                    if let Some(why) = (allowed.check)(s) {
                        return Err(invalid(format!("option `{key}` {why}")));
                    }
                    OptionValue::String(s.clone())
                }
                (kind, other) => {
                    return Err(invalid(format!("option `{key}` must be a {}, found {}", kind.name(), other.type_str())))
                }
            };
            options.insert(key.clone(), value);
        }
        Ok(Self { version, options })
    }

    /// The provider config value: the version alone, or an inline table with the options.
    pub fn to_toml(&self) -> toml::Value {
        if self.options.is_empty() {
            return toml::Value::String(self.version.clone());
        }
        let mut table = toml::Table::new();
        table.insert("version".into(), toml::Value::String(self.version.clone()));
        for (key, value) in &self.options {
            table.insert(key.clone(), value.to_toml());
        }
        toml::Value::Table(table)
    }

    /// Packslip trust options on a registry name: valid only if mise's registry installs the
    /// tool through packslip, which compile asks mise before resolving.
    pub fn needs_packslip_backend(&self, name: &str) -> bool {
        is_registry_name(name) && self.has_packslip_options()
    }

    pub fn has_packslip_options(&self) -> bool {
        self.options.keys().any(|k| PACKSLIP_OPTIONS.contains(&k.as_str()))
    }

    /// `mr_boxington = true`: mise publishes a Cargo wrapper that builds through Mr Boxington.
    pub fn mr_boxington(&self) -> bool {
        self.options.get(MR_BOXINGTON) == Some(&OptionValue::Bool(true))
    }
}

pub const MR_BOXINGTON: &str = "mr_boxington";

/// mise renders tool versions and option strings as templates whenever it loads a config,
/// `exec()` included, so a value with template syntax would run commands wherever stack asks
/// mise about the tool (resolution, discovery), not only where the project's config is used.
const TEMPLATE: &str = "must not contain template syntax (`{{`, `{%` or `{#`)";

/// Text mise would render as a template.
pub fn templated(value: &str) -> bool {
    ["{{", "{%", "{#"].iter().any(|t| value.contains(t))
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Bool,
    String,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Bool => "boolean",
            Kind::String => "string",
        }
    }
}

/// One allowlisted option.
struct Allowed {
    key: &'static str,
    kind: Kind,
    eligible: fn(&str) -> bool,
    /// Completes "option `<key>` ..." when the tool is not eligible.
    applies_to: &'static str,
    /// Why a string value is refused, if it is.
    check: fn(&str) -> Option<&'static str>,
}

/// The v1 allowlist. Extending it is a code change with a test, documented in commands.md.
const ALLOWED: &[Allowed] = &[
    Allowed { key: MR_BOXINGTON, kind: Kind::Bool, eligible: is_rust, applies_to: "applies only to `rust`", check: |_| None },
    Allowed { key: "pubkey", kind: Kind::String, eligible: may_be_packslip, applies_to: PACKSLIP_ONLY, check: minisign_key },
    Allowed { key: "identity", kind: Kind::String, eligible: may_be_packslip, applies_to: PACKSLIP_ONLY, check: nonempty },
    Allowed { key: "identity_prefix", kind: Kind::String, eligible: may_be_packslip, applies_to: PACKSLIP_ONLY, check: nonempty },
    Allowed { key: "issuer", kind: Kind::String, eligible: may_be_packslip, applies_to: PACKSLIP_ONLY, check: nonempty },
];

const PACKSLIP_ONLY: &str = "applies only to tools installed through mise's packslip backend (`packslip:<host>/<owner>/<repo>`, or a registry name whose backend is packslip)";

/// The Rust toolchain as mise names it.
pub fn is_rust(name: &str) -> bool {
    matches!(name, "rust" | "core:rust")
}

/// A tool named with mise's packslip backend (`packslip:<host>/<owner>/<repo>`).
pub fn is_packslip(name: &str) -> bool {
    name.strip_prefix("packslip:").is_some_and(|rest| !rest.is_empty())
}

/// A registry short name (`fnox`): mise's registry decides its backend.
pub fn is_registry_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(':')
}

/// Packslip trust options may apply: the name says packslip, or it is a registry name whose
/// backend only mise can tell (see [`ToolSpec::needs_packslip_backend`]).
fn may_be_packslip(name: &str) -> bool {
    is_packslip(name) || is_registry_name(name)
}

/// The packslip signer policy options in the allowlist.
pub const PACKSLIP_OPTIONS: &[&str] = &["pubkey", "identity", "identity_prefix", "issuer"];

/// Mr Boxington itself (`mbx`, `mr-boxington`, or a backend-qualified `.../mr-boxington`),
/// which mise requires among the active tools for `mr_boxington` to take effect.
pub fn is_mr_boxington(name: &str) -> bool {
    matches!(name.rsplit([':', '/']).next(), Some("mbx" | "mr-boxington"))
}

fn nonempty(value: &str) -> Option<&'static str> {
    value.trim().is_empty().then_some("must not be empty")
}

/// A minisign public key line: base64 of the `Ed` algorithm tag, an 8-byte key id and a 32-byte
/// key (42 bytes, 56 characters, so always starting `RW`). mise also accepts a path to a `.pub`
/// file, but a path resolved against an unspecified directory does not pin the key's contents.
fn minisign_key(value: &str) -> Option<&'static str> {
    let base64 = |b: u8| b.is_ascii_alphanumeric() || b == b'+' || b == b'/';
    let ok = value.len() == 56 && value.starts_with("RW") && value.bytes().all(base64);
    (!ok).then_some("must be the literal minisign public key line (56 base64 characters starting `RW`), not a path or a whole .pub file")
}

/// What `tools.<name>` accepts, for hints.
pub fn accepted(name: &str) -> String {
    let options: Vec<String> = ALLOWED
        .iter()
        .filter(|a| (a.eligible)(name))
        .map(|a| format!("`{}` ({})", a.key, a.kind.name()))
        .collect();
    if options.is_empty() {
        format!("tools.{name} accepts a version string or {{ version = \"...\" }}; no options are supported for this tool")
    } else {
        let note = if is_registry_name(name) && may_be_packslip(name) {
            "; the packslip options only where mise's registry installs it through packslip"
        } else {
            ""
        };
        format!("tools.{name} accepts a version string or {{ version = \"...\" }} with {}{note}", options.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    fn parse(name: &str, text: &str) -> Result<ToolSpec> {
        let doc: toml::Table = toml::from_str(&format!("v = {text}")).unwrap();
        ToolSpec::parse(name, &doc["v"], "project")
    }

    #[test]
    fn string_and_optionless_table_are_the_same_value() {
        let a = parse("jq", "\"1.7\"").unwrap();
        let b = parse("jq", "{ version = \"1.7\" }").unwrap();
        assert_eq!(a, b);
        assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap());
        assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::json!("1.7"));
        assert_eq!(a.to_toml(), toml::Value::String("1.7".into()));
    }

    #[test]
    fn rust_accepts_mr_boxington_as_a_boolean() {
        let spec = parse("rust", "{ version = \"1.93\", mr_boxington = true }").unwrap();
        assert!(spec.mr_boxington());
        assert_eq!(serde_json::to_value(&spec).unwrap(), serde_json::json!({ "version": "1.93", "mr_boxington": true }));
        assert_eq!(spec.at("1.93.1").to_toml().to_string(), "{ version = \"1.93.1\", mr_boxington = true }");
        assert!(!parse("core:rust", "{ version = \"1.93\", mr_boxington = false }").unwrap().mr_boxington());
    }

    #[test]
    fn packslip_tools_accept_trust_options() {
        let spec = parse(
            "packslip:example.com/acme/tool",
            &format!("{{ version = \"1\", pubkey = \"{KEY}\", identity = \"a\", identity_prefix = \"b\", issuer = \"c\" }}"),
        )
        .unwrap();
        assert_eq!(spec.options.len(), 4);
        // Canonical order is sorted, whatever order the table used.
        let keys: Vec<&str> = spec.options.keys().map(String::as_str).collect();
        assert_eq!(keys, ["identity", "identity_prefix", "issuer", "pubkey"]);
        assert!(!spec.needs_packslip_backend("packslip:example.com/acme/tool"));
        // A registry name is accepted here; compile asks mise which backend it has.
        let fnox = parse("fnox", "{ version = \"1.39.0\", identity = \"https://ci/x\" }").unwrap();
        assert!(fnox.needs_packslip_backend("fnox"));
        assert!(!parse("fnox", "\"1.39.0\"").unwrap().needs_packslip_backend("fnox"));
    }

    #[test]
    fn everything_else_is_invalid_tool_with_a_hint() {
        for (name, text, why) in [
            ("jq", "{ version = \"1\", mr_boxington = true }", "applies only to `rust`"),
            ("rust", "{ version = \"1\", pin = \"x\" }", "`pin` is not supported"),
            ("rust", "{ version = \"1\", mr_boxington = \"yes\" }", "must be a boolean, found string"),
            ("rust", "{ mr_boxington = true }", "must set `version`"),
            ("rust", "{ version = 1 }", "`version` must be a string"),
            ("rust", "{ version = \"\" }", "`version` is empty"),
            ("rust", "{ version = \"1\", mr_boxington = { a = 1 } }", "must be a boolean, found table"),
            ("packslip:x.dev/a/b", "{ version = \"1\", identity = [\"a\"] }", "must be a string, found array"),
            ("packslip:x.dev/a/b", "{ version = \"1\", pubkey = \"./keys/tool.pub\" }", "literal minisign public key"),
            ("packslip:x.dev/a/b", "{ version = \"1\", issuer = \" \" }", "must not be empty"),
            ("github:jdx/fnox", "{ version = \"1\", identity = \"a\" }", "packslip backend"),
            ("aqua:jqlang/jq", "{ version = \"1\", pubkey = \"x\" }", "packslip backend"),
            ("jq", "3", "found integer"),
            ("jq", "\"{{ exec(command='touch x') }}\"", "template syntax"),
            ("jq", "{ version = \"{% if true %}1{% endif %}\" }", "template syntax"),
            ("fnox", "{ version = \"1\", identity = \"{{ exec(command='touch x') }}\" }", "option `identity` must not contain template syntax"),
            ("packslip:x.dev/a/b", "{ version = \"1\", issuer = \"{# c #}\" }", "template syntax"),
            ("jq", "[\"1\"]", "found array"),
        ] {
            let e = parse(name, text).unwrap_err();
            assert_eq!(e.code, "invalid_tool", "{name} {text}");
            assert!(e.message.contains(why), "{name} {text}: {}", e.message);
            assert!(e.message.starts_with(&format!("tools.{name} in project: ")), "{}", e.message);
            assert!(e.hint.as_deref().unwrap().starts_with(&format!("tools.{name} accepts")), "{e:?}");
        }
        assert!(accepted("rust").contains("`mr_boxington` (boolean)"));
        assert!(accepted("github:jdx/fnox").contains("no options"));
        assert!(accepted("fnox").contains("`identity` (string)") && !accepted("fnox").contains("mr_boxington"));
        assert!(accepted("packslip:x.dev/a/b").contains("`pubkey` (string)"));
    }

    #[test]
    fn mr_boxington_is_recognised_by_any_name_mise_uses() {
        for name in ["mbx", "mr-boxington", "packslip:github.com/jdx/mr-boxington", "github:jdx/mr-boxington", "aqua:jdx/mbx"] {
            assert!(is_mr_boxington(name), "{name}");
        }
        for name in ["rust", "mbx-extra", "cargo:mbxd"] {
            assert!(!is_mr_boxington(name), "{name}");
        }
    }
}

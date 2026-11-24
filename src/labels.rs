//! Session labels: `KEY=VALUE` pairs `stack up --label` records on a checkout's session, which
//! `ps`, `status`, `logs` and `down` select checkouts by.
//!
//! A key is 1 to 64 ASCII letters, digits, `-`, `_`, `.` or `/` (so `team/owner` and
//! `agent.task` read naturally); a value is at most 256 characters, none of them a control
//! character. Both are checked wherever a label enters Stack (the CLI's `--label`, MCP's
//! `labels`), so a recorded label is always one that can be printed on one line.

use crate::error::{Result, StackError};
use serde_json::Value;
use std::collections::BTreeMap;

/// Labels by key, in key order.
pub type Labels = BTreeMap<String, String>;

pub const MAX_KEY: usize = 64;
pub const MAX_VALUE: usize = 256;

const HINT: &str = "give each label as KEY=VALUE: a key of 1-64 ASCII letters, digits, '-', '_', '.' or '/', and a value of at most 256 characters without control characters, e.g. --label agent=claude --label task=api-42";

/// Why `key` is not a label key, if it is not.
pub fn check_key(key: &str) -> Result<()> {
    let ok = (1..=MAX_KEY).contains(&key.len()) && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'));
    if ok {
        Ok(())
    } else {
        Err(StackError::new("usage", format!("invalid label key {key:?}")).hint(HINT))
    }
}

/// Why `value` is not a label value, if it is not.
pub fn check_value(key: &str, value: &str) -> Result<()> {
    if value.chars().count() > MAX_VALUE {
        return Err(StackError::new("usage", format!("label {key:?} has a value longer than {MAX_VALUE} characters")).hint(HINT));
    }
    if value.chars().any(char::is_control) {
        return Err(StackError::new("usage", format!("label {key:?} has a value with a control character")).hint(HINT));
    }
    Ok(())
}

/// One `KEY=VALUE` argument. The value may be empty and may itself contain `=`.
pub fn parse(arg: &str) -> Result<(String, String)> {
    let Some((key, value)) = arg.split_once('=') else {
        return Err(StackError::new("usage", format!("invalid label {arg:?}: expected KEY=VALUE")).hint(HINT));
    };
    check_key(key)?;
    check_value(key, value)?;
    Ok((key.to_string(), value.to_string()))
}

/// Every `--label` argument; a key given twice keeps its last value, as a later flag overrides
/// an earlier one.
pub fn parse_all(args: &[String]) -> Result<Labels> {
    args.iter().map(|a| parse(a)).collect()
}

/// MCP's `labels`: an object of strings (omitted or `null` is none).
pub fn from_json(value: &Value) -> Result<Labels> {
    match value {
        Value::Null => Ok(Labels::new()),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                let v = v.as_str().ok_or_else(|| StackError::new("usage", format!("label {k:?} must have a string value, not {v}")).hint(HINT))?;
                check_key(k)?;
                check_value(k, v)?;
                Ok((k.clone(), v.to_string()))
            })
            .collect(),
        other => Err(StackError::new("usage", format!("`labels` must be an object of strings, not {other}")).hint(HINT)),
    }
}

/// Whether `have` holds every pair of `want` (any labels match an empty `want`).
pub fn matches(have: &Labels, want: &Labels) -> bool {
    want.iter().all(|(k, v)| have.get(k) == Some(v))
}

/// `a=1,b=2`, for people.
pub fn shown(labels: &Labels) -> String {
    labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_and_values_are_checked_where_a_label_is_given() {
        assert_eq!(parse("agent=claude").unwrap(), ("agent".into(), "claude".into()));
        assert_eq!(parse("team/owner.v2_x-y=").unwrap(), ("team/owner.v2_x-y".into(), String::new()), "an empty value is a value");
        assert_eq!(parse("url=a=b").unwrap(), ("url".into(), "a=b".into()), "only the first = splits");
        assert_eq!(parse(&format!("{}=v", "k".repeat(64))).unwrap().0.len(), 64);
        for bad in ["agent", "=v", "sp ace=v", "ü=v", "a\tb=v", "a:b=v"] {
            let e = parse(bad).unwrap_err();
            assert_eq!(e.code, "usage", "{bad}");
            assert!(e.hint.as_deref().unwrap().contains("KEY=VALUE"), "{bad}");
        }
        assert!(parse("agent").unwrap_err().message.contains("expected KEY=VALUE"));
        assert_eq!(parse(&format!("{}=v", "k".repeat(65))).unwrap_err().code, "usage");
        assert!(parse(&format!("k={}", "v".repeat(256))).is_ok());
        assert!(parse(&format!("k={}", "v".repeat(257))).unwrap_err().message.contains("longer than 256"));
        assert!(parse("k=line\nbreak").unwrap_err().message.contains("control character"));
        assert!(parse("k=ünïcode ok").is_ok(), "values may be any printable text");
    }

    #[test]
    fn a_later_label_overrides_an_earlier_one_with_the_same_key() {
        let labels = parse_all(&["a=1".into(), "b=2".into(), "a=3".into()]).unwrap();
        assert_eq!(labels, Labels::from([("a".into(), "3".into()), ("b".into(), "2".into())]));
        assert!(parse_all(&["a=1".into(), "nope".into()]).is_err());
        assert_eq!(shown(&labels), "a=3,b=2");
    }

    #[test]
    fn mcp_labels_are_an_object_of_strings() {
        assert_eq!(from_json(&json!(null)).unwrap(), Labels::new());
        assert_eq!(from_json(&json!({ "agent": "claude" })).unwrap(), Labels::from([("agent".into(), "claude".into())]));
        for bad in [json!({ "agent": 1 }), json!({ "bad key": "v" }), json!(["a=b"]), json!("a=b")] {
            assert_eq!(from_json(&bad).unwrap_err().code, "usage", "{bad}");
        }
    }

    #[test]
    fn a_selection_matches_checkouts_holding_every_pair() {
        let have = Labels::from([("agent".into(), "claude".into()), ("task".into(), "api".into())]);
        assert!(matches(&have, &Labels::new()));
        assert!(matches(&have, &Labels::from([("agent".into(), "claude".into())])));
        assert!(matches(&have, &have.clone()));
        assert!(!matches(&have, &Labels::from([("agent".into(), "codex".into())])));
        assert!(!matches(&have, &Labels::from([("owner".into(), "x".into())])));
        assert!(!matches(&Labels::new(), &Labels::from([("agent".into(), "claude".into())])));
    }
}

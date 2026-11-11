//! The output contract shared by `--json` and `stack mcp`: the envelope every call returns.

use crate::error::StackError;
use serde_json::{Map, Value};
use std::time::Instant;

/// Version of the envelope and result shapes. Raised on an incompatible change.
pub const SCHEMA: u32 = 1;

/// One call being answered: what it was and since when.
#[derive(Debug, Clone)]
pub struct Call {
    /// The operation: a CLI subcommand, or an MCP tool's name without `stack_`.
    pub command: String,
    pub started: Instant,
}

impl Call {
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into(), started: Instant::now() }
    }

    fn head(&self, ok: bool, passed: bool) -> Map<String, Value> {
        let mut head = Map::new();
        head.insert("ok".into(), ok.into());
        head.insert("passed".into(), passed.into());
        head.insert("command".into(), self.command.clone().into());
        head.insert("stack_version".into(), env!("CARGO_PKG_VERSION").into());
        head.insert("schema".into(), SCHEMA.into());
        head.insert("duration_ms".into(), (self.started.elapsed().as_millis() as u64).into());
        head
    }

    /// `{ok: true, passed, command, stack_version, schema, duration_ms, data}`.
    pub fn success(&self, data: Value) -> Value {
        let mut envelope = self.head(true, passed(&self.command, &data));
        envelope.insert("data".into(), data);
        Value::Object(envelope)
    }

    /// `{ok: false, passed: false, command, stack_version, schema, duration_ms, error}`.
    pub fn failure(&self, e: &StackError) -> Value {
        let mut envelope = self.head(false, false);
        envelope.insert("error".into(), serde_json::to_value(e).expect("errors serialize"));
        Value::Object(envelope)
    }

    pub fn outcome(&self, result: &crate::error::Result<Value>) -> Value {
        match result {
            Ok(data) => self.success(data.clone()),
            Err(e) => self.failure(e),
        }
    }
}

/// Whether what was asked for is in the wanted state: a command that ran exited 0, a check
/// found everything healthy. Everything else that succeeded passed.
pub fn passed(command: &str, data: &Value) -> bool {
    match command {
        "exec" | "run" => data["exit_code"] == 0,
        "status" => data["healthy"] == true,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_envelope_describes_itself_and_says_whether_it_passed() {
        let call = Call::new("status");
        let ok = call.success(json!({ "healthy": false, "stale": false, "checks": [], "session": null }));
        assert_eq!((ok["ok"].as_bool(), ok["passed"].as_bool()), (Some(true), Some(false)));
        assert_eq!(ok["command"], "status");
        assert_eq!(ok["schema"], SCHEMA);
        assert_eq!(ok["stack_version"], env!("CARGO_PKG_VERSION"));
        assert!(ok["duration_ms"].is_u64());
        assert_eq!(ok["data"]["checks"], json!([]), "the result is kept whole");
        let call = Call::new("exec");
        assert_eq!(call.success(json!({ "exit_code": 0 }))["passed"], true);
        assert_eq!(call.success(json!({ "exit_code": 7 }))["passed"], false);
        assert_eq!(call.success(json!({ "exit_code": null, "signal": 9 }))["passed"], false);
        let failed = call.failure(&StackError::new("no_session", "none"));
        assert_eq!((failed["ok"].as_bool(), failed["passed"].as_bool()), (Some(false), Some(false)));
        assert_eq!(failed["error"]["code"], "no_session");
        assert_eq!(Call::new("down").success(json!({}))["passed"], true);
    }
}

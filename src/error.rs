use serde::Serialize;
use std::fmt;

/// A failure an agent can act on: a stable `code`, a human message, and an optional fix.
#[derive(Debug, Serialize)]
pub struct StackError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
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
}

impl fmt::Display for StackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(hint) = &self.hint {
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

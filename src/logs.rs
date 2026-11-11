//! `stack logs` across services: every service when none is named, a substring filter and a
//! time window. Built on [`session::logs_since`], which asks the supervisor for one service's
//! kept output.

use crate::error::{Result, StackError};
use crate::session::{self, Ctx, LogsReport};
use indexmap::IndexMap;
use serde::Serialize;

/// Most lines one request asks the supervisor for, as `--tail` allows.
pub const MAX_LINES: usize = 10_000;

#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Services to show; every service when empty.
    pub services: Vec<String>,
    pub tail: usize,
    pub since_start: bool,
    /// Seconds back from now.
    pub since_secs: Option<u64>,
    pub grep: Option<String>,
    pub ignore_case: bool,
}

impl Query {
    pub fn matches(&self, line: &str) -> bool {
        match &self.grep {
            None => true,
            Some(pattern) if self.ignore_case => line.to_lowercase().contains(&pattern.to_lowercase()),
            Some(pattern) => line.contains(pattern.as_str()),
        }
    }

    /// Lines to ask for: with a filter, as many as allowed, so `tail` counts matching lines.
    fn window(&self) -> usize {
        if self.grep.is_some() {
            MAX_LINES
        } else {
            self.tail
        }
    }

    /// The window's start in Unix seconds, if one was given.
    pub fn since(&self) -> Option<u64> {
        self.since_secs.map(|s| crate::state::now().saturating_sub(s))
    }
}

/// Logs of several services, keyed by service, with the services whose logs could not be read.
#[derive(Debug, Serialize)]
pub struct Many {
    pub services: IndexMap<String, LogsReport>,
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub errors: IndexMap<String, StackError>,
}

pub enum Logs {
    One(LogsReport),
    Many(Many),
}

impl Serialize for Logs {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Logs::One(r) => r.serialize(serializer),
            Logs::Many(m) => m.serialize(serializer),
        }
    }
}

fn one(ctx: &Ctx, service: &str, q: &Query) -> Result<LogsReport> {
    let mut r = session::logs_since(ctx, service, q.window(), q.since_start, q.since())?;
    if q.grep.is_some() {
        r.lines.retain(|l| q.matches(l));
        let skip = r.lines.len().saturating_sub(q.tail);
        r.lines.drain(..skip);
    }
    Ok(r)
}

/// The services `q` names, or every service the project defines when it names none.
pub fn selected(ctx: &Ctx, q: &Query) -> Result<Vec<String>> {
    let names = if q.services.is_empty() { session::service_names(ctx)? } else { q.services.clone() };
    if names.is_empty() {
        return Err(StackError::new("unknown_service", "this project defines no services"));
    }
    Ok(names)
}

/// One service's report when exactly one is named; otherwise each service's (all when none).
pub fn fetch(ctx: &Ctx, q: &Query) -> Result<Logs> {
    if let [service] = q.services.as_slice() {
        return one(ctx, service, q).map(Logs::One);
    }
    let names = selected(ctx, q)?;
    let mut many = Many { services: IndexMap::new(), errors: IndexMap::new() };
    for name in &names {
        match one(ctx, name, q) {
            Ok(r) => {
                many.services.insert(name.clone(), r);
            }
            // A name the project does not define is the caller's mistake, not a service's state.
            Err(e) if e.code == "unknown_service" => return Err(e),
            Err(e) => {
                many.errors.insert(name.clone(), e);
            }
        }
    }
    if many.services.is_empty() {
        let (_, first) = many.errors.shift_remove_index(0).expect("names is not empty");
        return Err(first);
    }
    Ok(Logs::Many(many))
}

/// Lines for people, prefixed with the service when several are shown.
pub fn print_lines(service: &str, width: Option<usize>, lines: &[String]) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for line in lines {
        let _ = match width {
            Some(w) => writeln!(out, "{service:<w$} | {line}"),
            None => writeln!(out, "{line}"),
        };
    }
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_is_a_substring_optionally_ignoring_case() {
        let mut q = Query { grep: Some("Error".into()), tail: 10, ..Query::default() };
        assert!(q.matches("an Error here") && !q.matches("an error here"));
        q.ignore_case = true;
        assert!(q.matches("an ERROR here"));
        assert_eq!(q.window(), MAX_LINES);
        assert_eq!(Query { tail: 7, ..Query::default() }.window(), 7);
    }
}

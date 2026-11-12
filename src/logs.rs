//! `stack logs` across services: every service when none is named, a substring filter, a time
//! window, and `--follow` for people. Built on [`session::logs_since`], which asks the
//! supervisor for one service's kept output; following polls it, because the supervisor offers
//! no stream through mise.

use crate::error::{Result, StackError};
use crate::session::{self, Ctx, LogsReport};
use indexmap::IndexMap;
use serde::Serialize;
use std::time::Duration;

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

/// Lines for people, prefixed with the service when several are shown. Fails when stdout
/// does, such as a pipe whose reader is gone.
pub fn print_lines(service: &str, width: Option<usize>, lines: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for line in lines {
        match width {
            Some(w) => writeln!(out, "{service:<w$} | {line}")?,
            None => writeln!(out, "{line}")?,
        }
    }
    out.flush()
}

/// Lines of `cur` written after the ones `prev` ended with. Both are windows of one output, and
/// `cur` was read later: the new lines start after the longest suffix of `prev` that `cur`
/// holds. Without timestamps, identical lines can make this undercount; with no overlap at all
/// (the window moved past `prev`), every line of `cur` is new.
pub fn new_lines<'a>(prev: &[String], cur: &'a [String]) -> &'a [String] {
    if prev.is_empty() {
        return cur;
    }
    for end in (1..=cur.len()).rev() {
        let m = end.min(prev.len());
        if cur[end - m..end] == prev[prev.len() - m..] {
            return &cur[end..];
        }
    }
    cur
}

/// Print what `q` selects, then keep printing new lines every `interval` until stdout is
/// gone (a closed pipe), or an error. Ctrl-C ends it as it ends `tail -f`: Stack installs no
/// handler for it, and each batch of lines is flushed as it is printed.
pub fn follow(ctx: &Ctx, q: &Query, interval: Duration) -> Result<()> {
    let names = selected(ctx, q)?;
    let width = (names.len() > 1).then(|| names.iter().map(String::len).max().unwrap_or(0));
    // The raw window last read for each service.
    let mut seen: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut from = crate::state::now();
    for name in &names {
        let raw = match session::logs_since(ctx, name, MAX_LINES, q.since_start, q.since()) {
            Ok(r) => r.lines,
            Err(e) if e.code == "logs_failed" && names.len() > 1 => {
                eprintln!("# {name}: {}", e.message);
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        let shown: Vec<String> = raw.iter().filter(|l| q.matches(l)).cloned().collect();
        if print_lines(name, width, &shown[shown.len().saturating_sub(q.tail)..]).is_err() {
            return Ok(());
        }
        seen.insert(name.clone(), raw);
    }
    loop {
        std::thread::sleep(interval);
        let at = crate::state::now();
        for name in &names {
            // Only output since the last read: a short window keeps polls cheap.
            let cur = match session::logs_since(ctx, name, MAX_LINES, q.since_start, Some(from)) {
                Ok(r) => r.lines,
                // A service not started yet, or restarting: try again next time.
                Err(e) if e.code == "logs_failed" => continue,
                Err(e) => return Err(e),
            };
            let prev = seen.get(name).map(Vec::as_slice).unwrap_or_default();
            let fresh: Vec<String> = new_lines(prev, &cur).iter().filter(|l| q.matches(l)).cloned().collect();
            if print_lines(name, width, &fresh).is_err() {
                return Ok(());
            }
            seen.insert(name.clone(), cur);
        }
        from = at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn new_lines_follow_the_overlap_of_successive_windows() {
        assert_eq!(new_lines(&v(&["a", "b", "c"]), &v(&["b", "c", "d", "e"])), v(&["d", "e"]));
        assert_eq!(new_lines(&v(&["a", "b"]), &v(&["a", "b"])), Vec::<String>::new());
        assert_eq!(new_lines(&[], &v(&["x"])), v(&["x"]));
        // A window that starts earlier than the last one still lines up.
        assert_eq!(new_lines(&v(&["c", "d"]), &v(&["a", "b", "c", "d", "e"])), v(&["e"]));
        // No overlap: the window moved past everything seen.
        assert_eq!(new_lines(&v(&["a"]), &v(&["y", "z"])), v(&["y", "z"]));
        assert_eq!(new_lines(&v(&["a"]), &[]), Vec::<String>::new());
    }

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

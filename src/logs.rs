//! `stack logs` across services: every service when none is named, a substring filter, a time
//! window, and `--follow` for people. Built on [`session::logs_since`], which reads the tail of
//! one service's kept output, and on cursor reads ([`Cursor`], [`crate::supervisor::read_after`]),
//! which return only what was written after a position an earlier read gave: every line once,
//! repeated lines included, with when it was read, and what could not be returned (a rotation or
//! truncation in between) said as a `gap`. A cursor read compiles nothing and reads only the new
//! bytes; `--follow` polls that way.

use crate::error::{Result, StackError};
use crate::session::{self, Ctx, LogsReport};
use crate::supervisor::{self, LogPos};
use indexmap::IndexMap;
use serde::Serialize;
use std::time::Duration;

/// Where reads of services' logs got to: for each service, its log file (device, inode) and the
/// offset after the last whole line read. Callers keep it as an opaque string: `v1`, then
/// `;<service, hex>:<device>:<inode>:<offset>` per service, or `;<service, hex>` for one with no
/// log yet (its tail is read next time). An empty string is a cursor with no services: every
/// service's tail is read, as a cursor read reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cursor(IndexMap<String, Option<LogPos>>);

impl Cursor {
    pub fn parse(text: &str) -> Result<Cursor> {
        let invalid = || StackError::new("usage", format!("invalid log cursor {text:?}")).hint("pass back the `cursor` a previous logs result gave, or \"\" to begin");
        let mut cursor = Cursor::default();
        if text.is_empty() {
            return Ok(cursor);
        }
        let mut parts = text.split(';');
        if parts.next() != Some("v1") {
            return Err(invalid());
        }
        for part in parts {
            let fields: Vec<&str> = part.split(':').collect();
            let num = |v: &str| v.parse::<u64>().map_err(|_| invalid());
            let (name, pos) = match fields.as_slice() {
                [name] => (name, None),
                [name, dev, ino, offset] => (name, Some(LogPos { dev: num(dev)?, ino: num(ino)?, offset: num(offset)? })),
                _ => return Err(invalid()),
            };
            cursor.0.insert(unhex(name).ok_or_else(invalid)?, pos);
        }
        Ok(cursor)
    }

    pub fn encode(&self) -> String {
        let mut out = String::from("v1");
        for (name, p) in &self.0 {
            let hex: String = name.bytes().map(|b| format!("{b:02x}")).collect();
            out.push(';');
            out.push_str(&hex);
            if let Some(p) = p {
                out.push_str(&format!(":{}:{}:{}", p.dev, p.ino, p.offset));
            }
        }
        out
    }

    fn one(service: &str, pos: Option<LogPos>) -> Cursor {
        Cursor(IndexMap::from([(service.to_string(), pos)]))
    }

    /// Where the cursor has `service`: `None` when it does not name it, `Some(None)` when it
    /// names it with no log read yet.
    pub fn entry(&self, service: &str) -> Option<Option<LogPos>> {
        self.0.get(service).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn unhex(text: &str) -> Option<String> {
    if text.is_empty() || text.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

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
    /// Read only what was written after these positions (see [`Cursor`]).
    pub cursor: Option<Cursor>,
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
    /// Every service's position, to pass back as `cursor` (see [`Cursor`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug)]
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
    let mut r = match q.cursor.as_ref().and_then(|c| c.entry(service)) {
        Some(Some(pos)) => return after(ctx, service, pos, q),
        // Named by a cursor with no log read yet: its tail, still compiling nothing.
        Some(None) => fresh(ctx, service, q)?,
        None => session::logs_since(ctx, service, q.window(), q.since_start, q.since())?,
    };
    if q.grep.is_some() {
        let kept: Vec<(String, u64)> = r.lines.drain(..).zip(std::mem::take(&mut r.stamps)).filter(|(l, _)| q.matches(l)).collect();
        let skip = kept.len().saturating_sub(q.tail);
        (r.lines, r.stamps) = kept.into_iter().skip(skip).unzip();
    }
    // Stack's supervisor's log, or (in cursor mode) none yet: a cursor to read on from.
    if r.pos.is_some() || q.cursor.is_some() {
        r.cursor = Some(Cursor::one(service, r.pos).encode());
    }
    if q.cursor.is_some() {
        r.timestamps = Some(r.stamps.clone());
    }
    Ok(r)
}

/// When the current process of `service` started (Unix ms), for `since_start`.
fn started(ctx: &Ctx, service: &str, q: &Query) -> Result<Option<u64>> {
    if !q.since_start {
        return Ok(None);
    }
    session::started_ms(ctx, service)?.map(Some).ok_or_else(|| {
        StackError::new("logs_failed", format!("no recorded start time for {service}")).hint("sessions started before stack 0.1.18 lack one; `stack restart` records it")
    })
}

/// The tail of `service`'s log for a cursor that names it with no log read yet: read from the
/// file, compiling nothing; nothing (and no position) while there is no log.
fn fresh(ctx: &Ctx, service: &str, q: &Query) -> Result<LogsReport> {
    let started = started(ctx, service, q)?;
    let from = q.since().map(|s| s * 1000).max(started);
    let dir = supervisor::dir(&ctx.state, &ctx.root);
    let (records, truncated, pos) = supervisor::tail(&dir, service, q.window(), from).map_err(|e| StackError::new("logs_failed", format!("cannot read {service}'s output: {e}")))?;
    let (stamps, lines) = records.into_iter().map(|r| (r.at_ms, r.line)).unzip();
    Ok(LogsReport { service: service.to_string(), lines, truncated, started_at: started.map(|ms| ms / 1000), since_start: q.since_start, cursor: None, timestamps: None, gap: None, more: false, stamps, pos })
}

/// What `service` wrote after `pos`: at most `q.tail` lines that pass `q`'s filters, from at most
/// [`supervisor::LOGS_LIMIT`] bytes. Nothing is compiled and only the new bytes are read (no
/// more than `q.tail` lines of them when nothing filters).
fn after(ctx: &Ctx, service: &str, pos: LogPos, q: &Query) -> Result<LogsReport> {
    let started = started(ctx, service, q)?;
    let from = q.since().map(|s| s * 1000).max(started);
    let dir = supervisor::dir(&ctx.state, &ctx.root);
    let filtered = q.grep.is_some() || from.is_some();
    let read = supervisor::read_after(&dir, service, pos, if filtered { MAX_LINES } else { q.tail }, supervisor::LOGS_LIMIT)
        .map_err(|e| StackError::new("logs_failed", format!("cannot read {service}'s output: {e}")))?;
    let (mut lines, mut stamps) = (Vec::new(), Vec::new());
    let (mut at, mut more) = (read.pos, read.more);
    for (i, record) in read.records.into_iter().enumerate() {
        if lines.len() == q.tail {
            // Full: the next read starts after the last line looked at.
            (at, more) = (Some(read.ends[i - 1]), true);
            break;
        }
        if from.map_or(true, |f| record.at_ms >= f) && q.matches(&record.line) {
            lines.push(record.line);
            stamps.push(record.at_ms);
        }
    }
    Ok(LogsReport {
        service: service.to_string(),
        lines,
        truncated: false,
        started_at: started.map(|ms| ms / 1000),
        since_start: q.since_start,
        cursor: Some(Cursor::one(service, at).encode()),
        timestamps: Some(stamps),
        gap: read.gap,
        more,
        stamps: Vec::new(),
        pos: at,
    })
}

/// The services `q` names; else those its cursor read (nothing is compiled); else every
/// service the project defines.
pub fn selected(ctx: &Ctx, q: &Query) -> Result<Vec<String>> {
    let names = match (&q.services, &q.cursor) {
        (named, _) if !named.is_empty() => named.clone(),
        (_, Some(c)) if !c.is_empty() => c.0.keys().cloned().collect(),
        _ => session::service_names(ctx)?,
    };
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
    let mut many = Many { services: IndexMap::new(), errors: IndexMap::new(), cursor: None };
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
    // Every service's position (none yet for a service with no log, so later reads keep it): a
    // service that failed keeps the one it was given. Only once Stack's supervisor's log is read.
    let positions: IndexMap<String, Option<LogPos>> =
        names.iter().map(|n| (n.clone(), many.services.get(n).and_then(|r| r.pos).or_else(|| q.cursor.as_ref()?.entry(n).flatten()))).collect();
    let any = q.cursor.is_some() || positions.values().any(Option::is_some);
    many.cursor = any.then(|| Cursor(positions).encode());
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
///
/// Each poll reads only what each service wrote since the last (a cursor read of its log file:
/// nothing is compiled, and no window is read again), so every line is printed once, repeated
/// lines included; output a rotation or truncation took before it was read is said on stderr.
/// Services an older Stack started under Pitchfork are polled through it, as before.
pub fn follow(ctx: &Ctx, q: &Query, interval: Duration) -> Result<()> {
    let names = selected(ctx, q)?;
    let width = (names.len() > 1).then(|| names.iter().map(String::len).max().unwrap_or(0));
    let legacy = session::legacy_logs(ctx)?;
    let dir = supervisor::dir(&ctx.state, &ctx.root);
    // The raw window last read for each service (Pitchfork), or where its log was read to.
    let mut seen: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut pos: IndexMap<String, LogPos> = IndexMap::new();
    let mut from = crate::state::now();
    for name in &names {
        let report = match session::logs_since(ctx, name, MAX_LINES, q.since_start, q.since()) {
            Ok(r) => Some(r),
            Err(e) if e.code == "logs_failed" && names.len() > 1 => {
                eprintln!("# {name}: {}", e.message);
                None
            }
            Err(e) => return Err(e),
        };
        let raw = report.as_ref().map(|r| r.lines.clone()).unwrap_or_default();
        let shown: Vec<String> = raw.iter().filter(|l| q.matches(l)).cloned().collect();
        if print_lines(name, width, &shown[shown.len().saturating_sub(q.tail)..]).is_err() {
            return Ok(());
        }
        if let Some(p) = report.and_then(|r| r.pos) {
            pos.insert(name.clone(), p);
        }
        seen.insert(name.clone(), raw);
    }
    loop {
        std::thread::sleep(interval);
        let at = crate::state::now();
        for name in &names {
            if legacy {
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
                continue;
            }
            // Every line since the last read, in as many reads as a burst takes.
            loop {
                let (lines, next, more) = match pos.get(name) {
                    Some(p) => match supervisor::read_after(&dir, name, *p, MAX_LINES, supervisor::LOGS_LIMIT) {
                        Ok(read) => {
                            if let Some(gap) = read.gap {
                                eprintln!("# {name}: some output was {} before it could be read", if gap == supervisor::Gap::Truncated { "truncated" } else { "rotated away" });
                            }
                            (read.records.into_iter().map(|r| r.line).collect::<Vec<_>>(), read.pos, read.more)
                        }
                        Err(_) => break,
                    },
                    // No log when following began: what was written since, once there is one.
                    None => match supervisor::tail(&dir, name, MAX_LINES, Some(from * 1000)) {
                        Ok((records, _, p)) => (records.into_iter().map(|r| r.line).collect(), p, false),
                        Err(_) => break,
                    },
                };
                let fresh: Vec<String> = lines.into_iter().filter(|l| q.matches(l)).collect();
                if print_lines(name, width, &fresh).is_err() {
                    return Ok(());
                }
                if let Some(p) = next {
                    pos.insert(name.clone(), p);
                }
                if !more {
                    break;
                }
            }
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
    fn cursors_round_trip_and_refuse_anything_else() {
        let mut c = Cursor::default();
        c.0.insert("db".into(), Some(LogPos { dev: 1, ino: 2, offset: 3 }));
        c.0.insert("web:1;x".into(), Some(LogPos { dev: u64::MAX, ino: 0, offset: 99 }));
        c.0.insert("quiet".into(), None);
        let text = c.encode();
        assert_eq!(text, "v1;6462:1:2:3;7765623a313b78:18446744073709551615:0:99;7175696574");
        assert_eq!(Cursor::parse(&text).unwrap(), c);
        assert!(Cursor::parse("").unwrap().is_empty());
        assert_eq!(Cursor::parse("v1").unwrap(), Cursor::default());
        for bad in ["v2;6462:1:2:3", "v1;6462:1:2", "v1;6462:1", "v1;646:1:2:3", "v1;zz:1:2:3", "v1;6462:1:2:-3", "x", "v1;;"] {
            assert_eq!(Cursor::parse(bad).unwrap_err().code, "usage", "{bad}");
        }
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

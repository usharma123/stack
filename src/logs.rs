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
/// service's tail is read, as a cursor read reports it. A name no service can have (see
/// [`crate::manifest::valid_service_name`]) makes the cursor invalid.
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
            // Only a name a service can have (see `crate::manifest::valid_service_name`): a cursor
            // is read back from a caller, and names the log file it reads.
            let name = unhex(name).filter(|n| crate::manifest::valid_service_name(n)).ok_or_else(invalid)?;
            cursor.0.insert(name, pos);
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
            // Full: the next read starts after the last line looked at (where this one began,
            // when none was: a tail of 0 takes nothing).
            (at, more) = (Some(i.checked_sub(1).map_or(pos, |last| read.ends[last])), true);
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
                let Ok(step) = poll(&dir, name, pos.get(name).copied(), from * 1000) else { break };
                if let Some(gap) = step.gap {
                    eprintln!("# {name}: some output was {} before it could be read", if gap == supervisor::Gap::Truncated { "truncated" } else { "rotated away" });
                }
                let fresh: Vec<String> = step.lines.into_iter().filter(|l| q.matches(l)).collect();
                if print_lines(name, width, &fresh).is_err() {
                    return Ok(());
                }
                if let Some(p) = step.pos {
                    pos.insert(name.clone(), p);
                }
                if !step.more {
                    break;
                }
            }
        }
        from = at;
    }
}

/// One read of `--follow`'s poll of a service.
struct Polled {
    lines: Vec<String>,
    /// Where the next read starts; `None` while the service has no log.
    pos: Option<LogPos>,
    /// A limit left whole records for another read.
    more: bool,
    gap: Option<supervisor::Gap>,
}

/// What `name` wrote after `pos` (with what a rotation or truncation took first), or, while its
/// log did not exist when following began (`pos` is `None`), what it has written at or after
/// `from_ms` once it has one.
fn poll(dir: &std::path::Path, name: &str, pos: Option<LogPos>, from_ms: u64) -> std::io::Result<Polled> {
    Ok(match pos {
        Some(p) => {
            let read = supervisor::read_after(dir, name, p, MAX_LINES, supervisor::LOGS_LIMIT)?;
            Polled { lines: read.records.into_iter().map(|r| r.line).collect(), pos: read.pos, more: read.more, gap: read.gap }
        }
        None => {
            let (records, _, pos) = supervisor::tail(dir, name, MAX_LINES, Some(from_ms))?;
            Polled { lines: records.into_iter().map(|r| r.line).collect(), pos, more: false, gap: None }
        }
    })
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
        // Names no service can have: a path out of the log directory, `.`, `..`, NUL.
        for name in ["../spec", "a/b", ".", "..", "/etc/x", "nul\0"] {
            let hex: String = name.bytes().map(|b| format!("{b:02x}")).collect();
            for text in [format!("v1;{hex}"), format!("v1;{hex}:1:2:3")] {
                assert_eq!(Cursor::parse(&text).unwrap_err().code, "usage", "{name:?}");
            }
        }
        // Every name a project gives a service still round-trips.
        let names = Cursor(["api.v2", "My Service", "...", "サービス"].into_iter().map(|n| (n.to_string(), None)).collect());
        assert_eq!(Cursor::parse(&names.encode()).unwrap(), names);
    }

    /// A context whose Stack state is `state`, for a checkout at `root` (nothing compiled).
    fn ctx(state: &std::path::Path, root: &std::path::Path) -> Ctx {
        Ctx { root: root.to_path_buf(), cache: state.join("cache"), state: state.to_path_buf() }
    }

    #[test]
    fn a_cursor_read_of_no_lines_takes_none_and_keeps_its_place() {
        let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let ctx = ctx(state.path(), root.path());
        let dir = supervisor::dir(&ctx.state, &ctx.root);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(supervisor::log_path(&dir, "db"), "100 dummy one\n200 dummy two\n").unwrap();
        let start = supervisor::read_after(&dir, "db", supervisor::LogPos { dev: 0, ino: 0, offset: 0 }, 0, 0).unwrap().pos.unwrap();
        let start = LogPos { offset: 0, ..start };
        for grep in [None, Some("dummy".to_string())] {
            let q = Query { services: vec!["db".into()], tail: 0, grep: grep.clone(), cursor: Some(Cursor::one("db", Some(start))), ..Query::default() };
            let Logs::One(report) = fetch(&ctx, &q).unwrap() else { panic!("one service") };
            assert!(report.lines.is_empty() && report.more, "{grep:?}: {report:?}");
            assert_eq!(report.pos, Some(start), "{grep:?}: the next read starts where this one did");
            // And a read that takes lines carries on from there.
            let q = Query { tail: 10, ..q };
            let Logs::One(report) = fetch(&ctx, &q).unwrap() else { panic!("one service") };
            assert_eq!(report.lines, ["dummy one", "dummy two"], "{grep:?}");
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

    /// Cursors as clients keep them: one naming several services, one of which has no log yet
    /// (its tail next time, then a position), and positions in files that are gone or cut.
    #[test]
    fn cursors_naming_several_services_read_each_on_and_positions_in_lost_files_say_so() {
        let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let ctx = ctx(state.path(), root.path());
        let dir = supervisor::dir(&ctx.state, &ctx.root);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let append = |service: &str, text: &str| {
            use std::io::Write;
            std::fs::OpenOptions::new().create(true).append(true).open(supervisor::log_path(&dir, service)).unwrap().write_all(text.as_bytes()).unwrap();
        };
        append("web", "1 same\n2 same\n");
        let both = Cursor(IndexMap::from([("web".to_string(), None), ("db".to_string(), None)])).encode();
        let read = |cursor: &str| match fetch(&ctx, &Query { tail: 10, cursor: Some(Cursor::parse(cursor).unwrap()), ..Query::default() }).unwrap() {
            Logs::Many(m) => m,
            Logs::One(_) => panic!("several services"),
        };
        // Selected by the cursor alone (nothing compiled); `db` has no log and stays named.
        let first = read(&both);
        assert_eq!(first.services["web"].lines, ["same", "same"]);
        assert!(first.services["db"].lines.is_empty() && first.errors.is_empty());
        let cursor = first.cursor.unwrap();
        let parsed = Cursor::parse(&cursor).unwrap();
        assert!(parsed.entry("web").unwrap().is_some() && parsed.entry("db") == Some(None), "{cursor}");
        append("web", "3 same\n");
        append("db", "4 up\n");
        let next = read(&cursor);
        assert_eq!((next.services["web"].lines.clone(), next.services["db"].lines.clone()), (vec!["same".to_string()], vec!["up".to_string()]));
        assert_eq!(next.services["web"].timestamps, Some(vec![3]));
        let cursor = next.cursor.unwrap();
        assert!(Cursor::parse(&cursor).unwrap().entry("db").unwrap().is_some());
        // Nothing new: nothing again, and the same cursor.
        let idle = read(&cursor);
        assert!(idle.services.values().all(|r| r.lines.is_empty() && r.gap.is_none() && !r.more));
        assert_eq!(idle.cursor.as_deref(), Some(cursor.as_str()));
        // `web`'s file replaced (kept elsewhere, so the new one cannot reuse its inode), `db`'s
        // cut under its cursor.
        std::fs::rename(supervisor::log_path(&dir, "web"), dir.join("replaced")).unwrap();
        append("web", "5 fresh\n");
        std::fs::OpenOptions::new().write(true).open(supervisor::log_path(&dir, "db")).unwrap().set_len(0).unwrap();
        let lost = read(&cursor);
        assert_eq!((lost.services["web"].lines.clone(), lost.services["web"].gap), (vec!["fresh".to_string()], Some(supervisor::Gap::Rotated)));
        assert_eq!((lost.services["db"].lines.len(), lost.services["db"].gap), (0, Some(supervisor::Gap::Truncated)));
    }

    /// A deterministic generator for the state machine below.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    /// One service's log, written as Stack's supervisor lays it out (`<ms> <line>\n` records,
    /// rotated to `<service>.log.1`), with what each file holds: record ids, oldest first.
    struct Writer {
        path: std::path::PathBuf,
        older_path: std::path::PathBuf,
        /// Where each rotated-away file stays linked, so no later log file can reuse its inode (a
        /// cursor names its file by device and inode; see the verify skill's limits).
        kept: std::path::PathBuf,
        /// Every record: when it was stamped, and its line (a cut record as it was cut).
        records: Vec<(u64, String)>,
        current: Vec<usize>,
        current_exists: bool,
        current_gen: u32,
        older: Vec<usize>,
        older_gen: Option<u32>,
        /// A record still being written into the current file: its stamp, line and the part
        /// of the line written so far.
        partial: Option<(u64, String, usize)>,
        clock: u64,
    }

    impl Writer {
        fn append(&mut self, text: &str) {
            use std::io::Write;
            std::fs::OpenOptions::new().create(true).append(true).open(&self.path).unwrap().write_all(text.as_bytes()).unwrap();
            self.current_exists = true;
        }

        fn write(&mut self, line: &str) {
            self.append(&format!("{} {line}\n", self.clock));
            self.records.push((self.clock, line.to_string()));
            self.current.push(self.records.len() - 1);
        }

        fn begin(&mut self, line: &str, split: usize) {
            self.append(&format!("{} {}", self.clock, &line[..split]));
            self.partial = Some((self.clock, line.to_string(), split));
        }

        fn finish(&mut self) {
            let (at, line, split) = self.partial.take().unwrap();
            self.append(&format!("{}\n", &line[split..]));
            self.records.push((at, line));
            self.current.push(self.records.len() - 1);
        }

        /// Rename the current file over the older one: what that held is gone. A record cut
        /// there (no newline yet) stays at the end of the rotated file as it is.
        fn rotate(&mut self) -> Vec<usize> {
            if let Some((at, line, split)) = self.partial.take() {
                self.records.push((at, line[..split].to_string()));
                self.current.push(self.records.len() - 1);
            }
            if let Some(gen) = self.older_gen {
                std::fs::hard_link(&self.older_path, self.kept.join(gen.to_string())).unwrap();
            }
            std::fs::rename(&self.path, &self.older_path).unwrap();
            let gone = std::mem::replace(&mut self.older, std::mem::take(&mut self.current));
            self.older_gen = Some(self.current_gen);
            self.current_gen += 1;
            self.current_exists = false;
            gone
        }

        /// Cut the current file to nothing by hand (Stack's writer never does).
        fn truncate(&mut self) -> Vec<usize> {
            std::fs::OpenOptions::new().write(true).open(&self.path).unwrap().set_len(0).unwrap();
            std::mem::take(&mut self.current)
        }
    }

    /// Where a reader got to, as the oracle sees it: a file (generation) and how many of its
    /// records it took; every record it took, and every one gone before it could.
    struct Reader {
        at: (u32, usize),
        took: Vec<usize>,
        lost: std::collections::BTreeSet<usize>,
    }

    impl Reader {
        /// What a read from here can return: the gap it must report, the records after the
        /// cursor (each with the position just after it), and where a read of all of them ends.
        fn plan(&self, w: &Writer) -> (Option<supervisor::Gap>, Vec<(usize, (u32, usize))>, (u32, usize)) {
            let current = |from: usize| -> Vec<(usize, (u32, usize))> { (from..w.current.len()).map(|i| (w.current[i], (w.current_gen, i + 1))).collect() };
            let older = |from: usize| -> Vec<(usize, (u32, usize))> { (from..w.older.len()).map(|i| (w.older[i], (w.older_gen.unwrap(), i + 1))).collect() };
            let end = if w.current_exists { (w.current_gen, w.current.len()) } else { (w.older_gen.unwrap(), w.older.len()) };
            let (gen, count) = self.at;
            if w.current_exists && gen == w.current_gen {
                if count > w.current.len() {
                    (Some(supervisor::Gap::Truncated), current(0), end)
                } else {
                    (None, current(count), end)
                }
            } else if w.older_gen == Some(gen) {
                (None, [older(count), current(0)].concat(), end)
            } else {
                (Some(supervisor::Gap::Rotated), [older(0), current(0)].concat(), end)
            }
        }

        fn lose(&mut self, gone: &[usize]) {
            let took: std::collections::BTreeSet<usize> = self.took.iter().copied().collect();
            self.lost.extend(gone.iter().filter(|id| !took.contains(id)));
        }
    }

    /// A log written with repeated lines, equal stamps, records written in two parts, rotations
    /// (one and two between reads), cut records and truncation by hand, read on the way by two
    /// readers: `stack logs --json --cursor` (through `fetch`, with the cursor passed back as the
    /// text a client keeps, varying `--tail` and `--grep`) and `--follow`'s poll. Each read
    /// returns exactly the whole records written after its cursor, in order, each once with its
    /// stamp, none split; it stops after `--tail` lines, saying `more` exactly when whole records
    /// are left; a record gone before it was read is gone only with the `gap` the read reports.
    #[test]
    fn a_seeded_log_read_by_cursor_and_by_follow_returns_each_record_once_or_reports_its_gap() {
        use std::collections::BTreeSet;
        let lines = ["same", "same", "x y", "", "sérvice ✓ done", "a longer line, 0123456789", "same"];
        let mut seen = BTreeSet::new();
        for seed in 1..=5u64 {
            let (state, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let ctx = ctx(state.path(), root.path());
            let dir = supervisor::dir(&ctx.state, &ctx.root);
            std::fs::create_dir_all(dir.join("logs")).unwrap();
            let path = supervisor::log_path(&dir, "web");
            let mut older_path = path.clone().into_os_string();
            older_path.push(".1");
            let mut w = Writer {
                path,
                older_path: older_path.into(),
                kept: state.path().to_path_buf(),
                records: Vec::new(),
                current: Vec::new(),
                current_exists: false,
                current_gen: 0,
                older: Vec::new(),
                older_gen: None,
                partial: None,
                clock: 1_700_000_000_000,
            };
            w.write("start");
            // The first read names the service with no position yet: its tail, and a cursor.
            let mut cursor = Cursor::one("web", None).encode();
            let q = Query { services: vec!["web".into()], tail: 5, cursor: Some(Cursor::parse(&cursor).unwrap()), ..Query::default() };
            let Logs::One(first) = fetch(&ctx, &q).unwrap() else { panic!("one service") };
            assert_eq!(first.lines, ["start"]);
            cursor = first.cursor.unwrap();
            let mut by_cursor = Reader { at: (0, 1), took: vec![0], lost: BTreeSet::new() };
            let (_, _, follow_pos) = supervisor::tail(&dir, "web", MAX_LINES, None).unwrap();
            let mut follow_pos = follow_pos.unwrap();
            let mut by_follow = Reader { at: (0, 1), took: vec![0], lost: BTreeSet::new() };
            let mut rng = Lcg(seed);
            for step in 0..300 {
                let op = rng.below(10);
                let label = format!("seed {seed} step {step} op {op}");
                let mut read_now = false;
                match op {
                    0..=3 if w.partial.is_none() => {
                        for _ in 0..=rng.below(3) {
                            w.clock += rng.below(3);
                            w.write(lines[rng.below(lines.len() as u64) as usize]);
                        }
                    }
                    4 if w.partial.is_none() => {
                        w.clock += 1;
                        let line = lines[rng.below(lines.len() as u64) as usize];
                        let splits: Vec<usize> = line.char_indices().map(|(i, _)| i).chain([line.len()]).collect();
                        w.begin(line, splits[rng.below(splits.len() as u64) as usize]);
                        seen.insert("partial");
                    }
                    5 if w.partial.is_some() => w.finish(),
                    6 if w.current_exists => {
                        if w.partial.is_some() {
                            seen.insert("cut");
                        }
                        let gone = w.rotate();
                        by_cursor.lose(&gone);
                        by_follow.lose(&gone);
                        seen.insert("rotated");
                    }
                    // Only where a read can tell (each reader in the current file, past its start)
                    // and before the file grows past the cursor again: read at once.
                    7 if w.current_exists && w.partial.is_none() && [&by_cursor, &by_follow].iter().all(|r| r.at.0 == w.current_gen && r.at.1 > 0) => {
                        let gone = w.truncate();
                        by_cursor.lose(&gone);
                        by_follow.lose(&gone);
                        read_now = true;
                        seen.insert("truncated");
                    }
                    8 | 9 => read_now = true,
                    _ => continue,
                }
                if !read_now {
                    continue;
                }
                // `stack logs --json --cursor`: the cursor read back from its text.
                let tail = [1, 2, 3, 7, 100][rng.below(5) as usize];
                let grep = (rng.below(4) == 0).then(|| "same".to_string());
                let (gap, available, end) = by_cursor.plan(&w);
                let q = Query { services: vec!["web".into()], tail, grep: grep.clone(), cursor: Some(Cursor::parse(&cursor).unwrap()), ..Query::default() };
                let Logs::One(report) = fetch(&ctx, &q).unwrap() else { panic!("one service") };
                let mut shown = Vec::new();
                let (mut taken, mut at, mut more) = (0, end, false);
                for (i, (id, _)) in available.iter().enumerate() {
                    if shown.len() == tail {
                        (at, more) = (available[i - 1].1, true);
                        break;
                    }
                    taken += 1;
                    if q.matches(&w.records[*id].1) {
                        shown.push(*id);
                    }
                }
                let expected: Vec<&str> = shown.iter().map(|id| w.records[*id].1.as_str()).collect();
                let stamps: Vec<u64> = shown.iter().map(|id| w.records[*id].0).collect();
                assert_eq!(report.lines, expected, "{label}: tail {tail} grep {grep:?}");
                assert_eq!(report.timestamps.as_ref(), Some(&stamps), "{label}");
                assert_eq!(report.gap, gap, "{label}");
                assert_eq!(report.more, more, "{label}: tail {tail} grep {grep:?}, {taken} of {} taken, writing {:?}", available.len(), w.partial);
                let text = report.cursor.unwrap();
                assert_eq!(Cursor::parse(&text).unwrap().encode(), text, "{label}: the cursor round-trips");
                cursor = text;
                by_cursor.took.extend(available[..taken].iter().map(|(id, _)| *id));
                by_cursor.at = at;
                if more {
                    seen.insert("more");
                }
                if gap.is_some() {
                    seen.insert("gap");
                }
                // `--follow`: one poll, in as many reads as it takes.
                let (gap, available, end) = by_follow.plan(&w);
                let mut got = Vec::new();
                let mut gaps = Vec::new();
                loop {
                    let step = poll(&dir, "web", Some(follow_pos), 0).unwrap();
                    got.extend(step.lines);
                    gaps.extend(step.gap);
                    follow_pos = step.pos.unwrap();
                    if !step.more {
                        break;
                    }
                }
                let expected: Vec<&str> = available.iter().map(|(id, _)| w.records[*id].1.as_str()).collect();
                assert_eq!(got, expected, "{label}: follow");
                assert_eq!(gaps, gap.into_iter().collect::<Vec<_>>(), "{label}: follow");
                by_follow.took.extend(available.iter().map(|(id, _)| *id));
                by_follow.at = end;
            }
            // Every record either read once, in order, or lost with a reported gap.
            for (name, reader) in [("cursor", &by_cursor), ("follow", &by_follow)] {
                let complete: BTreeSet<usize> = (0..w.records.len()).collect();
                let unread: BTreeSet<usize> = reader.plan(&w).1.iter().map(|(id, _)| *id).collect();
                let took: BTreeSet<usize> = reader.took.iter().copied().collect();
                assert_eq!(took.len(), reader.took.len(), "seed {seed} {name}: a record read twice");
                assert!(reader.took.windows(2).all(|p| p[0] < p[1]), "seed {seed} {name}: out of order");
                assert!(took.is_disjoint(&reader.lost), "seed {seed} {name}");
                let accounted: BTreeSet<usize> = took.union(&reader.lost).chain(unread.iter()).copied().collect();
                assert_eq!(accounted, complete, "seed {seed} {name}");
            }
        }
        for case in ["partial", "cut", "rotated", "truncated", "more", "gap"] {
            assert!(seen.contains(case), "no run reached {case}: {seen:?}");
        }
    }
}

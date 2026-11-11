//! Lifecycle progress as it happens. The lifecycle (`up`, `install`, `restart`) reports each
//! step to whatever observer the calling thread installed, if any: the [`Terminal`] printer
//! for people, or anything else that implements [`Observer`], such as the MCP server's progress
//! notifier (through a [`Sink`]). Without an observer nothing is reported, and results are
//! unchanged either way.

use serde_json::Value;
use std::cell::RefCell;
use std::io::{IsTerminal, Write};
use std::rc::Rc;
use std::time::Instant;

/// Where a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Started,
    Ok,
    Warning,
    Failed,
}

impl Phase {
    /// The status a result's `steps` records for the step (`started` is never recorded).
    pub fn status(self) -> &'static str {
        match self {
            Phase::Started => "started",
            Phase::Ok => "ok",
            Phase::Warning => "warning",
            Phase::Failed => "failed",
        }
    }
}

/// One step event. `code` is the error code of a failed step; `detail` what a completed step
/// recorded.
#[derive(Debug)]
pub struct Event<'a> {
    pub step: &'a str,
    pub phase: Phase,
    pub code: Option<&'a str>,
    pub detail: Option<&'a Value>,
}

pub trait Observer {
    fn step(&self, event: &Event<'_>);
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<dyn Observer>>> = const { RefCell::new(None) };
}

/// Restores the previous observer when dropped.
#[must_use = "progress is reported only while the scope is alive"]
pub struct Scope {
    previous: Option<Rc<dyn Observer>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|c| *c.borrow_mut() = previous);
    }
}

/// Report this thread's lifecycle steps to `observer` while the returned scope lives.
pub fn observe(observer: impl Observer + 'static) -> Scope {
    let observer: Rc<dyn Observer> = Rc::new(observer);
    Scope { previous: CURRENT.with(|c| c.borrow_mut().replace(observer)) }
}

/// Tell this thread's observer, if any, about `event`.
pub fn notify(event: Event<'_>) {
    // Cloned out first, so an observer may itself notify without a re-entrant borrow.
    let observer = CURRENT.with(|c| c.borrow().clone());
    if let Some(observer) = observer {
        observer.step(&event);
    }
}

/// A step is starting.
pub fn started(step: &str) {
    notify(Event { step, phase: Phase::Started, code: None, detail: None });
}

/// A plain listener for step events, shareable across threads: `(step, status)`, where status
/// is `started`, `ok`, `warning` or `failed`, as the step appears in a result's `steps`. The
/// MCP server sends these as `notifications/progress`.
pub type Sink = std::sync::Arc<dyn Fn(&str, &str) + Send + Sync>;

struct SinkObserver(Sink);

impl Observer for SinkObserver {
    fn step(&self, event: &Event<'_>) {
        (self.0)(event.step, event.phase.status());
    }
}

/// Send this thread's steps to `sink` (or to nobody, for `None`) until the scope is dropped.
pub fn scope(sink: Option<Sink>) -> Scope {
    let observer = sink.map(|sink| Rc::new(SinkObserver(sink)) as Rc<dyn Observer>);
    Scope { previous: CURRENT.with(|c| c.replace(observer)) }
}

/// Progress for people on stderr: one line when a step starts, one when it ends.
pub struct Terminal {
    color: bool,
    started: RefCell<Vec<(String, Instant)>>,
}

impl Terminal {
    /// A printer when stderr is a terminal; `None` otherwise (pipes and files stay clean).
    pub fn if_tty() -> Option<Self> {
        std::io::stderr().is_terminal().then(|| Self::new(color_allowed()))
    }

    pub fn new(color: bool) -> Self {
        Self { color, started: RefCell::new(Vec::new()) }
    }

    fn render(&self, event: &Event<'_>) -> String {
        let elapsed = {
            let mut started = self.started.borrow_mut();
            match event.phase {
                Phase::Started => {
                    started.push((event.step.to_string(), Instant::now()));
                    None
                }
                _ => started.iter().rposition(|(s, _)| s == event.step).map(|i| started.remove(i).1.elapsed()),
            }
        };
        let paint = |text: &str, code: &str| if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() };
        let took = elapsed.map(|d| format!(" ({:.1}s)", d.as_secs_f64())).unwrap_or_default();
        match event.phase {
            Phase::Started => format!("stack: {} ...", event.step),
            Phase::Ok => format!("stack: {} {}{took}", event.step, paint("ok", "32")),
            Phase::Warning => format!("stack: {} {}{took}", event.step, paint("warning", "33")),
            Phase::Failed => {
                format!("stack: {} {}{took}{}", event.step, paint("FAIL", "31"), event.code.map(|c| format!(" [{c}]")).unwrap_or_default())
            }
        }
    }
}

impl Observer for Terminal {
    fn step(&self, event: &Event<'_>) {
        let line = self.render(event);
        let _ = writeln!(std::io::stderr(), "{line}");
    }
}

/// Colour only on a terminal, and never when `NO_COLOR` is set to anything non-empty.
pub fn color_allowed() -> bool {
    std::env::var_os("NO_COLOR").map_or(true, |v| v.is_empty()) && std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell as Cell;

    struct Recorder(Rc<Cell<Vec<String>>>);
    impl Observer for Recorder {
        fn step(&self, e: &Event<'_>) {
            self.0.borrow_mut().push(format!("{}:{:?}", e.step, e.phase));
        }
    }

    #[test]
    fn events_reach_the_installed_observer_until_its_scope_ends() {
        let seen = Rc::new(Cell::new(Vec::new()));
        {
            let _scope = observe(Recorder(seen.clone()));
            started("install");
            notify(Event { step: "install", phase: Phase::Ok, code: None, detail: None });
        }
        started("after");
        assert_eq!(*seen.borrow(), ["install:Started", "install:Ok"]);
    }

    #[test]
    fn a_sink_hears_steps_by_status_name_only_while_its_scope_lives() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        started("gc");
        {
            let seen = seen.clone();
            let _scope = scope(Some(std::sync::Arc::new(move |s: &str, st: &str| seen.lock().unwrap().push(format!("{s} {st}")))));
            started("install");
            notify(Event { step: "install", phase: Phase::Failed, code: Some("install_failed"), detail: None });
        }
        started("verify");
        assert_eq!(*seen.lock().unwrap(), ["install started", "install failed"]);
    }

    #[test]
    fn the_terminal_printer_marks_outcomes_without_colour_when_asked() {
        let t = Terminal::new(false);
        assert_eq!(t.render(&Event { step: "compile", phase: Phase::Started, code: None, detail: None }), "stack: compile ...");
        let done = t.render(&Event { step: "compile", phase: Phase::Ok, code: None, detail: None });
        assert!(done.starts_with("stack: compile ok ("), "{done}");
        let failed = t.render(&Event { step: "start", phase: Phase::Failed, code: Some("start_failed"), detail: None });
        assert_eq!(failed, "stack: start FAIL [start_failed]");
        assert!(!failed.contains('\x1b'));
        let coloured = Terminal::new(true).render(&Event { step: "x", phase: Phase::Ok, code: None, detail: None });
        assert!(coloured.contains("\x1b[32mok\x1b[0m"), "{coloured}");
    }
}

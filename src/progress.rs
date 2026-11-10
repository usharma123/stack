//! Lifecycle progress as it happens. The lifecycle (`up`, `install`, `restart`) reports each
//! step to whatever observer the calling thread installed, if any: anything that implements
//! [`Observer`], such as the MCP server's progress notifier (through a [`Sink`]). Without an
//! observer nothing is reported, and results are unchanged either way.

use serde_json::Value;
use std::cell::RefCell;
use std::rc::Rc;

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
}

//! Opt-in phase timings for diagnosing command-entry latency.
//!
//! Set `STACK_TIMINGS=1` to print one `stack: timing <phase> <ms>` line per phase to stderr.
//! Stdout, including `--json` output, is never touched, and nothing is recorded when unset.

use std::time::{Duration, Instant};

/// Phase timer. Disabled timers cost one environment lookup at creation.
pub struct Timings {
    label: &'static str,
    start: Option<Instant>,
    last: Option<Instant>,
}

impl Timings {
    pub fn new(label: &'static str) -> Self {
        let enabled = std::env::var_os("STACK_TIMINGS").is_some_and(|v| !v.is_empty() && v != "0");
        let start = enabled.then(Instant::now);
        Self {
            label,
            start,
            last: start,
        }
    }

    pub fn enabled(&self) -> bool {
        self.start.is_some()
    }

    /// Record the time since the previous mark (or creation) under `phase`.
    pub fn mark(&mut self, phase: &str) {
        let Some(last) = self.last else { return };
        let now = Instant::now();
        report(self.label, phase, now - last);
        self.last = Some(now);
    }

    /// Record a duration measured elsewhere, e.g. one probe among concurrent ones.
    pub fn record(&self, phase: &str, elapsed: Duration) {
        if self.enabled() {
            report(self.label, phase, elapsed);
        }
    }
}

impl Drop for Timings {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            report(self.label, "total", start.elapsed());
        }
    }
}

fn report(label: &str, phase: &str, elapsed: Duration) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "stack: timing {label}.{phase} {:.1}ms",
        elapsed.as_secs_f64() * 1000.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_formats_without_panicking() {
        report("test", "phase", Duration::from_millis(12));
    }
}

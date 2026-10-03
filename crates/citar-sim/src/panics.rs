//! Where a caught panic happened, for its crash record.
//!
//! A payload carries only the message, so a crash record would say what went wrong but not
//! where. Python's records carried a traceback (`traceback.format_exc(limit=...)`,
//! `headless.py:56`, `baseline.py:118`). A panic hook is the only place the location and the
//! backtrace are known, so [`install`] adds one that keeps them in a thread-local until the
//! runner that catches the panic on the same thread takes them ([`take`]).
//!
//! The hook is process-wide, so installing it is the host's choice: the CLI installs it, a
//! library caller may. It chains to the hook it replaces, which still prints the panic to stderr
//! (Python printed its tracebacks to the console too). Backtraces are captured only when
//! `RUST_BACKTRACE` asks for them: a panic in a release game is rare, and its location is what a
//! crash line needs first.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::cell::RefCell;
use std::sync::Once;

/// Where the last panic on this thread happened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    /// `file:line:column`, when the hook saw one.
    pub location: Option<String>,
    /// The backtrace, when `RUST_BACKTRACE` asked for one.
    pub backtrace: Option<String>,
}

impl Trace {
    /// The trace as a crash record's text, at most `frames` frames of the backtrace (Python's
    /// `traceback_limit`); empty when nothing was kept.
    #[must_use]
    pub fn text(&self, frames: usize) -> String {
        let mut out = String::new();
        if let Some(l) = &self.location {
            out.push_str("at ");
            out.push_str(l);
        }
        if let Some(b) = &self.backtrace {
            // A frame is a numbered line and, with debug information, an `at` line under it.
            let mut kept = 0;
            for line in b.lines() {
                let numbered = line
                    .trim_start()
                    .split_once(':')
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
                if numbered {
                    if kept == frames {
                        break;
                    }
                    kept += 1;
                }
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(line);
            }
        }
        out
    }
}

thread_local! {
    static LAST: RefCell<Option<Trace>> = const { RefCell::new(None) };
}

static INSTALL: Once = Once::new();

/// Installs the hook that keeps each panic's location (and backtrace, when `RUST_BACKTRACE` asks)
/// for [`take`], chained to the hook already installed. Only the first call installs it.
pub fn install() {
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location =
                info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let bt = Backtrace::capture();
            let backtrace = (bt.status() == BacktraceStatus::Captured).then(|| bt.to_string());
            LAST.with(|t| *t.borrow_mut() = Some(Trace { location, backtrace }));
            previous(info);
        }));
    });
}

/// Takes what the hook kept of the last panic on this thread, if it is installed and one
/// happened since the last take.
#[must_use]
pub fn take() -> Option<Trace> {
    LAST.with(|t| t.borrow_mut().take())
}

/// Forgets the last panic's trace on this thread, so that a later [`take`] cannot report a
/// panic that was caught and handled elsewhere.
pub fn forget() {
    LAST.with(|t| *t.borrow_mut() = None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_keeps_its_location_and_at_most_the_frames_asked_for() {
        let t = Trace {
            location: Some("src/x.rs:3:5".into()),
            backtrace: Some(
                "   0: a\n             at a.rs:1\n   1: b\n             at b.rs:2\n   2: c\n"
                    .into(),
            ),
        };
        assert_eq!(t.text(1), "at src/x.rs:3:5\n   0: a\n             at a.rs:1");
        assert_eq!(t.text(5).lines().count(), 6);
        assert_eq!(Trace::default().text(5), "");
    }

    #[test]
    fn the_hook_keeps_the_location_of_a_panic_on_its_thread() {
        install();
        forget();
        let caught = std::panic::catch_unwind(|| panic!("kept"));
        assert!(caught.is_err());
        let t = take().expect("the hook kept it");
        assert!(t.location.as_deref().is_some_and(|l| l.contains("panics.rs")), "{t:?}");
        assert_eq!(take(), None, "taken once");
    }
}

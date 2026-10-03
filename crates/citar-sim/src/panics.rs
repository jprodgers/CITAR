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
//!
//! A backtrace taken in a hook starts with the capture itself, the hook and the runtime's
//! handling of the panic, ten frames or so before the code that panicked. A crash record keeps
//! a few frames, as Python's `traceback_limit` did, so [`Trace::text`] counts them from the code
//! that panicked, as the default hook's short backtrace starts there.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::cell::RefCell;
use std::sync::Once;

/// Where the last panic on this thread happened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trace {
    /// `file:line:column`, when the hook saw one.
    pub location: Option<String>,
    /// The backtrace as captured, from the capture down, when `RUST_BACKTRACE` asked for one.
    pub backtrace: Option<String>,
}

impl Trace {
    /// The trace as a crash record's text: the location, then at most `frames` frames of the
    /// backtrace (Python's `traceback_limit`), counted from the code that panicked; empty when
    /// nothing was kept. The frames keep their numbers in the whole backtrace, so a reader sees
    /// how many were left out above them.
    #[must_use]
    pub fn text(&self, frames: usize) -> String {
        let mut out = String::new();
        if let Some(l) = &self.location {
            out.push_str("at ");
            out.push_str(l);
        }
        if let Some(b) = &self.backtrace {
            let all = split_frames(b);
            let from = first_frame_of_the_panic(&all);
            for f in all.iter().skip(from).take(frames) {
                for line in &f.lines {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(line);
                }
            }
        }
        out
    }
}

/// One frame of a printed backtrace: its numbered line (`  4: symbol`) and, with debug
/// information, the `at file:line` lines under it.
#[derive(Debug)]
struct Frame<'a> {
    symbol: &'a str,
    lines: Vec<&'a str>,
}

/// The frames of a backtrace as `Backtrace`'s `Display` prints it. Lines before the first frame
/// are no frame's, and are left out.
fn split_frames(bt: &str) -> Vec<Frame<'_>> {
    let mut out: Vec<Frame<'_>> = Vec::new();
    for line in bt.lines() {
        let numbered = line
            .trim_start()
            .split_once(':')
            .filter(|(n, _)| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
        match (numbered, out.last_mut()) {
            (Some((_, symbol)), _) => out.push(Frame { symbol: symbol.trim(), lines: vec![line] }),
            (None, Some(f)) => f.lines.push(line),
            (None, None) => {}
        }
    }
    out
}

/// Where the code that panicked starts in `frames`: past the capture and this module's hook,
/// past the runtime's handling of the panic from its first frame down to the panic's entry
/// point (`panic_fmt` for `panic!`, an `unwrap` or an index; `begin_panic` for a payload that is
/// no message), and past the standard library's frames that raised it (`unwrap_failed`, an
/// index's bounds check).
///
/// The capture and the hook are skipped by position, not by name: without debug information
/// (the ci and release profiles on Windows) a frame of ours takes the name of the nearest public
/// symbol, and the hook's closure has been seen named `std::rt::lang_start`. The runtime's own
/// frames keep their names, since the standard library ships its symbols. The `catch_unwind`
/// frames that the panic unwinds into lie below the code that panicked, so the first runtime
/// frame from the top is never one of them. A backtrace without the runtime's names (no
/// symbols) is kept from its first frame that is not the capture or the hook.
fn first_frame_of_the_panic(frames: &[Frame<'_>]) -> usize {
    let Some(runtime) = frames.iter().position(|f| is_panic_runtime(f.symbol)) else {
        return frames.iter().position(|f| !is_capture(f.symbol)).unwrap_or(0);
    };
    let raised = |f: &Frame<'_>| is_panic_runtime(f.symbol) || is_standard_library(f.symbol);
    let first = frames[runtime..].iter().position(|f| !raised(f)).map(|i| runtime + i);
    // Nothing but the standard library below the runtime: keep the frames below its last rather
    // than none.
    first.unwrap_or_else(|| {
        frames.iter().rposition(|f| is_panic_runtime(f.symbol)).map_or(runtime, |e| e + 1)
    })
}

/// A frame of the capture or of this module's hook: the boxed hook's call included, which is
/// how the runtime calls it.
fn is_capture(symbol: &str) -> bool {
    let s = symbol.trim_start_matches('<');
    const PREFIXES: [&str; 5] = [
        "std::backtrace",
        "backtrace::",
        "citar_sim::panics::install",
        "citar_sim::panics::capture",
        "alloc::boxed::",
    ];
    PREFIXES.iter().any(|p| s.starts_with(p))
}

/// A frame of the runtime's handling of a panic, from the hook's caller down to the panic's entry
/// point: names that never stand in a stack that is not panicking. `std::panicking`'s
/// `catch_unwind` and `try` are not among them: they lie below the code that panicked.
fn is_panic_runtime(symbol: &str) -> bool {
    let s = symbol.trim_start_matches('<');
    const PREFIXES: [&str; 7] = [
        "core::panicking::",
        "std::panicking::begin_panic",
        "std::panicking::rust_panic",
        "std::panicking::panic_with_hook",
        "std::panicking::panic_handler",
        "std::panicking::default_hook",
        "std::rt::panic_fmt",
    ];
    PREFIXES.iter().any(|p| s.starts_with(p))
        || s.contains("__rust_end_short_backtrace")
        || s.ends_with("rust_begin_unwind")
        || s == "rust_panic"
}

/// A frame of the standard library: `core::...`, `alloc::...` or `std::...`, an impl of one of
/// their types (`<alloc::vec::Vec<T> as core::ops::index::Index<I>>::index`), or a library
/// trait's impl for a primitive (`<usize as core::slice::index::SliceIndex<[T]>>::index`). The
/// impl of a library trait for one of ours (`<citar_engine::X as core::fmt::Display>`) is ours.
fn is_standard_library(symbol: &str) -> bool {
    const ROOTS: [&str; 3] = ["core::", "alloc::", "std::"];
    let s = symbol.trim_start_matches('<');
    if ROOTS.iter().any(|r| s.starts_with(r)) {
        return true;
    }
    let Some((ty, rest)) = s.split_once(" as ") else { return false };
    let primitive = ty.starts_with('[')
        || ty.starts_with('&')
        || matches!(
            ty,
            "usize"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "f32"
                | "f64"
                | "bool"
                | "char"
                | "str"
        );
    primitive && ROOTS.iter().any(|r| rest.starts_with(r))
}

thread_local! {
    static LAST: RefCell<Option<Trace>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Captures a backtrace on this thread whatever `RUST_BACKTRACE` says: std reads it once
    /// per process, so a test cannot set it for itself.
    static FORCE_BACKTRACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The backtrace of the panic being handled, as `RUST_BACKTRACE` asks (and in this crate's
/// tests, as the test asks).
fn capture() -> Backtrace {
    #[cfg(test)]
    if FORCE_BACKTRACE.with(std::cell::Cell::get) {
        return Backtrace::force_capture();
    }
    Backtrace::capture()
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
            let bt = capture();
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

    fn trace(bt: &str) -> Trace {
        Trace { location: Some("src/x.rs:3:5".into()), backtrace: Some(bt.into()) }
    }

    #[test]
    fn a_trace_keeps_its_location_and_at_most_the_frames_asked_for() {
        let t =
            trace("   0: a\n             at a.rs:1\n   1: b\n             at b.rs:2\n   2: c\n");
        assert_eq!(t.text(1), "at src/x.rs:3:5\n   0: a\n             at a.rs:1");
        assert_eq!(t.text(5).lines().count(), 6);
        assert_eq!(t.text(0), "at src/x.rs:3:5");
        assert_eq!(Trace::default().text(5), "");
    }

    /// A Linux backtrace of `Option::unwrap` failing in a runner step, as `Backtrace` prints it
    /// in a debug build (the `at` lines shortened).
    const LINUX: &str = "   0: std::backtrace_rs::backtrace::libunwind::trace
             at /rustc/x/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1: std::backtrace_rs::backtrace::trace_unsynchronized
   2: std::backtrace::Backtrace::create
   3: std::backtrace::Backtrace::capture
   4: citar_sim::panics::install::{{closure}}::{{closure}}
             at ./src/panics.rs:180:22
   5: <alloc::boxed::Box<F,A> as core::ops::function::Fn<Args>>::call
   6: std::panicking::rust_panic_with_hook
   7: std::panicking::begin_panic_handler::{{closure}}
   8: std::sys::backtrace::__rust_end_short_backtrace
   9: __rustc::rust_begin_unwind
  10: core::panicking::panic_fmt
  11: core::panicking::panic
  12: core::option::unwrap_failed
  13: core::option::Option<T>::unwrap
  14: citar_bot::turn::found_capital
             at ./crates/citar-bot/src/turn.rs:40:5
  15: citar_bot::driver::Bot::play
  16: citar_engine::game::turn::drive::<impl citar_engine::game::Game>::drive
  17: citar_sim::runner::Runner::step::{{closure}}
  18: std::panicking::catch_unwind::do_call
  19: std::panicking::catch_unwind
  20: std::panic::catch_unwind
  21: citar_sim::runner::Runner::step
  22: std::sys::backtrace::__rust_begin_short_backtrace
  23: std::rt::lang_start::{{closure}}
";

    /// The same on Windows, where the names are the debugger's.
    const WINDOWS: &str = "   0: std::backtrace_rs::backtrace::win64::trace
             at /rustc/x/library/std/src/../../backtrace/src/backtrace/win64.rs:85
   1: std::backtrace_rs::backtrace::trace_unsynchronized
   2: std::backtrace::Backtrace::create
   3: std::backtrace::Backtrace::capture
   4: citar_sim::panics::install::closure$0::closure$0
   5: alloc::boxed::impl$30::call<tuple$<ref$<std::panic::PanicHookInfo> >,dyn$<core::ops::function::Fn<tuple$<ref$<std::panic::PanicHookInfo> >,assoc$<Output,tuple$<> > > >,alloc::alloc::Global>
   6: std::panicking::panic_with_hook
   7: std::panicking::panic_handler::closure$0
   8: std::sys::backtrace::__rust_end_short_backtrace<std::panicking::panic_handler::closure_env$0,never$>
   9: std::panicking::panic_handler
  10: core::panicking::panic_fmt
  11: core::panicking::panic_bounds_check
  12: core::slice::index::impl$2::index<u32>
  13: alloc::vec::impl$13::index<u32,usize,alloc::alloc::Global>
  14: citar_engine::game::cities::grow
  15: citar_engine::game::turn::stages::run_player
  16: std::panicking::catch_unwind::do_call<citar_sim::runner::impl$3::step::closure_env$0>
  17: citar_sim::runner::Runner::step
";

    #[test]
    fn the_frames_kept_start_at_the_code_that_panicked() {
        let t = trace(LINUX);
        let text = t.text(2);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "at src/x.rs:3:5");
        assert_eq!(lines[1], "  14: citar_bot::turn::found_capital", "{text}");
        assert_eq!(lines[2], "             at ./crates/citar-bot/src/turn.rs:40:5");
        assert_eq!(lines[3], "  15: citar_bot::driver::Bot::play");
        assert_eq!(lines.len(), 4);
        // The catch_unwind frames below are not taken for the panic's: everything up to the
        // runner is kept when asked for.
        assert!(t.text(100).ends_with("  23: std::rt::lang_start::{{closure}}"));
        let first = |bt: &str| trace(bt).text(1).lines().nth(1).map(str::to_owned);
        assert_eq!(first(WINDOWS).as_deref(), Some("  14: citar_engine::game::cities::grow"));
        // An explicit `panic!` enters at panic_fmt, and the function that called it is first.
        let direct = "   0: std::backtrace::Backtrace::capture\n   1: core::panicking::panic_fmt\n   \
                      2: citar_sim::x::f\n   3: std::panic::catch_unwind\n";
        assert_eq!(first(direct).as_deref(), Some("   2: citar_sim::x::f"));
        // A library trait's impl for one of our types is ours.
        let ours = "   0: core::panicking::panic_fmt\n   1: <citar_engine::X as core::fmt::Display>::fmt\n";
        assert_eq!(
            first(ours).as_deref(),
            Some("   1: <citar_engine::X as core::fmt::Display>::fmt")
        );
        // Without debug information on Windows a frame of ours takes the nearest public
        // symbol's name, a hook's closure once `std::rt::lang_start` (the ci profile with every
        // feature): the runtime's first frame still starts the cut.
        let misnamed = "   3: std::backtrace::Backtrace::force_capture
   4: std::rt::lang_start::<()>::{closure#0}
   5: std::panicking::panic_with_hook
   6: std::panicking::panic_handler::closure$0
   7: std::sys::backtrace::__rust_end_short_backtrace<std::panicking::panic_handler::closure_env$0,never$>
   8: std::panicking::panic_handler
   9: core::panicking::panic_fmt
  10: <serde_json::value::de::KeyClassifier as serde_core::de::DeserializeSeed>::deserialize
  11: citar_sim::panics::tests::a_test
  12: <&str as core::fmt::Debug>::fmt
  13: core::ops::function::FnOnce::call_once
  14: std::panicking::catch_unwind
";
        assert_eq!(
            trace(misnamed).text(2).lines().skip(1).collect::<Vec<_>>(),
            [
                "  10: <serde_json::value::de::KeyClassifier as serde_core::de::DeserializeSeed>::deserialize",
                "  11: citar_sim::panics::tests::a_test"
            ]
        );
        // Nothing but the standard library below the runtime: the frames below it are kept.
        let all_std = "   0: core::panicking::panic_fmt\n   1: core::option::unwrap_failed\n   \
                       2: std::panicking::catch_unwind\n";
        assert_eq!(first(all_std).as_deref(), Some("   1: core::option::unwrap_failed"));
        // Without names, everything but the capture is kept.
        let unnamed =
            "   0: std::backtrace::Backtrace::capture\n   1: <unknown>\n   2: <unknown>\n";
        assert_eq!(first(unnamed).as_deref(), Some("   1: <unknown>"));
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

    /// Panics here, in a frame of its own.
    #[inline(never)]
    fn raise_the_test_panic(n: usize) -> usize {
        if n > 0 {
            panic!("the test panics in a named function");
        }
        n
    }

    #[test]
    fn a_captured_backtrace_is_counted_from_the_function_that_panicked() {
        install();
        forget();
        FORCE_BACKTRACE.with(|f| f.set(true));
        let caught = std::panic::catch_unwind(|| raise_the_test_panic(std::hint::black_box(1)));
        FORCE_BACKTRACE.with(|f| f.set(false));
        assert!(caught.is_err());
        let t = take().expect("the hook kept it");
        let bt = t.backtrace.as_deref().expect("a backtrace was forced");
        let text = t.text(5);
        let frames: Vec<&str> = text.lines().filter(|l| split_frames(l).len() == 1).collect();
        assert!((1..=5).contains(&frames.len()), "{text}\n\nthe whole backtrace:\n{bt}");
        let all = split_frames(bt);
        let named = all.iter().position(|f| f.symbol.contains("raise_the_test_panic"));
        if let Some(i) = named {
            assert_eq!(frames[0], all[i].lines[0], "{text}\n\nthe whole backtrace:\n{bt}");
        } else {
            // Without debug information (the ci profile on Windows) a frame takes the name of
            // the nearest public symbol, so the function cannot be found by name: the frames
            // kept start below the panic's entry.
            let entry = all.iter().rposition(|f| f.symbol.starts_with("core::panicking::"));
            let entry = entry.expect("the entry is a public symbol");
            let kept = all.iter().position(|f| f.lines[0] == frames[0]).unwrap_or_default();
            assert!(kept > entry, "{text}\n\nthe whole backtrace:\n{bt}");
        }
    }
}

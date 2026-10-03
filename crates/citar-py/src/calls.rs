//! Calls with the GIL released, counted, and kept from re-attaching to an interpreter that is
//! finalizing (DESIGN.md P2.6.2, P2.6.4).
//!
//! Every heavy call runs through [`detached`]: the GIL is released for it, so other Python
//! threads run and two games use two cores, and it is counted while it runs. Nothing inside it
//! touches Python.
//!
//! **Interpreter exit.** A daemon thread (a session's driver) may be inside a call when Python
//! exits. Re-attaching it to a finalizing interpreter ends the thread from inside
//! `PyEval_RestoreThread` (`pthread_exit` on Python 3.11 to 3.13), an unwind through the Rust
//! frames below it that aborts the process. So the module registers [`shutdown`] with `atexit`
//! at import: it marks the process as exiting and waits, up to its timeout, for the calls in
//! flight. Atexit runs after the non-daemon threads are joined and before the interpreter
//! starts to finalize, so from then on every other thread is a daemon thread: one whose call
//! ends, or that starts a call, parks forever with the GIL released instead of re-attaching, and
//! the process ends around it. The thread that called `shutdown` (the main thread) goes on
//! calling as before, so other exit hooks may still save games. Python 3.14 hangs such threads
//! itself; this makes every supported version behave so.

use std::sync::{Condvar, Mutex, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use pyo3::prelude::*;

/// The calls in flight, and the thread that began the shutdown, if one has.
struct Flight {
    calls: usize,
    exiting: Option<ThreadId>,
}

static FLIGHT: Mutex<Flight> = Mutex::new(Flight { calls: 0, exiting: None });

/// Signalled when the last call in flight ends.
static IDLE: Condvar = Condvar::new();

fn flight() -> std::sync::MutexGuard<'static, Flight> {
    // Nothing panics while holding it: it guards two plain fields.
    FLIGHT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this thread must not go back to Python: the process is exiting and another thread
/// began it.
fn barred(f: &Flight) -> bool {
    f.exiting.is_some_and(|t| t != std::thread::current().id())
}

/// Parks this thread for good: its call must not re-attach to a finalizing interpreter, and the
/// process ends around it.
fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

/// One call in flight, counted until dropped (also when its work unwinds).
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        let mut f = flight();
        if barred(&f) {
            drop(f);
            park_forever();
        }
        f.calls += 1;
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut f = flight();
        f.calls = f.calls.saturating_sub(1);
        if f.calls == 0 {
            IDLE.notify_all();
        }
    }
}

/// Runs `work` with the GIL released, counted as a call in flight. A call that starts or ends
/// once another thread has begun the shutdown parks instead of returning to Python.
pub fn detached<T, F>(py: Python<'_>, work: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    py.detach(|| {
        let counted = InFlight::enter();
        let out = work();
        drop(counted);
        if barred(&flight()) {
            park_forever();
        }
        out
    })
}

/// How many calls are in flight now.
#[pyfunction]
pub fn calls_in_flight() -> usize {
    flight().calls
}

/// Marks the process as exiting and waits, at most `timeout` seconds, for the calls in flight
/// to end; whether they all did. The module registers it with `atexit`. From now on a call
/// another thread starts or ends parks that thread for good, so no thread re-attaches to the
/// interpreter as it finalizes; the calling thread goes on as before.
#[pyfunction]
#[pyo3(signature = (timeout = 5.0))]
pub fn shutdown(py: Python<'_>, timeout: f64) -> bool {
    // The latest caller is the one that goes on: atexit calls it last, on the main thread.
    flight().exiting = Some(std::thread::current().id());
    let wait = if timeout.is_finite() && timeout > 0.0 {
        Duration::from_secs_f64(timeout.min(3600.0))
    } else {
        Duration::ZERO
    };
    // With the GIL released, a daemon thread between calls runs on to its next call and parks
    // there; the calls in flight need nothing of Python to finish.
    py.detach(|| {
        let deadline = Instant::now() + wait;
        let mut f = flight();
        while f.calls > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            f = IDLE.wait_timeout(f, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        true
    })
}

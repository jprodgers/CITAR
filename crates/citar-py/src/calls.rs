//! Calls with the GIL released, counted, and kept from re-attaching to an interpreter that is
//! finalizing (DESIGN.md P2.6.2, P2.6.4).
//!
//! Every heavy call runs through [`detached`]: the GIL is released for it, so other Python
//! threads run and two games use two cores. Nothing inside it touches Python.
//!
//! **Interpreter exit.** A daemon thread (a session's driver) may be inside a call when Python
//! exits. Re-attaching it to a finalizing interpreter ends the thread from inside
//! `PyEval_RestoreThread` (`pthread_exit` on Python 3.11 to 3.13), an unwind through the Rust
//! frames below it that aborts the process. So:
//! - a call counts as in flight from when it releases the GIL until it has it back, and Python
//!   code that runs from inside a Rust call (`run_game`'s hooks) counts too ([`hold`]): either
//!   is a thread with Rust frames on its stack that may need the GIL;
//! - the module registers [`shutdown`] with `atexit` at import. Atexit runs once the non-daemon
//!   threads are joined and before the interpreter starts to finalize: `shutdown` marks the
//!   process as exiting and waits, with the GIL released and up to its timeout, for the counts
//!   to reach zero;
//! - from then on any other thread (a daemon thread) that starts a call, ends one, or comes back
//!   to a hook parks for good with the GIL released, instead of re-attaching, and the process
//!   ends around it. A thread waiting for the GIL to come back from a call is counted, so the
//!   wait lets it through before the interpreter finalizes, and it parks at its next call. The
//!   thread that called `shutdown` (the main thread) goes on calling as before, so other exit
//!   hooks may still save games. Python 3.14 hangs such threads itself; this makes every
//!   supported version behave so.

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

/// Parks this thread for good: it must not re-attach to a finalizing interpreter, and the
/// process ends around it. Called with the GIL released.
fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

/// One call in flight, counted until dropped (also when its work unwinds).
pub struct InFlight(());

impl InFlight {
    /// Counts a call, unless the process is exiting under another thread: then the caller,
    /// which has released the GIL, parks.
    fn enter() -> Self {
        let mut f = flight();
        if barred(&f) {
            drop(f);
            park_forever();
        }
        f.calls += 1;
        Self(())
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

/// Runs `work` with the GIL released, counted as a call in flight until the GIL is back. A
/// call that starts or ends once another thread has begun the shutdown parks instead of going
/// back to Python.
pub fn detached<T, F>(py: Python<'_>, work: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    let (out, counted) = py.detach(|| {
        let counted = InFlight::enter();
        let out = work();
        if barred(&flight()) {
            drop(counted);
            park_forever();
        }
        (out, counted)
    });
    // Counted until here: a thread waiting for the GIL to come back has Rust frames below it.
    drop(counted);
    out
}

/// Parks this thread, with the GIL released, if another thread has begun the shutdown: for a
/// Rust call about to run Python code (a hook) that must not run into a finalizing interpreter.
pub fn checkpoint(py: Python<'_>) {
    if barred(&flight()) {
        py.detach(|| park_forever());
    }
}

/// Counts Python code that runs from inside a Rust call (`run_game`'s hooks) as in flight, so
/// the shutdown waits for it; parks first if another thread has begun the shutdown.
pub fn hold(py: Python<'_>) -> InFlight {
    checkpoint(py);
    let mut f = flight();
    f.calls += 1;
    InFlight(())
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
    // With the GIL released, a thread waiting for it to come back from a call gets it, and a
    // daemon thread between calls runs on to its next call and parks there; the calls in flight
    // need nothing of Python to finish.
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

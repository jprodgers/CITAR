//! The CITAR game engine.
//!
//! Pure and deterministic: no I/O, no threads, no clock and no C code. The same seed and the same
//! actions give the same game on every supported target. `DESIGN.md` beside this crate is the
//! design, and `README.md` holds the rules no lint can see.
//!
//! The modules are layers (DESIGN.md 3.1). A layer may use only the layers listed for it, and
//! `cargo xtask check` enforces that:
//!
//! | Layer | Module | May use |
//! |---|---|---|
//! | 0 | [`base`] | nothing |
//! | 1 | [`rules`] | `base`, `unique` |
//! | 1 | [`unique`] | `base`, `rules` |
//! | 2 | [`state`] | `base`, `rules`, `unique` |
//! | 2 | [`save`] | `base`, `rules`, `unique`, `state` |
//! | 2 | `compat` (feature `legacy`) | `base`, `rules`, `unique`, `state`, `save` |
//! | 3a | [`mapgen`] | `base`, `rules`, `unique`, `state` |
//! | 3 | [`game`] | all of the above |
//! | 4 | [`api`] | everything |
//!
//! It replaces the Python engine in `citar/engine/` as of commit `4b5a912`, without bit parity:
//! `refcheck/intended.toml` lists every deliberate rule difference.

#![forbid(unsafe_code)]

// Determinism is proven on five targets, all 64-bit and little-endian (DESIGN.md 1.1). Anything
// else is refused at compile time rather than trusted untested.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("citar-engine supports 64-bit targets only; determinism is not tested elsewhere");
#[cfg(not(target_endian = "little"))]
compile_error!(
    "citar-engine supports little-endian targets only; determinism is not tested elsewhere"
);
const _: () = assert!(usize::BITS == 64, "citar-engine supports 64-bit targets only");
const _: () =
    assert!(u16::from_ne_bytes([1, 0]) == 1, "citar-engine supports little-endian targets only");

pub mod base;
pub mod rules;
pub mod unique;

pub mod save;
pub mod state;

#[cfg(feature = "legacy")]
pub mod compat;

pub mod mapgen;

pub mod game;

pub mod api;

/// Proves at compile time that a type is `Send`.
///
/// Hosts move a `Game` between threads: citar-py drives it inside `allow_threads`, whose closure
/// must be `Send`.
///
/// ```
/// citar_engine::assert_send!(std::cell::Cell<u64>);
/// ```
///
/// ```compile_fail
/// citar_engine::assert_send!(std::rc::Rc<u8>);
/// ```
#[doc(hidden)]
#[macro_export]
macro_rules! assert_send {
    ($t:ty) => {
        const _: fn() = || {
            fn send<T: ?Sized + Send>() {}
            send::<$t>();
        };
    };
}

/// Proves at compile time that a type is not `Sync`.
///
/// A `Game`'s memos validate themselves on read through `&Game`, using `Cell` and `RefCell`
/// stamps (DESIGN.md 6.3). Sharing one `&Game` between threads would race on those stamps, so it
/// must stay `!Sync`, and hosts hold it behind `&mut` or a lock. If `$t` were `Sync`, both impls
/// below would apply and the call would be ambiguous, which is a compile error.
///
/// ```
/// citar_engine::assert_not_sync!(std::cell::Cell<u64>);
/// ```
///
/// ```compile_fail
/// citar_engine::assert_not_sync!(u64);
/// ```
#[doc(hidden)]
#[macro_export]
macro_rules! assert_not_sync {
    ($t:ty) => {
        const _: fn() = || {
            trait AmbiguousIfSync<A> {
                fn some_item() {}
            }
            impl<T: ?Sized> AmbiguousIfSync<()> for T {}
            struct Invalid;
            impl<T: ?Sized + Sync> AmbiguousIfSync<Invalid> for T {}
            let _ = <$t as AmbiguousIfSync<_>>::some_item;
        };
    };
}

// The premise of DESIGN.md 6.1: memos are built from `Cell` and `RefCell`, which are `Send` and
// not `Sync`, so a `Game` built from them can move between threads but never be shared. `Game`
// itself (1b-01) gets its own asserts here when it exists.
assert_send!(core::cell::Cell<u64>);
assert_not_sync!(core::cell::Cell<u64>);
assert_send!(core::cell::RefCell<Vec<u64>>);
assert_not_sync!(core::cell::RefCell<Vec<u64>>);

// A `State` is plain data: a snapshot of one moves to another thread to be saved off the lock
// (DESIGN.md 4.9), and so does the `Game` that holds it.
assert_send!(state::State);
assert_send!(state::chronicle::Chronicle);

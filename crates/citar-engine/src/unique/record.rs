//! What a computation read, by class (DESIGN.md 6.3, 6.5): a memo whose value comes from many
//! uniques records the classes of those it evaluated, and validates against them alone, rather
//! than against every class the ruleset's uniques of those types could read.
//!
//! [`recorded`] runs a computation with a recorder on: every unique evaluated meanwhile
//! ([`super::cond::applies`]) adds what it reads ([`super::UniqueTable::reads`]: its conditionals
//! and the filters of its parameters), whether it applies or not, since a conditional that fails
//! now may hold after a write. A computation may also [`note`](note_classes) classes of state it
//! reads directly. [`isolated`] runs a computation with the recorder off: a memo's validation and
//! recomputation run so (`derive::rev::Memo::get`), since what a memo upstream read is its own,
//! and the memo downstream validates against that memo's stamp instead.
//!
//! The recorder is the thread's: the engine computes on one thread (DESIGN.md 6.13), and a later
//! `par` must carry it into its sections. It holds nothing between computations.

use core::cell::Cell;

use super::table::{CondDeps, UniqueTable};
use crate::base::ids::UniqueId;

std::thread_local! {
    /// The classes read since the innermost [`recorded`] began; `None` when no computation
    /// records.
    static READ: Cell<Option<CondDeps>> = const { Cell::new(None) };
}

/// Puts the recorder back as it was when dropped: a computation that panics leaves the thread's
/// recorder as it found it.
struct Restore(Option<CondDeps>);

impl Drop for Restore {
    fn drop(&mut self) {
        READ.with(|r| r.set(self.0));
    }
}

/// Adds what evaluating unique `id` reads, if a computation records.
#[inline]
pub fn note(t: &UniqueTable, id: UniqueId) {
    READ.with(|r| {
        if let Some(d) = r.get() {
            r.set(Some(d | t.reads(id)));
        }
    });
}

/// Adds `classes`, if a computation records: state a computation reads directly, as a class
/// names it.
#[inline]
pub fn note_classes(classes: CondDeps) {
    READ.with(|r| {
        if let Some(d) = r.get() {
            r.set(Some(d | classes));
        }
    });
}

/// Runs `f` recording what it reads, and returns that with its value. What an enclosing
/// computation records is kept apart: `f`'s reads are not added to it.
pub fn recorded<T>(f: impl FnOnce() -> T) -> (T, CondDeps) {
    let _restore = Restore(READ.with(|r| r.replace(Some(CondDeps::empty()))));
    let v = f();
    let d = READ.with(Cell::get).unwrap_or_else(CondDeps::empty);
    (v, d)
}

/// Runs `f` with the recorder off: nothing it reads is added to an enclosing computation's.
pub fn isolated<T>(f: impl FnOnce() -> T) -> T {
    let _restore = Restore(READ.with(|r| r.replace(None)));
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_keeps_to_itself() {
        note_classes(CondDeps::TURN);
        let (inner, d) = recorded(|| {
            note_classes(CondDeps::TECHS);
            let ((), nested) = recorded(|| note_classes(CondDeps::MAP));
            isolated(|| note_classes(CondDeps::UNIT_SET));
            note_classes(CondDeps::WAR);
            nested
        });
        assert_eq!(inner, CondDeps::MAP);
        assert_eq!(d, CondDeps::TECHS | CondDeps::WAR, "neither the nested nor the isolated");
        assert_eq!(READ.with(Cell::get), None, "off again, as it was");
    }

    #[test]
    fn a_panic_leaves_the_recorder_as_it_was() {
        let r = std::panic::catch_unwind(|| recorded(|| panic!("a bug")));
        assert!(r.is_err());
        assert_eq!(READ.with(Cell::get), None);
    }
}

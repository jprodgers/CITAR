//! What can be wrong with a ruleset, collected into one report (DESIGN.md 5.3 and 8.5).
//!
//! Python checked a handful of references and raised on the first batch (`rules.py:238-260`);
//! everything else failed later, in a way that looked like an engine bug. The loader here checks
//! every reference, name, table size and table shape, and reports every problem it finds, each
//! with its file, its object and a sentence saying what is wrong.

use core::fmt;

/// What kind of problem a [`RulesetError`] is, for tests and tools to match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RulesetErrorKind {
    /// A required file is not among the files given.
    MissingFile,
    /// A file the ruleset does not have was given.
    UnknownFile,
    /// The same file was given twice.
    DuplicateFile,
    /// A file is not JSON, or an object in it has the same key twice.
    Json,
    /// A value has the wrong shape: an unknown or misspelt field, a missing field, a wrong type.
    Schema,
    /// A name that should name an object of some table names nothing there.
    UnknownReference,
    /// A name used twice where it must be unique, or an object whose name is not its key.
    Name,
    /// A table larger than the id type or set that holds it.
    Capacity,
    /// A value outside what the rules allow, such as a speed's turn table out of order.
    Invalid,
    /// An object the engine relies on by name, such as the Hill feature, is absent.
    Missing,
    /// A unique text UnCiv has no type for, and that is no tag a filter names (DESIGN.md 5.6).
    UnknownUnique,
    /// A unique of an UnCiv type the engine does not support (`unique_supported.toml`).
    UnsupportedUnique,
    /// A unique's parameter that does not compile: not a number, out of range, an unknown stat
    /// or name.
    UniqueParameter,
    /// A unique's modifiers out of place: two triggers, a `for every` multiplier, the same
    /// action modifier twice, a modifier written as a unique or a unique as a modifier, a region
    /// condition off map generation, or a condition map generation cannot read.
    UniqueModifier,
    /// A filter that does not compile: a term that matches nothing, a filter nested too deep, or
    /// a filter map generation reads that asks more than the terrain (DESIGN.md 5.7).
    Filter,
}

/// One problem in a ruleset.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct RulesetError {
    /// The file, as named in `RulesetFiles`: `ruleset/units.json`.
    pub file: Box<str>,
    /// The object the problem is in, such as `Warrior`, or empty for the file as a whole.
    pub object: Box<str>,
    /// What is wrong, as a sentence.
    pub text: String,
    /// What kind of problem it is.
    pub kind: RulesetErrorKind,
}

impl fmt::Display for RulesetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.object.is_empty() {
            write!(f, "{}: {}", self.file, self.text)
        } else {
            write!(f, "{}: {}: {}", self.file, self.object, self.text)
        }
    }
}

/// Every problem found in a ruleset that did not load. Never empty.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct RulesetErrors(pub Vec<RulesetError>);

impl RulesetErrors {
    /// Whether any problem is of `kind`.
    #[must_use]
    pub fn has(&self, kind: RulesetErrorKind) -> bool {
        self.0.iter().any(|e| e.kind == kind)
    }
}

impl fmt::Display for RulesetErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.as_slice() {
            [one] => write!(f, "the ruleset does not load: {one}"),
            all => {
                write!(f, "the ruleset does not load: {} problems", all.len())?;
                for e in all {
                    write!(f, "\n  - {e}")?;
                }
                Ok(())
            }
        }
    }
}

/// The loader's error collector: every stage adds what it finds, and the loader stops after a
/// stage that found anything, since later stages would only report its consequences.
#[derive(Default)]
pub(crate) struct Problems(Vec<RulesetError>);

impl Problems {
    pub(crate) fn push(
        &mut self,
        kind: RulesetErrorKind,
        file: &str,
        object: &str,
        text: impl Into<String>,
    ) {
        self.0.push(RulesetError {
            file: file.into(),
            object: object.into(),
            text: text.into(),
            kind,
        });
    }

    /// `Err` with everything found so far, if anything was.
    pub(crate) fn check(&mut self) -> Result<(), RulesetErrors> {
        if self.0.is_empty() { Ok(()) } else { Err(RulesetErrors(core::mem::take(&mut self.0))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_name_the_file_and_object() {
        let mut p = Problems::default();
        assert!(p.check().is_ok());
        p.push(RulesetErrorKind::UnknownReference, "ruleset/units.json", "Warrior", "unknown tech");
        p.push(RulesetErrorKind::Json, "game.json", "", "not JSON");
        let errs = p.check().expect_err("two problems");
        assert!(errs.has(RulesetErrorKind::Json));
        assert!(!errs.has(RulesetErrorKind::Capacity));
        assert_eq!(
            errs.to_string(),
            "the ruleset does not load: 2 problems\n  - ruleset/units.json: Warrior: unknown \
             tech\n  - game.json: not JSON"
        );
        assert!(p.check().is_ok(), "check hands the problems over");
    }
}

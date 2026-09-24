//! Upgrades of older save versions, applied to the JSON before it is read (DESIGN.md 4.9).
//!
//! A migration rewrites one version's document into the next's, so the reader only ever sees
//! the current version. Version 1 is the first, so there are none yet: the table is where the
//! first change of format adds its function, and [`upgrade`] applies each in turn.
//!
//! Replaces nothing in Python, whose saves carried no version.

use serde_json::Value;

use super::LoadError;

/// The save format version this engine writes.
pub const CURRENT: u32 = 1;

/// The oldest version this engine reads.
pub const OLDEST: u32 = 1;

/// One migration: rewrites a document of version `from` into version `from + 1`.
type Migration = fn(&mut Value) -> Result<(), LoadError>;

/// The migrations, the one from version `OLDEST + i` at position `i`.
const MIGRATIONS: &[Migration] = &[];

const _: () = assert!(MIGRATIONS.len() as u32 == CURRENT - OLDEST, "one migration per version");

/// Brings a document of version `from` up to [`CURRENT`], setting its `version` key. A version
/// older than [`OLDEST`] or newer than [`CURRENT`] is refused.
pub fn upgrade(doc: &mut Value, from: u32) -> Result<(), LoadError> {
    if !(OLDEST..=CURRENT).contains(&from) {
        return Err(LoadError::Version(from));
    }
    for (i, m) in MIGRATIONS.iter().enumerate().skip((from - OLDEST) as usize) {
        m(doc)?;
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("version".to_owned(), Value::from(OLDEST + i as u32 + 1));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_version_needs_nothing_and_others_are_refused() {
        let mut doc = serde_json::json!({"version": 1});
        assert_eq!(upgrade(&mut doc, 1), Ok(()));
        assert_eq!(doc, serde_json::json!({"version": 1}));
        assert_eq!(upgrade(&mut doc, 0), Err(LoadError::Version(0)));
        assert_eq!(upgrade(&mut doc, 2), Err(LoadError::Version(2)));
    }
}

//! The bot versions compiled in (DESIGN.md P2.8.5): a code module plus its parameter schema.
//!
//! A bot change that should not move existing results is a new version, a deliberate copy
//! (`basic1/` and `params/basic-1.json` to `basic2/` and `basic-2.json`) with its own row here. A
//! version goes once no queued experiment or saved profile names it. `basic` names the latest,
//! which profiles that follow every change use; this replaces `profiles.freeze()`, which copied
//! Python source.

/// A version of the bot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum VersionId {
    /// The port of `citar/bots/basic.py` as of the swap.
    Basic1,
    /// Founds its capital, then does nothing (`citar/bots/idle.py`).
    Idle,
}

/// The version `basic` names.
pub const LATEST: VersionId = VersionId::Basic1;

impl VersionId {
    /// Its id, as profiles and the Bots page write it: `basic-1`, `idle`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Basic1 => "basic-1",
            Self::Idle => "idle",
        }
    }

    /// The version whose id is exactly `id`.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        VERSIONS.iter().map(|v| v.id).find(|v| v.id() == id)
    }

    /// The version `name` names: an id, or `basic` for the latest.
    #[must_use]
    pub fn resolve(name: &str) -> Option<Self> {
        if name == "basic" { Some(LATEST) } else { Self::from_id(name) }
    }

    /// The `DriverMemory` kind its memory is kept as, if it keeps one.
    #[must_use]
    pub const fn memory_kind(self) -> Option<u16> {
        match self {
            Self::Basic1 => Some(crate::memory::MEMORY_KIND),
            Self::Idle => None,
        }
    }
}

/// One row of [`VERSIONS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub id: VersionId,
    /// A short name for the Bots page.
    pub label: &'static str,
    /// What it is, in a sentence.
    pub description: &'static str,
    /// Whether `basic` names it.
    pub latest: bool,
    /// The `DriverMemory` kind of its memory; `None` for a version that keeps none.
    pub memory_kind: Option<u16>,
}

/// Every version, the latest first.
pub static VERSIONS: [Version; 2] = [
    Version {
        id: VersionId::Basic1,
        label: "Basic 1",
        description: "The scripted opponent of 0.1.5, ported to Rust: research by need, buildings \
                      valued by the city's what-if, policies, religion, great people, espionage, \
                      city-states, and wars with siege units and rally points.",
        latest: true,
        memory_kind: VersionId::Basic1.memory_kind(),
    },
    Version {
        id: VersionId::Idle,
        label: "Idle",
        description: "Founds its capital and then does nothing: a control, and a punching bag \
                      for conquest tests.",
        latest: false,
        memory_kind: VersionId::Idle.memory_kind(),
    },
];

/// The version ids, comma-separated, for messages.
pub(crate) fn names() -> String {
    VERSIONS.iter().map(|v| v.id.id()).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_resolve_and_basic_names_the_latest() {
        for v in &VERSIONS {
            assert_eq!(VersionId::from_id(v.id.id()), Some(v.id));
            assert_eq!(v.latest, v.id == LATEST);
        }
        assert_eq!(VersionId::resolve("basic"), Some(LATEST));
        assert_eq!(VersionId::from_id("basic"), None);
        assert_eq!(VersionId::resolve("frozen_1234567890"), None);
        assert_eq!(VERSIONS.iter().filter(|v| v.latest).count(), 1);
    }
}

//! The ruleset context of the save's codecs, and the forms of rule ids: a name in JSON, the
//! integer in `CANON_V1` (DESIGN.md 4.1, 4.9).
//!
//! A save names every rule object, so a save outlives a reordered or rebalanced ruleset and reads
//! as text. The state types derive their serde impls, so a rule id's `Serialize` cannot be handed
//! the ruleset; [`with_rules`] installs it for the call instead, as a scoped thread-local. Only the
//! save and journal codecs install it. Without it a rule id cannot be written or read as a name,
//! which is a serde error, never a panic.
//!
//! The digest needs no context: `CANON_V1` is not human-readable, and a rule id there is its
//! integer at its declared width.
//!
//! What names each kind of id:
//! - the 16 tables of [`NameKind`](crate::rules::NameKind): the object's name, read back exactly;
//! - a terrain feature ([`FeatureId`]): its terrain's name;
//! - city-state types, founded-religion names, quest kinds and ruin rewards: their names;
//! - map sizes, map types and barbarian levels: the lobby key a game's settings were made with;
//! - a unique ([`UniqueId`]): its key (`UniqueMeta::key`), 16 hex digits, which is its identity
//!   across rulesets: the same text on two sources has two keys, and editing one unique moves no
//!   other's;
//! - an ability ([`AbilityKey`]) and an interned text ([`TextId`]): the text.
//!
//! A name the loading ruleset does not have is recorded here as well as returned as a serde
//! error, so the loader can report it as `LoadError::UnknownName` with its place.
//!
//! Replaces nothing in Python, which saved names because it had nothing else.

use core::fmt;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};

use serde::de::{self, Deserializer, Visitor};
use serde::ser::{self, Serializer};

use crate::base::collections::LookupMap;
use crate::base::ids::{
    AbilityKey, BarbarianLevelId, BaseUnitId, BeliefId, BuildingId, CityStateTypeId, DifficultyId,
    EraId, FeatureId, Id, ImprovementId, MapSizeId, MapTypeId, NationId, PolicyId, PromotionId,
    QuestKindId, ResourceId, RuinId, RulesReligionId, SpecialistId, SpeedId, TechId, TerrainId,
    TextId, UniqueId, UnitTypeId, VictoryId,
};
use crate::rules::{Ruleset, RulesetId};

thread_local! {
    static RULES: Cell<Option<&'static Ruleset>> = const { Cell::new(None) };
    static UNKNOWN: RefCell<Option<Unknown>> = const { RefCell::new(None) };
    static REVERSE: RefCell<Option<Reverse>> = const { RefCell::new(None) };
}

/// Runs `f` with `rules` as the context rule ids are named by, and puts back whatever context
/// was there before, even if `f` panics. Contexts nest.
pub fn with_rules<R>(rules: &'static Ruleset, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<&'static Ruleset>);

    impl Drop for Restore {
        fn drop(&mut self) {
            RULES.with(|c| c.set(self.0));
        }
    }

    let _restore = Restore(RULES.with(|c| c.replace(Some(rules))));
    f()
}

/// The ruleset installed by [`with_rules`], if any.
#[must_use]
pub fn current() -> Option<&'static Ruleset> {
    RULES.with(Cell::get)
}

/// A name the ruleset in context did not have, as the last failed read recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    /// What kind of object: `building`.
    pub what: &'static str,
    /// The name as the save wrote it.
    pub name: String,
}

/// Takes the unknown name the last failed read recorded, if one did.
pub fn take_unknown() -> Option<Unknown> {
    UNKNOWN.with(|u| u.borrow_mut().take())
}

/// Forgets any unknown name recorded, before a read that will check for one.
pub fn clear_unknown() {
    UNKNOWN.with(|u| *u.borrow_mut() = None);
}

fn note_unknown(what: &'static str, name: &str) {
    UNKNOWN.with(|u| *u.borrow_mut() = Some(Unknown { what, name: name.to_owned() }));
}

const NO_CONTEXT: &str = "a rule id is written as a name, which needs a ruleset: call \
                          save::ctx::with_rules";

/// Lookups from a name back to an id that the ruleset keeps no index for: uniques by key, texts
/// and abilities by text. Built on first use for the ruleset in context.
struct Reverse {
    rules: RulesetId,
    uniques: Option<LookupMap<u64, UniqueId>>,
    texts: Option<LookupMap<&'static str, TextId>>,
    abilities: Option<LookupMap<&'static str, AbilityKey>>,
}

/// Runs `f` on the reverse indexes of `r`, starting them afresh when the context changed rulesets.
fn with_reverse<V>(r: &'static Ruleset, f: impl FnOnce(&mut Reverse) -> Option<V>) -> Option<V> {
    REVERSE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_none_or(|rev| rev.rules != r.id()) {
            *slot = Some(Reverse { rules: r.id(), uniques: None, texts: None, abilities: None });
        }
        f(slot.as_mut()?)
    })
}

// ---- The names of each kind of rule id --------------------------------------------------------

/// A rule id that a save writes as a name.
pub(crate) trait RuleName: Id {
    /// What it names, for messages: `building`.
    const WHAT: &'static str;

    /// Its name in `r`; `None` for an id `r` does not have.
    fn name_in(self, r: &'static Ruleset) -> Option<Cow<'static, str>>;

    /// The id `name` names in `r`, exactly.
    fn resolve_in(r: &'static Ruleset, name: &str) -> Option<Self>;
}

macro_rules! named_table {
    ($($id:ty => $what:literal),* $(,)?) => {$(
        impl RuleName for $id {
            const WHAT: &'static str = $what;

            fn name_in(self, r: &'static Ruleset) -> Option<Cow<'static, str>> {
                r.name(self).map(Cow::Borrowed)
            }

            fn resolve_in(r: &'static Ruleset, name: &str) -> Option<Self> {
                r.lookup(name)
            }
        }
    )*};
}

named_table! {
    TechId => "tech",
    BaseUnitId => "unit",
    BuildingId => "building",
    PromotionId => "promotion",
    TerrainId => "terrain",
    ResourceId => "resource",
    ImprovementId => "improvement",
    BeliefId => "belief",
    PolicyId => "policy",
    NationId => "nation",
    EraId => "era",
    SpecialistId => "specialist",
    SpeedId => "speed",
    DifficultyId => "difficulty",
    UnitTypeId => "unit type",
    VictoryId => "victory",
}

/// An id whose table holds its name in a field: found by scanning, as these tables are small.
macro_rules! scanned_table {
    ($($id:ty => $what:literal, |$r:ident| $table:expr, |$d:ident| $name:expr;)*) => {$(
        impl RuleName for $id {
            const WHAT: &'static str = $what;

            fn name_in(self, $r: &'static Ruleset) -> Option<Cow<'static, str>> {
                let $d = $table.get(self)?;
                Some(Cow::Borrowed($name))
            }

            fn resolve_in($r: &'static Ruleset, name: &str) -> Option<Self> {
                $table.iter().find(|(_, $d)| $name == name).map(|(id, _)| id)
            }
        }
    )*};
}

scanned_table! {
    CityStateTypeId => "city-state type", |r| r.city_state_types(), |d| &*d.name;
    RulesReligionId => "religion", |r| r.religions(), |d| AsRef::<str>::as_ref(d);
    QuestKindId => "quest", |r| r.quests(), |d| &*d.name;
    RuinId => "ruin reward", |r| r.ruins(), |d| &*d.name;
    MapSizeId => "map size", |r| r.constants().map_sizes, |d| &*d.key;
    MapTypeId => "map type", |r| r.constants().map_types, |d| &*d.key;
    BarbarianLevelId => "barbarian level", |r| r.constants().barbarian_levels, |d| &*d.key;
}

impl RuleName for FeatureId {
    const WHAT: &'static str = "feature";

    fn name_in(self, r: &'static Ruleset) -> Option<Cow<'static, str>> {
        let &t = r.derived().features.get(self)?;
        r.name(t).map(Cow::Borrowed)
    }

    fn resolve_in(r: &'static Ruleset, name: &str) -> Option<Self> {
        let t = r.lookup::<TerrainId>(name)?;
        r.derived().features.iter().find(|&(_, &x)| x == t).map(|(f, _)| f)
    }
}

impl RuleName for UniqueId {
    const WHAT: &'static str = "unique";

    fn name_in(self, r: &'static Ruleset) -> Option<Cow<'static, str>> {
        let m = r.uniques().meta.get(self)?;
        Some(Cow::Owned(format!("{:016x}", m.key)))
    }

    fn resolve_in(r: &'static Ruleset, name: &str) -> Option<Self> {
        if name.len() != 16 {
            return None;
        }
        let key = u64::from_str_radix(name, 16).ok()?;
        with_reverse(r, |rev| {
            let map = rev.uniques.get_or_insert_with(|| {
                let mut map = LookupMap::with_capacity(r.uniques().len());
                for (id, m) in r.uniques().meta.iter() {
                    map.insert(m.key, id);
                }
                map
            });
            map.get(&key).copied()
        })
    }
}

impl RuleName for TextId {
    const WHAT: &'static str = "text";

    fn name_in(self, r: &'static Ruleset) -> Option<Cow<'static, str>> {
        r.uniques().texts.get(self).map(|t| Cow::Borrowed(&**t))
    }

    fn resolve_in(r: &'static Ruleset, name: &str) -> Option<Self> {
        with_reverse(r, |rev| {
            let map = rev.texts.get_or_insert_with(|| {
                let texts = &r.uniques().texts;
                let mut map = LookupMap::with_capacity(texts.len());
                for (id, t) in texts.iter() {
                    map.insert(&**t, id);
                }
                map
            });
            map.get(name).copied()
        })
    }
}

impl RuleName for AbilityKey {
    const WHAT: &'static str = "ability";

    fn name_in(self, r: &'static Ruleset) -> Option<Cow<'static, str>> {
        let &t = r.uniques().abilities.get(self)?;
        r.uniques().texts.get(t).map(|t| Cow::Borrowed(&**t))
    }

    fn resolve_in(r: &'static Ruleset, name: &str) -> Option<Self> {
        with_reverse(r, |rev| {
            let map = rev.abilities.get_or_insert_with(|| {
                let u = r.uniques();
                let mut map = LookupMap::with_capacity(u.abilities.len());
                for (id, &t) in u.abilities.iter() {
                    if let Some(text) = u.texts.get(t) {
                        map.insert(&**text, id);
                    }
                }
                map
            });
            map.get(name).copied()
        })
    }
}

/// The name of `id` in the ruleset in context, for the save's own writers.
pub(crate) fn name_of<I: RuleName>(id: I) -> Result<Cow<'static, str>, String> {
    let r = current().ok_or_else(|| NO_CONTEXT.to_owned())?;
    id.name_in(r).ok_or_else(|| format!("{} {} is not in this ruleset", I::WHAT, id.index()))
}

/// The id `name` names in the ruleset in context; an unknown name is recorded for the loader.
pub(crate) fn resolve<I: RuleName>(name: &str) -> Result<I, String> {
    let r = current().ok_or_else(|| NO_CONTEXT.to_owned())?;
    I::resolve_in(r, name).ok_or_else(|| {
        note_unknown(I::WHAT, name);
        format!("unknown {} {name:?}", I::WHAT)
    })
}

/// Writes a rule id: its name in a human-readable format, else `raw`.
pub(crate) fn serialize_rule_id<I: RuleName, S: Serializer>(
    id: I,
    s: S,
    raw: impl FnOnce(S) -> Result<S::Ok, S::Error>,
) -> Result<S::Ok, S::Error> {
    if s.is_human_readable() {
        let name = name_of(id).map_err(ser::Error::custom)?;
        s.serialize_str(&name)
    } else {
        raw(s)
    }
}

/// Reads a rule id: by name in a human-readable format, else by its integer.
pub(crate) fn deserialize_rule_id<'de, I: RuleName, D: Deserializer<'de>>(
    d: D,
) -> Result<I, D::Error> {
    struct ByName<I>(core::marker::PhantomData<I>);

    impl<I: RuleName> Visitor<'_> for ByName<I> {
        type Value = I;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "the name of a {}", I::WHAT)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<I, E> {
            resolve(v).map_err(E::custom)
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<I, E> {
            usize::try_from(v)
                .ok()
                .and_then(I::from_index)
                .ok_or_else(|| E::custom(format!("{v} is no {} id", I::WHAT)))
        }
    }

    let visitor = ByName::<I>(core::marker::PhantomData);
    if d.is_human_readable() { d.deserialize_str(visitor) } else { d.deserialize_u64(visitor) }
}

macro_rules! rule_id_serde {
    ($($id:ty => $ser:ident),* $(,)?) => {$(
        impl serde::Serialize for $id {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let raw = self.0;
                serialize_rule_id(*self, s, |s| s.$ser(raw))
            }
        }

        impl<'de> serde::Deserialize<'de> for $id {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                deserialize_rule_id(d)
            }
        }
    )*};
}

rule_id_serde! {
    TechId => serialize_u16,
    BaseUnitId => serialize_u16,
    BuildingId => serialize_u16,
    PromotionId => serialize_u16,
    TerrainId => serialize_u8,
    ResourceId => serialize_u8,
    ImprovementId => serialize_u8,
    BeliefId => serialize_u16,
    PolicyId => serialize_u16,
    NationId => serialize_u16,
    EraId => serialize_u8,
    SpecialistId => serialize_u8,
    SpeedId => serialize_u8,
    DifficultyId => serialize_u8,
    UnitTypeId => serialize_u16,
    VictoryId => serialize_u8,
    FeatureId => serialize_u8,
    CityStateTypeId => serialize_u8,
    RulesReligionId => serialize_u8,
    QuestKindId => serialize_u16,
    RuinId => serialize_u16,
    MapSizeId => serialize_u8,
    MapTypeId => serialize_u8,
    BarbarianLevelId => serialize_u8,
    UniqueId => serialize_u16,
    TextId => serialize_u32,
    AbilityKey => serialize_u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::digest::to_canon_vec;

    #[cfg(feature = "embedded-ruleset")]
    #[test]
    fn rule_ids_are_names_in_json_and_integers_in_the_digest() -> Result<(), serde_json::Error> {
        let r = Ruleset::shared();
        let tech = r.lookup::<TechId>("Pottery").expect("Pottery");
        let json = with_rules(r, || serde_json::to_string(&tech))?;
        assert_eq!(json, r#""Pottery""#);
        let back: TechId = with_rules(r, || serde_json::from_str(&json))?;
        assert_eq!(back, tech);
        assert_eq!(to_canon_vec(&tech).expect("canon"), tech.0.to_le_bytes());

        clear_unknown();
        let e = with_rules(r, || serde_json::from_str::<BuildingId>(r#""Moon Base""#));
        assert!(e.is_err());
        assert_eq!(take_unknown(), Some(Unknown { what: "building", name: "Moon Base".into() }));

        let u = UniqueId(3);
        let key = with_rules(r, || serde_json::to_string(&u))?;
        assert_eq!(key.len(), 18, "16 hex digits, quoted: {key}");
        assert_eq!(with_rules(r, || serde_json::from_str::<UniqueId>(&key))?, u);
        for (id, _) in r.uniques().meta.iter().take(50) {
            let k = with_rules(r, || serde_json::to_string(&id))?;
            assert_eq!(with_rules(r, || serde_json::from_str::<UniqueId>(&k))?, id);
        }
        let hill = r.derived().known.hill;
        let name = with_rules(r, || serde_json::to_string(&hill))?;
        assert_eq!(name, r#""Hill""#);
        assert_eq!(with_rules(r, || serde_json::from_str::<FeatureId>(&name))?, hill);
        let size = with_rules(r, || serde_json::from_str::<MapSizeId>(r#""small""#))?;
        assert_eq!(r.map_sizes().get(size).map(|m| &*m.key), Some("small"));
        Ok(())
    }

    #[test]
    fn without_a_context_a_name_is_an_error_not_a_panic() {
        assert!(current().is_none());
        let e = serde_json::to_string(&TechId(0)).expect_err("no context");
        assert!(e.to_string().contains("with_rules"), "{e}");
        assert!(serde_json::from_str::<TechId>(r#""Pottery""#).is_err());
        // The digest needs no names.
        assert_eq!(to_canon_vec(&TechId(7)).expect("canon"), [7, 0]);
    }

    #[cfg(feature = "embedded-ruleset")]
    #[test]
    fn contexts_nest_and_are_put_back_after_a_panic() {
        let r = Ruleset::shared();
        with_rules(r, || {
            assert!(current().is_some());
            with_rules(r, || assert!(current().is_some()));
            assert!(current().is_some());
        });
        assert!(current().is_none());
        let caught = std::panic::catch_unwind(|| with_rules(r, || panic!("inside")));
        assert!(caught.is_err());
        assert!(current().is_none(), "the context was put back as the panic unwound");
    }
}

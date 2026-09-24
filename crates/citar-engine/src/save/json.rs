//! Save format v1: the whole state as one JSON document, and loading it back (DESIGN.md 4.9).
//!
//! The top-level keys, in the order they are written:
//!
//! ```json
//! {"format": "citar-state", "version": 1, "engine": "0.1.6+<BUILD_ID>", "rules": {"id": "<hex>"},
//!  "config": {...}, "map": {...}, "tiles": {...}, "clock": {...}, "ids": {...},
//!  "chronicle": {...}, "host": {...}, "players": [...], "units": [...], "cities": [...],
//!  "diplomacy": {"relations": [[lo, hi, {...}]], "opinions": [[holder, about, {...}]],
//!                "deals": [...], "negotiations": [...]},
//!  "world": {...}}
//! ```
//!
//! Everything below the top level is the state types' own derives, with rule ids written as
//! names (`save::ctx`), the map and memories as columns (`save::columns`), and the forms this
//! module gives the containers:
//! - units and cities as lists in id order (the entity stores' live items);
//! - relations as the pairs that differ from a fresh relation, `[lo, hi, relation]`, in the
//!   matrix's order; opinions as `[holder, about, {reason: value}]`, each value that is not
//!   `+0.0`;
//! - a driver's memory as `{"kind", "version", "bytes": "<base64>"}`.
//!
//! Floats are written by ryu and read back exactly (`float_roundtrip`), `-0.0` included, so
//! `load(save(s)) == s` bit for bit and a save written again is the same bytes. A NaN or an
//! infinity is refused before anything is written: JSON would write it as `null`.
//!
//! Loading reads the document into a JSON value, upgrades an older version (`save::migrate`),
//! refuses an unknown top-level key in every build, reads each part with the ruleset in context
//! (a name that no longer resolves is `LoadError::UnknownName`, with its place), puts the lists
//! kept sorted by rule id back in order (the names may have other ids now), builds the state with
//! `State::from_parts`, which checks the parts fit, and then `save::validate`s it. A save made
//! under another ruleset, reordered or retuned, loads, with `LoadReport::rules_changed` set.
//!
//! Replaces the save path of `citar/session.py` (`GameState.to_dict` and `from_dict`,
//! `state.py:13-405`); Python saved `rng_state` too, which keyed draws make needless.

use serde::de::{self, DeserializeOwned, Deserializer};
use serde::ser::{SerializeSeq, SerializeStruct, SerializeTuple, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::ctx::{self, with_rules};
use super::journal;
use super::migrate;
use super::validate::validate;
use super::{LoadError, LoadReport, Loaded, SaveError, canon};
use crate::base::codec::{b64_decode, b64_encode};
use crate::base::ids::PlayerId;
use crate::base::sets::{PlayerSet, PlayerVec};
use crate::rules::{BUILD_ID, Ruleset, RulesetId};
use crate::state::chronicle::{ChronicleHeads, HostHeads};
use crate::state::cities::{Cities, City};
use crate::state::config::{GameConfig, HostOnly};
use crate::state::diplo::{
    Deal, Diplomacy, Negotiation, OpinionBook, OpinionKey, PairMatrix, Relation,
};
use crate::state::map::{MapInfo, Tiles};
use crate::state::players::{DriverMemory, Player};
use crate::state::units::{Unit, Units};
use crate::state::world::World;
use crate::state::{IdCounters, State, StateParts, TurnClock};

/// The document's `format`.
pub const FORMAT: &str = "citar-state";

/// The top-level keys, in the order they are written.
pub const KEYS: [&str; 16] = [
    "format",
    "version",
    "engine",
    "rules",
    "config",
    "map",
    "tiles",
    "clock",
    "ids",
    "chronicle",
    "host",
    "players",
    "units",
    "cities",
    "diplomacy",
    "world",
];

/// The engine version a save names: the crate's version and the build.
#[must_use]
pub fn engine_version() -> String {
    format!("{}+{BUILD_ID}", env!("CARGO_PKG_VERSION"))
}

// ---- Container forms --------------------------------------------------------------------------

/// The units in id order, in both encodings (the store's live items).
impl Serialize for Units {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len()))?;
        for u in self.iter() {
            seq.serialize_element(u)?;
        }
        seq.end()
    }
}

/// The cities in id order, in both encodings.
impl Serialize for Cities {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len()))?;
        for c in self.iter() {
            seq.serialize_element(c)?;
        }
        seq.end()
    }
}

/// Bytes the canonical writer passes on in one piece.
struct Raw<'a>(&'a [u8]);

impl Serialize for Raw<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(self.0)
    }
}

/// JSON: `{"kind", "version", "bytes": "<base64>"}`. `CANON_V1`: kind, version, then the length
/// and the bytes.
impl Serialize for DriverMemory {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            let mut st = s.serialize_struct("DriverMemory", 3)?;
            st.serialize_field("kind", &self.kind())?;
            st.serialize_field("version", &self.version())?;
            st.serialize_field("bytes", &b64_encode(self.bytes()))?;
            st.end()
        } else {
            let mut t = s.serialize_tuple(3)?;
            t.serialize_element(&self.kind())?;
            t.serialize_element(&self.version())?;
            t.serialize_element(&Raw(self.bytes()))?;
            t.end()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverDoc {
    kind: u16,
    version: u16,
    bytes: String,
}

/// Held to `DriverMemory::MAX_LEN`, refused before decoding when the text alone is too long.
impl<'de> Deserialize<'de> for DriverMemory {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let doc = DriverDoc::deserialize(d)?;
        if doc.bytes.len() > DriverMemory::MAX_LEN.div_ceil(3) * 4 {
            return Err(de::Error::custom(format!(
                "a driver's memory of {} base64 characters is over the limit of {} bytes",
                doc.bytes.len(),
                DriverMemory::MAX_LEN
            )));
        }
        let bytes = b64_decode(&doc.bytes).map_err(de::Error::custom)?;
        DriverMemory::new(doc.kind, doc.version, bytes).map_err(de::Error::custom)
    }
}

/// A relation matrix: in JSON the pairs whose relation differs from a fresh one, in `CANON_V1`
/// the player count and every cell.
struct Relations<'a>(&'a PairMatrix<Relation>);

impl Serialize for Relations<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            let fresh = Relation::default();
            s.collect_seq(self.0.pairs().filter(|(_, _, r)| **r != fresh))
        } else {
            (self.0.n(), self.0.cells()).serialize(s)
        }
    }
}

/// One pair's opinions: in JSON the reasons whose value is not `+0.0`, by name; in `CANON_V1`
/// all sixteen.
struct OpinionRow<'a>(&'a [f64; OpinionKey::COUNT]);

impl Serialize for OpinionRow<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.collect_map(
                OpinionKey::ALL
                    .iter()
                    .zip(self.0)
                    .filter(|(_, v)| v.to_bits() != 0)
                    .map(|(k, v)| (k.name(), v)),
            )
        } else {
            self.0.serialize(s)
        }
    }
}

/// Every holder's opinions: in JSON `[holder, about, {reason: value}]`, in `CANON_V1` a map by
/// pair.
struct Opinions<'a>(&'a OpinionBook);

impl Serialize for Opinions<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            s.collect_seq(self.0.iter().map(|((h, a), v)| (h, a, OpinionRow(v))))
        } else {
            s.collect_map(self.0.iter().map(|(k, v)| (k, OpinionRow(v))))
        }
    }
}

/// The relations, opinions, deals and negotiations; the war and contact masks are rebuilt on
/// load.
impl Serialize for Diplomacy {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            let mut st = s.serialize_struct("Diplomacy", 4)?;
            st.serialize_field("relations", &Relations(self.relations()))?;
            st.serialize_field("opinions", &Opinions(&self.opinions))?;
            st.serialize_field("deals", &self.deals)?;
            st.serialize_field("negotiations", &self.negotiations)?;
            st.end()
        } else {
            let mut t = s.serialize_tuple(4)?;
            t.serialize_element(&Relations(self.relations()))?;
            t.serialize_element(&Opinions(&self.opinions))?;
            t.serialize_element(&self.deals)?;
            t.serialize_element(&self.negotiations)?;
            t.end()
        }
    }
}

/// One pair's opinions as a save lists them, by reason name.
struct OpinionValues([f64; OpinionKey::COUNT]);

impl<'de> Deserialize<'de> for OpinionValues {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let named = std::collections::BTreeMap::<String, f64>::deserialize(d)?;
        let mut out = [0.0; OpinionKey::COUNT];
        for (name, v) in named {
            let k = OpinionKey::from_name(&name)
                .ok_or_else(|| de::Error::custom(format!("{name:?} is no opinion reason")))?;
            out[k as usize] = v;
        }
        Ok(Self(out))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiploDoc {
    relations: Vec<(PlayerId, PlayerId, Relation)>,
    opinions: Vec<(PlayerId, PlayerId, OpinionValues)>,
    deals: Vec<Deal>,
    negotiations: Vec<Negotiation>,
}

impl DiploDoc {
    /// The diplomacy of `n` players these lists describe.
    fn build(self, n: u8) -> Result<Diplomacy, LoadError> {
        let bad = |m: String| LoadError::invalid("diplomacy", m);
        let mut relations = PairMatrix::<Relation>::new(n);
        let mut last: Option<(PlayerId, PlayerId)> = None;
        for (lo, hi, r) in self.relations {
            if lo >= hi || hi.0 >= n {
                return Err(bad(format!("relation [{lo}, {hi}] is not a pair of {n} players")));
            }
            // Written in the matrix's order, by the higher id, then the lower.
            if last.is_some_and(|(l, h)| (h, l) >= (hi, lo)) {
                return Err(bad(format!("relation [{lo}, {hi}] is out of order or repeated")));
            }
            last = Some((lo, hi));
            if let Some(cell) = relations.get_mut(lo, hi) {
                *cell = r;
            }
        }
        let mut diplo = Diplomacy::from_relations(relations, Default::default());
        let mut seen = std::collections::BTreeSet::new();
        for (h, a, v) in self.opinions {
            if h == a || h.0 >= n || a.0 >= n {
                return Err(bad(format!("an opinion of {h} about {a}")));
            }
            if !seen.insert((h, a)) {
                return Err(bad(format!("the opinions of {h} about {a} are listed twice")));
            }
            diplo.opinions.insert(h, a, v.0);
        }
        diplo.deals = self.deals;
        diplo.negotiations = self.negotiations;
        Ok(diplo)
    }
}

// ---- The document -----------------------------------------------------------------------------

#[derive(Serialize)]
struct RulesRef {
    id: String,
}

/// The document as written: the state's parts, borrowed, behind the header.
#[derive(Serialize)]
struct SaveRef<'a> {
    format: &'static str,
    version: u32,
    engine: String,
    rules: RulesRef,
    config: &'a GameConfig,
    map: &'a MapInfo,
    tiles: &'a Tiles,
    clock: &'a TurnClock,
    ids: &'a IdCounters,
    chronicle: &'a ChronicleHeads,
    host: &'a HostHeads,
    players: &'a PlayerVec<Player>,
    units: &'a Units,
    cities: &'a Cities,
    diplomacy: &'a Diplomacy,
    world: &'a World,
}

/// The save of `st` under `rules`: format v1 JSON, compact.
///
/// Refused if a float is not finite (JSON would write `null`) or a rule id is not in `rules`.
pub fn to_json(rules: &'static Ruleset, st: &State) -> Result<Vec<u8>, SaveError> {
    canon::check_finite(st).map_err(SaveError::NonFinite)?;
    let doc = SaveRef {
        format: FORMAT,
        version: migrate::CURRENT,
        engine: engine_version(),
        rules: RulesRef { id: rules.id().to_hex() },
        config: st.config(),
        map: st.map(),
        tiles: st.tiles(),
        clock: st.clock(),
        ids: st.ids(),
        chronicle: st.chronicle(),
        host: st.host(),
        players: st.players(),
        units: st.units(),
        cities: st.cities(),
        diplomacy: st.diplo(),
        world: st.world(),
    };
    with_rules(rules, || serde_json::to_vec(&doc)).map_err(|e| SaveError::Json(e.to_string()))
}

/// Reads one part of the document, naming its place in any error.
fn read<T: DeserializeOwned>(v: &Value, path: &str) -> Result<T, LoadError> {
    ctx::clear_unknown();
    T::deserialize(v).map_err(|e| match ctx::take_unknown() {
        Some(u) => LoadError::UnknownName { path: format!("{path} ({})", u.what), name: u.name },
        None => LoadError::Json(format!("{path}: {e}")),
    })
}

/// Reads a list part item by item, so an error names the item.
fn read_list<T: DeserializeOwned>(v: &Value, path: &str) -> Result<Vec<T>, LoadError> {
    let items = v.as_array().ok_or_else(|| LoadError::Json(format!("{path}: not a list")))?;
    items.iter().enumerate().map(|(i, x)| read(x, &format!("{path}[{i}]"))).collect()
}

/// The document's top level, upgraded to the current version, with every key checked.
fn document(bytes: &[u8]) -> Result<Map<String, Value>, LoadError> {
    let mut doc: Value =
        serde_json::from_slice(bytes).map_err(|e| LoadError::Json(e.to_string()))?;
    let obj = doc.as_object().ok_or_else(|| LoadError::Json("not a JSON object".to_owned()))?;
    if obj.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(LoadError::Json(format!("not a {FORMAT} document")));
    }
    let version = obj
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| LoadError::Json("no version".to_owned()))?;
    let version = u32::try_from(version).map_err(|_| LoadError::Version(u32::MAX))?;
    migrate::upgrade(&mut doc, version)?;
    let Value::Object(obj) = doc else {
        return Err(LoadError::Json("not a JSON object".to_owned()));
    };
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(LoadError::UnknownKey(k.clone()));
    }
    if let Some(k) = KEYS.iter().find(|k| !obj.contains_key(**k)) {
        return Err(LoadError::Json(format!("no {k:?}")));
    }
    Ok(obj)
}

/// A save's state and report, without its history: the document read, the state built from its
/// parts and validated.
pub fn read_state(rules: &'static Ruleset, bytes: &[u8]) -> Result<(State, LoadReport), LoadError> {
    let doc = document(bytes)?;
    let key = |k: &str| doc.get(k).unwrap_or(&Value::Null);
    let engine = key("engine")
        .as_str()
        .ok_or_else(|| LoadError::Json("engine: not a string".to_owned()))?
        .to_owned();
    let saved = key("rules")
        .as_object()
        .filter(|m| m.len() == 1)
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .and_then(RulesetId::from_hex)
        .ok_or_else(|| LoadError::Json("rules: not {\"id\": \"<64 hex digits>\"}".to_owned()))?;
    let report = LoadReport {
        rules_changed: (saved != rules.id()).then(|| (saved, engine.clone())),
        chronicle_incomplete: false,
        engine,
    };
    let st = with_rules(rules, || build(key))?;
    validate(&st, rules).map_err(LoadError::Invalid)?;
    Ok((st, report))
}

/// Puts back in order the lists kept sorted by rule id. A save made under a reordered ruleset
/// names the same objects, which now have other ids, so its lists read out of order
/// (DESIGN.md 4.9). The sort is stable: an object named twice stays twice, for `validate` to
/// refuse.
fn sort_by_rule_id(config: &mut GameConfig, players: &mut [Player], units: &mut [Unit]) {
    config.disabled_victories.sort();
    let res = &mut config.resources;
    for kind in [&mut res.strategic, &mut res.luxury, &mut res.bonus] {
        kind.each.sort_by_key(|&(id, _)| id);
    }
    for p in players {
        p.civ.free_specific_buildings.sort();
    }
    for u in units {
        u.abilities_used.sort_by_key(|&(key, _)| key);
    }
}

/// The state the document's parts make.
fn build<'a>(key: impl Fn(&str) -> &'a Value) -> Result<State, LoadError> {
    let map: MapInfo = read(key("map"), "map")?;
    // The unit indexes are sized by the map, so its shape is checked before anything else.
    map.grid().map_err(|e| LoadError::invalid("map", e.to_string()))?;
    let tiles: Tiles = read(key("tiles"), "tiles")?;
    let mut players: Vec<Player> = read_list(key("players"), "players")?;
    // The relations' contact and war masks are player sets, so the count is checked before
    // they are built from it.
    let n = u8::try_from(players.len())
        .ok()
        .filter(|&n| usize::from(n) <= PlayerSet::CAPACITY)
        .ok_or_else(|| {
            LoadError::invalid("players", format!("{} players; at most 64 fit", players.len()))
        })?;
    let mut units: Vec<Unit> = read_list(key("units"), "units")?;
    let cities: Vec<City> = read_list(key("cities"), "cities")?;
    let diplo: DiploDoc = read(key("diplomacy"), "diplomacy")?;
    let mut config: GameConfig = read(key("config"), "config")?;
    sort_by_rule_id(&mut config, &mut players, &mut units);
    let units = Units::from_units(units, map.size())
        .map_err(|e| LoadError::invalid("units", e.to_string()))?;
    let cities =
        Cities::from_cities(cities).map_err(|e| LoadError::invalid("cities", e.to_string()))?;
    let parts = StateParts {
        config,
        map,
        tiles,
        players: PlayerVec::from_vec(players),
        units,
        cities,
        diplo: diplo.build(n)?,
        world: read(key("world"), "world")?,
        clock: read(key("clock"), "clock")?,
        ids: read(key("ids"), "ids")?,
        chronicle: read(key("chronicle"), "chronicle")?,
        host: HostOnly(read(key("host"), "host")?),
    };
    State::from_parts(parts).map_err(|e| LoadError::invalid("state", e.to_string()))
}

/// Loads a save and its history: the state from `state_json`, the chronicle rebuilt from the
/// journal chunks' payloads, oldest first (DESIGN.md 4.9, 4.11).
///
/// The history never stops a load: chunks that are missing, unreadable or that disagree with the
/// state's heads set `LoadReport::chronicle_incomplete`.
pub fn load(
    rules: &'static Ruleset,
    state_json: &[u8],
    chunks: &mut dyn Iterator<Item = &[u8]>,
) -> Result<Loaded, LoadError> {
    let (state, mut report) = read_state(rules, state_json)?;
    let (chronicle, complete) = journal::rebuild(rules, chunks, state.chronicle(), state.host());
    report.chronicle_incomplete = !complete;
    Ok(Loaded { state, chronicle, report })
}

/// Reads an opinion row, for the columns' tests.
#[cfg(test)]
pub(crate) fn opinion_row(v: &Value) -> Result<[f64; OpinionKey::COUNT], serde_json::Error> {
    OpinionValues::deserialize(v).map(|o| o.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opinion_rows_name_their_reasons() -> Result<(), serde_json::Error> {
        let row = opinion_row(&serde_json::json!({"warmonger": -10.0, "scenario": -0.0}))?;
        assert_eq!(row[OpinionKey::Warmonger as usize].to_bits(), (-10.0f64).to_bits());
        assert_eq!(row[OpinionKey::Scenario as usize].to_bits(), (-0.0f64).to_bits());
        assert!(opinion_row(&serde_json::json!({"grudge": 1.0})).is_err());
        Ok(())
    }
}

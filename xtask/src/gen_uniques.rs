//! `cargo xtask gen-uniques`: writes `crates/citar-engine/src/unique/gen.rs` from
//! `unique_types.tsv` and `unique_supported.toml` (DESIGN.md 5.4).
//!
//! The generator lives here, in Rust, so the engine's build never needs Python. The output is
//! committed, and `cargo xtask check` regenerates it in memory and fails on any difference, so
//! gen.rs cannot drift from its two sources.
//!
//! What it writes:
//! - `UniqueType`, UnCiv's 637 types in the TSV's order, with `TYPE_INFO` (name, placeholder,
//!   signature, and for the supported ones the role, parameter kinds, field names and stages)
//!   and `BY_PLACEHOLDER`, sorted for binary search;
//! - `ParamKind` and `Stage`, from the tables below;
//! - one payload struct per supported type with parameters, in `mod p`, each asserted to fit 12
//!   bytes at 4-byte alignment;
//! - `UniqueData` (the main types), `CondData` (conditionals), `TriggerCond` (triggers) and
//!   `ModifierData` (action and meta modifiers), each with `ty`, `params` and a builder that
//!   compiles the parameters in order through `ParamCx`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

/// Appends a formatted line to a `String`: `writeln!` returns a `Result` that a `String` never
/// fails, and the workspace denies discarding it with `let _`.
macro_rules! wl {
    ($o:expr, $($t:tt)*) => {{
        $o.push_str(&format!($($t)*));
        $o.push('\n');
    }};
}

/// Where the output goes, relative to the workspace root.
pub const OUT: &str = "crates/citar-engine/src/unique/gen.rs";
const TSV: &str = "crates/citar-engine/unique_types.tsv";
const TOML: &str = "crates/citar-engine/unique_supported.toml";

/// The largest payload a main type may have, and its alignment (DESIGN.md 5.5): with the `u16`
/// tag, `UniqueData` is then 16 bytes.
const PAYLOAD_MAX: usize = 12;

/// A parameter kind: the name UnCiv's signature uses, the `ParamKind` variant, the field type
/// in a payload, the type's size and alignment, and what it is.
struct Kind {
    sig: &'static str,
    variant: &'static str,
    ty: &'static str,
    size: usize,
    align: usize,
    doc: &'static str,
}

const fn kind(
    sig: &'static str,
    variant: &'static str,
    ty: &'static str,
    size: usize,
    align: usize,
    doc: &'static str,
) -> Kind {
    Kind { sig, variant, ty, size, align, doc }
}

/// Every parameter kind the engine compiles. `amount16` and `promotionOrStatus` are not UnCiv's:
/// they are overrides `unique_supported.toml` writes after a field's name (`name:amount16`).
const KINDS: &[Kind] = &[
    kind("amount", "Amount", "i32", 4, 4, "a whole number"),
    kind("amount16", "Amount16", "i16", 2, 2, "a whole number stored in 16 bits"),
    kind("relativeAmount", "RelativeAmount", "i32", 4, 4, "a change in percent: `+15`, `-33`"),
    kind("positiveAmount", "PositiveAmount", "i32", 4, 4, "a whole number, at least 1"),
    kind("nonNegativeAmount", "NonNegativeAmount", "i32", 4, 4, "a whole number, at least 0"),
    kind("fraction", "Fraction", "FracId", 2, 2, "a real number, interned in `Ruleset::fracs`"),
    kind("stats", "Stats", "StatsId", 2, 2, "stats such as `+1 Food, +2 Gold`, interned"),
    kind("stat", "Stat", "Stat", 1, 1, "one stat: `Gold`"),
    kind("civWideStat", "CivWideStat", "Stat", 1, 1, "a stat pooled per civilization"),
    kind("stockpile", "Stockpile", "Stat", 1, 1, "a stat a civilization stockpiles"),
    kind("stat/resource", "StatOrResource", "StatOrResource", 2, 1, "a stat or a resource"),
    kind("cityFilter", "CityFilter", "CityFilterId", 2, 2, "a city filter"),
    kind("mapUnitFilter", "MapUnitFilter", "UnitFilterId", 2, 2, "a filter on units on the map"),
    kind("civFilter", "CivFilter", "CivFilterId", 2, 2, "a civilization filter"),
    kind("combatantFilter", "CombatantFilter", "CombatantFilterId", 2, 2, "a combatant filter"),
    kind("tileFilter", "TileFilter", "TileFilterId", 2, 2, "a tile filter"),
    kind("terrainFilter", "TerrainFilter", "TileFilterId", 2, 2, "a filter on a tile's terrain"),
    kind(
        "simpleTerrain",
        "SimpleTerrain",
        "TileFilterId",
        2,
        2,
        "a terrain, `Land`, `Water` or `Elevated`",
    ),
    kind("baseUnitFilter", "BaseUnitFilter", "SetRef", 2, 2, "a filter on the units of units.json"),
    kind("buildingFilter", "BuildingFilter", "SetRef", 2, 2, "a building filter"),
    kind("improvementFilter", "ImprovementFilter", "SetRef", 2, 2, "an improvement filter"),
    kind("resourceFilter", "ResourceFilter", "SetRef", 2, 2, "a resource filter"),
    kind("techFilter", "TechFilter", "SetRef", 2, 2, "a technology filter"),
    kind("eraFilter", "EraFilter", "SetRef", 2, 2, "an era filter"),
    kind(
        "tileFilter/buildingFilter",
        "TileOrBuildingFilter",
        "ObjectFilterId",
        2,
        2,
        "a tile or building filter",
    ),
    kind(
        "tileFilter/specialist/buildingFilter",
        "TileSpecialistOrBuildingFilter",
        "ObjectFilterId",
        2,
        2,
        "a tile filter, a specialist or a building filter",
    ),
    kind(
        "improvementFilter/terrainFilter",
        "ImprovementOrTerrainFilter",
        "ObjectFilterId",
        2,
        2,
        "an improvement or terrain filter",
    ),
    kind("buildingName", "BuildingName", "BuildingId", 2, 2, "a building, by name"),
    kind("unit", "Unit", "BaseUnitId", 2, 2, "a unit of units.json, by name"),
    kind("greatPerson", "GreatPerson", "BaseUnitId", 2, 2, "a great person's unit, by name"),
    kind("promotion", "Promotion", "PromotionId", 2, 2, "a promotion, by name"),
    kind(
        "promotionOrStatus",
        "PromotionOrStatus",
        "PromotionOrStatus",
        4,
        2,
        "a promotion by name, or the status `Set Up`",
    ),
    kind("resource", "Resource", "ResourceId", 1, 1, "a resource, by name"),
    kind("tech", "Tech", "TechId", 2, 2, "a technology, by name"),
    kind("era", "Era", "EraId", 1, 1, "an era, by name"),
    kind("difficulty", "Difficulty", "DifficultyId", 1, 1, "a difficulty, by name"),
    kind("speed", "Speed", "SpeedId", 1, 1, "a game speed, by name"),
    kind("victoryType", "VictoryType", "VictoryId", 1, 1, "a victory, by name"),
    kind("terrainName", "TerrainName", "TerrainId", 1, 1, "a terrain, by name"),
    kind("terrainFeature", "TerrainFeature", "FeatureId", 1, 1, "a terrain feature, by name"),
    kind(
        "baseTerrain/terrainFeature",
        "BaseTerrainOrFeature",
        "TerrainId",
        1,
        1,
        "a base terrain or a feature, by name",
    ),
    kind(
        "policy/belief",
        "PolicyOrBelief",
        "PolicyOrBelief",
        4,
        2,
        "a policy or a belief, by name",
    ),
    kind("populationFilter", "PopulationFilter", "PopulationFilter", 2, 1, "which citizens count"),
    kind("costOrStrength", "CostOrStrength", "CostOrStrength", 1, 1, "`Cost` or `Strength`"),
    kind(
        "foundingOrEnhancing",
        "FoundingOrEnhancing",
        "FoundingOrEnhancing",
        1,
        1,
        "`founding` or `enhancing`",
    ),
    kind("beliefType", "BeliefType", "BeliefKind", 1, 1, "a belief type, or `Any`"),
    kind("spyAction", "SpyAction", "SpyAction", 1, 1, "what a spy is doing: `Stealing Tech`"),
    kind(
        "terrainQuality",
        "TerrainQuality",
        "TerrainQuality",
        1,
        1,
        "how a start values a terrain",
    ),
    kind("regionType", "RegionType", "RegionType", 2, 1, "a kind of start region"),
    kind(
        "unitTriggerTarget",
        "UnitTriggerTarget",
        "UnitTriggerTarget",
        1,
        1,
        "the unit a one-time effect acts on",
    ),
    kind("countable", "Countable", "Countable", 8, 4, "something counted: a number, `Cities`, ..."),
    kind("positiveAmount/'all'", "CountOrAll", "CountOrAll", 4, 4, "a count, or `All`"),
    kind("comment", "Comment", "TextId", 4, 4, "free text, interned"),
    kind("pediaLink", "PediaLink", "TextId", 4, 4, "a Civilopedia link, as text"),
    kind(
        "validationWarning",
        "ValidationWarning",
        "TextId",
        4,
        4,
        "a ruleset checker's warning, as text",
    ),
];

/// The roles of `unique_supported.toml`, and which generated enum holds each.
const ROLES: &[(&str, &str, Holder)] = &[
    ("effect", "Effect", Holder::Main),
    ("flag", "Flag", Holder::Main),
    ("requirement", "Requirement", Holder::Main),
    ("one_time", "OneTime", Holder::Main),
    ("action", "Action", Holder::Main),
    ("ai", "Ai", Holder::Main),
    ("mapgen", "Mapgen", Holder::Main),
    ("inert", "Inert", Holder::Main),
    ("cond", "Cond", Holder::Cond),
    ("trigger", "Trigger", Holder::Trigger),
    ("action_mod", "ActionMod", Holder::Modifier),
    ("meta", "Meta", Holder::Modifier),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Holder {
    Main,
    Cond,
    Trigger,
    Modifier,
}

/// The engine systems a type's `stages` may name: (name, `Stage` variant, what it is).
const STAGES: &[(&str, &str, &str)] = &[
    ("actions", "Actions", "unit actions (`game::actions`)"),
    ("ai", "Ai", "the bot (`citar/bots/basic.py`, Phase 2's citar-bot)"),
    ("api", "Api", "tools, views and the briefing (`api`)"),
    ("automation", "Automation", "unit automation (`game::automation`)"),
    ("barbarians", "Barbarians", "barbarians (`game::barbarians`)"),
    ("cities", "Cities", "cities (`game::cities`)"),
    ("city_states", "CityStates", "city-states (`game::city_states`)"),
    ("combat", "Combat", "combat (`game::combat`)"),
    ("conquest", "Conquest", "conquest (`game::conquest`)"),
    ("diplomacy", "Diplomacy", "diplomacy (`game::diplomacy`)"),
    ("economy", "Economy", "the civilization's economy (`game::economy`)"),
    ("espionage", "Espionage", "espionage (`game::espionage`)"),
    ("great_people", "GreatPeople", "great people and golden ages (`game::great_people`)"),
    ("mapgen", "Mapgen", "map generation (`mapgen`)"),
    ("movement", "Movement", "movement (`game::movement`, `game::path`)"),
    ("policies", "Policies", "social policies (`game::policies`)"),
    ("religion", "Religion", "religion (`game::religion`)"),
    ("research", "Research", "research (`game::research`)"),
    ("rules", "Rules", "the ruleset's derived tables (`rules::derived`)"),
    ("ruins", "Ruins", "ancient ruins (`game::ruins`)"),
    ("setup", "Setup", "new-game setup (`game::setup`)"),
    ("tiles", "Tiles", "tile yields (`game::tiles`)"),
    ("triggers", "Triggers", "one-time effects (`game::triggers`)"),
    ("turn", "Turn", "turn flow (`game::turn`)"),
    ("unique", "Unique", "conditionals and filters (`unique`)"),
    ("units", "Units", "units (`game::units`)"),
    ("victory", "Victory", "victory (`game::victory`)"),
    ("vis", "Vis", "visibility (`game::vis`)"),
    ("workers", "Workers", "workers and improvements (`game::workers`)"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Supported {
    types: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    role: String,
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    gain: bool,
    #[serde(default)]
    stages: Vec<String>,
    reason: Option<String>,
}

/// One row of the TSV.
struct Row {
    name: String,
    placeholder: String,
    signature: String,
}

/// A supported type, checked.
struct Type {
    row: usize,
    role_variant: &'static str,
    holder: Holder,
    /// (field name, kind)
    fields: Vec<(String, &'static Kind)>,
    gain: bool,
    stages: Vec<&'static str>,
    reason: Option<String>,
}

/// Reads both sources from the workspace at `root` and returns gen.rs.
pub fn generate(root: &Path) -> Result<String, String> {
    let read = |rel: &str| {
        std::fs::read_to_string(root.join(rel)).map_err(|e| format!("cannot read {rel}: {e}"))
    };
    let rows = parse_tsv(&read(TSV)?)?;
    let supported: Supported =
        toml::from_str(&read(TOML)?).map_err(|e| format!("{TOML} is not valid: {e}"))?;
    let types = check(&rows, supported)?;
    Ok(render(&rows, &types))
}

fn parse_tsv(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut names = BTreeSet::new();
    let mut placeholders = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = || format!("{TSV}:{}", i + 1);
        let cols: Vec<&str> = line.split('\t').collect();
        let [name, placeholder, signature] = cols.as_slice() else {
            return Err(format!("{}: expected 3 tab-separated columns", at()));
        };
        if !is_ident(name) || !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return Err(format!("{}: {name:?} is not a type name", at()));
        }
        if placeholder_of(signature) != *placeholder {
            return Err(format!("{}: the placeholder is not the signature's", at()));
        }
        if !names.insert((*name).to_owned()) {
            return Err(format!("{}: {name} is listed twice", at()));
        }
        if !placeholders.insert((*placeholder).to_owned()) {
            return Err(format!("{}: the placeholder of {name} is another type's", at()));
        }
        rows.push(Row {
            name: (*name).to_owned(),
            placeholder: (*placeholder).to_owned(),
            signature: (*signature).to_owned(),
        });
    }
    if rows.is_empty() {
        return Err(format!("{TSV} lists no types"));
    }
    Ok(rows)
}

/// The signature with each top-level `[...]` emptied, as `unique::text::placeholder` does.
fn placeholder_of(signature: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in signature.chars() {
        match c {
            '[' => {
                if depth == 0 {
                    out.push_str("[]");
                }
                depth += 1;
            }
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// The kinds in a signature: the contents of each top-level `[...]`.
fn kinds_of(signature: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for c in signature.chars() {
        match c {
            '[' => {
                if depth > 0 {
                    cur.push(c);
                }
                depth += 1;
            }
            ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
            _ if depth > 0 => cur.push(c),
            _ => {}
        }
    }
    out
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "gen", "try", "box",
];

fn check(rows: &[Row], supported: Supported) -> Result<Vec<Type>, String> {
    let index: BTreeMap<&str, usize> =
        rows.iter().enumerate().map(|(i, r)| (r.name.as_str(), i)).collect();
    let mut problems = Vec::new();
    let mut out = Vec::new();
    for (name, e) in supported.types {
        let mut bad = |why: String| problems.push(format!("{TOML}: {name}: {why}"));
        let Some(&row) = index.get(name.as_str()) else {
            bad("not a type of unique_types.tsv".into());
            continue;
        };
        let Some(&(role, role_variant, holder)) = ROLES.iter().find(|r| r.0 == e.role) else {
            bad(format!("unknown role {:?}", e.role));
            continue;
        };
        let sig_kinds = kinds_of(&rows[row].signature);
        if e.fields.len() != sig_kinds.len() {
            bad(format!(
                "{} field name(s) for {} parameter(s) in `{}`",
                e.fields.len(),
                sig_kinds.len(),
                rows[row].signature
            ));
            continue;
        }
        let mut fields = Vec::new();
        let mut seen = BTreeSet::new();
        for (f, sig) in e.fields.iter().zip(&sig_kinds) {
            let (field, kind_name) = match f.split_once(':') {
                Some((field, over)) => (field, over),
                None => (f.as_str(), sig.as_str()),
            };
            if !is_ident(field)
                || KEYWORDS.contains(&field)
                || field.starts_with(char::is_uppercase)
            {
                bad(format!("field name {field:?} is not a lower-case identifier"));
            }
            if !seen.insert(field) {
                bad(format!("field name {field:?} twice"));
            }
            match KINDS.iter().find(|k| k.sig == kind_name) {
                Some(k) => fields.push((field.to_owned(), k)),
                None => bad(format!("parameter kind {kind_name:?} has no compiler (xtask KINDS)")),
            }
        }
        if role == "flag" && !fields.is_empty() {
            bad("a flag has no parameters: make it an effect".into());
        }
        if e.gain && !matches!(role, "effect" | "flag") {
            bad("only a standing effect or flag can also happen on gain".into());
        }
        match (role, &e.reason) {
            ("inert", None) => bad("an inert type needs a reason".into()),
            ("inert", Some(_)) if !e.stages.is_empty() => {
                bad("an inert type is read by no stage".into());
            }
            (_, Some(_)) if role != "inert" => bad("only an inert type has a reason".into()),
            // Each package finds the types it must implement by its stage, so a type with none
            // would be compiled and then read by nothing, silently.
            (_, None) if role != "inert" && e.stages.is_empty() => bad(
                "a type the engine reads names at least one stage (or make it inert with a reason)"
                    .into(),
            ),
            _ => {}
        }
        let mut stages = Vec::new();
        for s in &e.stages {
            match STAGES.iter().find(|x| x.0 == s) {
                Some(x) if !stages.contains(&x.0) => stages.push(x.0),
                Some(_) => bad(format!("stage {s:?} twice")),
                None => bad(format!("unknown stage {s:?}")),
            }
        }
        if holder == Holder::Main {
            let (size, align) = layout(fields.iter().map(|f| f.1));
            if size > PAYLOAD_MAX || align > 4 {
                bad(format!(
                    "the payload takes {size} bytes at alignment {align}, more than {PAYLOAD_MAX} \
                     at 4: narrow a field (`name:amount16`)"
                ));
            }
        }
        out.push(Type {
            row,
            role_variant,
            holder,
            fields,
            gain: e.gain,
            stages,
            reason: e.reason,
        });
    }
    if problems.is_empty() {
        out.sort_by_key(|t| t.row);
        Ok(out)
    } else {
        Err(problems.join("\n"))
    }
}

/// The size and alignment of a struct of these fields, laid out as rustc does: largest
/// alignment first, so the only padding is at the end.
fn layout<'a>(kinds: impl Iterator<Item = &'a Kind>) -> (usize, usize) {
    let (mut size, mut align) = (0, 1);
    for k in kinds {
        size += k.size;
        align = align.max(k.align);
    }
    (size.div_ceil(align) * align, align)
}

fn rust_str(s: &str) -> String {
    format!("{s:?}")
}

fn render(rows: &[Row], types: &[Type]) -> String {
    let mut o = String::new();
    let used_types: BTreeSet<&str> =
        types.iter().flat_map(|t| t.fields.iter().map(|f| f.1.ty)).collect();
    header(&mut o, &used_types);
    unique_type(&mut o, rows);
    type_info(&mut o, rows, types);
    by_placeholder(&mut o, rows);
    param_kind(&mut o);
    stage(&mut o);
    payloads(&mut o, rows, types, &used_types);
    for (holder, name, doc) in [
        (
            Holder::Main,
            "UniqueData",
            "The payload of a main unique: one variant per supported type that is not a modifier, \
             plus [`UniqueData::Tag`]. At most 16 bytes: a `u16` tag and a payload of at most 12.",
        ),
        (
            Holder::Cond,
            "CondData",
            "A compiled conditional: one variant per supported conditional.",
        ),
        (
            Holder::Trigger,
            "TriggerCond",
            "When a one-time unique fires: one variant per trigger the engine fires.",
        ),
        (
            Holder::Modifier,
            "ModifierData",
            "An action or meta modifier, before the compiler folds it into the unique \
             (`ActionMods`, the speed flag, the timer).",
        ),
    ] {
        data_enum(&mut o, rows, types, holder, name, doc);
    }
    o
}

fn header(o: &mut String, used: &BTreeSet<&str>) {
    o.push_str(
        "//! GENERATED by `cargo xtask gen-uniques` from `crates/citar-engine/unique_types.tsv` \
         and\n//! `crates/citar-engine/unique_supported.toml`. Do not edit: change those files and \
         run the\n//! command; `cargo xtask check` fails while this file is stale (DESIGN.md \
         5.4).\n\n",
    );
    o.push_str("#![allow(clippy::too_many_lines, reason = \"one arm per unique type\")]\n\n");
    let from_ids = [
        "BaseUnitId",
        "BuildingId",
        "CityFilterId",
        "CivFilterId",
        "CombatantFilterId",
        "DifficultyId",
        "EraId",
        "FeatureId",
        "FracId",
        "ObjectFilterId",
        "PromotionId",
        "ResourceId",
        "SetRef",
        "SpeedId",
        "StatsId",
        "TechId",
        "TerrainId",
        "TextId",
        "TileFilterId",
        "UnitFilterId",
        "VictoryId",
    ];
    let from_params = [
        "CostOrStrength",
        "CountOrAll",
        "FoundingOrEnhancing",
        "PolicyOrBelief",
        "PopulationFilter",
        "PromotionOrStatus",
        "RegionType",
        "StatOrResource",
        "TerrainQuality",
        "UnitTriggerTarget",
    ];
    // Vocabularies that game state shares with uniques live with the ruleset's other types.
    let from_defs = ["BeliefKind", "SpyAction"];
    if used.contains("Countable") {
        o.push_str("use super::countable::Countable;\n");
    }
    let mut params: Vec<&str> = vec!["Param", "ParamCx", "ParamError"];
    params.extend(from_params.iter().filter(|t| used.contains(*t)));
    params.sort();
    wl!(o, "use super::params::{{{}}};", params.join(", "));
    o.push_str("use super::table::Role;\n");
    let mut ids: Vec<&str> = from_ids.iter().copied().filter(|t| used.contains(t)).collect();
    ids.push("TagId");
    ids.sort();
    wl!(o, "use crate::base::ids::{{{}}};", ids.join(", "));
    if used.contains("Stat") {
        o.push_str("use crate::base::stats::Stat;\n");
    }
    let defs: Vec<&str> = from_defs.iter().copied().filter(|t| used.contains(t)).collect();
    if !defs.is_empty() {
        wl!(o, "use crate::rules::defs::{{{}}};", defs.join(", "));
    }
    o.push('\n');
}

fn unique_type(o: &mut String, rows: &[Row]) {
    wl!(
        o,
        "/// UnCiv's unique types, in `unique_types.tsv`'s order ({}). Most are never used by the\n\
         /// shipped ruleset; [`TypeInfo::support`] says which the engine compiles.",
        rows.len()
    );
    o.push_str(
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]\n#[repr(u16)]\n",
    );
    o.push_str("pub enum UniqueType {\n");
    for r in rows {
        wl!(o, "    /// `{}`", r.signature.replace('`', "'"));
        wl!(o, "    {},", r.name);
    }
    o.push_str("}\n\n");
    o.push_str("impl UniqueType {\n");
    wl!(o, "    /// How many types there are.\n    pub const COUNT: usize = {};\n", rows.len());
    wl!(o, "    /// Every type, in order.\n    pub const ALL: [UniqueType; {}] = [", rows.len());
    for r in rows {
        wl!(o, "        UniqueType::{},", r.name);
    }
    o.push_str("    ];\n}\n\n");
}

fn type_info(o: &mut String, rows: &[Row], types: &[Type]) {
    o.push_str(
        "/// What the engine knows about a unique type.\n#[derive(Debug)]\npub struct TypeInfo {\n    \
         /// The type's name: `StatsFromTiles`.\n    pub name: &'static str,\n    /// The \
         placeholder that identifies it: `[] from [] tiles []`.\n    pub placeholder: &'static \
         str,\n    /// UnCiv's text with each parameter's kind: `[stats] from [tileFilter] tiles \
         [cityFilter]`.\n    pub signature: &'static str,\n    /// How the engine supports it, or \
         `None`: a unique of this type does not load.\n    pub support: Option<&'static \
         Support>,\n}\n\n",
    );
    o.push_str(
        "/// How the engine supports a type (`unique_supported.toml`).\n#[derive(Debug)]\npub \
         struct Support {\n    pub role: Role,\n    /// The kind of each parameter, as \
         compiled.\n    pub params: &'static [ParamKind],\n    /// The payload's field name for \
         each parameter.\n    pub fields: &'static [&'static str],\n    /// Also happens once \
         when its source is gained.\n    pub gain: bool,\n    /// The engine systems that read \
         it.\n    pub stages: &'static [Stage],\n    /// For an inert type, why nothing reads \
         it.\n    pub reason: Option<&'static str>,\n}\n\n",
    );
    let by_row: BTreeMap<usize, &Type> = types.iter().map(|t| (t.row, t)).collect();
    wl!(
        o,
        "/// Every type's [`TypeInfo`], indexed by `UniqueType as usize`.\npub static TYPE_INFO: [TypeInfo; {}] = [",
        rows.len()
    );
    for (i, r) in rows.iter().enumerate() {
        let support = match by_row.get(&i) {
            None => "None".to_owned(),
            Some(t) => {
                let params: Vec<String> =
                    t.fields.iter().map(|f| format!("ParamKind::{}", f.1.variant)).collect();
                let fields: Vec<String> = t.fields.iter().map(|f| rust_str(&f.0)).collect();
                let stages: Vec<String> = t
                    .stages
                    .iter()
                    .map(|s| {
                        let v = STAGES.iter().find(|x| x.0 == *s).map_or("", |x| x.1);
                        format!("Stage::{v}")
                    })
                    .collect();
                let reason = t
                    .reason
                    .as_deref()
                    .map_or("None".to_owned(), |r| format!("Some({})", rust_str(r)));
                format!(
                    "Some(&Support {{ role: Role::{}, params: &[{}], fields: &[{}], gain: {}, \
                     stages: &[{}], reason: {} }})",
                    t.role_variant,
                    params.join(", "),
                    fields.join(", "),
                    t.gain,
                    stages.join(", "),
                    reason
                )
            }
        };
        wl!(
            o,
            "    TypeInfo {{ name: {}, placeholder: {}, signature: {}, support: {} }},",
            rust_str(&r.name),
            rust_str(&r.placeholder),
            rust_str(&r.signature),
            support
        );
    }
    o.push_str("];\n\n");
}

fn by_placeholder(o: &mut String, rows: &[Row]) {
    let mut sorted: Vec<&Row> = rows.iter().collect();
    sorted.sort_by(|a, b| a.placeholder.as_bytes().cmp(b.placeholder.as_bytes()));
    wl!(
        o,
        "/// Every type by its placeholder, sorted by the placeholder's bytes, for a binary \
         search.\npub static BY_PLACEHOLDER: [(&str, UniqueType); {}] = [",
        rows.len()
    );
    for r in sorted {
        wl!(o, "    ({}, UniqueType::{}),", rust_str(&r.placeholder), r.name);
    }
    o.push_str("];\n\n");
}

fn param_kind(o: &mut String) {
    o.push_str(
        "/// The kinds of parameter the engine compiles, named as UnCiv's signatures name them.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum ParamKind {\n",
    );
    for k in KINDS {
        wl!(o, "    /// `{}`: {}.\n    {},", k.sig, k.doc, k.variant);
    }
    o.push_str("}\n\nimpl ParamKind {\n    /// The kind's name in UnCiv's signatures.\n    #[must_use]\n    pub const fn name(self) -> &'static str {\n        match self {\n");
    for k in KINDS {
        wl!(o, "            Self::{} => {},", k.variant, rust_str(k.sig));
    }
    o.push_str("        }\n    }\n}\n\n");
}

fn stage(o: &mut String) {
    o.push_str(
        "/// An engine system that reads a unique type: `unique_supported.toml`'s `stages`.\n\
         #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\npub enum Stage {\n",
    );
    for (_, v, doc) in STAGES {
        wl!(o, "    /// {}.\n    {v},", capitalise(doc));
    }
    o.push_str("}\n\nimpl Stage {\n    /// The stage's name in `unique_supported.toml`.\n    #[must_use]\n    pub const fn name(self) -> &'static str {\n        match self {\n");
    for (n, v, _) in STAGES {
        wl!(o, "            Self::{v} => {},", rust_str(n));
    }
    o.push_str("        }\n    }\n}\n\n");
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn payloads(o: &mut String, rows: &[Row], types: &[Type], used: &BTreeSet<&str>) {
    o.push_str(
        "/// The payload of each supported type that has parameters, one field per parameter.\n",
    );
    o.push_str("pub mod p {\n");
    let mut imports: Vec<&str> =
        used.iter().copied().filter(|t| *t != "i32" && *t != "i16").collect();
    imports.sort();
    wl!(o, "    use super::{{{}}};\n", imports.join(", "));
    for t in types.iter().filter(|t| !t.fields.is_empty()) {
        let r = &rows[t.row];
        wl!(o, "    /// `{}`", r.signature.replace('`', "'"));
        o.push_str("    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
        wl!(o, "    pub struct {} {{", r.name);
        for (f, k) in &t.fields {
            wl!(o, "        pub {f}: {},", k.ty);
        }
        o.push_str("    }\n\n");
    }
    o.push_str("}\n\n");
    for t in types.iter().filter(|t| t.holder == Holder::Main && !t.fields.is_empty()) {
        let n = &rows[t.row].name;
        wl!(
            o,
            "const _: () = assert!(core::mem::size_of::<p::{n}>() <= {PAYLOAD_MAX} && core::mem::align_of::<p::{n}>() <= 4);"
        );
    }
    o.push('\n');
}

fn data_enum(o: &mut String, rows: &[Row], types: &[Type], holder: Holder, name: &str, doc: &str) {
    let mine: Vec<&Type> = types.iter().filter(|t| t.holder == holder).collect();
    wl!(o, "/// {doc}");
    o.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    wl!(o, "pub enum {name} {{");
    for t in &mine {
        let r = &rows[t.row];
        wl!(o, "    /// `{}`", r.signature.replace('`', "'"));
        if t.fields.is_empty() {
            wl!(o, "    {},", r.name);
        } else {
            wl!(o, "    {0}(p::{0}),", r.name);
        }
    }
    if holder == Holder::Main {
        o.push_str(
            "    /// A text UnCiv does not know, with no parameters and no modifiers, that a filter \
             names\n    /// (`Aircraft`): a tag (DESIGN.md 5.6).\n    Tag(TagId),\n",
        );
    }
    o.push_str("}\n\n");
    wl!(o, "impl {name} {{");
    // ty
    let ret = if holder == Holder::Main { "Option<UniqueType>" } else { "UniqueType" };
    wl!(
        o,
        "    /// The unique's type{}.\n    #[must_use]\n    pub const fn ty(&self) -> {ret} {{\n        match self {{",
        if holder == Holder::Main { "; `None` for a tag" } else { "" }
    );
    for t in &mine {
        let n = &rows[t.row].name;
        let pat = if t.fields.is_empty() { n.clone() } else { format!("{n}(_)") };
        let val = if holder == Holder::Main {
            format!("Some(UniqueType::{n})")
        } else {
            format!("UniqueType::{n}")
        };
        wl!(o, "            Self::{pat} => {val},");
    }
    if holder == Holder::Main {
        o.push_str("            Self::Tag(_) => None,\n");
    }
    o.push_str("        }\n    }\n\n");
    // params
    o.push_str(
        "    /// The compiled parameters, in the order the text writes them.\n    #[must_use]\n    \
         pub fn params(&self) -> Vec<Param> {\n        match self {\n",
    );
    for t in &mine {
        let n = &rows[t.row].name;
        if t.fields.is_empty() {
            wl!(o, "            Self::{n} => Vec::new(),");
        } else {
            let vals: Vec<String> = t.fields.iter().map(|f| format!("x.{}.into()", f.0)).collect();
            wl!(o, "            Self::{n}(x) => vec![{}],", vals.join(", "));
        }
    }
    if holder == Holder::Main {
        o.push_str("            Self::Tag(_) => Vec::new(),\n");
    }
    o.push_str("        }\n    }\n\n");
    // build
    wl!(
        o,
        "    /// Compiles a unique of type `ty` whose parameters `cx` holds, in order.\n    ///\n    \
         /// # Errors\n    /// A parameter that does not compile, or a type whose role is not \
         this enum's.\n    pub(crate) fn build(ty: UniqueType, cx: &mut ParamCx<'_, '_>) -> \
         Result<Self, ParamError> {{\n        Ok(match ty {{"
    );
    for t in &mine {
        let n = &rows[t.row].name;
        if t.fields.is_empty() {
            wl!(o, "            UniqueType::{n} => Self::{n},");
        } else {
            let vals: Vec<String> = t
                .fields
                .iter()
                .enumerate()
                .map(|(i, (f, k))| format!("{f}: cx.get({i}, ParamKind::{})?", k.variant))
                .collect();
            wl!(o, "            UniqueType::{n} => Self::{n}(p::{n} {{ {} }}),", vals.join(", "));
        }
    }
    wl!(o, "            _ => return Err(ParamError::role(ty, {})),", rust_str(name));
    o.push_str("        })\n    }\n}\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        crate::workspace_root()
    }

    #[test]
    fn the_output_is_the_same_every_time() {
        let a = generate(&root()).expect("generate");
        let b = generate(&root()).expect("generate again");
        assert_eq!(a, b);
        assert!(a.starts_with("//! GENERATED"));
    }

    #[test]
    fn signatures_split_into_placeholders_and_kinds() {
        let sig = "[stats] from [tileFilter] tiles [cityFilter]";
        assert_eq!(placeholder_of(sig), "[] from [] tiles []");
        assert_eq!(kinds_of(sig), ["stats", "tileFilter", "cityFilter"]);
        assert_eq!(
            kinds_of("Reveal up to [positiveAmount/'all'] [tileFilter]"),
            ["positiveAmount/'all'", "tileFilter"]
        );
        assert_eq!(placeholder_of("[a [b] c] d"), "[] d");
        assert_eq!(kinds_of("[a [b] c] d"), ["a [b] c"]);
    }

    #[test]
    fn a_payload_over_twelve_bytes_is_refused() {
        let k = |sig| KINDS.iter().find(|k| k.sig == sig).expect("kind");
        assert_eq!(
            layout([k("amount"), k("amount"), k("stat"), k("cityFilter")].into_iter()),
            (12, 4)
        );
        assert_eq!(
            layout(
                [
                    k("baseUnitFilter"),
                    k("nonNegativeAmount"),
                    k("stat"),
                    k("cityFilter"),
                    k("amount")
                ]
                .into_iter()
            ),
            (16, 4)
        );
        assert_eq!(
            layout(
                [
                    k("baseUnitFilter"),
                    k("nonNegativeAmount"),
                    k("stat"),
                    k("cityFilter"),
                    k("amount16")
                ]
                .into_iter()
            ),
            (12, 4)
        );
    }

    /// How many types check, or what is wrong.
    fn one(rows: &str, toml: &str) -> Result<usize, String> {
        let rows = parse_tsv(rows)?;
        let s: Supported = toml::from_str(toml).map_err(|e| e.to_string())?;
        check(&rows, s).map(|types| types.len())
    }

    #[test]
    fn the_supported_list_is_checked() {
        let tsv = "A\t[] x\t[amount] x\nB\ty\ty\n";
        assert!(
            one(tsv, "[types]\nA = { role = \"effect\", fields = [\"n\"], stages = [\"cities\"] }")
                .is_ok()
        );
        let e = one(tsv, "[types]\nA = { role = \"effect\" }").expect_err("no field names");
        assert!(e.contains("0 field name(s) for 1 parameter(s)"), "{e}");
        let e = one(tsv, "[types]\nB = { role = \"flag\" }").expect_err("no stage");
        assert!(e.contains("names at least one stage"), "{e}");
        assert!(
            one(tsv, "[types]\nB = { role = \"inert\", reason = \"read by nothing\" }").is_ok(),
            "an inert type needs no stage"
        );
        let e = one(tsv, "[types]\nC = { role = \"flag\" }").expect_err("unknown type");
        assert!(e.contains("not a type"), "{e}");
        let e = one(tsv, "[types]\nB = { role = \"inert\" }").expect_err("no reason");
        assert!(e.contains("needs a reason"), "{e}");
        let e = one(tsv, "[types]\nB = { role = \"flag\", stages = [\"nowhere\"] }")
            .expect_err("stage");
        assert!(e.contains("unknown stage"), "{e}");
        let e = one(tsv, "[types]\nA = { role = \"flag\", fields = [\"n\"] }").expect_err("flag");
        assert!(e.contains("a flag has no parameters"), "{e}");
        let e = one(tsv, "[types]\nA = { role = \"effect\", fields = [\"n:nothing\"] }")
            .expect_err("kind");
        assert!(e.contains("has no compiler"), "{e}");
        let e = one("A\t[]\t[amount]\nB\t[]\t[stats]\n", "[types]").expect_err("same placeholder");
        assert!(e.contains("another type's"), "{e}");
    }
}

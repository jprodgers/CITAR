//! The generator's settings: the map types, what happens at the edges, how many rivers and how
//! many resources (`mapgen.py:24-105`, `MAP_TYPES`, `EDGE_MODES`, `MapOptions`).
//!
//! The lobby's resource settings are read as leniently as Python read them: what is not a
//! number or an object is left at its default, and a resource the ruleset lacks, or of another
//! kind, is skipped, so settings written for one ruleset do not break another (`mapgen.py:62-63`).

use serde_json::{Map, Value};

use crate::base::fmt::PyFloat;
use crate::base::ids::ResourceId;
use crate::base::num;
use crate::base::py;
use crate::rules::Ruleset;
use crate::rules::defs::ResourceType;
use crate::state::config::{MapEdges, ResourceKindOptions, ResourceOptions, ResourceRule};

/// How the land is shaped (`MAP_TYPES`, `mapgen.py:24`). Everything after the land's shape is
/// the same for every type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MapType {
    /// Two to four continents spread east to west.
    #[default]
    Continents,
    /// One landmass in the middle.
    Pangaea,
    /// Many small islands.
    Archipelago,
    /// A ring of land round a central sea.
    InlandSea,
    /// The noise alone.
    Fractal,
}

/// Every map type, in Python's order (`mapgen.MAP_TYPES`).
pub const MAP_TYPES: [MapType; 5] = [
    MapType::Continents,
    MapType::Pangaea,
    MapType::Archipelago,
    MapType::InlandSea,
    MapType::Fractal,
];

impl MapType {
    /// The key the lobby uses: `inland_sea`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Continents => "continents",
            Self::Pangaea => "pangaea",
            Self::Archipelago => "archipelago",
            Self::InlandSea => "inland_sea",
            Self::Fractal => "fractal",
        }
    }

    /// The type a lobby key names; a key the generator has no shape for is Continents, as
    /// `generate_map` fell back (`mapgen.py:1674-1675`).
    #[must_use]
    pub fn from_key(key: &str) -> Self {
        MAP_TYPES.into_iter().find(|t| t.key() == key).unwrap_or_default()
    }
}

/// Which sides of the map carry a polar ice cap (`EDGE_MODES`, `mapgen.py:27-33`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IceSides {
    pub north: bool,
    pub south: bool,
    pub east: bool,
    pub west: bool,
}

impl IceSides {
    /// The capped sides of these edges.
    #[must_use]
    pub const fn of(edges: MapEdges) -> Self {
        let (ns, ew) = match edges {
            MapEdges::IceCaps | MapEdges::WrapX => (true, false),
            MapEdges::WrapY | MapEdges::WrapBoth => (false, false),
            MapEdges::Boxed => (true, true),
        };
        Self { north: ns, south: ns, east: ew, west: ew }
    }

    /// Whether no side has ice.
    #[must_use]
    pub const fn none(self) -> bool {
        !(self.north || self.south || self.east || self.west)
    }
}

/// The generator's knobs (`MapOptions`, `mapgen.py:52-93`).
#[derive(Clone, Debug, PartialEq)]
pub struct MapOptions {
    pub edges: MapEdges,
    /// Scales how many rivers are traced: 0 none, 1 normal, at most 5.
    pub rivers: f64,
    pub resources: ResourceOptions,
}

impl Default for MapOptions {
    fn default() -> Self {
        Self { edges: MapEdges::default(), rivers: 1.0, resources: ResourceOptions::default() }
    }
}

impl MapOptions {
    /// Whether the map wraps east-west and north-south.
    #[must_use]
    pub const fn wraps(&self) -> (bool, bool) {
        self.edges.wraps()
    }

    /// The sides with an ice cap.
    #[must_use]
    pub const fn ice_sides(&self) -> IceSides {
        IceSides::of(self.edges)
    }

    fn kind(&self, kind: ResourceType) -> &ResourceKindOptions {
        match kind {
            ResourceType::Strategic => &self.resources.strategic,
            ResourceType::Luxury => &self.resources.luxury,
            ResourceType::Bonus => &self.resources.bonus,
        }
    }

    /// How densely a kind of resource is placed: the overall density times the kind's.
    #[must_use]
    pub fn density(&self, kind: ResourceType) -> f64 {
        self.resources.density * self.kind(kind).density
    }

    /// The lobby's rule for one resource of `kind`, if it has one.
    #[must_use]
    pub fn rule(&self, kind: ResourceType, r: ResourceId) -> Option<ResourceRule> {
        self.kind(kind).each.iter().find(|&&(x, _)| x == r).map(|&(_, rule)| rule)
    }

    /// Every resource of `kind` with a rule, by resource.
    pub fn rules_of(&self, kind: ResourceType) -> impl Iterator<Item = (ResourceId, ResourceRule)> {
        self.kind(kind).each.iter().copied()
    }

    /// A one-line summary for a generated map's description (`MapOptions.describe`,
    /// `mapgen.py:95-105`): `edges ice caps, rivers x0.5, Silk off`.
    #[must_use]
    #[allow(clippy::float_cmp, reason = "a setting left at 1 is exactly 1, as Python compared it")]
    pub fn describe(&self, rules: &Ruleset) -> String {
        let mut out = format!("edges {}", self.edges.name().replace('_', " "));
        if self.rivers != 1.0 {
            out.push_str(&format!(", rivers x{}", fmt_g(self.rivers)));
        }
        for (kind, name) in KINDS {
            let d = self.density(kind);
            if d != 1.0 {
                out.push_str(&format!(", {name} x{}", fmt_g(d)));
            }
        }
        // Sorted by name, as Python sorted the rules' dict.
        let mut rules_by_name: Vec<(&str, ResourceRule)> = KINDS
            .iter()
            .flat_map(|&(kind, _)| self.rules_of(kind))
            .map(|(id, rule)| (&*rules.resources()[id].name, rule))
            .collect();
        rules_by_name.sort_by(|a, b| a.0.cmp(b.0));
        for (name, rule) in rules_by_name {
            let what = match rule {
                ResourceRule::Off => "off".to_owned(),
                ResourceRule::Cap(v) => format!("max {}", fmt_g(v)),
                ResourceRule::Share(v) => format!("{}%", fmt_g(v)),
            };
            out.push_str(&format!(", {name} {what}"));
        }
        out
    }
}

/// The three kinds as the lobby names them, in Python's order.
const KINDS: [(ResourceType, &str); 3] = [
    (ResourceType::Strategic, "strategic"),
    (ResourceType::Luxury, "luxury"),
    (ResourceType::Bonus, "bonus"),
];

/// Python's `f"{x:g}"` for the numbers a description shows: six significant digits, without
/// trailing zeros.
fn fmt_g(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{}", PyFloat(x)).trim_end_matches(".0").to_owned();
    }
    let exp = num::floor_i32(num::log10(x.abs()));
    if !(-4..6).contains(&exp) {
        return format!("{}", PyFloat(x));
    }
    let places = usize::try_from(5 - exp).unwrap_or(0);
    let s = format!("{x:.places$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
}

/// A number from a lobby option, with a default and clamped (`mapgen._num`, `mapgen.py:44-49`).
#[must_use]
pub fn option_number(v: Option<&Value>, default: f64, lo: f64, hi: f64) -> f64 {
    v.and_then(py::float_of).filter(|x| !x.is_nan()).map_or(default, |x| x.clamp(lo, hi))
}

/// The lobby's resource options (`MapOptions.__init__`, `mapgen.py:77-93`).
#[must_use]
pub fn resource_options(rules: &Ruleset, v: Option<&Value>) -> ResourceOptions {
    let res = v.and_then(Value::as_object);
    let num = |o: Option<&Map<String, Value>>, k: &str| {
        option_number(o.and_then(|o| o.get(k)), 1.0, 0.0, 5.0)
    };
    let mut out = ResourceOptions { density: num(res, "density"), ..ResourceOptions::default() };
    for (kind, key) in KINDS {
        let sub = res.and_then(|r| r.get(key)).and_then(Value::as_object);
        let mut opts = ResourceKindOptions { density: num(sub, "density"), each: Vec::new() };
        let each = sub.and_then(|s| s.get("each")).and_then(Value::as_object);
        for (name, rule) in each.into_iter().flatten() {
            let Some(rule) = rule.as_object() else { continue };
            let mode = rule.get("mode").and_then(Value::as_str);
            let Some(id) = rules.resolve::<ResourceId>(name) else { continue };
            if rules.resources()[id].kind != kind {
                continue;
            }
            let value = || option_number(rule.get("value"), 0.0, 0.0, 10_000.0);
            let r = match mode {
                Some("off") => ResourceRule::Off,
                Some("cap") => ResourceRule::Cap(value()),
                Some("share") => ResourceRule::Share(value()),
                _ => continue,
            };
            opts.each.retain(|&(x, _)| x != id);
            opts.each.push((id, r));
        }
        opts.each.sort_by_key(|&(id, _)| id);
        match kind {
            ResourceType::Strategic => out.strategic = opts,
            ResourceType::Luxury => out.luxury = opts,
            ResourceType::Bonus => out.bonus = opts,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_types_read_their_keys_and_fall_back_to_continents() {
        for t in MAP_TYPES {
            assert_eq!(MapType::from_key(t.key()), t);
        }
        assert_eq!(MapType::from_key("custom"), MapType::Continents);
    }

    #[test]
    fn ice_lies_on_the_sides_that_do_not_wrap() {
        assert_eq!(
            IceSides::of(MapEdges::IceCaps),
            IceSides { north: true, south: true, east: false, west: false }
        );
        assert!(IceSides::of(MapEdges::WrapBoth).none());
        assert!(IceSides::of(MapEdges::WrapY).none());
        assert!(IceSides::of(MapEdges::Boxed).east);
    }

    #[test]
    fn numbers_are_written_as_python_writes_g() {
        assert_eq!(fmt_g(2.0), "2");
        assert_eq!(fmt_g(0.5), "0.5");
        assert_eq!(fmt_g(0.05), "0.05");
        assert_eq!(fmt_g(10_000.0), "10000");
        assert_eq!(fmt_g(100.0 / 3.0), "33.3333");
        assert_eq!(fmt_g(0.0), "0");
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod ruleset_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_description_names_what_differs_from_normal() {
        let r = Ruleset::shared();
        let mut o = MapOptions::default();
        assert_eq!(o.describe(r), "edges ice caps");
        o.edges = MapEdges::WrapBoth;
        o.rivers = 0.5;
        o.resources = resource_options(
            r,
            Some(&json!({"luxury": {"density": 2, "each": {"Silk": {"mode": "off"},
                "Wine": {"mode": "share", "value": 25}}}, "strategic": {"each": {
                "Iron": {"mode": "cap", "value": 3}, "Silk": {"mode": "off"}}}})),
        );
        assert_eq!(
            o.describe(r),
            "edges wrap both, rivers x0.5, luxury x2, Iron max 3, Silk off, Wine 25%"
        );
        let silk = r.lookup::<ResourceId>("Silk").expect("silk");
        assert_eq!(o.rule(ResourceType::Luxury, silk), Some(ResourceRule::Off));
        assert_eq!(o.rule(ResourceType::Strategic, silk), None, "Silk is no strategic resource");
        assert!((o.density(ResourceType::Luxury) - 2.0).abs() < 1e-12);
    }
}

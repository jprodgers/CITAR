//! The golden sets of package 1a-06 (DESIGN.md 5.7 and 5.10):
//! - **`filters.json`**: every dynamic filter of the embedded ruleset, compiled: unit, tile (full
//!   and terrain forms, the terrains it names, whether map generation can read it), city,
//!   civilization and combatant filters, each as its folded tree with every set named;
//! - **`gen.json`**: the tables map generation, the AI and victory read: each terrain's,
//!   resource's and natural wonder's generation entries, the AI weights, the victory milestones,
//!   the inert uniques, and every map-generation unique the tables hold (gate 5).
//!
//! Written by `golden bless` when the ruleset data, the filters or the tables change; a diff shows
//! which filter, or which table entry, moved.

use citar_engine::base::ids::{Id, IdVec};
use citar_engine::rules::gen_tables::{GenCond, GenValue, Milestone, Near};
use citar_engine::rules::{Named, Ruleset, embedded};
use citar_engine::unique::params::RegionType;
use citar_engine::unique::{CityLeaf, CivLeaf, Expr, GenFilter, TileLeaf, UnitLeaf};
use serde_json::{Value, json};

use super::{SetReport, capped, diff_rows, digest_of, read_committed, render_rows};

/// The lists of `filters.json`, one row each.
pub(super) const FILTER_LISTS: [&str; 5] = ["units", "tiles", "cities", "civs", "combatants"];

/// The lists of `gen.json`, one row each.
pub(super) const GEN_LISTS: [&str; 7] =
    ["terrains", "resources", "wonders", "ai", "milestones", "inert", "placed"];

/// A tree as JSON: `true`/`false`, a leaf, `{"not": x}`, `{"all": [..]}` or `{"any": [..]}`.
fn expr<L>(e: &Expr<L>, leaf: &dyn Fn(&L) -> Value) -> Value {
    match e {
        Expr::Const(b) => json!(b),
        Expr::Leaf(l) => leaf(l),
        Expr::Not(x) => json!({"not": expr(x, leaf)}),
        Expr::All(xs) => json!({"all": xs.iter().map(|x| expr(x, leaf)).collect::<Vec<_>>()}),
        Expr::Any(xs) => json!({"any": xs.iter().map(|x| expr(x, leaf)).collect::<Vec<_>>()}),
    }
}

/// The names of a set's members.
fn names<I: Named + Id>(r: &Ruleset, ids: impl Iterator<Item = I>) -> Value {
    json!(ids.map(|i| r.name(i).unwrap_or("?")).collect::<Vec<_>>())
}

fn civ_leaf(r: &Ruleset, l: &CivLeaf) -> Value {
    match l {
        CivLeaf::Kind(k) => json!({"Kind": format!("{k:?}")}),
        CivLeaf::Nation(s) => json!({"Nation": names(r, s.iter())}),
        other => json!(format!("{other:?}")),
    }
}

fn unit_leaf(r: &Ruleset, l: &UnitLeaf) -> Value {
    match l {
        UnitLeaf::Base(s) => json!({"Base": names(r, s.iter())}),
        UnitLeaf::Promotion(s) => json!({"Promotion": names(r, s.iter())}),
        UnitLeaf::Owner(c) => json!({"Owner": civ_leaf(r, c)}),
        other => json!(format!("{other:?}")),
    }
}

fn city_leaf(r: &Ruleset, l: &CityLeaf) -> Value {
    match l {
        CityLeaf::Has(s) => json!({"Has": names(r, s.iter())}),
        CityLeaf::Owner(c) => json!({"Owner": civ_leaf(r, c)}),
        other => json!(format!("{other:?}")),
    }
}

fn tile_leaf(r: &Ruleset, l: &TileLeaf) -> Value {
    match l {
        TileLeaf::Terrains(s) => json!({"Terrains": names(r, s.iter())}),
        TileLeaf::Resource(s) => json!({"Resource": names(r, s.iter())}),
        TileLeaf::Improvement(s) => json!({"Improvement": names(r, s.iter())}),
        TileLeaf::Owner(c) => json!({"Owner": civ_leaf(r, c)}),
        other => json!(format!("{other:?}")),
    }
}

/// Everything `filters.json` holds, computed by this build.
#[must_use]
pub fn filters_answers() -> Value {
    let r = match Ruleset::load(&embedded()) {
        Ok(r) => r,
        Err(e) => return json!({"format": 1, "error": e.to_string()}),
    };
    let t = r.uniques();
    let f = t.filters();
    let ul = |l: &UnitLeaf| unit_leaf(&r, l);
    let tl = |l: &TileLeaf| tile_leaf(&r, l);
    let cl = |l: &CityLeaf| city_leaf(&r, l);
    let vl = |l: &CivLeaf| civ_leaf(&r, l);
    let units: Vec<Value> =
        f.units().iter().map(|(id, e)| json!([t.unit_filter(id), expr(e, &ul)])).collect();
    let tiles: Vec<Value> = f
        .tiles()
        .iter()
        .map(|(id, x)| {
            json!([
                t.tile_filter(id),
                expr(&x.full, &tl),
                expr(&x.terrain, &tl),
                names(&r, x.terrains.iter()),
                x.terrain_level,
            ])
        })
        .collect();
    let cities: Vec<Value> =
        f.cities().iter().map(|(id, e)| json!([t.city_filter(id), expr(e, &cl)])).collect();
    let civs: Vec<Value> =
        f.civs().iter().map(|(id, e)| json!([t.civ_filter(id), expr(e, &vl)])).collect();
    let combatants: Vec<Value> = f
        .combatants()
        .iter()
        .map(|(id, x)| json!([t.combatant_filter(id), expr(&x.unit, &ul), expr(&x.city, &cl)]))
        .collect();
    json!({
        "format": 1,
        "about": "every dynamic filter of the embedded ruleset, compiled (DESIGN.md 5.7): units, cities and civs [text, tree]; tiles [text, full, terrain, terrains, terrain_level]; combatants [text, as a unit, as a city]",
        "counts": {
            "units": units.len(),
            "tiles": tiles.len(),
            "cities": cities.len(),
            "civs": civs.len(),
            "combatants": combatants.len(),
        },
        "units": units,
        "tiles": tiles,
        "cities": cities,
        "civs": civs,
        "combatants": combatants,
    })
}

fn cond(r: &Ruleset, c: &GenCond) -> Value {
    let t = r.uniques();
    let texts =
        |fs: &[GenFilter]| fs.iter().map(|&f| t.tile_filter(f.id()).to_owned()).collect::<Vec<_>>();
    let regions = |rs: &[RegionType]| {
        rs.iter()
            .map(|x| match *x {
                RegionType::Hybrid => "Hybrid".to_owned(),
                RegionType::Terrain(t) => r.name(t).unwrap_or("?").to_owned(),
            })
            .collect::<Vec<_>>()
    };
    json!({
        "tiles": texts(&c.tiles),
        "without": texts(&c.without),
        "regions": regions(&c.regions),
        "except_regions": regions(&c.except_regions),
    })
}

fn values(r: &Ruleset, vs: &[GenValue]) -> Value {
    json!(vs.iter().map(|v| json!([v.value, cond(r, &v.cond)])).collect::<Vec<_>>())
}

/// Everything `gen.json` holds, computed by this build.
#[must_use]
pub fn gen_answers() -> Value {
    let r = match Ruleset::load(&embedded()) {
        Ok(r) => r,
        Err(e) => return json!({"format": 1, "error": e.to_string()}),
    };
    let g = r.gen_tables();
    let t = r.uniques();
    let tname = |id| r.name(id).unwrap_or("?").to_owned();
    let terrains: Vec<Value> = g
        .terrains
        .iter()
        .filter(|(_, x)| **x != Default::default())
        .map(|(id, x)| {
            json!([tname(id), {
                "climates": x.climates.iter().map(|c| json!([c.temperature.0, c.temperature.1, c.humidity.0, c.humidity.1])).collect::<Vec<_>>(),
                "chains": x.chains, "groups": x.groups, "rare": x.rare, "vegetation": x.vegetation,
                "coastal_water": x.coastal_water, "never": x.never,
                "not_where": x.not_where.iter().map(|c| cond(&r, c)).collect::<Vec<_>>(),
                "fertility": [x.fertility.add, x.fertility.fixed],
                "changes": x.changes.iter().map(|c| json!([tname(c.into), match c.near {
                    Near::River => json!("River"),
                    Near::Tiles(f) => json!(t.tile_filter(f.id())),
                }])).collect::<Vec<_>>(),
                "major_deposits": x.major_deposits,
                "blocks_resources": x.blocks_resources.iter().map(|c| cond(&r, c)).collect::<Vec<_>>(),
            }])
        })
        .collect();
    let resources: Vec<Value> = g
        .resources
        .iter()
        .filter(|(_, x)| **x != Default::default())
        .map(|(id, x)| {
            json!([r.name(id), {
                "never": x.never,
                "not_where": x.not_where.iter().map(|c| cond(&r, c)).collect::<Vec<_>>(),
                "frequencies": values(&r, &x.frequencies),
                "weights": values(&r, &x.weights),
                "minor_weights": values(&r, &x.minor_weights),
                "city_state_weight": x.city_state_weight,
                "amounts": x.amounts.iter().map(|a| json!([t.tile_filter(a.tiles.id()), a.amount])).collect::<Vec<_>>(),
            }])
        })
        .collect();
    let wonders: Vec<Value> = g
        .wonders
        .iter()
        .filter(|(_, x)| **x != Default::default())
        .map(|(id, x)| {
            json!([tname(id), {
                "neighbours": x.neighbours.iter().map(|n| json!([n.min, n.max, t.tile_filter(n.tiles.id())])).collect::<Vec<_>>(),
                "not_on_largest": x.not_on_largest,
                "latitudes": x.latitudes,
                "group": x.group,
                "converts": x.converts.iter().map(|(to, c)| json!([tname(*to), cond(&r, c)])).collect::<Vec<_>>(),
            }])
        })
        .collect();
    let mut ai = Vec::new();
    let mut weights = |kind: &str, name: &str, ws: &[citar_engine::rules::gen_tables::AiWeight]| {
        if !ws.is_empty() {
            let ws: Vec<Value> = ws.iter().map(|w| json!([w.percent, w.unique.0])).collect();
            ai.push(json!([format!("{kind}:{name}"), ws]));
        }
    };
    each(&r, &g.ai.techs, "Tech", &mut weights);
    each(&r, &g.ai.policies, "Policy", &mut weights);
    each(&r, &g.ai.beliefs, "Belief", &mut weights);
    each(&r, &g.ai.promotions, "Promotion", &mut weights);
    each(&r, &g.ai.buildings, "Building", &mut weights);
    each(&r, &g.ai.units, "Unit", &mut weights);
    let milestones: Vec<Value> = r
        .victories()
        .as_slice()
        .iter()
        .map(|v| {
            let ms: Vec<Value> =
                v.milestones.iter().map(|m| json!([milestone(&r, m.milestone), m.text])).collect();
            json!([v.name, ms])
        })
        .collect();
    let inert: Vec<Value> =
        g.inert.iter().map(|x| json!([x.unique.0, t.text_of(x.unique), x.reason])).collect();
    let placed: Vec<Value> = g.placed.iter().map(|&id| json!([id.0, t.text_of(id)])).collect();
    json!({
        "format": 1,
        "about": "the tables map generation, the AI and victory read (DESIGN.md 5.10): terrains, resources and natural wonders with their generation entries; AI weights [object, [[percent, unique]]]; milestones per victory; inert uniques [id, text, reason]; placed map-generation uniques [id, text]",
        "counts": {
            "terrains": terrains.len(),
            "resources": resources.len(),
            "wonders": wonders.len(),
            "ai": ai.len(),
            "inert": inert.len(),
            "placed": placed.len(),
        },
        "terrains": terrains,
        "resources": resources,
        "wonders": wonders,
        "ai": ai,
        "milestones": milestones,
        "inert": inert,
        "placed": placed,
    })
}

/// A milestone, its buildings named.
fn milestone(r: &Ruleset, m: Milestone) -> Value {
    match m {
        Milestone::Build(b) => json!({"Build": r.name(b)}),
        Milestone::AnyoneBuilds(b) => json!({"AnyoneBuilds": r.name(b)}),
        Milestone::CompletePolicyBranches(n) => json!({"CompletePolicyBranches": n}),
        other => json!(format!("{other:?}")),
    }
}

/// Calls `f` with each object's name and weights.
fn each<I: Named + Id>(
    r: &Ruleset,
    table: &IdVec<I, Vec<citar_engine::rules::gen_tables::AiWeight>>,
    kind: &str,
    f: &mut dyn FnMut(&str, &str, &[citar_engine::rules::gen_tables::AiWeight]),
) {
    for (id, ws) in table.iter() {
        f(kind, r.name(id).unwrap_or("?"), ws);
    }
}

fn check(name: &'static str, file: &str, got: &Value, lists: &[&str]) -> SetReport {
    let mut problems = Vec::new();
    if let Some(e) = got.get("error") {
        problems.push(format!("{name}: the embedded ruleset does not load: {e}"));
    }
    match read_committed(file) {
        Err(e) => problems.push(e),
        Ok(want) => {
            if want.get("counts") != got.get("counts") {
                problems.push(format!(
                    "{file}: the counts are {} in the file, {} in this build",
                    want.get("counts").unwrap_or(&Value::Null),
                    got.get("counts").unwrap_or(&Value::Null)
                ));
            }
            for key in lists {
                problems.extend(diff_rows(file, key, want.get(*key), &got[*key]));
            }
        }
    }
    SetReport { name, computed: digest_of(got), problems: capped(problems) }
}

pub(super) fn check_filters() -> SetReport {
    check("filters", "filters.json", &filters_answers(), &FILTER_LISTS)
}

pub(super) fn check_gen() -> SetReport {
    check("gen", "gen.json", &gen_answers(), &GEN_LISTS)
}

pub(super) fn blessed() -> [(&'static str, String); 2] {
    [
        ("filters.json", render_rows(&filters_answers(), &FILTER_LISTS)),
        ("gen.json", render_rows(&gen_answers(), &GEN_LISTS)),
    ]
}

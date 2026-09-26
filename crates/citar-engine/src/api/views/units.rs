//! A unit as a viewer sees it, and everything its owner needs to give it orders
//! (`views.unit_info` and `_unit_detail`, `views.py:50-157`).

use serde_json::{Map, Value, json};

use super::{rule_text, xy};
use crate::base::ids::{PlayerId, UnitId};
use crate::base::num;
use crate::game::cities::borders;
use crate::game::combat::resolve;
use crate::game::units::{self, health, promotions, upgrades};
use crate::game::{Game, actions, automation, movement, religion, workers};
use crate::rules::defs::{BaseUnitDef, Domain};
use crate::unique::UniqueType;

/// How many reachable tiles `get_unit` lists (`views.py:143`).
const REACHABLE_SHOWN: usize = 80;

/// The keys of an attack preview a unit's detail repeats for each target (`views.py:153-154`).
const TARGET_KEYS: [&str; 6] =
    ["target", "defender", "damage_to_defender", "damage_to_attacker", "defender_hp", "note"];

/// A unit's domain as the client names it (`views.DOMAIN`).
#[must_use]
pub fn domain_name(d: Domain) -> &'static str {
    match d {
        Domain::Land => "land",
        Domain::Water => "sea",
        Domain::Air => "air",
    }
}

/// A coarse class for the map's glyphs, from the unit's type (`views.unit_class`,
/// `views.py:51-63`): a civilian, or its type's class, melee for a type the table does not
/// name. Presentation only: the rules never read it.
#[must_use]
pub fn unit_class(g: &Game, d: &BaseUnitDef) -> &'static str {
    if !d.military {
        return "civilian";
    }
    match &*g.rules().unit_types()[d.unit_type].name {
        "Civilian" | "Civilian Water" => "civilian",
        "Archery" | "Ranged Gunpowder" | "Ranged" => "ranged",
        "Mounted" => "mounted",
        "Armored" | "Armor" | "Helicopter" => "armor",
        "Siege" => "siege",
        "Scout" => "recon",
        "Melee Water" => "naval_melee",
        "Ranged Water" | "Submarine" | "Aircraft Carrier" => "naval_ranged",
        "Fighter" | "Bomber" | "Atomic Bomber" | "Missile" => "air",
        _ => "melee",
    }
}

/// A unit as `viewer` sees it (`views.unit_info`): what anyone sees of it, and for its owner (or
/// a spectator) its movement, orders, experience and promotions; with `detail`, what its owner
/// needs to play its turn.
#[must_use]
pub fn unit_info(g: &Game, u: UnitId, viewer: Option<PlayerId>, detail: bool) -> Value {
    let Some(x) = g.unit(u) else { return Value::Null };
    let r = g.rules();
    let d = &r.base_units()[x.base];
    let (ux, uy) = g.xy(x.tile());
    let own = viewer.is_none_or(|v| v == x.owner());
    let type_name = &*d.name;
    let mut m = Map::new();
    m.insert("id".into(), json!(u.get()));
    m.insert("type".into(), json!(type_name));
    m.insert("name".into(), json!(x.name.as_deref().unwrap_or(type_name)));
    m.insert("owner".into(), json!(x.owner().0));
    m.insert("x".into(), json!(ux));
    m.insert("y".into(), json!(uy));
    m.insert("hp".into(), json!(x.hp));
    m.insert("military".into(), json!(d.military));
    m.insert("domain".into(), json!(domain_name(d.domain)));
    m.insert("unit_type".into(), json!(&*r.unit_types()[d.unit_type].name));
    m.insert("class".into(), json!(unit_class(g, d)));
    if d.great_person {
        m.insert("great_person".into(), json!(true));
    }
    if let Some(rel) = x.religion
        && g.religion_enabled()
    {
        m.insert("religion".into(), json!(religion::display_name(g, rel)));
    }
    if own {
        let sc = f64::from(r.constants().move_scale);
        let promote = promotions::can_promote(g, u);
        m.insert("moves".into(), json!(num::round_ndigits(f64::from(x.moves) / sc, 2)));
        m.insert(
            "max_moves".into(),
            json!(num::round_ndigits(f64::from(movement::max_moves(g, u)) / sc, 2)),
        );
        m.insert("activity".into(), json!(x.activity.map(|a| a.name())));
        m.insert("xp".into(), json!(x.xp));
        let promos: Vec<&str> = x.promotions.iter().filter_map(|p| r.name(p)).collect();
        m.insert("promotions".into(), json!(promos));
        m.insert("can_promote".into(), json!(promote));
        m.insert("promotion_ready".into(), json!(promote));
        m.insert("attacks_made".into(), json!(x.attacks));
        m.insert("embarked".into(), json!(movement::is_embarked(g, u)));
        if x.fortify > 0 {
            m.insert("fortified_turns".into(), json!(x.fortify));
        }
        if let (Some(back), Some(v)) = (x.return_offer, viewer) {
            let name = if g.has_met(v, back) {
                json!(super::name_of(g, back))
            } else {
                json!("its original owner")
            };
            m.insert("return_offer".into(), json!({"player": back.0, "name": name}));
        }
        if x.set_up {
            m.insert("status".into(), json!(["Set Up"]));
        }
        let building = matches!(
            x.activity,
            Some(crate::state::units::Activity::Build | crate::state::units::Activity::Automate)
        );
        let steps = g.state().tiles().builds(x.tile());
        if building && !steps.is_empty() {
            let list: Vec<Value> = steps
                .iter()
                .filter(|s| s.turns_left >= 0)
                .map(|s| {
                    json!({"improvement": &*r.improvements()[s.improvement].name, "turns_left": s.turns_left})
                })
                .collect();
            m.insert("building".into(), Value::Array(list));
        }
        if let Some(to) = x.goto {
            m.insert("goto".into(), xy(g, to));
        }
        if x.religion.is_some() && units::type_has(g, x.base, UniqueType::ReligiousUnit) {
            m.insert("religious_strength".into(), json!(x.religious_strength));
        }
    }
    if detail {
        unit_detail(g, u, d, own, &mut m);
    }
    Value::Object(m)
}

/// Everything an owner needs to give a unit orders: what it can do, where it can go and what it
/// can attack (`views._unit_detail`), so that one query is enough to play a unit's turn.
fn unit_detail(g: &Game, u: UnitId, d: &BaseUnitDef, own: bool, m: &mut Map<String, Value>) {
    let Some(x) = g.unit(u) else { return };
    let r = g.rules();
    m.insert("strength".into(), json!(d.strength));
    if d.ranged_strength != 0 {
        m.insert("ranged_strength".into(), json!(d.ranged_strength));
        m.insert("range".into(), json!(health::attack_range(g, u)));
    }
    m.insert("abilities".into(), json!(rule_text(g, &d.uniques)));
    if !own {
        return;
    }
    let owner = x.owner();
    let at = x.tile();
    let acts: Vec<Value> = actions::unit_actions(g, u).iter().map(|a| a.to_json()).collect();
    m.insert("actions".into(), Value::Array(acts));
    if units::type_has(g, x.base, UniqueType::FoundCity) {
        let sites: Vec<Value> = automation::suggest_city_sites(g, owner, at, 8, 3)
            .into_iter()
            .map(|(t, s)| {
                let (sx, sy) = g.xy(t);
                json!({"x": sx, "y": sy, "score": num::round_ndigits(s, 1)})
            })
            .collect();
        m.insert("suggested_city_sites".into(), Value::Array(sites));
    }
    let opts = build_options(g, u);
    if !opts.is_empty() {
        m.insert("build_options".into(), Value::Array(opts));
    }
    if units::type_has(g, x.base, UniqueType::CreateWaterImprovements) {
        let mut spots: Vec<_> = g
            .state()
            .tiles()
            .iter()
            .filter(|(t, tile)| {
                tile.owner() == Some(owner)
                    && tile.improvement().is_none()
                    && g.is_water(*t)
                    && tile.resource().is_some_and(|res| workers::resource_visible(g, owner, res))
            })
            .map(|(t, tile)| (t, tile.resource()))
            .collect();
        spots.sort_by_key(|&(t, _)| g.grid().distance(t, at));
        let list: Vec<Value> = spots
            .iter()
            .take(3)
            .map(|&(t, res)| {
                let (sx, sy) = g.xy(t);
                json!({"x": sx, "y": sy, "resource": res.and_then(|res| r.name(res))})
            })
            .collect();
        m.insert("suggested_sites".into(), Value::Array(list));
    }
    let up = upgrades::check_upgrade(g, u);
    if let Some(to) = up.target {
        m.insert(
            "upgrade".into(),
            json!({
                "to": r.name(to),
                "gold": up.cost,
                "possible": up.refusal.is_none(),
                "reason": up.refusal,
            }),
        );
    }
    if promotions::can_promote(g, u) {
        let mut names: Vec<&str> =
            promotions::available_promotions(g, u).into_iter().filter_map(|p| r.name(p)).collect();
        names.sort();
        m.insert("available_promotions".into(), json!(names));
    }
    m.insert("xp_for_next_promotion".into(), json!(promotions::xp_for_next(g, u)));
    let mut reach: Vec<u32> =
        movement::reachable_this_turn(g, u).into_iter().map(|(t, _)| t.0).collect();
    reach.sort();
    reach.dedup();
    let reach: Vec<Value> = reach
        .into_iter()
        .take(REACHABLE_SHOWN)
        .map(|t| xy(g, crate::base::ids::TileIdx(t)))
        .collect();
    m.insert("reachable_this_turn".into(), Value::Array(reach));
    let targets = attack_targets(g, u, d);
    if !targets.is_empty() {
        m.insert("attack_targets".into(), Value::Array(targets));
    }
}

/// What a unit could start building where it stands, the instant improvements left out
/// (`views.py:127-129`): each option's `id`, `name` and `turns`, with the feature removed first
/// and the improvement replaced where there are.
fn build_options(g: &Game, u: UnitId) -> Vec<Value> {
    let r = g.rules();
    let (Some(b), Some(t)) = (workers::Builder::unit(g, u), g.unit(u).map(|x| x.tile())) else {
        return Vec::new();
    };
    workers::build_options(g, &b, t, None)
        .into_iter()
        .map(|o| {
            let imp = &r.improvements()[o.imp];
            let id = if o.repair { "repair" } else { imp.key.as_deref().unwrap_or(&imp.name) };
            let mut m = Map::new();
            m.insert("id".into(), json!(id));
            m.insert("name".into(), json!(&*imp.name));
            m.insert("turns".into(), json!(o.turns));
            if let Some(f) = o.first_removes {
                let name = r.derived().features.get(f).and_then(|&t| r.name(t));
                m.insert("first_removes".into(), json!(name));
            }
            if let Some(old) = o.replaces {
                m.insert("replaces".into(), json!(r.name(old)));
            }
            Value::Object(m)
        })
        .collect()
}

/// What a military unit could attack now, with the damage both ways (`views.py:144-156`): every
/// tile in its range whose attack preview is not refused.
fn attack_targets(g: &Game, u: UnitId, d: &BaseUnitDef) -> Vec<Value> {
    let Some(x) = g.unit(u) else { return Vec::new() };
    if !d.military || d.domain == Domain::Air || resolve::can_attack_now(g, u).is_some() {
        return Vec::new();
    }
    let radius = if d.ranged { health::attack_range(g, u) } else { 1 };
    let radius = u32::try_from(radius).unwrap_or(0);
    let mut out = Vec::new();
    // In Python's `within` order, as it listed them.
    let mut near = g.grid().within(x.tile(), radius);
    near.sort_by_key(|&t| borders::within_order(g, x.tile(), t));
    for t in near.into_iter().skip(1) {
        let Ok(Value::Object(pv)) = resolve::preview(g, u, t) else { continue };
        let (tx, ty) = g.xy(t);
        let mut m = Map::new();
        m.insert("x".into(), json!(tx));
        m.insert("y".into(), json!(ty));
        for k in TARGET_KEYS {
            if let Some(v) = pv.get(k) {
                m.insert(k.into(), v.clone());
            }
        }
        out.push(Value::Object(m));
    }
    out
}

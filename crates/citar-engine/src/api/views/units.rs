//! A unit as a viewer sees it, and everything its owner needs to give it orders
//! (`views.unit_info` and `_unit_detail`, `views.py:50-157`).

use serde::Serialize;
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

/// A unit as a viewer sees it (`views.unit_info` without the detail), typed so that the client
/// view writes it straight to JSON: what anyone sees of it, and for its owner (or a spectator)
/// [`OwnUnit`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnitView<'a> {
    pub id: u32,
    #[serde(rename = "type")]
    pub base: &'a str,
    pub name: &'a str,
    pub owner: u8,
    pub x: i32,
    pub y: i32,
    pub hp: i16,
    pub military: bool,
    pub domain: &'static str,
    pub unit_type: &'a str,
    pub class: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub great_person: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub religion: Option<String>,
    #[serde(flatten)]
    pub own: Option<OwnUnit<'a>>,
}

/// What a unit's owner sees of it besides: its movement, orders, experience and promotions.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OwnUnit<'a> {
    pub moves: f64,
    pub max_moves: f64,
    pub activity: Option<&'static str>,
    pub xp: i32,
    pub promotions: Vec<&'a str>,
    pub can_promote: bool,
    pub promotion_ready: bool,
    pub attacks_made: u8,
    pub embarked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fortified_turns: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_offer: Option<ReturnOffer<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<[&'static str; 1]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub building: Option<Vec<BuildStepView<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goto: Option<[i32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub religious_strength: Option<i16>,
}

/// A recaptured civilian's first owner, whom it may be given back to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReturnOffer<'a> {
    pub player: u8,
    /// Its name, or "its original owner" to a viewer that has not met it.
    pub name: &'a str,
}

/// An improvement being built on a tile, and the turns it has left.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BuildStepView<'a> {
    pub improvement: &'a str,
    pub turns_left: i16,
}

/// A unit as `viewer` sees it, typed (`views.unit_info` without the detail).
#[must_use]
pub fn unit_view(g: &Game, u: UnitId, viewer: Option<PlayerId>) -> Option<UnitView<'_>> {
    let x = g.unit(u)?;
    let r = g.rules();
    let d = &r.base_units()[x.base];
    let (ux, uy) = g.xy(x.tile());
    let own = viewer.is_none_or(|v| v == x.owner());
    let type_name = &*d.name;
    let religion =
        x.religion.filter(|_| g.religion_enabled()).map(|rel| religion::display_name(g, rel));
    let own = own.then(|| {
        let sc = f64::from(r.constants().move_scale);
        let promote = promotions::can_promote(g, u);
        let return_offer = match (x.return_offer, viewer) {
            (Some(back), Some(v)) => Some(ReturnOffer {
                player: back.0,
                name: if g.has_met(v, back) {
                    super::name_of(g, back)
                } else {
                    "its original owner"
                },
            }),
            _ => None,
        };
        let building = matches!(
            x.activity,
            Some(crate::state::units::Activity::Build | crate::state::units::Activity::Automate)
        );
        let steps = g.state().tiles().builds(x.tile());
        let building = (building && !steps.is_empty()).then(|| {
            steps
                .iter()
                .filter(|s| s.turns_left >= 0)
                .map(|s| BuildStepView {
                    improvement: &r.improvements()[s.improvement].name,
                    turns_left: s.turns_left,
                })
                .collect()
        });
        let religious =
            x.religion.is_some() && units::type_has(g, x.base, UniqueType::ReligiousUnit);
        OwnUnit {
            moves: num::round_ndigits(f64::from(x.moves) / sc, 2),
            max_moves: num::round_ndigits(f64::from(movement::max_moves(g, u)) / sc, 2),
            activity: x.activity.map(|a| a.name()),
            xp: x.xp,
            promotions: x.promotions.iter().filter_map(|p| r.name(p)).collect(),
            can_promote: promote,
            promotion_ready: promote,
            attacks_made: x.attacks,
            embarked: movement::is_embarked(g, u),
            fortified_turns: (x.fortify > 0).then_some(x.fortify),
            return_offer,
            status: x.set_up.then_some(["Set Up"]),
            building,
            goto: x.goto.map(|t| {
                let (a, b) = g.xy(t);
                [a, b]
            }),
            religious_strength: religious.then_some(x.religious_strength),
        }
    });
    Some(UnitView {
        id: u.get(),
        base: type_name,
        name: x.name.as_deref().unwrap_or(type_name),
        owner: x.owner().0,
        x: ux,
        y: uy,
        hp: x.hp,
        military: d.military,
        domain: domain_name(d.domain),
        unit_type: &r.unit_types()[d.unit_type].name,
        class: unit_class(g, d),
        great_person: d.great_person.then_some(true),
        religion,
        own,
    })
}

/// A unit as `viewer` sees it (`views.unit_info`): [`unit_view`], and with `detail` what its
/// owner needs to play its turn.
#[must_use]
pub fn unit_info(g: &Game, u: UnitId, viewer: Option<PlayerId>, detail: bool) -> Value {
    let Some(view) = unit_view(g, u, viewer) else { return Value::Null };
    let own = view.own.is_some();
    // A view is text, numbers, lists and maps with text keys, which always convert.
    let mut v = serde_json::to_value(view).unwrap_or(Value::Null);
    if detail && let (Some(m), Some(x)) = (v.as_object_mut(), g.unit(u)) {
        unit_detail(g, u, &g.rules().base_units()[x.base], own, m);
    }
    v
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

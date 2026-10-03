//! The bot's deterministic sub-decisions, asked one at a time and written nowhere: what the
//! reference checks compare with the Python bot's (DESIGN.md P2.3.11, refcheck's `bot_decisions`
//! group and `cargo refcheck bot-agreement`).
//!
//! `scripts/refcheck/bot_dump.py` recorded, on each reference state and for each living major,
//! a fresh `BasicBot(seed=0)` with `tech_noise` 0 at the default aggression, its tool calls
//! recorded instead of made. [`ask`] answers the same questions of the same bot
//! ([`reference_spec`]: `basic-1`, `tech_noise` 0, aggression 0.4, a fresh memory, espionage
//! left to the model as the recorder left it), in the recording's JSON shape: names for
//! technologies, policies, beliefs and great people, `[x, y]` for tiles, ids for cities and
//! units, lists sorted as the recording sorted them.
//!
//! Stage 1 (package 2-01b) is the economy's: [`Question`]. Stages 2 and 3 (units and fighting,
//! diplomacy) come with packages 2-03 and 2-05.

use std::sync::{Arc, OnceLock};

use citar_engine::base::ids::{PlayerId, TileIdx};
use citar_engine::game::Game;
use citar_engine::game::diplomacy::category::Category;
use citar_engine::game::religion::found::can_found_pantheon;
use serde_json::{Map, Value, json};

use crate::basic1::context::{self, Context, city_defense, in_danger, needs_garrison};
use crate::basic1::{Seat, empire, gold, research};
use crate::memory::Memory;
use crate::owners::Owner;
use crate::params::{Overrides, Tuning};
use crate::versions::VersionId;
use crate::{BotSpec, clean};

/// A question of stage 1, as `bot_dump.py` names its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Question {
    /// `BasicBot.context`: the army target, the unit supply, gold per turn, happiness, the era,
    /// the wars, offense, the exposed cities, the luxuries owned, the resources waiting for an
    /// improvement, the enemies seen and the military.
    Context,
    /// `_tech_value` of every technology the civilization lacks and can research, in both modes.
    TechValues,
    /// `choose_research` as if nothing were being researched, in both modes: the free technology
    /// it would take now and the path's first step; and `preferred_free`, the free technology it
    /// would take if it held one.
    NextResearch,
    /// `empire_choices` without spies: the policy, the free great person and the pantheon it
    /// would take now, and each as if it could (`preferred_*`).
    Empire,
    /// Each city's threat, defence, danger and need of a garrison, by id.
    Cities,
    /// `expansion_sites`, best first.
    Sites,
    /// `_spare_units`, in order.
    Spare,
}

impl Question {
    /// Every question of stage 1, in `bot_dump.py`'s order.
    pub const ALL: [Self; 7] = [
        Self::Context,
        Self::TechValues,
        Self::NextResearch,
        Self::Empire,
        Self::Cities,
        Self::Sites,
        Self::Spare,
    ];

    /// Its kind's name in the recording.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::TechValues => "tech_values",
            Self::NextResearch => "next_research",
            Self::Empire => "empire",
            Self::Cities => "cities",
            Self::Sites => "sites",
            Self::Spare => "spare",
        }
    }

    /// The question a kind's name names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|q| q.name() == name)
    }
}

/// The bot the recording asked: `basic-1` at its defaults but `tech_noise` 0 (and the tech mode
/// given), aggression 0.4, espionage the model's.
#[must_use]
pub fn reference_spec(tech_mode: Option<&str>) -> Arc<BotSpec> {
    let mut o = Map::new();
    o.insert("tech_noise".to_owned(), json!(0));
    if let Some(m) = tech_mode {
        o.insert("tech_mode".to_owned(), json!(m));
    }
    // The overrides are literals of the schema, so they always clean; the defaults stand in if
    // they ever did not.
    let overrides = clean("basic-1", &Value::Object(o)).unwrap_or_else(|_| Overrides::default());
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, overrides));
    let owners = crate::Owners::default().with(Category::Espionage, Owner::Llm);
    Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None).with_owners(owners))
}

/// The three bots the questions are asked of: the default tech mode, classic and potential.
fn specs() -> &'static [Arc<BotSpec>; 3] {
    static SPECS: OnceLock<[Arc<BotSpec>; 3]> = OnceLock::new();
    SPECS.get_or_init(|| {
        [reference_spec(None), reference_spec(Some("classic")), reference_spec(Some("potential"))]
    })
}

/// Runs `f` with a seat of `spec` and a fresh memory, and the context of `pid`'s turn.
fn with_seat<T>(
    g: &Game,
    pid: PlayerId,
    spec: &BotSpec,
    f: impl FnOnce(&Seat<'_>, &Context) -> T,
) -> T {
    let resolved = spec.tuning.resolved(g.rules());
    let mut memory = Memory::default();
    let seat = Seat::new(spec, &resolved, &mut memory);
    let ctx = context::build(g, pid, &seat);
    f(&seat, &ctx)
}

/// The answer of `q` for major `pid` of `g`, in the recording's shape.
#[must_use]
pub fn ask(g: &Game, pid: PlayerId, q: Question) -> Value {
    let [plain, classic, potential] = specs();
    match q {
        Question::Context => with_seat(g, pid, plain, |_, ctx| context_json(g, ctx)),
        Question::TechValues => {
            let values = |spec: &BotSpec| {
                with_seat(g, pid, spec, |s, ctx| {
                    let mut memo = research::Memo::default();
                    let mut out = Map::new();
                    for t in g.rules().techs().ids() {
                        if g.has_tech(pid, Some(t))
                            || citar_engine::game::research::is_unresearchable(g, pid, t)
                        {
                            continue;
                        }
                        let v = research::value(g, pid, t, s, ctx, &mut memo);
                        out.insert(name(g, t), json!(v));
                    }
                    Value::Object(out)
                })
            };
            json!({"classic": values(classic), "potential": values(potential)})
        }
        Question::NextResearch => {
            let mode = |spec: &BotSpec| {
                with_seat(g, pid, spec, |s, ctx| {
                    json!({
                        "free": research::free_now(g, pid).map(|t| name(g, t)),
                        "tech": research::next(g, pid, s, ctx).map(|t| name(g, t)),
                    })
                })
            };
            json!({
                "classic": mode(classic),
                "potential": mode(potential),
                "preferred_free": research::free_choice(g, pid).map(|t| name(g, t)),
            })
        }
        Question::Empire => with_seat(g, pid, plain, |s, ctx| {
            let major = g.player(pid).is_some_and(|p| p.is_major());
            let holds = g.player(pid).is_some_and(|p| p.gp.free > 0);
            let (person, _) = empire::great_person_choice(s, ctx);
            let pantheon = || empire::pantheon_choice(g, s).map(|b| name(g, b));
            let religion = g.religion_enabled();
            json!({
                "policy": empire::policy_now(g, pid, s).map(|q| name(g, q)),
                "preferred_policy": major
                    .then(|| empire::policy_choice(g, pid, s))
                    .flatten()
                    .map(|q| name(g, q)),
                "great_person": holds.then_some(person),
                "preferred_great_person": person,
                "pantheon": (religion && can_found_pantheon(g, pid).is_none())
                    .then(pantheon)
                    .flatten(),
                "preferred_pantheon": (religion && major).then(pantheon).flatten(),
            })
        }),
        Question::Cities => with_seat(g, pid, plain, |s, ctx| {
            let mut cities = ctx.cities.clone();
            cities.sort();
            let rows: Vec<Value> = cities
                .into_iter()
                .map(|c| {
                    json!({
                        "city": c.get(),
                        "threat": ctx.threat(c),
                        "defense": city_defense(g, c),
                        "danger": in_danger(g, ctx, c, &s.advisor),
                        "garrison": needs_garrison(ctx, c),
                    })
                })
                .collect();
            Value::Array(rows)
        }),
        Question::Sites => with_seat(g, pid, plain, |s, _| {
            let adv = crate::basic1::advisor(g, pid, s);
            Value::Array(adv.sites(g).iter().map(|&t| xy(g, t)).collect())
        }),
        Question::Spare => with_seat(g, pid, plain, |s, ctx| {
            json!(gold::spare_units(g, pid, s, ctx).iter().map(|u| u.get()).collect::<Vec<_>>())
        }),
    }
}

/// The context as the recording wrote it.
fn context_json(g: &Game, ctx: &Context) -> Value {
    let sorted = |mut v: Vec<u32>| {
        v.sort();
        v
    };
    let names = |set: &citar_engine::base::sets::ResourceSet| {
        let mut v: Vec<String> = set.iter().map(|r| name(g, r)).collect();
        v.sort();
        v
    };
    let mut wars: Vec<u8> = ctx.wars.iter().map(|p| p.0).collect();
    wars.sort();
    json!({
        "army_target": ctx.army_target,
        "supply": ctx.supply,
        "gpt": ctx.gpt,
        "hap": ctx.hap,
        "era": ctx.era,
        "wars": wars,
        "offense": ctx.offense,
        "exposed": ctx.exposed.as_ref().map(|e| sorted(e.iter().map(|c| c.get()).collect())),
        "lux_owned": names(&ctx.lux_owned),
        "pending_res": names(&ctx.pending_res),
        "hostile": sorted(ctx.hostile.iter().map(|u| u.get()).collect()),
        "military": sorted(ctx.military.iter().map(|u| u.get()).collect()),
    })
}

/// A ruleset object's name.
fn name<I: citar_engine::rules::Named>(g: &Game, id: I) -> String {
    g.rules().name(id).unwrap_or_default().to_owned()
}

/// A tile as `[x, y]`.
fn xy(g: &Game, t: TileIdx) -> Value {
    let (x, y) = g.xy(t);
    json!([x, y])
}

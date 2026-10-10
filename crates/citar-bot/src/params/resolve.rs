//! A spec's parameter names resolved against a ruleset (DESIGN.md P2.3.2): the policy branch
//! orders, the beliefs per kind, the pantheon order, the free great-person choices and the
//! promotion lines, turned into ids once per ruleset, so the bot compares ids where Python
//! compared names (`_policy_order`, basic.py:1003-1009; `empire_choices`, 1032-1038;
//! `choose_beliefs`, 1702-1708; `_promote`, 1809-1819).
//!
//! Names are looked up exactly, as Python's `in` and `==` compared them. A name the ruleset
//! lacks (a mod's) is skipped and counted, never an error: the order keeps the names it has.
//! The resources kept for the spaceship (`_space_res`, basic.py:856-860) are a fact of the
//! ruleset alone, which it derives already (`derived().advisor.space_resources`). So are the five
//! city-state types the typed gifts weigh (`_gift_city_state`, basic.py:1670-1672), which the
//! bot names as Python did and finds here by id ([`Resolved::city_state_kind`]).

use citar_engine::base::ids::{BaseUnitId, BeliefId, CityStateTypeId, PolicyId};
use citar_engine::base::sets::PromotionSet;
use citar_engine::rules::Ruleset;
use citar_engine::rules::defs::BeliefKind;

use super::{NameList, Params, Spec, spec};

/// A city-state type the typed gifts weigh by name (`_gift_city_state`'s `type_w`,
/// basic.py:1670-1672); any other type weighs 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CityStateKind {
    Mercantile,
    Maritime,
    Cultured,
    Religious,
    Militaristic,
}

impl CityStateKind {
    /// Every kind, in Python's order.
    pub const ALL: [Self; 5] =
        [Self::Mercantile, Self::Maritime, Self::Cultured, Self::Religious, Self::Militaristic];

    /// The type's name in the ruleset.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mercantile => "Mercantile",
            Self::Maritime => "Maritime",
            Self::Cultured => "Cultured",
            Self::Religious => "Religious",
            Self::Militaristic => "Militaristic",
        }
    }
}

/// The names of a [`Params`] as one ruleset's ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The policy branches in order of preference below `aggressive_above`
    /// (`policy_order_peaceful`) and above it (`policy_order_aggressive`).
    pub policy_order_peaceful: Vec<PolicyId>,
    pub policy_order_aggressive: Vec<PolicyId>,
    /// The beliefs of each kind in order of preference, for `belief_mode = "prefs"`. An empty
    /// order stays empty here, and its user gives it Python's meaning: `empire_choices` takes
    /// the first pantheon available (1036). `choose_beliefs` ranks by
    /// [`belief_place`](Self::belief_place) instead, which keeps the places of the names the
    /// ruleset lacks and ranks a kind whose order is written empty by the four orders one after
    /// another (basic.py:1707).
    pub beliefs_pantheon: Vec<BeliefId>,
    pub beliefs_founder: Vec<BeliefId>,
    pub beliefs_follower: Vec<BeliefId>,
    pub beliefs_enhancer: Vec<BeliefId>,
    /// The pantheon order for `belief_mode = "unciv"`.
    pub pantheon_unciv: Vec<BeliefId>,
    /// The great person a free one is taken as, before `free_gp_switch_era` and from it.
    pub free_gp_early: Option<BaseUnitId>,
    pub free_gp_late: Option<BaseUnitId>,
    /// The promotions a unit in one of its cities takes first (`promo_in_city`), and those it
    /// takes otherwise (`promo_lines`): every promotion whose name starts with a listed line, as
    /// `str.startswith` with a tuple matched them.
    pub promo_in_city: PromotionSet,
    pub promo_lines: PromotionSet,
    /// Names the ruleset lacks, skipped: a policy that is no branch, a belief or a great person
    /// it does not have, a promotion line no promotion's name starts with.
    pub unknown_names: u32,
    /// Each policy's place among the policies by name, by id: the last key of the policy the
    /// bot adopts (`empire_choices`' `rank`, basic.py:1028), so that names are compared once.
    policy_rank: Vec<u16>,
    /// Each belief's place among the beliefs by name, by id (`choose_beliefs`' sort,
    /// basic.py:1708).
    belief_rank: Vec<u16>,
    /// Each belief's place in the order `choose_beliefs` ranks a kind by (basic.py:1707), by
    /// the kind's slot ([`BeliefKind::index`]) and then by id; `None` where the order does not
    /// name the belief. A place counts in the order as written, names the ruleset lacks
    /// included, as Python's `index` counted them, and a belief's first place counts. A kind
    /// written empty, and `Any`, are ranked by the four orders one after another (pantheon,
    /// founder, follower, enhancer): Python asked whether the order as written was empty, not
    /// whether the ruleset had its names.
    belief_places: [Vec<Option<u32>>; BeliefKind::COUNT],
    /// The kind of each city-state type, by id: `None` for a type none of the five names.
    city_state_kinds: Vec<Option<CityStateKind>>,
}

impl Resolved {
    /// `params`' names in `rules`.
    #[must_use]
    pub fn new(params: &Params, rules: &'static Ruleset) -> Self {
        let mut unknown: u32 = 0;
        let mut count = |found: bool| {
            if !found {
                unknown = unknown.saturating_add(1);
            }
        };
        let mut branches = |key: &str, list: &NameList| -> Vec<PolicyId> {
            let mut names = names_of(key, list);
            // Python's `order or POLICY_ORDER[...]`: an empty order is the default one.
            if names.is_empty() {
                names = names_of(key, &NameList::Default);
            }
            let mut out = Vec::with_capacity(names.len());
            for n in names {
                let id = rules
                    .lookup::<PolicyId>(&n)
                    .filter(|&id| rules.policies().get(id).is_some_and(|p| p.is_branch()));
                count(id.is_some());
                out.extend(id);
            }
            out
        };
        let policy_order_peaceful =
            branches("policy_order_peaceful", &params.policy_order_peaceful);
        let policy_order_aggressive =
            branches("policy_order_aggressive", &params.policy_order_aggressive);
        let mut beliefs = |names: &[String]| -> Vec<BeliefId> {
            let mut out = Vec::new();
            for n in names {
                let id = rules.lookup::<BeliefId>(n);
                count(id.is_some());
                out.extend(id);
            }
            out
        };
        // The four orders as written, in `BeliefType::ALL`'s order (Python's `prefs`).
        let written = [
            names_of("beliefs_pantheon", &params.beliefs_pantheon),
            names_of("beliefs_founder", &params.beliefs_founder),
            names_of("beliefs_follower", &params.beliefs_follower),
            names_of("beliefs_enhancer", &params.beliefs_enhancer),
        ];
        let beliefs_pantheon = beliefs(&written[0]);
        let beliefs_founder = beliefs(&written[1]);
        let beliefs_follower = beliefs(&written[2]);
        let beliefs_enhancer = beliefs(&written[3]);
        let pantheon_unciv = beliefs(&names_of("pantheon_unciv", &params.pantheon_unciv));
        let belief_places = belief_places(rules, &written);
        let mut great = |name: &str| {
            let id = rules.lookup::<BaseUnitId>(name);
            count(id.is_some());
            id
        };
        let free_gp_early = great(params.free_gp_early.name());
        let free_gp_late = great(params.free_gp_late.name());
        let mut lines = |key: &str, list: &NameList| -> PromotionSet {
            let mut set = PromotionSet::new();
            for line in names_of(key, list) {
                let mut any = false;
                for (id, p) in rules.promotions().iter() {
                    if p.name.starts_with(line.as_str()) {
                        set.insert(id);
                        any = true;
                    }
                }
                count(any);
            }
            set
        };
        let promo_in_city = lines("promo_in_city", &params.promo_in_city);
        let promo_lines = lines("promo_lines", &params.promo_lines);
        let city_state_kinds = rules
            .city_state_types()
            .iter()
            .map(|(_, t)| CityStateKind::ALL.into_iter().find(|k| k.name() == &*t.name))
            .collect();
        let policy_rank = name_ranks(rules.policies().iter().map(|(_, p)| &*p.name));
        let belief_rank = name_ranks(rules.beliefs().iter().map(|(_, b)| &*b.name));
        Self {
            policy_order_peaceful,
            policy_order_aggressive,
            beliefs_pantheon,
            beliefs_founder,
            beliefs_follower,
            beliefs_enhancer,
            pantheon_unciv,
            free_gp_early,
            free_gp_late,
            promo_in_city,
            promo_lines,
            unknown_names: unknown,
            policy_rank,
            belief_rank,
            belief_places,
            city_state_kinds,
        }
    }

    /// The kind of city-state type `t`, if it is one of the five the typed gifts weigh.
    #[must_use]
    pub fn city_state_kind(&self, t: CityStateTypeId) -> Option<CityStateKind> {
        self.city_state_kinds.get(usize::from(t.0)).copied().flatten()
    }

    /// Policy `p`'s place among the ruleset's policies by name (Python's order of `str`, by code
    /// point, which is UTF-8's byte order).
    #[must_use]
    pub fn policy_rank(&self, p: PolicyId) -> u16 {
        self.policy_rank.get(usize::from(p.0)).copied().unwrap_or(u16::MAX)
    }

    /// Belief `b`'s place among the ruleset's beliefs by name.
    #[must_use]
    pub fn belief_rank(&self, b: BeliefId) -> u16 {
        self.belief_rank.get(usize::from(b.0)).copied().unwrap_or(u16::MAX)
    }

    /// Belief `b`'s place in the order `choose_beliefs` ranks kind `kind` by: the kind's own
    /// order, or the four together when it was written empty or the kind is `Any`. `None` when
    /// that order does not name it.
    #[must_use]
    pub fn belief_place(&self, kind: BeliefKind, b: BeliefId) -> Option<u32> {
        self.belief_places[kind.index()].get(usize::from(b.0)).copied().flatten()
    }
}

/// The places [`Resolved::belief_place`] gives, from the four orders as written.
fn belief_places(
    rules: &Ruleset,
    written: &[Vec<String>; 4],
) -> [Vec<Option<u32>>; BeliefKind::COUNT] {
    let places = |names: &[String]| {
        let mut out = vec![None; rules.beliefs().len()];
        for (i, n) in names.iter().enumerate() {
            if let Some(slot) =
                rules.lookup::<BeliefId>(n).and_then(|b| out.get_mut(usize::from(b.0)))
                && slot.is_none()
            {
                *slot = Some(u32::try_from(i).unwrap_or(u32::MAX));
            }
        }
        out
    };
    let together = places(&written.concat());
    let own = |k: usize| if written[k].is_empty() { together.clone() } else { places(&written[k]) };
    // In `BeliefKind::index` order: the four types, then `Any`.
    [own(0), own(1), own(2), own(3), together.clone()]
}

/// The place of each of `names` (given in id order) among them sorted, by id.
fn name_ranks<'a>(names: impl Iterator<Item = &'a str>) -> Vec<u16> {
    let names: Vec<&str> = names.collect();
    let mut by_name: Vec<usize> = (0..names.len()).collect();
    by_name.sort_by(|&a, &b| names[a].cmp(names[b]).then(a.cmp(&b)));
    let mut rank = vec![0u16; names.len()];
    for (place, id) in by_name.into_iter().enumerate() {
        rank[id] = u16::try_from(place).unwrap_or(u16::MAX);
    }
    rank
}

/// The names `list` gives for parameter `key` (DESIGN.md P2.3.2, [`NameList`]): its own; a
/// preset's; or, for `Default`, the parameter's `default` preset, else its default names (or
/// the preset its default names).
pub(crate) fn names_of(key: &str, list: &NameList) -> Vec<String> {
    let owned = |names: &[&str]| names.iter().map(|&n| n.to_owned()).collect();
    let Some(s) = spec(key) else { return Vec::new() };
    match list {
        NameList::Names(names) => names.iter().map(|n| n.to_string()).collect(),
        NameList::Preset(p) => s.preset(p).map(owned).unwrap_or_default(),
        NameList::Default => default_names(s).map(owned).unwrap_or_default(),
    }
}

/// What `Default` means for spec `s`.
fn default_names(s: &Spec) -> Option<&'static [&'static str]> {
    use super::Fixed;
    s.preset("default").or(match s.default {
        Fixed::Names(names) => Some(names),
        Fixed::Name(Some(p)) => s.preset(p),
        _ => None,
    })
}

//! A spec's parameter names resolved against a ruleset (DESIGN.md P2.3.2): the policy branch
//! orders, the beliefs per kind, the pantheon order, the free great-person choices and the
//! promotion lines, turned into ids once per ruleset, so the bot compares ids where Python
//! compared names (`_policy_order`, basic.py:1003-1009; `empire_choices`, 1032-1038;
//! `choose_beliefs`, 1702-1708; `_promote`, 1809-1819).
//!
//! Names are looked up exactly, as Python's `in` and `==` compared them. A name the ruleset
//! lacks (a mod's) is skipped and counted, never an error: the order keeps the names it has.
//! The resources kept for the spaceship (`_space_res`, basic.py:856-860) are a fact of the
//! ruleset alone, which it derives already (`derived().advisor.space_resources`).

use citar_engine::base::ids::{BaseUnitId, BeliefId, PolicyId};
use citar_engine::base::sets::PromotionSet;
use citar_engine::rules::Ruleset;

use super::{NameList, Params, Spec, spec};

/// The names of a [`Params`] as one ruleset's ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The policy branches in order of preference below `aggressive_above`
    /// (`policy_order_peaceful`) and above it (`policy_order_aggressive`).
    pub policy_order_peaceful: Vec<PolicyId>,
    pub policy_order_aggressive: Vec<PolicyId>,
    /// The beliefs of each kind in order of preference, for `belief_mode = "prefs"`.
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
        let mut beliefs = |key: &str, list: &NameList| -> Vec<BeliefId> {
            let mut out = Vec::new();
            for n in names_of(key, list) {
                let id = rules.lookup::<BeliefId>(&n);
                count(id.is_some());
                out.extend(id);
            }
            out
        };
        let beliefs_pantheon = beliefs("beliefs_pantheon", &params.beliefs_pantheon);
        let beliefs_founder = beliefs("beliefs_founder", &params.beliefs_founder);
        let beliefs_follower = beliefs("beliefs_follower", &params.beliefs_follower);
        let beliefs_enhancer = beliefs("beliefs_enhancer", &params.beliefs_enhancer);
        let pantheon_unciv = beliefs("pantheon_unciv", &params.pantheon_unciv);
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
        }
    }
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

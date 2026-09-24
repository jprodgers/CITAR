//! How each group's answer is compared: which lists are keyed, which are multisets, which need a
//! custom rule, and what is not compared at all (DESIGN.md 9.2).
//!
//! Lists are compared in order unless the spec says otherwise. Answer modules replay the recorded
//! inputs in order, so most lists line up by index anyway; keying a list by its id gives reports a
//! readable selector (`cities[id=9]` rather than `cities[4]`) and survives a Rust answer that
//! lists the same things in another order. Lists that are sets in meaning are multisets
//! (refcheck/README.md, and DESIGN.md 4.6 for the lists that became sets).
//!
//! The package that writes a group's answer module owns its spec and refines it; the specs below
//! are what the recorded answers already show.

use super::Seg;
use super::path::{Cursor, Pattern, accepted, start, step};
use crate::Group;

/// What identifies an element of a keyed list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKey {
    /// The value of this field of each element, which is an object: `[pid=0]`.
    Field(String),
    /// The value at this position of each element, which is a list: `[#0=12]`.
    Pos(usize),
}

/// The fields of a route entry, for [`Rule::Route`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteFields {
    /// The list of tiles, start and end included.
    pub path: String,
    pub turns: String,
    /// One cost per step.
    pub costs: String,
}

/// A comparison rule other than "in order", for the node a pattern matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// A list whose elements are matched by key, in any order.
    Keyed(ListKey),
    /// A list whose order means nothing.
    Multiset,
    /// An object holding a route, compared as [`super::route`] describes: a custom rule.
    Route(RouteFields),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tag {
    /// Not compared: `fn`, which names the Python functions, and has no Rust counterpart.
    Ignore,
    /// Compared only with `--with-bot` (Phase 2): the bot's valuations.
    BotOnly,
    Rule(Rule),
}

/// What a node of an answer is to the comparer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Treatment<'a> {
    Skip,
    Compare(Option<&'a Rule>),
}

/// The comparison rules of one group. Patterns are matched against the concrete path of each
/// node as the comparer walks down, all at once (see [`super::path::Cursor`]).
#[derive(Debug, Clone)]
pub struct CompareSpec {
    group: Group,
    patterns: Vec<Pattern>,
    tags: Vec<Tag>,
}

impl CompareSpec {
    /// A spec with no rules: everything compared, lists in order.
    pub fn new(group: Group) -> CompareSpec {
        CompareSpec { group, patterns: Vec::new(), tags: Vec::new() }
    }

    pub fn group(&self) -> Group {
        self.group
    }

    fn add(mut self, pattern: &str, tag: Tag) -> CompareSpec {
        // The built-in patterns are literals, and a unit test builds every spec, so a typo fails
        // there rather than in a run.
        let pattern = Pattern::parse(pattern).unwrap_or_else(|e| panic!("compare spec: {e}"));
        self.patterns.push(pattern);
        self.tags.push(tag);
        self
    }

    #[must_use]
    pub fn ignore(self, pattern: &str) -> CompareSpec {
        self.add(pattern, Tag::Ignore)
    }

    #[must_use]
    pub fn bot_only(self, pattern: &str) -> CompareSpec {
        self.add(pattern, Tag::BotOnly)
    }

    #[must_use]
    pub fn keyed(self, pattern: &str, field: &str) -> CompareSpec {
        self.add(pattern, Tag::Rule(Rule::Keyed(ListKey::Field(field.into()))))
    }

    #[must_use]
    pub fn keyed_pos(self, pattern: &str, pos: usize) -> CompareSpec {
        self.add(pattern, Tag::Rule(Rule::Keyed(ListKey::Pos(pos))))
    }

    #[must_use]
    pub fn multiset(self, pattern: &str) -> CompareSpec {
        self.add(pattern, Tag::Rule(Rule::Multiset))
    }

    #[must_use]
    pub fn route(self, pattern: &str, path: &str, turns: &str, costs: &str) -> CompareSpec {
        let fields = RouteFields { path: path.into(), turns: turns.into(), costs: costs.into() };
        self.add(pattern, Tag::Rule(Rule::Route(fields)))
    }

    pub(super) fn start(&self) -> Cursor {
        start(&self.patterns)
    }

    pub(super) fn step(&self, cursor: &Cursor, seg: &Seg) -> Cursor {
        if cursor.is_empty() {
            return Cursor::new();
        }
        step(&self.patterns, cursor, seg)
    }

    /// How to treat the node the cursor stands at. The first rule in the spec wins
    /// ([`accepted`] yields patterns in the spec's order).
    pub(super) fn treatment(&self, cursor: &Cursor, with_bot: bool) -> Treatment<'_> {
        let mut rule = None;
        for i in accepted(&self.patterns, cursor) {
            match &self.tags[i] {
                Tag::Ignore => return Treatment::Skip,
                Tag::BotOnly if !with_bot => return Treatment::Skip,
                Tag::BotOnly => {}
                Tag::Rule(r) => rule = rule.or(Some(r)),
            }
        }
        Treatment::Compare(rule)
    }

    /// Whether everything a pattern can match is compared only with `--with-bot`. An intended
    /// entry there is not covered by a run without it, so it is not stale.
    pub fn is_bot_only(&self, pattern: &Pattern) -> bool {
        self.patterns
            .iter()
            .zip(&self.tags)
            .any(|(p, t)| *t == Tag::BotOnly && pattern.starts_with(p))
    }

    /// The built-in spec of a group.
    pub fn for_group(group: Group) -> CompareSpec {
        let spec = CompareSpec::new(group).ignore("fn");
        match group {
            // The Rust answer folds action and meta modifiers, so their order is not kept.
            Group::Uniques => spec.keyed("uniques", "id").multiset("uniques[*].modifiers"),
            // Lists that became sets compare as multisets (DESIGN.md 4.5); entities by their ids.
            Group::StateEcho => spec
                .keyed("tiles", "i")
                .multiset("tiles[*].features")
                .keyed("players", "id")
                .multiset("players[*].techs")
                .multiset("players[*].policies")
                .multiset("players[*].natural_wonders")
                .multiset("players[*].met")
                .multiset("players[*].protectors")
                .multiset("players[*].eras_spy_earned")
                .multiset("players[*].skip_explore")
                .multiset("players[*].gained")
                .multiset("players[*].free_stat_buildings")
                .multiset("players[*].free_specific_buildings")
                .multiset("players[*].explored")
                .multiset("players[*].memory.*.f")
                .keyed_pos("players[*].remembered_cities", 0)
                .keyed("units", "id")
                .multiset("units[*].promotions")
                .keyed("cities", "id")
                .multiset("cities[*].buildings")
                .multiset("cities[*].free_buildings")
                .multiset("cities[*].worked")
                .multiset("cities[*].locked")
                .keyed("diplomacy.relations", "pair")
                .multiset("diplomacy.relations[*].embassy")
                .multiset("diplomacy.opinions")
                .keyed("diplomacy.deals", "id")
                .keyed("diplomacy.negotiations", "id")
                .multiset("world.religions[*].founder_beliefs")
                .multiset("world.religions[*].follower_beliefs")
                .multiset("world.un.won")
                .keyed("world.camps", "id")
                .keyed("history.events", "id")
                .multiset("history.events[*].players")
                .keyed("history.messages", "id")
                .multiset("history.messages[*].to"),
            Group::FixedPoint => {
                spec.keyed("civs", "pid").multiset("civs[*].explored").multiset("civs[*].met")
            }
            Group::TileYields => spec.keyed("owned", "idx"),
            Group::CityStats => spec.keyed("cities", "id").multiset("cities[*].workable"),
            Group::Civs => spec
                .keyed("civs", "pid")
                .multiset("civs[*].happiness.luxury_types")
                .multiset("civs[*].detailed_resources")
                .multiset("civs[*].adoptable_policies"),
            Group::Buildable => spec.keyed("cities", "city").multiset("cities[*].items.*"),
            Group::Movement => spec
                .keyed("reachable", "unit")
                .keyed_pos("reachable[*].reachable", 0)
                .route("paths[*]", "path", "turns", "step_costs"),
            Group::Visible => spec.keyed("civs", "pid").multiset("civs[*].tiles"),
            Group::CombatPreviews => spec,
            Group::DealChecks => spec.bot_only("deals[*].bot_value"),
            Group::ToolErrors => spec,
            Group::Views => spec
                .keyed("civs", "pid")
                .keyed("civs[*].view.units", "id")
                .keyed("civs[*].view.cities", "id")
                .multiset("civs[*].view.cities[*].buildings")
                .keyed("civs[*].view.players", "id")
                .multiset("civs[*].view.empire.policies")
                .multiset("civs[*].view.empire.happiness.luxury_types")
                .keyed("civs[*].view.diplomacy.players", "id")
                .multiset("civs[*].view.diplomacy.players[*].trade_options.*.*")
                .multiset(
                    "civs[*].view.diplomacy.players[*].trade_options.agreements_possible_now",
                ),
            Group::Briefing => spec.keyed("civs", "pid"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::path::Path;

    fn at<'s>(spec: &'s CompareSpec, path: &Path, with_bot: bool) -> Treatment<'s> {
        let mut cursor = spec.start();
        for seg in path.segs() {
            cursor = spec.step(&cursor, seg);
        }
        spec.treatment(&cursor, with_bot)
    }

    #[test]
    fn every_group_has_a_spec_that_builds() {
        for g in Group::ALL {
            assert_eq!(CompareSpec::for_group(g).group(), g);
        }
    }

    #[test]
    fn treatments_follow_the_patterns() {
        let spec = CompareSpec::for_group(Group::DealChecks);
        let bot = Path(vec![Seg::Key("deals".into()), Seg::Index(0), Seg::Key("bot_value".into())]);
        assert_eq!(at(&spec, &bot, false), Treatment::Skip);
        assert_eq!(at(&spec, &bot, true), Treatment::Compare(None));
        assert_eq!(at(&spec, &Path(vec![Seg::Key("fn".into())]), true), Treatment::Skip);
        assert!(spec.is_bot_only(&Pattern::parse("deals[*].bot_value.**").unwrap()));
        assert!(!spec.is_bot_only(&Pattern::parse("deals[*].valid").unwrap()));

        let spec = CompareSpec::for_group(Group::Civs);
        let civs = Path(vec![Seg::Key("civs".into())]);
        assert_eq!(
            at(&spec, &civs, false),
            Treatment::Compare(Some(&Rule::Keyed(ListKey::Field("pid".into()))))
        );
    }
}

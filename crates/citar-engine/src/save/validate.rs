//! What a loaded state must satisfy before a game may run on it (DESIGN.md 4.9).
//!
//! A corrupt save must be refused on load, never panic later: rule code indexes tables by the
//! ids the state holds and looks up the cities and players it names. `State::from_parts` already
//! refuses what would break the containers (entity ids above `MAX_ENTITY_ID`, owners that are
//! not players, tiles claimed by missing cities, cities and units off the map, broken carrier
//! links, explored sets and memories that do not fit the map, id counters behind ids in use).
//! [`validate`] checks the rest:
//! - **references:** capitals are cities their player holds; every other player, tile, religion
//!   and city a field names exists, or for a city that may since have been razed (an original
//!   capital, a unit's home), was handed out by the counter; goto and path tiles, worked and
//!   locked tiles, start tiles, spies' cities and remembered cities lie on the map; remembered
//!   owners and opinions' holders and subjects are players (an opinion never of oneself);
//! - **rule ids** within the ruleset's tables, remembered improvements and features included, for
//!   states built other than from a save (the converter), where no name was resolved;
//! - **ranges:** player-indexed lists no longer than the players, sorted lists sorted, the id
//!   counters from 1 and within `MAX_ENTITY_ID`, the barbarian aggression a percentage, map
//!   dimensions the grid takes, at most 256 founded religions;
//! - **shape:** majors and only majors have major data, city-states and only city-states theirs;
//! - **floats:** every one finite, by the canonical walk (`save::canon::check_finite`);
//! - a driver's memory within `DriverMemory::MAX_LEN`.
//!
//! Replaces nothing in Python, whose `from_dict` trusted the file.

use core::fmt;

use crate::base::hex::{MAX_SIDE, MIN_SIDE};
use crate::base::ids::{CityId, Id, PlayerId, ReligionId, TileIdx};
use crate::base::sets::{IdSet, PlayerSet};
use crate::rules::Ruleset;
use crate::state::State;
use crate::state::cities::Constructible;
use crate::state::config::{MapSource, ResourceRule};
use crate::state::diplo::DealItem;
use crate::state::memory::TileMemoryLayer;
use crate::state::players::{DriverMemory, Player, QuestTarget};
use crate::state::store::MAX_ENTITY_ID;
use crate::state::world::ReligionName;

use super::canon;

/// One thing wrong with a state: where, and what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// The place: `players[2].capital`.
    pub path: String,
    /// What is wrong there.
    pub message: String,
}

impl ValidationError {
    /// A problem at `path`.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into() }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// The most problems reported: past this many the save is plainly broken.
const MAX_ERRORS: usize = 100;

/// Checks everything a loaded state must satisfy beyond `State::from_parts`; every problem
/// found, up to a hundred.
pub fn validate(st: &State, rules: &Ruleset) -> Result<(), Vec<ValidationError>> {
    let mut c = Check {
        st,
        r: rules,
        n: st.players().len(),
        size: st.map().size(),
        religions: st.world().religions.len(),
        errs: Vec::new(),
    };
    if let Err(e) = canon::check_finite(st) {
        c.err("state", e.to_string());
    }
    c.clock_and_config();
    c.tiles();
    for (p, player) in st.players().iter() {
        c.player_data(&format!("players[{p}]"), p, player);
    }
    c.units();
    c.cities();
    c.diplomacy();
    c.world();
    c.chronicle();
    if c.errs.is_empty() { Ok(()) } else { Err(c.errs) }
}

struct Check<'a> {
    st: &'a State,
    r: &'a Ruleset,
    n: usize,
    size: u32,
    religions: usize,
    errs: Vec<ValidationError>,
}

impl Check<'_> {
    fn err(&mut self, path: impl Into<String>, message: impl Into<String>) {
        if self.errs.len() < MAX_ERRORS {
            self.errs.push(ValidationError::new(path, message));
        }
    }

    fn player(&mut self, path: &str, p: PlayerId) {
        if usize::from(p.0) >= self.n {
            self.err(path, format!("player {p} is not one of the {} players", self.n));
        }
    }

    fn players(&mut self, path: &str, set: PlayerSet) {
        if let Some(p) = set.iter().find(|p| usize::from(p.0) >= self.n) {
            self.err(path, format!("player {p} is not one of the {} players", self.n));
        }
    }

    fn tile(&mut self, path: &str, t: TileIdx) {
        if t.0 >= self.size {
            self.err(path, format!("tile {t} is off the map of {} tiles", self.size));
        }
    }

    fn religion(&mut self, path: &str, r: ReligionId) {
        if usize::from(r.0) >= self.religions {
            self.err(path, format!("religion {r} was never founded"));
        }
    }

    /// A city that may have been razed since: its id was handed out.
    fn city_ever(&mut self, path: &str, c: CityId) {
        if c.get() >= self.st.ids().city {
            self.err(path, format!("city {c} was never founded"));
        }
    }

    fn rule<I: Id>(&mut self, path: &str, id: I, len: usize) {
        if id.index() >= len {
            self.err(path, format!("{} {} is not in the ruleset's {len}", I::NAME, id.index()));
        }
    }

    fn rules<I: Id>(&mut self, path: &str, ids: impl IntoIterator<Item = I>, len: usize) {
        for id in ids {
            self.rule(path, id, len);
        }
    }

    fn set<I: Id, const W: usize>(&mut self, path: &str, set: &IdSet<I, W>, len: usize) {
        self.rules(path, set.iter(), len);
    }

    fn sorted<T: Ord + fmt::Debug>(&mut self, path: &str, items: &[T]) {
        if items.windows(2).any(|w| w[0] >= w[1]) {
            self.err(path, "is not strictly ascending");
        }
    }

    fn constructible(&mut self, path: &str, c: Constructible) {
        match c {
            Constructible::Building(b) => self.rule(path, b, self.r.buildings().len()),
            Constructible::Unit(u) => self.rule(path, u, self.r.base_units().len()),
            Constructible::Perpetual(_) => {}
        }
    }

    fn clock_and_config(&mut self) {
        let (st, r) = (self.st, self.r);
        let clock = st.clock();
        if usize::from(clock.current.0) >= self.n {
            self.err(
                "clock.current",
                format!("player {} is not one of the players", clock.current),
            );
        }
        if let Some(w) = clock.winner {
            self.player("clock.winner", w);
        }
        if let Some(v) = clock.victory {
            self.rule("clock.victory", v, r.victories().len());
        }
        let cfg = st.config();
        self.rule("config.speed", cfg.speed, r.speeds().len());
        self.rule("config.difficulty", cfg.difficulty, r.difficulties().len());
        self.rule("config.barbarian_difficulty", cfg.barbarian_difficulty, r.difficulties().len());
        self.rule("config.starting_era", cfg.starting_era, r.eras().len());
        self.rule("config.barbarians", cfg.barbarians, r.constants().barbarian_levels.len());
        match &cfg.map {
            MapSource::Generated { size, map_type, dims, .. } => {
                self.rule("config.map.size", *size, r.map_sizes().len());
                self.rule("config.map.map_type", *map_type, r.constants().map_types.len());
                if let Some((w, h)) = *dims
                    && ![w, h].iter().all(|s| (MIN_SIDE..=MAX_SIDE).contains(s))
                {
                    self.err("config.map.dims", format!("{w}x{h} is not a map the grid takes"));
                }
            }
            MapSource::Editor { size, .. } => {
                self.rule("config.map.size", *size, r.map_sizes().len());
            }
        }
        if cfg.barbarian_aggression.is_some_and(|a| a > 100) {
            self.err("config.barbarian_aggression", "is not a percentage");
        }
        self.sorted("config.disabled_victories", &cfg.disabled_victories);
        self.rules(
            "config.disabled_victories",
            cfg.disabled_victories.iter().copied(),
            r.victories().len(),
        );
        let res = &cfg.resources;
        for (path, kind) in [
            ("config.resources.strategic", &res.strategic),
            ("config.resources.luxury", &res.luxury),
            ("config.resources.bonus", &res.bonus),
        ] {
            let ids: Vec<_> = kind.each.iter().map(|(id, _)| *id).collect();
            self.sorted(path, &ids);
            self.rules(path, ids, r.resources().len());
            let negative = kind.density < 0.0
                || kind.each.iter().any(|(_, rule)| match rule {
                    ResourceRule::Off => false,
                    ResourceRule::Cap(x) | ResourceRule::Share(x) => *x < 0.0,
                });
            if negative {
                self.err(path, "a density or a rule is negative");
            }
        }
        if res.density < 0.0 || cfg.river_density < 0.0 {
            self.err("config", "a density is negative");
        }
        let ids = st.ids();
        let cap = MAX_ENTITY_ID + 1;
        for (what, next) in [("unit", ids.unit), ("city", ids.city), ("camp", ids.camp)] {
            if next > cap {
                self.err(format!("ids.{what}"), format!("{next} is past the largest entity id"));
            }
        }
        // Ids start at 1, so a counter at 0 would never hand one out again.
        let counters = [
            ("unit", ids.unit),
            ("city", ids.city),
            ("camp", ids.camp),
            ("deal", ids.deal),
            ("negotiation", ids.negotiation),
        ];
        for (what, _) in counters.into_iter().filter(|&(_, next)| next == 0) {
            self.err(format!("ids.{what}"), "is 0, and the first id is 1");
        }
    }

    fn tiles(&mut self) {
        let (st, r) = (self.st, self.r);
        let features = r.derived().features.len();
        for (t, tile) in st.tiles().iter() {
            let path = format!("tiles[{t}]");
            self.rule(&path, tile.terrain(), r.terrains().len());
            if let Some(w) = tile.wonder() {
                self.rule(&path, w, r.terrains().len());
            }
            if let Some(x) = tile.resource() {
                self.rule(&path, x, r.resources().len());
            }
            if let Some(i) = tile.improvement() {
                self.rule(&path, i, r.improvements().len());
            }
            self.rules(&path, tile.features().iter(), features);
            if self.errs.len() >= MAX_ERRORS {
                return;
            }
        }
        for (t, q) in st.tiles().all_builds() {
            self.rules(
                &format!("tiles.builds[{t}]"),
                q.iter().map(|b| b.improvement),
                r.improvements().len(),
            );
        }
    }

    fn player_data(&mut self, path: &str, p: PlayerId, pl: &Player) {
        let (st, r) = (self.st, self.r);
        self.rule(&format!("{path}.nation"), pl.nation, r.nations().len());
        if let Some(d) = pl.seat().difficulty() {
            self.rule(&format!("{path}.seat.difficulty"), d, r.difficulties().len());
        }
        if let Some(d) = pl.seat().driver()
            && d.bytes().len() > DriverMemory::MAX_LEN
        {
            self.err(format!("{path}.seat.driver"), "is over the size limit");
        }
        if let Some(c) = pl.capital
            && st.cities().get(c).map(|c| c.owner()) != Some(p)
        {
            self.err(format!("{path}.capital"), format!("city {c} is not one of this player's"));
        }
        if let Some(c) = pl.original_capital {
            self.city_ever(&format!("{path}.original_capital"), c);
        }
        if let Some(t) = pl.start_tile {
            self.tile(&format!("{path}.start_tile"), t);
        }
        let tech = &pl.tech;
        let techs = r.techs().len();
        self.set(&format!("{path}.tech.known"), &tech.known, techs);
        self.rules(&format!("{path}.tech.queue"), tech.queue.iter().copied(), techs);
        self.rules(&format!("{path}.tech.goal"), tech.goal, techs);
        self.rules(&format!("{path}.tech.progress"), tech.progress.keys().copied(), techs);
        self.set(&format!("{path}.policy.adopted"), &pl.policy.adopted, r.policies().len());
        let gp = &pl.gp;
        let units = r.base_units().len();
        self.rules(&format!("{path}.gp.points"), gp.points.keys().copied(), units);
        self.rules(&format!("{path}.gp.combat_points"), gp.combat_points.keys().copied(), units);
        self.rules(
            &format!("{path}.gp.combat_threshold"),
            gp.combat_threshold.keys().copied(),
            units,
        );
        self.rules(
            &format!("{path}.gp.long_count_pool"),
            gp.long_count_pool.iter().copied(),
            units,
        );
        let texts = r.uniques().texts.len();
        self.rules(
            &format!("{path}.gp.pool_threshold"),
            gp.pool_threshold.keys().filter_map(|k| *k),
            texts,
        );
        if let Some(f) = pl.religion.founded {
            self.religion(&format!("{path}.religion.founded"), f);
        }
        let civ = &pl.civ;
        let uniques = r.uniques().len();
        self.rules(
            &format!("{path}.civ.temp_uniques"),
            civ.temp_uniques.iter().map(|t| t.unique),
            uniques,
        );
        for c in civ.built_increasing.keys().chain(civ.bought_increasing.keys()) {
            self.constructible(&format!("{path}.civ"), *c);
        }
        self.sorted(&format!("{path}.civ.free_stat_buildings"), &civ.free_stat_buildings);
        self.sorted(&format!("{path}.civ.free_specific_buildings"), &civ.free_specific_buildings);
        for &(b, c) in &civ.free_specific_buildings {
            self.rule(&format!("{path}.civ.free_specific_buildings"), b, r.buildings().len());
            self.city_ever(&format!("{path}.civ.free_specific_buildings"), c);
        }
        for &(_, c) in &civ.free_stat_buildings {
            self.city_ever(&format!("{path}.civ.free_stat_buildings"), c);
        }
        self.set(&format!("{path}.civ.natural_wonders"), &civ.natural_wonders, r.terrains().len());
        self.set(&format!("{path}.civ.units_gained"), &civ.units_gained, units);
        self.rules(
            &format!("{path}.civ.last_ruins"),
            civ.last_ruins.iter().flatten().copied(),
            r.ruins().len(),
        );
        self.sorted(&format!("{path}.civ.explore_skip"), &civ.explore_skip);
        for &t in &civ.explore_skip {
            self.tile(&format!("{path}.civ.explore_skip"), t);
        }
        if pl.major.is_some() != pl.is_major() || pl.city_state.is_some() != pl.is_city_state() {
            self.err(path, "its data does not match its kind");
        }
        if let Some(m) = &pl.major {
            for (i, spy) in m.spies.iter().enumerate() {
                if let Some(c) = spy.city {
                    self.city_ever(&format!("{path}.major.spies[{i}].city"), c);
                }
            }
            self.set(&format!("{path}.major.spy_eras_earned"), &m.spy_eras_earned, r.eras().len());
            self.rules(&format!("{path}.major.spaceship"), m.spaceship.keys().copied(), units);
            self.memory(&format!("{path}.major.memory"), &m.memory);
            for (t, c) in m.memory.cities() {
                self.tile(&format!("{path}.major.memory"), t);
                self.player(&format!("{path}.major.memory[{t}].owner"), c.owner);
            }
        }
        if let Some(cs) = &pl.city_state {
            let cp = format!("{path}.city_state");
            if let Some(t) = cs.cs_type {
                self.rule(&cp, t, r.city_state_types().len());
            }
            self.rules(&cp, cs.resource, r.resources().len());
            self.rules(&cp, cs.unique_unit, units);
            if cs.influence.len() > self.n || cs.pairs.len() > self.n {
                self.err(&cp, "a list by player is longer than the players");
            }
            self.players(&format!("{cp}.protectors"), cs.protectors);
            if let Some(a) = cs.ally() {
                self.player(&format!("{cp}.ally"), a);
            }
            for (i, q) in cs.quests.iter().enumerate() {
                let qp = format!("{cp}.quests[{i}]");
                self.player(&qp, q.assignee);
                self.rule(&qp, q.kind, r.quests().len());
                match q.target {
                    QuestTarget::None | QuestTarget::Baseline(_) | QuestTarget::Percent(_) => {}
                    QuestTarget::Tile(t) => self.tile(&qp, t),
                    QuestTarget::Resource(x) => self.rule(&qp, x, r.resources().len()),
                    QuestTarget::Building(b) => self.rule(&qp, b, r.buildings().len()),
                    QuestTarget::UnitType(u) => self.rule(&qp, u, units),
                    QuestTarget::Player(x) => self.player(&qp, x),
                    QuestTarget::NaturalWonder(w) => self.rule(&qp, w, r.terrains().len()),
                    QuestTarget::Religion(x) => self.religion(&qp, x),
                }
            }
            for &x in cs.timers.individual.keys() {
                self.player(&format!("{cp}.timers"), x);
            }
            for (&x, w) in &cs.war_quests {
                self.player(&format!("{cp}.war_quests"), x);
                for &k in w.kills.keys() {
                    self.player(&format!("{cp}.war_quests[{x}]"), k);
                }
            }
        }
    }

    /// A major's remembered tiles: owners among the players, improvements and features in the
    /// ruleset. A save's columns resolve improvements and features by name, but copy the owner
    /// byte, and the converter's layers may hold anything. Only a tile found wrong costs a path.
    fn memory(&mut self, path: &str, layer: &TileMemoryLayer) {
        let improvements = self.r.improvements().len();
        let features = self.r.derived().features.len();
        // The feature bits no feature of the ruleset has.
        let beyond: u16 = if features >= 16 { 0 } else { u16::MAX << features };
        for (t, m) in layer.tiles().iter().enumerate() {
            let owner = m.owner().filter(|p| usize::from(p.0) >= self.n);
            let improvement = m.improvement().filter(|i| i.index() >= improvements);
            let feature = (m.features().bits() & beyond != 0)
                .then(|| m.features().iter().find(|f| f.index() >= features))
                .flatten();
            if owner.is_none() && improvement.is_none() && feature.is_none() {
                continue;
            }
            let at = format!("{path}[{t}]");
            if let Some(p) = owner {
                self.player(&at, p);
            }
            if let Some(i) = improvement {
                self.rule(&at, i, improvements);
            }
            if let Some(f) = feature {
                self.rule(&at, f, features);
            }
            if self.errs.len() >= MAX_ERRORS {
                return;
            }
        }
    }

    fn units(&mut self) {
        let (st, r) = (self.st, self.r);
        let ids = st.ids();
        for u in st.units().iter() {
            let path = format!("units[{}]", u.id());
            self.rule(&path, u.base, r.base_units().len());
            self.set(&format!("{path}.promotions"), &u.promotions, r.promotions().len());
            if let Some(t) = u.goto {
                self.tile(&format!("{path}.goto"), t);
            }
            for &t in &u.path {
                self.tile(&format!("{path}.path"), t);
            }
            if let Some(c) = u.camp
                && c.get() >= ids.camp
            {
                self.err(format!("{path}.camp"), format!("camp {c} was never made"));
            }
            if let Some(x) = u.religion {
                self.religion(&format!("{path}.religion"), x);
            }
            let keys: Vec<_> = u.abilities_used.iter().map(|(k, _)| *k).collect();
            self.sorted(&format!("{path}.abilities_used"), &keys);
            self.rules(&format!("{path}.abilities_used"), keys, r.uniques().abilities.len());
            if let Some(c) = u.origin_city {
                self.city_ever(&format!("{path}.origin_city"), c);
            }
            for x in [u.original_owner, u.return_offer].into_iter().flatten() {
                self.player(&path, x);
            }
            if let Some(t) = u.explore.target {
                self.tile(&format!("{path}.explore"), t);
            }
            for &t in &u.explore.recent {
                self.tile(&format!("{path}.explore"), t);
            }
            if u.explore.recent.len() > 4 {
                self.err(format!("{path}.explore.recent"), "holds more than four tiles");
            }
            if self.errs.len() >= MAX_ERRORS {
                return;
            }
        }
    }

    fn cities(&mut self) {
        let (st, r) = (self.st, self.r);
        for c in st.cities().iter() {
            let path = format!("cities[{}]", c.id());
            self.player(&format!("{path}.founder"), c.founder);
            if let Some(p) = c.previous_owner {
                self.player(&format!("{path}.previous_owner"), p);
            }
            self.set(&format!("{path}.buildings"), &c.buildings, r.buildings().len());
            self.set(&format!("{path}.free_buildings"), &c.free_buildings, r.buildings().len());
            for &item in c.queue.iter().chain(c.progress.keys()).chain(c.bought_this_turn.iter()) {
                self.constructible(&path, item);
            }
            self.sorted(&format!("{path}.worked"), &c.worked);
            self.sorted(&format!("{path}.locked"), &c.locked);
            for &t in c.worked.iter().chain(&c.locked) {
                self.tile(&path, t);
            }
            let kinds = r.specialists().len();
            if c.specialists.iter().skip(kinds).any(|&n| n != 0) {
                self.err(format!("{path}.specialists"), "counts a specialist the ruleset lacks");
            }
            let keys: Vec<_> = c.pressures.iter().map(|(k, _)| *k).collect();
            self.sorted(&format!("{path}.pressures"), &keys);
            for x in keys.into_iter().flatten().chain(c.religions_adopted.iter().copied()) {
                self.religion(&path, x);
            }
            if let Some(x) = c.holy_city_of {
                self.religion(&format!("{path}.holy_city_of"), x);
            }
            self.rules(&path, c.demanded_resource, r.resources().len());
            if self.errs.len() >= MAX_ERRORS {
                return;
            }
        }
    }

    fn diplomacy(&mut self) {
        let st = self.st;
        let d = st.diplo();
        for (lo, hi, rel) in d.relations().pairs() {
            if let Some(p) = rel.war_declared_by {
                self.player(&format!("diplomacy.relations[{lo},{hi}]"), p);
            }
        }
        // A save's opinions were checked as they were read; the converter's were not.
        for ((holder, about), _) in d.opinions.iter() {
            let n = self.n;
            if holder == about || usize::from(holder.0) >= n || usize::from(about.0) >= n {
                self.err(
                    "diplomacy.opinions",
                    format!("an opinion of {holder} about {about} among {n} players"),
                );
            }
        }
        for (i, deal) in d.deals.iter().enumerate() {
            let path = format!("diplomacy.deals[{i}]");
            for p in deal.parties {
                self.player(&path, p);
            }
            if deal.parties[0] == deal.parties[1] {
                self.err(&path, "a deal needs two parties");
            }
            if deal.id.get() >= st.ids().deal {
                self.err(&path, format!("deal {} was never made", deal.id));
            }
            for side in &deal.terms.sides {
                self.player(&path, side.giver);
                for item in &side.items {
                    self.deal_item(&path, item);
                }
            }
            for o in &deal.ongoing {
                self.player(&path, o.from);
                self.player(&path, o.to);
                self.deal_item(&path, &o.item);
            }
        }
        for (i, neg) in d.negotiations.iter().enumerate() {
            let path = format!("diplomacy.negotiations[{i}]");
            for p in [Some(neg.initiator), Some(neg.responder), neg.awaiting, neg.proposal_by]
                .into_iter()
                .flatten()
            {
                self.player(&path, p);
            }
            if let Some(deal) = neg.deal
                && deal.get() >= st.ids().deal
            {
                self.err(&path, format!("deal {deal} was never made"));
            }
            let proposals =
                neg.proposal.iter().chain(neg.history.iter().filter_map(|e| e.proposal.as_ref()));
            for terms in proposals {
                for side in &terms.sides {
                    self.player(&path, side.giver);
                    for item in &side.items {
                        self.deal_item(&path, item);
                    }
                }
            }
            for e in &neg.history {
                if let Some(p) = e.by {
                    self.player(&path, p);
                }
            }
        }
    }

    fn deal_item(&mut self, path: &str, item: &DealItem) {
        let r = self.r;
        match *item {
            DealItem::Resource { resource, .. } => self.rule(path, resource, r.resources().len()),
            DealItem::Tech { tech } => self.rule(path, tech, r.techs().len()),
            DealItem::DeclareWar { target } => self.player(path, target),
            DealItem::City { city_id } => self.city_ever(path, city_id),
            _ => {}
        }
    }

    fn world(&mut self) {
        let (st, r) = (self.st, self.r);
        let w = st.world();
        if w.religions.len() > 256 {
            self.err("world.religions", "more than 256 religions");
        }
        for (i, rel) in w.religions.iter().enumerate() {
            let path = format!("world.religions[{i}]");
            self.player(&path, rel.founder);
            match rel.name {
                ReligionName::Pantheon(b) => self.rule(&path, b, r.beliefs().len()),
                ReligionName::Religion(x) => self.rule(&path, x, r.religions().len()),
            }
            self.set(&path, &rel.founder_beliefs, r.beliefs().len());
            self.set(&path, &rel.follower_beliefs, r.beliefs().len());
        }
        for (&b, &c) in &w.wonders_built {
            self.rule("world.wonders_built", b, r.buildings().len());
            self.city_ever("world.wonders_built", c);
        }
        for (&voter, &choice) in &w.un.votes {
            self.player("world.un.votes", voter);
            if let Some(c) = choice {
                self.player("world.un.votes", c);
            }
        }
        if let Some(res) = &w.un.results {
            for &(p, _) in &res.tally {
                self.player("world.un.results", p);
            }
            if let Some(p) = res.winner {
                self.player("world.un.results", p);
            }
        }
        self.players("world.un.won", w.un.won);
        for (id, camp) in &w.camps {
            self.tile(&format!("world.camps[{id}]"), camp.tile);
        }
    }

    fn chronicle(&mut self) {
        let (st, r) = (self.st, self.r);
        if let Some(row) = &st.chronicle().last_stats {
            for c in &row.civs {
                self.player("chronicle.last_stats", c.player);
                self.rule("chronicle.last_stats", c.era, r.eras().len());
            }
        }
    }
}

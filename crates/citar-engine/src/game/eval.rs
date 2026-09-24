//! [`EvalView`]: the one production implementation of the unique evaluator's world
//! (DESIGN.md 5.11).
//!
//! Filters, conditionals, countables and queries (`unique::*`) ask their questions through the
//! traits `TileFacts`, `FilterFacts` and `EvalWorld`; this view answers them from a game. Every
//! derived answer comes from a memo that validates itself on read (DESIGN.md 6.3), so a caller
//! never has to know which caches an evaluation will touch, and no read can be stale.
//!
//! Ports the reads the Python evaluator made of `Game` (`uniques.py:398-701, 778-1083`): the
//! accessors of `game.py` (`at_war`, `has_met`, `is_friend`, `has_open_borders`, `stat_reserve`,
//! `religion_enabled`, `victory_enabled`), the tile predicates of `tiles.py:100-165`, and the
//! plain fields of the state; the unique indexes, the resource supply and the civilization's era
//! come from the memos of `game::derive::civ` (package 1b-05); coast, the trade network and
//! religious majorities from `game::tiles`, the `Connectivity` memo and `game::religion`
//! (package 1b-06).
//!
//! The resource supply is computed in a view of its own (`EvalView::for_supply`), in which a
//! civilization's index has no resource layer, a city's own index none of its resources' uniques,
//! and resources read as none: the uniques of the resources a civilization has depend on its
//! supply, so the supply cannot read them (DESIGN.md 6.6), as Python's `_civ_uniques_nores` and
//! `local_umaps` held none.

use crate::base::hex::HexGrid;
use crate::base::ids::{
    BaseUnitId, CityId, DifficultyId, EraId, ImprovementId, NationId, PlayerId, ReligionId,
    ResourceId, SpecialistId, SpeedId, TechId, TileIdx, Turn, UnitId, VictoryId,
};
use crate::base::sets::{BeliefSet, BuildingSet, PolicySet, PromotionSet, TechSet, TerrainSet};
use crate::base::stats::Stat;
use crate::game::Game;
use crate::game::derive::civ;
use crate::rules::Ruleset;
use crate::rules::defs::{BeliefType, Domain, NationKind, PolicyKind, ReligionProgress, Route};
use crate::state::State;
use crate::state::cities::City;
use crate::state::map::Tile;
use crate::state::players::Player;
use crate::state::units::Unit;
use crate::state::world::Religion;
use crate::unique::{Ctx, EvalWorld, FilterFacts, IndexLayer, IndexRef, TileFacts, UniqueType, uq};

/// A game as the unique evaluator reads it. Cheap to make: it borrows the game.
#[derive(Clone, Copy)]
pub struct EvalView<'a> {
    g: &'a Game,
    /// The view the resource supply is computed in: no resource layer, no resources.
    supply: bool,
}

impl<'a> EvalView<'a> {
    /// The view of `g`.
    #[must_use]
    pub const fn new(g: &'a Game) -> Self {
        Self { g, supply: false }
    }

    /// The view a civilization's resource supply is computed in (DESIGN.md 6.6): every
    /// civilization's index without its resource layer (`_civ_uniques_nores`,
    /// `economy.py:314-320`), every city's own index without its resources' uniques (Python's
    /// `local_umaps` held none), and every civilization's resources none. A resource's uniques
    /// depend on the supply, which is why the supply may not read them; and reading none, rather
    /// than the supply of some other civilization, keeps the supplies of an ally and its
    /// city-states from depending on each other.
    #[must_use]
    pub(crate) const fn for_supply(g: &'a Game) -> Self {
        Self { g, supply: true }
    }

    /// The game it views.
    #[must_use]
    pub const fn game(&self) -> &'a Game {
        self.g
    }

    fn st(&self) -> &'a State {
        &self.g.st
    }

    fn r(&self) -> &'static Ruleset {
        self.g.rules
    }

    fn tile_at(&self, t: TileIdx) -> Option<&'a Tile> {
        self.st().tiles().get(t)
    }

    fn unit_at(&self, u: UnitId) -> Option<&'a Unit> {
        self.st().units().get(u)
    }

    fn city_at(&self, c: CityId) -> Option<&'a City> {
        self.st().cities().get(c)
    }

    fn civ_at(&self, p: PlayerId) -> Option<&'a Player> {
        self.st().player(p)
    }

    fn religion(&self, r: ReligionId) -> Option<&'a Religion> {
        self.st().world().religion(r)
    }

    /// Whether any terrain on the tile is a source of fresh water, a lake or an oasis
    /// (`tiles._is_fresh_source`, `tiles.py:120-124`): one set test against the terrains the
    /// ruleset marks at load.
    fn fresh_source(&self, t: TileIdx) -> bool {
        !self.tile_terrains(t).is_disjoint(&self.r().derived().fresh_water)
    }

    /// Whether a religion has a belief of this type among its founder beliefs
    /// (`religion.is_major`, `is_enhanced`, `religion.py:31-40`).
    fn has_founder_belief(&self, r: ReligionId, kind: BeliefType) -> bool {
        let beliefs = self.r().beliefs();
        self.religion(r).is_some_and(|rel| {
            rel.founder_beliefs.iter().any(|b| beliefs.get(b).is_some_and(|d| d.kind == kind))
        })
    }
}

impl TileFacts for EvalView<'_> {
    fn tile_terrains(&self, t: TileIdx) -> TerrainSet {
        let mut out = TerrainSet::new();
        let Some(tile) = self.tile_at(t) else { return out };
        out.insert(tile.terrain());
        let features = &self.r().derived().features;
        for f in tile.features().iter() {
            if let Some(&terrain) = features.get(f) {
                out.insert(terrain);
            }
        }
        if let Some(w) = tile.wonder() {
            out.insert(w);
        }
        out
    }

    fn tile_river(&self, t: TileIdx) -> bool {
        self.tile_at(t).is_some_and(|x| x.river_mask() != 0)
    }

    fn tile_fresh_water(&self, t: TileIdx) -> bool {
        self.tile_river(t)
            || self.g.grid().neighbors(t).any(|n| self.fresh_source(n))
            || self.fresh_source(t)
    }

    /// `tiles.adjacent_to_coast` (`tiles.py:127-134`): a neighbour is the ruleset's Coast.
    fn tile_next_to_coast(&self, t: TileIdx) -> bool {
        crate::game::tiles::adjacent_to_coast(self.g, t)
    }
}

impl FilterFacts for EvalView<'_> {
    fn civ_nation(&self, p: PlayerId) -> NationId {
        self.civ_at(p).map_or(NationId(0), |x| x.nation)
    }

    fn civ_kind(&self, p: PlayerId) -> NationKind {
        self.civ_at(p).map_or(NationKind::Major, |x| x.kind)
    }

    fn civ_is_human(&self, p: PlayerId) -> bool {
        self.g.is_humanlike(p)
    }

    fn civ_religion(&self, p: PlayerId) -> Option<ReligionId> {
        self.civ_at(p).and_then(|x| x.religion.founded)
    }

    fn at_war(&self, a: PlayerId, b: PlayerId) -> bool {
        self.g.at_war(a, b)
    }

    fn has_met(&self, a: PlayerId, b: PlayerId) -> bool {
        self.g.has_met(a, b)
    }

    fn is_friend(&self, a: PlayerId, b: PlayerId) -> bool {
        self.g.is_friend(a, b)
    }

    fn has_open_borders(&self, a: PlayerId, b: PlayerId) -> bool {
        self.g.has_open_borders(a, b)
    }

    fn tile_owner(&self, t: TileIdx) -> Option<PlayerId> {
        self.tile_at(t).and_then(Tile::owner)
    }

    /// `tiles.is_friendly_territory` (`tiles.py:151-165`).
    fn tile_friendly_to(&self, t: TileIdx, p: PlayerId) -> bool {
        let Some(owner) = self.tile_owner(t) else { return false };
        if owner == p {
            return true;
        }
        if !self.g.has_met(p, owner) {
            return false;
        }
        if self.g.is_city_state(owner)
            && (self.g.is_friend_level(owner, p)
                || uq::any(uq::civ(
                    self,
                    p,
                    UniqueType::CityStateTerritoryAlwaysFriendly,
                    &Ctx::civ(p),
                )))
        {
            return true;
        }
        self.g.has_open_borders(owner, p)
    }

    fn tile_resource(&self, t: TileIdx) -> Option<ResourceId> {
        self.tile_at(t).and_then(Tile::resource)
    }

    /// `tiles.resource_visible` (`tiles.py:143-148`).
    fn resource_visible(&self, p: PlayerId, r: ResourceId) -> bool {
        let revealed_by = self.r().resources().get(r).and_then(|d| d.revealed_by);
        self.g.has_tech(p, revealed_by)
    }

    fn tile_improvement(&self, t: TileIdx) -> Option<ImprovementId> {
        self.tile_at(t).filter(|x| !x.improvement_pillaged()).and_then(Tile::improvement)
    }

    fn tile_route(&self, t: TileIdx) -> Option<ImprovementId> {
        let tile = self.tile_at(t).filter(|x| !x.route_pillaged())?;
        let known = &self.r().derived().known;
        match tile.route()? {
            Route::Road => Some(known.road),
            Route::Railroad => Some(known.railroad),
        }
    }

    fn tile_pillaged(&self, t: TileIdx) -> bool {
        self.tile_at(t).is_some_and(|x| x.improvement_pillaged() || x.route_pillaged())
    }

    /// `uniques.py:413-415`: the city whose territory it is works it.
    fn tile_worked(&self, t: TileIdx) -> bool {
        self.tile_at(t)
            .and_then(Tile::city)
            .and_then(|c| self.city_at(c))
            .is_some_and(|c| c.worked.binary_search(&t).is_ok())
    }

    fn unit_owner(&self, u: UnitId) -> PlayerId {
        self.unit_at(u).map_or(PlayerId(0), Unit::owner)
    }

    fn unit_base(&self, u: UnitId) -> BaseUnitId {
        self.unit_at(u).map_or(BaseUnitId(0), |x| x.base)
    }

    fn unit_promotions(&self, u: UnitId) -> PromotionSet {
        self.unit_at(u).map_or_else(PromotionSet::new, |x| x.promotions)
    }

    fn unit_wounded(&self, u: UnitId) -> bool {
        self.unit_at(u).is_some_and(|x| x.hp < 100)
    }

    /// `movement.is_embarked` (`movement.py:99-107`): a land unit on water outside a city,
    /// unless its profile lets it move on water.
    fn unit_embarked(&self, u: UnitId) -> bool {
        let Some(x) = self.unit_at(u) else { return false };
        let land = self.r().base_units().get(x.base).is_some_and(|b| b.domain == Domain::Land);
        land && self.g.is_water(x.tile())
            && self.st().city_at(x.tile()).is_none()
            && !uq::any(uq::unit(self, u, UniqueType::CanMoveOnWater, &Ctx::IGNORE))
    }

    fn unit_set_up(&self, u: UnitId) -> bool {
        self.unit_at(u).is_some_and(|x| x.set_up)
    }

    fn city_owner(&self, c: CityId) -> PlayerId {
        self.city_at(c).map_or(PlayerId(0), City::owner)
    }

    fn city_founder(&self, c: CityId) -> PlayerId {
        self.city_at(c).map_or(PlayerId(0), |x| x.founder)
    }

    fn city_buildings(&self, c: CityId) -> BuildingSet {
        self.city_at(c).map_or_else(BuildingSet::new, |x| x.buildings)
    }

    fn city_is_capital(&self, c: CityId) -> bool {
        self.city_at(c)
            .is_some_and(|x| self.civ_at(x.owner()).is_some_and(|p| p.capital == Some(c)))
    }

    fn city_coastal(&self, c: CityId) -> bool {
        self.city_at(c).is_some_and(|x| self.tile_next_to_coast(x.tile()))
    }

    /// `cities.has_annex_unhappiness` (`cities.py:120-128`): a conquered city that is not a
    /// puppet, unless one of its own buildings removes it.
    fn city_annex_unhappiness(&self, c: CityId) -> bool {
        let Some(x) = self.city_at(c) else { return false };
        if x.owner() == x.founder || x.puppet {
            return false;
        }
        let ctx = Ctx::city(self, c);
        let buildings = self.r().buildings();
        !x.buildings.iter().any(|b| {
            buildings.get(b).is_some_and(|d| {
                uq::any(uq::object(self, &d.uniques, UniqueType::RemovesAnnexUnhappiness, &ctx))
            })
        })
    }

    fn city_puppet(&self, c: CityId) -> bool {
        self.city_at(c).is_some_and(|x| x.puppet)
    }

    /// `cities.connected_to_capital` (`cities.py:2067-2076`): the `Connectivity` memo; in the
    /// supply's view the links computed afresh, since the memo reads the index the supply
    /// feeds.
    fn city_connected_to_capital(&self, c: CityId) -> bool {
        if !self.supply {
            return crate::game::derive::stats::connected_to_capital(self.g, c);
        }
        let owner = self.city_owner(c);
        self.st().cities().of(owner).len() >= 2
            && crate::game::cities::connections::connected_cities_in(self, owner).media(c).is_some()
    }

    /// `cities.is_garrisoned` (`cities.py:131-134`).
    fn city_garrisoned(&self, c: CityId) -> bool {
        let Some(x) = self.city_at(c) else { return false };
        self.g.military_at(x.tile()).is_some_and(|m| {
            m.owner() == x.owner()
                && self.r().base_units().get(m.base).is_some_and(|b| b.domain == Domain::Land)
        })
    }

    fn city_resisting(&self, c: CityId) -> bool {
        self.city_at(c).is_some_and(|x| x.resistance > 0)
    }

    fn city_razing(&self, c: CityId) -> bool {
        self.city_at(c).is_some_and(|x| x.razing)
    }

    fn city_holy(&self, c: CityId) -> bool {
        self.city_at(c).is_some_and(|x| x.holy_city_of.is_some())
    }

    /// `religion.majority_religion` (`religion.py:136-149`).
    fn city_majority_religion(&self, c: CityId) -> Option<ReligionId> {
        crate::game::religion::majority_religion(self.g, c)
    }

    fn religion_is_major(&self, r: ReligionId) -> bool {
        self.has_founder_belief(r, BeliefType::Founder)
    }

    fn religion_is_enhanced(&self, r: ReligionId) -> bool {
        self.has_founder_belief(r, BeliefType::Enhancer)
    }
}

impl EvalWorld for EvalView<'_> {
    fn rules(&self) -> &Ruleset {
        self.g.rules
    }

    fn seed(&self) -> u64 {
        self.st().seed()
    }

    fn grid(&self) -> &HexGrid {
        self.g.grid()
    }

    fn turn(&self) -> Turn {
        self.g.turn()
    }

    fn speed(&self) -> SpeedId {
        self.g.speed_id()
    }

    fn starting_era(&self) -> EraId {
        self.st().config().starting_era
    }

    fn difficulty(&self, p: Option<PlayerId>) -> DifficultyId {
        self.g.difficulty(p)
    }

    fn victory_enabled(&self, v: VictoryId) -> bool {
        self.g.victory_enabled(v)
    }

    fn religion_enabled(&self) -> bool {
        self.g.religion_enabled()
    }

    fn espionage_enabled(&self) -> bool {
        self.g.espionage_enabled()
    }

    fn nukes_enabled(&self) -> bool {
        self.g.nukes_enabled()
    }

    fn civs(&self) -> impl Iterator<Item = PlayerId> + '_ {
        self.st().players().iter().filter(|(_, p)| p.alive()).map(|(id, _)| id)
    }

    fn cities(&self) -> impl Iterator<Item = CityId> + '_ {
        self.st().cities().iter().map(City::id)
    }

    fn civ_at_war(&self, p: PlayerId) -> bool {
        self.g.is_at_war_any(p)
    }

    fn civ_golden_age(&self, p: PlayerId) -> bool {
        self.civ_at(p).is_some_and(|x| x.econ.golden_age_turns > 0)
    }

    fn civ_happiness(&self, p: PlayerId) -> i32 {
        self.civ_at(p).map_or(0, |x| x.econ.happiness_seen)
    }

    fn civ_stock(&self, p: PlayerId, s: Stat) -> f64 {
        self.g.stat_reserve(p, s)
    }

    /// `economy.resource_amount` (`economy.py:356-358`): the `ResourceSupply` memo; none while a
    /// supply is being computed.
    fn civ_resource(&self, p: PlayerId, r: ResourceId) -> i32 {
        if self.supply { 0 } else { crate::game::economy::resource_amount(self.g, p, r) }
    }

    /// `research.player_era` (`research.py:254-275`): the civilization's era memo.
    fn civ_era(&self, p: PlayerId) -> EraId {
        civ::era(self.g, p)
    }

    fn civ_techs(&self, p: PlayerId) -> TechSet {
        self.civ_at(p).map_or_else(TechSet::new, |x| x.tech.known)
    }

    fn civ_researching(&self, p: PlayerId) -> Option<TechId> {
        self.civ_at(p).and_then(|x| x.tech.queue.first().copied())
    }

    fn civ_policies(&self, p: PlayerId) -> PolicySet {
        self.civ_at(p).map_or_else(PolicySet::new, |x| x.policy.adopted)
    }

    /// `policies.completed_branches` (`policies.py:104-107`): a branch is complete once its
    /// finisher is adopted.
    fn civ_completed_branches(&self, p: PlayerId) -> i32 {
        let policies = self.r().policies();
        let n = self
            .civ_policies(p)
            .iter()
            .filter(|&x| {
                policies
                    .get(x)
                    .is_some_and(|d| matches!(d.kind, PolicyKind::Member { finisher: true, .. }))
            })
            .count();
        i32::try_from(n).unwrap_or(i32::MAX)
    }

    /// `religion.civ_beliefs` (`religion.py:85-88`): every belief of its own religion or
    /// pantheon.
    fn civ_beliefs(&self, p: PlayerId) -> BeliefSet {
        let mut out = BeliefSet::new();
        if let Some(rel) = self.civ_religion(p).and_then(|r| self.religion(r)) {
            for b in rel.founder_beliefs.iter().chain(rel.follower_beliefs.iter()) {
                out.insert(b);
            }
        }
        out
    }

    fn civ_religion_progress(&self, p: PlayerId) -> ReligionProgress {
        self.civ_at(p).map_or_else(ReligionProgress::default, |x| x.religion.progress)
    }

    fn civ_prophets_earned(&self, p: PlayerId) -> i32 {
        self.civ_at(p).map_or(0, |x| x.gp.prophets_earned)
    }

    fn civ_capital(&self, p: PlayerId) -> Option<CityId> {
        self.civ_at(p).and_then(|x| x.capital)
    }

    fn civ_cities(&self, p: PlayerId) -> impl Iterator<Item = CityId> + '_ {
        self.st().cities().of(p).iter().copied()
    }

    fn civ_units(&self, p: PlayerId) -> impl Iterator<Item = UnitId> + '_ {
        self.st().units().of(p).iter().copied()
    }

    fn city_tile(&self, c: CityId) -> TileIdx {
        self.city_at(c).map_or(TileIdx(0), City::tile)
    }

    fn city_health(&self, c: CityId) -> i32 {
        self.city_at(c).map_or(0, |x| x.health)
    }

    fn city_food(&self, c: CityId) -> f64 {
        self.city_at(c).map_or(0.0, |x| x.food)
    }

    fn city_population(&self, c: CityId) -> i32 {
        self.city_at(c).map_or(0, |x| i32::from(x.pop))
    }

    fn city_specialists(&self, c: CityId, s: Option<SpecialistId>) -> i32 {
        let Some(x) = self.city_at(c) else { return 0 };
        match s {
            None => x.specialists.iter().map(|&n| i32::from(n)).sum(),
            Some(s) => x.specialists.get(usize::from(s.0)).map_or(0, |&n| i32::from(n)),
        }
    }

    /// `cities.free_population` (`cities.py:142-144`).
    fn city_unemployed(&self, c: CityId) -> i32 {
        let Some(x) = self.city_at(c) else { return 0 };
        let worked = i32::try_from(x.worked.len()).unwrap_or(i32::MAX);
        i32::from(x.pop) - worked - self.city_specialists(c, None)
    }

    /// `religion.followers_of_majority` (`religion.py:152-156`).
    fn city_majority_followers(&self, c: CityId) -> i32 {
        crate::game::religion::followers_of_majority(self.g, c)
    }

    fn tile_city(&self, t: TileIdx) -> Option<CityId> {
        self.tile_at(t).and_then(Tile::city)
    }

    fn tile_landmass(&self, t: TileIdx) -> Option<u16> {
        self.g.continent(t)
    }

    fn units_at(&self, t: TileIdx) -> impl Iterator<Item = UnitId> + '_ {
        self.st().units().at(t)
    }

    fn unit_tile(&self, u: UnitId) -> TileIdx {
        self.unit_at(u).map_or(TileIdx(0), Unit::tile)
    }

    fn unit_health(&self, u: UnitId) -> i32 {
        self.unit_at(u).map_or(0, |x| i32::from(x.hp))
    }

    fn unit_used_actions(&self, u: UnitId) -> bool {
        self.unit_at(u).is_some_and(|x| !x.abilities_used.is_empty())
    }

    /// `economy.civ_umaps` and `civ_umaps_no_resources` (`economy.py:77-129`): the `CivIndex`
    /// and `CivIndexFull` memos; without the resource layer while a supply is being computed.
    fn civ_index(&self, p: PlayerId, layer: IndexLayer) -> IndexRef<'_> {
        match layer {
            IndexLayer::Full if !self.supply => civ::civ_index_full(self.g, p),
            _ => civ::civ_index(self.g, p),
        }
    }

    /// `cities.local_umaps` without the religion (`cities.py:48-66`): the `CityLocalFull`
    /// memo, with the uniques that hold in the city alone of the resources it gives its owner
    /// (the Marble decision, DESIGN.md 5.12); in the supply's view `CityLocal`, without them.
    fn city_local(&self, c: CityId) -> IndexRef<'_> {
        if self.supply { civ::city_local(self.g, c) } else { civ::city_local_full(self.g, c) }
    }

    /// `religion.follower_umap` (`religion.py:72-82`): the follower index table.
    fn follower(&self, r: ReligionId) -> IndexRef<'_> {
        civ::follower(self.g, r)
    }

    /// `units.unit_umap` (`units.py:21-33`): the unit profile table.
    fn unit_index(&self, u: UnitId) -> IndexRef<'_> {
        civ::unit_profile(self.g, u)
    }
}

#[cfg(all(test, feature = "embedded-ruleset"))]
mod tests {
    use super::*;
    use crate::game::core::testing;
    use crate::state::TileClaim;

    #[test]
    fn the_view_reads_the_state() -> Result<(), crate::state::StateError> {
        let mut g = testing::duel();
        let rome = PlayerId(0);
        let c = CityId::FIRST;
        g.add_city(City::new(c, "Roma".into(), rome, TileIdx(22), 1))?;
        g.set_tile_owner(TileIdx(22), TileClaim::city(rome, c))?;
        g.set_tile_owner(TileIdx(23), TileClaim::city(rome, c))?;
        if let Some(x) = g.city_mut(c, crate::game::derive::rev::CityTouch::WORK) {
            x.worked = vec![TileIdx(23)];
            x.pop = 3;
        }
        g.set_river(TileIdx(40), 1)?;
        let v = g.view();
        assert_eq!(v.tile_owner(TileIdx(22)), Some(rome));
        assert_eq!(v.tile_city(TileIdx(23)), Some(c));
        assert!(v.tile_worked(TileIdx(23)) && !v.tile_worked(TileIdx(22)));
        assert_eq!(v.city_unemployed(c), 2);
        assert!(v.tile_river(TileIdx(40)) && v.tile_fresh_water(TileIdx(40)));
        assert!(!v.tile_fresh_water(TileIdx(0)));
        assert!(!v.civ_at_war(rome), "the war with the barbarians does not count");
        assert!(!v.has_met(rome, PlayerId(1)) && v.has_met(rome, rome));
        assert_eq!(v.civs().count(), 4);
        assert_eq!(v.civ_cities(rome).collect::<Vec<_>>(), [c]);
        assert!(!v.city_is_capital(c));
        assert_eq!(v.civ_kind(PlayerId(2)), NationKind::CityState);
        assert!(v.tile_terrains(TileIdx(0)).len() == 1);
        Ok(())
    }

    #[test]
    fn friendship_and_open_borders_run_until_their_turn() -> Result<(), crate::state::StateError> {
        let mut g = testing::duel();
        let (a, b) = (PlayerId(0), PlayerId(1));
        g.update_relation(a, b, |r| {
            r.friendship_until = 1;
            r.open_borders_until[crate::state::diplo::side(b, a)] = 3;
        })?;
        let v = g.view();
        assert!(v.is_friend(a, b) && v.is_friend(b, a));
        assert!(v.has_open_borders(b, a) && !v.has_open_borders(a, b));
        // A city-state counts a major a friend by influence, never while at war.
        assert!(!v.is_friend(PlayerId(2), a));
        g.set_influence(PlayerId(2), a, 30.0)?;
        assert!(g.view().is_friend(a, PlayerId(2)));
        g.update_relation(a, PlayerId(2), |r| r.war = true)?;
        assert!(!g.view().is_friend(a, PlayerId(2)));
        Ok(())
    }
}

//! [`Game`]: the state, its caches, its history and the work in flight (DESIGN.md 4.8, 6.1).
//!
//! Replaces `Game.__init__` (`game.py:100-145`), the accessors of `game.py:320-540`, the
//! diplomacy basics and unit bookkeeping of `game.py:656-812`, and loading (`Game.from_state`,
//! `scripts/refcheck/common.py:375-386`). What Python kept on the `Game` beside the state is
//! derived here ([`Derived`]), or history ([`Chronicle`]), or work a settle does (`pending`):
//! - the occupancy index (`_occ`, `game.py:720-727`) is the state's own unit index
//!   (`Units::at`), rebuilt on load;
//! - the caches and their invalidation are self-validating memos (`derive::rev`);
//! - `g.rng` and `save_rng` are gone: every draw is keyed from the seed (DESIGN.md 7);
//! - the listeners are gone: every call returns the events it appended ([`EventBatch`]);
//! - `frames` and `action_log` are the chronicle's.
//!
//! **Five rules** (DESIGN.md 6.1): `Derived` is a pure function of `State`; reads take `&self`
//! and validate lazily; writes go through `game::mutate`; consequential writes happen only in
//! settle; everything is deterministic.

use smallvec::SmallVec;

use super::debug::DebugOptions;
use super::derive::Derived;
use super::error::ActionError;
use super::events::{EventBatch, Mention};
use super::invariants::Violation;
use super::pending::{EffectQueue, PendingWork};
use super::{Porting, pending};
use crate::base::digest::{CanonError, Digest};
use crate::base::hex::HexGrid;
use crate::base::ids::{
    BaseUnitId, CityId, DifficultyId, PlayerId, SpeedId, TechId, TileIdx, Turn, UnitId, VictoryId,
};
use crate::base::stats::Stat;
use crate::rules::Ruleset;
use crate::rules::defs::{DifficultyDef, Domain, SpeedDef, TerrainType};
use crate::save::journal::{self, FrameWriter, JournalChunk, JournalCursor, Record};
use crate::save::{LoadError, LoadReport, SaveError, Snapshot};
use crate::state::chronicle::{
    Chronicle, EngineEvent, Event, EventData, NameRef, StatsRow, Thought,
};
use crate::state::cities::City;
use crate::state::config::AiBaseValues;
use crate::state::diplo::{Relation, side};
use crate::state::map::Tile;
use crate::state::players::Player;
use crate::state::units::Unit;
use crate::state::{Phase, State};
use crate::unique::{SourceUniques, UniqueType};

/// The influence at which a city-state counts a civilization a friend (`city_states.py:16`).
pub const FRIEND_INFLUENCE: f64 = 30.0;

/// A game in progress: the state, everything derived from it, its history, and the work the next
/// settle will do (DESIGN.md 6.1).
///
/// `Send` and `Clone`, never `Sync`: its memos validate themselves on reads through `&Game`,
/// with `Cell` stamps, so one `&Game` must not be shared between threads. Hosts hold it behind
/// `&mut` or a lock.
#[derive(Clone, Debug)]
pub struct Game {
    pub(crate) rules: &'static Ruleset,
    pub(crate) st: State,
    pub(crate) dv: Derived,
    pub(crate) chron: Chronicle,
    /// Work the next settle does: empty at every settle point, never saved.
    pub(crate) pending: PendingWork,
    /// Follow-ups of derived reactions, applied in settle.
    pub(crate) fx: EffectQueue,
    /// The last frame, to take the next delta from.
    #[allow(dead_code, reason = "end_round records a frame each round from package 1c-08")]
    pub(crate) frames: FrameWriter,
    /// How far into the chronicle the journal has been taken; never saved.
    pub(crate) journal: JournalCursor,
    /// The first event id of the public call in progress.
    pub(crate) batch_start: u32,
    pub(crate) debug: DebugOptions,
    /// Set when a panic escaped a call: the game takes no more commands.
    pub(crate) poisoned: Option<Box<str>>,
    /// What the checks found, for tests to collect; never saved.
    pub(crate) violations: Vec<Violation>,
    /// The chain of round digests, for a game that keeps one (DESIGN.md 4.10); never saved.
    pub(crate) chain: Option<Box<super::turn::driver::RoundChain>>,
    /// The seat whose driver is playing inside [`Game::drive`], which alone ends its turn;
    /// never saved.
    pub(crate) driving: Option<PlayerId>,
}

impl Game {
    // ---- Building one -------------------------------------------------------------------------

    /// A game over a state that is known to be sound (a save that validated, a conversion),
    /// with its history. `journaled` says whether that history is already in the host's journal,
    /// as a loaded save's is; otherwise the first chunk taken carries all of it. Nothing is
    /// settled.
    pub(crate) fn assemble(
        rules: &'static Ruleset,
        st: State,
        chron: Chronicle,
        journaled: bool,
    ) -> Self {
        let dv = Derived::new(rules, &st);
        let journal =
            if journaled { JournalCursor::at_end(&chron) } else { JournalCursor::default() };
        let batch_start = st.host().next_event_id;
        Self {
            rules,
            st,
            dv,
            chron,
            pending: PendingWork::new(),
            fx: EffectQueue::default(),
            frames: FrameWriter::new(),
            journal,
            batch_start,
            debug: DebugOptions::default(),
            poisoned: None,
            violations: Vec::new(),
            chain: None,
            driving: None,
        }
    }

    /// A game over `st` and its history, once `st` passes the checks a save must pass
    /// (`save::validate`). For tests and tools that build states directly. What each
    /// civilization sees is built from nothing, as Python's first refresh did: the tiles its units
    /// and cities see are explored, and those who see each other meet (DESIGN.md 6.9).
    pub fn from_state(
        rules: &'static Ruleset,
        st: State,
        chron: Chronicle,
    ) -> Result<Self, LoadError> {
        crate::save::validate(&st, rules).map_err(LoadError::Invalid)?;
        let mut g = Self::assemble(rules, st, chron, false);
        g.sight_from_scratch();
        g.settle_sight();
        Ok(g)
    }

    /// Loads a save: format v1 state JSON, and the journal chunks that rebuild its history
    /// (DESIGN.md 4.9, 4.11). A corrupt save is refused here, whole. The loaded game is at a
    /// settle point, as it was saved, so nothing is settled: what each civilization sees is
    /// rebuilt from its sources with no effect (DESIGN.md 6.9).
    pub fn load(
        rules: &'static Ruleset,
        state_json: &[u8],
        chunks: &mut dyn Iterator<Item = &[u8]>,
    ) -> Result<(Self, LoadReport), LoadError> {
        let loaded = crate::save::load(rules, state_json, chunks)?;
        let mut g = Self::assemble(rules, loaded.state, loaded.chronicle, true);
        g.rebuild_sight();
        Ok((g, loaded.report))
    }

    /// Loads a state Python wrote (`GameState.to_dict()`): converts it strictly
    /// (`compat::python`), builds the caches, and settles once, the counterpart of Python's
    /// refresh on load (`scripts/refcheck/common.py:375-386`; DESIGN.md 4.12). Test-only.
    #[cfg(feature = "legacy")]
    pub fn from_python(
        rules: &'static Ruleset,
        json: &[u8],
    ) -> Result<(Self, crate::compat::python::ConvertReport), crate::compat::python::ConvertError>
    {
        let c = crate::compat::python::state_from_python(json, rules)?;
        let mut g = Self::assemble(rules, c.state, c.chronicle, false);
        // A civilian a ranged attack brought to 0 health stayed on the map at 0 (combat.py:109,
        // 806), which no unit of this engine ever is (invariant UNIT-1): it keeps 1.
        // refcheck: civilians-at-zero-health
        for u in g.st.units().iter().filter(|u| u.hp <= 0).map(Unit::id).collect::<Vec<_>>() {
            if let Some(x) = g.unit_mut(u, super::derive::rev::UnitTouch::CORE) {
                x.hp = 1;
            }
        }
        // Python's conditionals see the happiness `happiness()` computed while it was computing
        // it from 0 (economy.py:404-433): commit it once from 0, once Happiness exists.
        pending(Porting::Pending("1b-06"));
        // Sight from nothing, as Python's refresh on load: a state it saved after a refresh is
        // its fixed point (refcheck `fixed_point`).
        g.sight_from_scratch();
        g.settle();
        Ok((g, c.report))
    }

    // ---- The parts ----------------------------------------------------------------------------

    /// The ruleset the game plays by.
    #[must_use]
    pub const fn rules(&self) -> &'static Ruleset {
        self.rules
    }

    /// The persisted state.
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.st
    }

    /// The caches.
    #[must_use]
    pub const fn derived(&self) -> &Derived {
        &self.dv
    }

    /// The history.
    #[must_use]
    pub const fn chronicle(&self) -> &Chronicle {
        &self.chron
    }

    /// The map's grid.
    #[must_use]
    pub const fn grid(&self) -> &HexGrid {
        self.dv.grid()
    }

    /// The unique evaluator's view of the game (DESIGN.md 5.11).
    #[must_use]
    pub fn view(&self) -> super::EvalView<'_> {
        super::EvalView::new(self)
    }

    /// The revision the game is at: an ETag for the host's session, which moves on every write
    /// and never on a read. Not saved.
    #[must_use]
    pub const fn rev(&self) -> u64 {
        self.dv.revs.now().get()
    }

    /// The state's digest (DESIGN.md 4.10).
    pub fn digest(&self) -> Result<Digest, CanonError> {
        crate::save::digest(self.rules, &self.st)
    }

    /// A copy of the state to save off the host's lock (DESIGN.md 4.9).
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::new(self.rules, &self.st)
    }

    /// Everything the chronicle gained since the last take, as a journal chunk (DESIGN.md 4.11);
    /// `None` if nothing did.
    pub fn take_journal_chunk(&mut self) -> Result<Option<JournalChunk>, SaveError> {
        let host = &mut self.st.heads_mut().1;
        journal::take_chunk(self.rules, &self.chron, &mut self.journal, host)
    }

    /// Which checks run at every settle.
    #[must_use]
    pub const fn debug_options(&self) -> DebugOptions {
        self.debug
    }

    /// Sets which checks run at every settle. They only read, so they cannot change a game.
    pub fn set_debug_options(&mut self, d: DebugOptions) {
        self.debug = d;
    }

    /// Every invariant of DESIGN.md 9.4 the game breaks now. Meaningful at a settle point, where
    /// nothing is pending: after a load or a public call.
    #[must_use]
    pub fn check_invariants(&self) -> Vec<Violation> {
        super::invariants::check(self)
    }

    /// Where the caches disagree with a cold recompute (the cache oracle, DESIGN.md 9.4).
    #[must_use]
    pub fn verify_caches(&self) -> Vec<String> {
        let mut out = self.dv.verify(self.rules, &self.st);
        out.extend(super::derive::civ::verify(self));
        out.extend(super::vis::verify(self));
        if self.dv.terrain_floor(self) != super::path::terrain_floor(self) {
            out.push("the cheapest terrain differs from a cold look at the map".to_owned());
        }
        if *self.dv.route_net(self) != super::path::route_net(self) {
            out.push("where routes run differs from a cold look at the map".to_owned());
        }
        out
    }

    /// What the checks have found since the last take, oldest first.
    pub fn take_violations(&mut self) -> Vec<Violation> {
        core::mem::take(&mut self.violations)
    }

    /// Records what a check found.
    pub(crate) fn report(&mut self, v: Violation) {
        self.violations.push(v);
    }

    /// Stops the game after an internal error: it refuses every further command and can only be
    /// snapshotted for debugging. Hosts call this when they catch a panic out of a call.
    pub fn poison(&mut self, why: &str) {
        if self.poisoned.is_none() {
            self.poisoned = Some(why.into());
        }
    }

    /// Why the game was stopped, if it was.
    #[must_use]
    pub fn poisoned(&self) -> Option<&str> {
        self.poisoned.as_deref()
    }

    /// Refuses a command on a poisoned game.
    pub(crate) fn ensure_live(&self) -> Result<(), ActionError> {
        if self.poisoned.is_some() { Err(ActionError::poisoned()) } else { Ok(()) }
    }

    // ---- History ------------------------------------------------------------------------------

    /// Starts a public call: the events it appends from here on are its batch.
    pub(crate) fn begin_call(&mut self) {
        self.batch_start = self.st.host().next_event_id;
    }

    /// The events appended since the call began, as the batch it returns.
    pub(crate) fn take_batch(&mut self) -> EventBatch {
        let since = self.batch_start.saturating_sub(1);
        self.batch_start = self.st.host().next_event_id;
        EventBatch::new(self.chron.events_since(since).to_vec())
    }

    /// Events after id `since`, oldest first, at most `limit` of the newest.
    #[must_use]
    pub fn events(&self, since: u32, limit: usize) -> &[Event] {
        let all = self.chron.events_since(since);
        &all[all.len().saturating_sub(limit)..]
    }

    /// The stats rows, oldest first; the last `last` if given.
    #[must_use]
    pub fn stats(&self, last: Option<usize>) -> &[StatsRow] {
        let all = self.chron.stats();
        &all[all.len().saturating_sub(last.unwrap_or(all.len()))..]
    }

    /// The thoughts from position `since` on, oldest first, only `pid`'s if given
    /// (`engine_api.thoughts`).
    #[must_use]
    pub fn thoughts(&self, pid: Option<PlayerId>, since: usize) -> Vec<&Thought> {
        let all = self.chron.thoughts();
        all.get(since..)
            .unwrap_or(&[])
            .iter()
            .filter(|t| pid.is_none_or(|p| t.player == p))
            .collect()
    }

    /// Records a seat's reasoning, an action or a system note for spectators and the replay
    /// (`engine_api.add_thought`). Host activity: counted in the host heads, never digested.
    pub fn add_thought(&mut self, pid: PlayerId, text: &str, kind: Option<&str>) {
        let t = Thought {
            turn: self.st.clock().turn,
            player: pid,
            text: text.into(),
            kind: kind.map(Into::into),
        };
        Record::of(&mut self.st, &mut self.chron).thought(t);
    }

    // ---- Accessors (game.py:320-540) ----------------------------------------------------------

    /// The turn.
    #[must_use]
    pub const fn turn(&self) -> Turn {
        self.st.clock().turn
    }

    /// Whose turn it is.
    #[must_use]
    pub const fn current(&self) -> PlayerId {
        self.st.clock().current
    }

    /// Whether the game goes on.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.st.clock().phase
    }

    /// The barbarians' id, if the game has them.
    #[must_use]
    pub fn barbarian_id(&self) -> Option<PlayerId> {
        self.st.players().iter().find(|(_, p)| p.is_barbarian()).map(|(id, _)| id)
    }

    /// The game speed, which scales nearly every cost.
    #[must_use]
    pub fn speed(&self) -> &'static SpeedDef {
        &self.rules.speeds()[self.st.config().speed]
    }

    /// The speed's id.
    #[must_use]
    pub const fn speed_id(&self) -> SpeedId {
        self.st.config().speed
    }

    /// Whether religion is in play: switched on, and not disabled by the starting era
    /// (`game.py:343-349`).
    #[must_use]
    pub fn religion_enabled(&self) -> bool {
        let era = self.rules.eras().get(self.st.config().starting_era);
        let disabled =
            era.is_some_and(|e| has_type(self.rules, &e.uniques, UniqueType::DisablesReligion));
        self.st.config().religion && !disabled
    }

    /// Whether espionage is in play.
    #[must_use]
    pub const fn espionage_enabled(&self) -> bool {
        self.st.config().espionage
    }

    /// Whether nuclear weapons may be built.
    #[must_use]
    pub const fn nukes_enabled(&self) -> bool {
        self.st.config().nuclear_weapons
    }

    /// Whether a victory counts in this game (`game.py:360-362`).
    #[must_use]
    pub fn victory_enabled(&self, v: VictoryId) -> bool {
        self.st.config().disabled_victories.binary_search(&v).is_err()
    }

    /// The turn the game ends on (`game.py:364-367`): the settings always hold one, the
    /// speed's own unless the lobby set another.
    #[must_use]
    pub const fn total_turns(&self) -> Turn {
        self.st.config().turn_limit
    }

    /// The calendar year of a turn, the current one by default, negative for BC
    /// (`game.py:369-383`, UnCiv's `GameInfo.getYear`): CITAR's turn 1 is UnCiv's turn 0.
    #[must_use]
    pub fn year(&self, turn: Option<Turn>) -> f64 {
        let turn = turn.unwrap_or(self.turn()) - 1;
        let sp = self.speed();
        let mut year = f64::from(sp.start_year);
        let mut t = 0;
        for step in sp.turns.iter() {
            let n = turn.min(step.until_turn) - t;
            if n <= 0 {
                break;
            }
            year += f64::from(n) * step.years_per_turn;
            t += n;
        }
        if turn > t
            && let Some(last) = sp.turns.last()
        {
            year += f64::from(turn - t) * last.years_per_turn;
        }
        year
    }

    /// The year as text, with BC and AD (`game.py:385-388`).
    #[must_use]
    pub fn year_text(&self, turn: Option<Turn>) -> String {
        // int() truncates toward zero, as `as` does for a finite float in range.
        #[allow(clippy::cast_possible_truncation, reason = "years are far inside i64")]
        let y = self.year(turn) as i64;
        if y < 0 { format!("{} BC", -y) } else { format!("{y} AD") }
    }

    /// The game's difficulty.
    #[must_use]
    pub fn game_difficulty(&self) -> &'static DifficultyDef {
        &self.rules.difficulties()[self.st.config().difficulty]
    }

    /// The difficulty chosen for a seat, or the game's for a seat without one or no seat
    /// (`economy.seat_difficulty`, `economy.py:28-34`).
    #[must_use]
    pub fn seat_difficulty(&self, p: Option<PlayerId>) -> DifficultyId {
        p.and_then(|p| self.st.player(p))
            .and_then(|pl| pl.seat().difficulty())
            .unwrap_or(self.st.config().difficulty)
    }

    /// A civilization's effective difficulty (`economy.difficulty`, `economy.py:37-50`,
    /// UnCiv's `Civilization.getDifficulty`): a humanlike seat plays on its seat's difficulty,
    /// an AI on that difficulty's `aiDifficultyLevel`. With `ai_base_values = monotonic`, an AI
    /// seated below Prince plays on Prince's base values instead (UnCiv gives every non-Prince AI
    /// Chieftain's, so a Chieftain AI could out-expand a Prince one).
    #[must_use]
    pub fn difficulty(&self, p: Option<PlayerId>) -> DifficultyId {
        let base = self.seat_difficulty(p);
        let Some(p) = p else { return base };
        if self.is_humanlike(p) {
            return base;
        }
        if self.st.config().ai_base_values == AiBaseValues::Monotonic
            && let Some(prince) = self.rules.derived().known.prince
            && base < prince
        {
            return prince;
        }
        self.rules.difficulties()[base].ai_difficulty_level
    }

    /// Whether a seat gets a human's difficulty numbers (`game.py:405-415`): its handicap
    /// decides, not who drives it.
    #[must_use]
    pub fn is_humanlike(&self, p: PlayerId) -> bool {
        self.st.player(p).is_some_and(|pl| pl.seat().is_humanlike())
    }

    /// A tile.
    #[must_use]
    pub fn tile(&self, t: TileIdx) -> Option<&Tile> {
        self.st.tiles().get(t)
    }

    /// A player.
    #[must_use]
    pub fn player(&self, p: PlayerId) -> Option<&Player> {
        self.st.player(p)
    }

    /// Every major civilization, living ones only unless `alive_only` is false.
    pub fn majors(&self, alive_only: bool) -> impl Iterator<Item = &Player> + '_ {
        self.st
            .players()
            .iter()
            .map(|(_, p)| p)
            .filter(move |p| p.is_major() && (p.alive() || !alive_only))
    }

    /// Every city-state, living ones only unless `alive_only` is false.
    pub fn city_states(&self, alive_only: bool) -> impl Iterator<Item = &Player> + '_ {
        self.st
            .players()
            .iter()
            .map(|(_, p)| p)
            .filter(move |p| p.is_city_state() && (p.alive() || !alive_only))
    }

    /// Whether the player is the barbarians.
    #[must_use]
    pub fn is_barbarian(&self, p: PlayerId) -> bool {
        self.st.player(p).is_some_and(Player::is_barbarian)
    }

    /// Whether the player is a city-state.
    #[must_use]
    pub fn is_city_state(&self, p: PlayerId) -> bool {
        self.st.player(p).is_some_and(Player::is_city_state)
    }

    /// A unit, or `None` if it no longer exists: units die all the time, and whoever holds an id
    /// across a turn must cope (`game.py:444-451`).
    #[must_use]
    pub fn unit(&self, u: UnitId) -> Option<&Unit> {
        self.st.units().get(u)
    }

    /// A city, or `None`.
    #[must_use]
    pub fn city(&self, c: CityId) -> Option<&City> {
        self.st.cities().get(c)
    }

    /// The units on a tile, in id order, from the occupancy index.
    pub fn units_at(&self, t: TileIdx) -> impl Iterator<Item = &Unit> + '_ {
        self.st.units().units_at(t)
    }

    fn base_domain(&self, u: &Unit) -> Option<(Domain, bool)> {
        self.rules.base_units().get(u.base).map(|b| (b.domain, b.military))
    }

    /// The military land or sea unit on a tile, which defends it (`game.py:457-463`).
    #[must_use]
    pub fn military_at(&self, t: TileIdx) -> Option<&Unit> {
        self.units_at(t).find(|u| self.base_domain(u).is_some_and(|(d, m)| m && d != Domain::Air))
    }

    /// The civilian unit on a tile, which is what gets captured (`game.py:465-471`).
    #[must_use]
    pub fn civilian_at(&self, t: TileIdx) -> Option<&Unit> {
        self.units_at(t).find(|u| self.base_domain(u).is_some_and(|(d, m)| !m && d != Domain::Air))
    }

    /// The aircraft based on a tile, which stack apart from everything else.
    pub fn air_units_at(&self, t: TileIdx) -> impl Iterator<Item = &Unit> + '_ {
        self.units_at(t).filter(|u| self.base_domain(u).is_some_and(|(d, _)| d == Domain::Air))
    }

    /// The city on a tile, if any.
    #[must_use]
    pub fn city_at(&self, t: TileIdx) -> Option<&City> {
        self.st.city_at(t).and_then(|c| self.st.cities().get(c))
    }

    /// A player's units, in id order.
    pub fn player_units(&self, p: PlayerId) -> impl Iterator<Item = &Unit> + '_ {
        self.st.units().of(p).iter().filter_map(|&u| self.st.units().get(u))
    }

    /// A player's cities, in id order.
    pub fn player_cities(&self, p: PlayerId) -> impl Iterator<Item = &City> + '_ {
        self.st.cities().of(p).iter().filter_map(|&c| self.st.cities().get(c))
    }

    /// Whether a tile is water.
    #[must_use]
    pub fn is_water(&self, t: TileIdx) -> bool {
        self.tile(t).is_some_and(|x| {
            self.rules.terrains().get(x.terrain()).is_some_and(|d| d.kind == TerrainType::Water)
        })
    }

    /// Whether a tile is land.
    #[must_use]
    pub fn is_land(&self, t: TileIdx) -> bool {
        self.tile(t).is_some() && !self.is_water(t)
    }

    /// The landmass a tile is on, or `None` for water (`game.py:517-520`).
    #[must_use]
    pub fn continent(&self, t: TileIdx) -> Option<u16> {
        self.st.map().continent(t)
    }

    /// Whether a player has a tech; `None`, a missing requirement, counts as had
    /// (`game.py:522-532`).
    #[must_use]
    pub fn has_tech(&self, p: PlayerId, tech: Option<TechId>) -> bool {
        tech.is_none_or(|t| self.st.player(p).is_some_and(|pl| pl.tech.known.contains(t)))
    }

    /// A tile's offset coordinates.
    #[must_use]
    pub fn xy(&self, t: TileIdx) -> (i32, i32) {
        self.grid().xy(t)
    }

    /// A tile as `(x,y)`, for messages (`game.py:545-548`).
    #[must_use]
    pub fn fmt_xy(&self, t: TileIdx) -> String {
        let (x, y) = self.xy(t);
        format!("({x},{y})")
    }

    /// A player's stockpile of a stat (`game.py:630-634`): gold, culture and faith, golden age
    /// points for happiness, and nothing for science, food and production.
    #[must_use]
    pub fn stat_reserve(&self, p: PlayerId, s: Stat) -> f64 {
        let Some(e) = self.st.player(p).map(|pl| &pl.econ) else { return 0.0 };
        match s {
            Stat::Gold => e.gold,
            Stat::Culture => e.culture,
            Stat::Faith => e.faith,
            Stat::Happiness => e.golden_age_points,
            _ => 0.0,
        }
    }

    // ---- Diplomacy basics (game.py:656-718) ---------------------------------------------------

    /// The relation between two players.
    #[must_use]
    pub fn relation(&self, a: PlayerId, b: PlayerId) -> Option<&Relation> {
        self.st.diplo().relation(a, b)
    }

    /// Whether two players are at war; the barbarians always are, with everyone
    /// (`game.py:664-671`).
    #[must_use]
    pub fn at_war(&self, a: PlayerId, b: PlayerId) -> bool {
        self.st.diplo().at_war(a, b)
    }

    /// Whether a player is at war with anyone living but the barbarians (`game.py:673-675`).
    #[must_use]
    pub fn is_at_war_any(&self, p: PlayerId) -> bool {
        let war = self.st.diplo().war_mask(p);
        war.iter().any(|q| self.st.player(q).is_some_and(|x| x.alive() && !x.is_barbarian()))
    }

    /// Whether `a` counts `b` a friend (`game.py:677-686`): a friendship declared until a turn
    /// not yet past, or, with a city-state, influence at the friend level.
    #[must_use]
    pub fn is_friend(&self, a: PlayerId, b: PlayerId) -> bool {
        if self.is_city_state(a) {
            return self.is_friend_level(a, b);
        }
        if self.is_city_state(b) {
            return self.is_friend_level(b, a);
        }
        self.relation(a, b).is_some_and(|r| r.friendship_until >= self.turn())
    }

    /// Whether a major's influence with a city-state is at the friend level
    /// (`city_states.is_friend_level`, `city_states.py:113-115`): at war, influence is at its
    /// floor, below the level (`city_states.py:67-75`).
    #[must_use]
    pub fn is_friend_level(&self, cs: PlayerId, major: PlayerId) -> bool {
        let Some(data) = self.st.player(cs).and_then(|p| p.city_state.as_deref()) else {
            return false;
        };
        !self.at_war(cs, major) && data.influence_of(major) >= FRIEND_INFLUENCE
    }

    /// Whether two players know of each other; everyone knows themselves (`game.py:688-692`).
    #[must_use]
    pub fn has_met(&self, a: PlayerId, b: PlayerId) -> bool {
        self.st.diplo().has_met(a, b)
    }

    /// Whether `owner` lets `visitor`'s units through its territory now (`game.py:703-706`).
    #[must_use]
    pub fn has_open_borders(&self, owner: PlayerId, visitor: PlayerId) -> bool {
        self.relation(owner, visitor)
            .is_some_and(|r| r.open_borders_until[side(owner, visitor)] >= self.turn())
    }

    /// Whether a player's units may be on a tile at all (`game.py:708-718`).
    #[must_use]
    pub fn can_enter_territory(&self, p: PlayerId, t: TileIdx) -> bool {
        self.tile(t).and_then(Tile::owner).is_none_or(|o| self.can_enter_owner(p, o))
    }

    /// Whether a player's units may be on land `owner` owns (`game.py:708-718`): its own; the
    /// barbarians' once the barbarian level lets them past borders; anyone's at war with it, the
    /// barbarians' and a city-state's; and anyone's with open borders.
    #[must_use]
    pub fn can_enter_owner(&self, p: PlayerId, owner: PlayerId) -> bool {
        if self.is_barbarian(p) && !self.is_barbarian(owner) {
            let level = &self.rules.difficulties()[self.st.config().barbarian_difficulty];
            // Python's `turn - 1 >= n`.
            return self.turn() > level.turn_barbarians_can_enter_player_tiles;
        }
        if owner == p || self.is_barbarian(p) || self.is_barbarian(owner) || self.at_war(p, owner) {
            return true;
        }
        if self.is_city_state(owner) || self.is_city_state(p) {
            return true;
        }
        self.has_open_borders(owner, p)
    }

    /// Records that two players have met, with what follows a first contact (`game.py:694-701`):
    /// the city-states' greeting and the `first_contact` event. Nothing happens for a player and
    /// itself, the barbarians, or two who have met.
    pub(crate) fn make_contact(&mut self, a: PlayerId, b: PlayerId) {
        if a == b || self.is_barbarian(a) || self.is_barbarian(b) || self.has_met(a, b) {
            return;
        }
        let (Some(na), Some(nb)) =
            (self.player(a).map(|p| p.name.clone()), self.player(b).map(|p| p.name.clone()))
        else {
            return;
        };
        if self.set_met(a, b).is_err() {
            return;
        }
        // city_states.on_meet: the gift and the greeting (city_states.py).
        pending(Porting::Pending("1c-06"));
        let audience = [a, b].into_iter().collect();
        let data = EventData { a: Some(a), b: Some(b), ..EventData::default() };
        self.emit(
            EngineEvent::FirstContact,
            &format!("{na} and {nb} have made contact."),
            Some(audience),
            None,
            data,
            &[],
        );
    }

    // ---- Units (game.py:720-804) --------------------------------------------------------------

    /// Creates a unit of `base` for `p` on `t` (`game.py:729-742`) and returns its id.
    pub(crate) fn create_unit(
        &mut self,
        p: PlayerId,
        base: BaseUnitId,
        t: TileIdx,
        xp: i32,
    ) -> Result<UnitId, super::EngineError> {
        let id = self.st.ids_mut().next_unit().ok_or_else(|| {
            super::EngineError::Config("the game has run out of unit ids".to_owned())
        })?;
        let mut u = Unit::new(id, base, p, t, self.turn());
        u.xp = xp;
        if let Some(def) = self.rules.base_units().get(base)
            && def.religious_strength != 0
        {
            u.religious_strength = i16::try_from(def.religious_strength).unwrap_or(i16::MAX);
        }
        self.spawn_unit(u)?;
        super::units::on_created(self, id);
        // A unit made on its owner's turn can move at once (`game.py:747`).
        if self.current() == p {
            let full = super::units::health::max_moves(self, id);
            if let Some(x) = self.unit_mut(id, super::derive::rev::UnitTouch::MOVES) {
                x.moves = full;
            }
        }
        Ok(id)
    }

    /// Hands a unit to another player, clearing the orders that were the old owner's
    /// (`game.py:796-804`).
    #[allow(dead_code, reason = "capture and gifts call it from 1c-03")]
    pub(crate) fn change_unit_owner(
        &mut self,
        u: UnitId,
        new: PlayerId,
    ) -> Result<(), super::EngineError> {
        self.set_unit_owner(u, new)?;
        let x = self.unit_mut(
            u,
            super::derive::rev::UnitTouch::CORE | super::derive::rev::UnitTouch::MOVES,
        );
        if let Some(x) = x {
            x.activity = None;
            x.goto = None;
            x.path.clear();
            x.fortify = 0;
            x.moves = 0;
        }
        Ok(())
    }

    // ---- Events -------------------------------------------------------------------------------

    /// Records an event of the host's own (an agent error, a pause) and returns it as a batch
    /// (`engine_api.emit`). Host activity: counted in the host heads, never digested.
    pub fn emit_host(
        &mut self,
        kind: &str,
        text: &str,
        audience: Option<crate::base::sets::PlayerSet>,
        data: EventData,
    ) -> EventBatch {
        self.begin_call();
        self.emit_host_event(kind, text, audience, data);
        self.take_batch()
    }

    /// Where `text` names a civilization, leader or city the game knows, or one of `mentions`,
    /// as an event emitted now would record it (`game.py:842-858`).
    #[must_use]
    pub fn name_refs(&self, text: &str, mentions: &[Mention<'_>]) -> SmallVec<[NameRef; 2]> {
        self.dv.names(&self.st).refs(text, mentions)
    }
}

/// Whether one of an object's uniques is of type `ty`, whatever its conditionals.
pub(crate) fn has_type(rules: &Ruleset, uniques: &SourceUniques, ty: UniqueType) -> bool {
    let t = rules.uniques();
    uniques.ids().any(|id| t.meta(id).ty == Some(ty))
}

#[cfg(all(test, feature = "embedded-ruleset"))]
pub(crate) mod testing {
    //! A small game for the unit tests: two majors, a city-state and the barbarians on a 10x8
    //! grassland map, nothing on it.

    use super::Game;
    use crate::base::ids::{
        BarbarianLevelId, BaseUnitId, CityId, DifficultyId, EraId, MapSizeId, MapTypeId, NationId,
        PlayerId, SpeedId, TerrainId, TileIdx, UnitId,
    };
    use crate::base::sets::PlayerVec;
    use crate::game::derive::rev::PlayerTouch;
    use crate::rules::Ruleset;
    use crate::state::chronicle::Chronicle;
    use crate::state::cities::City;
    use crate::state::config::{GameConfig, MapEdges, MapSource};
    use crate::state::map::{MapInfo, Tile, Tiles};
    use crate::state::players::{Controller, Player, PlayerKind, Rgb, Seat, SeatOverrides};
    use crate::state::units::Unit;
    use crate::state::{State, TileClaim};

    pub const W: u16 = 10;
    pub const H: u16 = 8;

    fn nation(r: &Ruleset, name: &str) -> NationId {
        r.lookup::<NationId>(name).unwrap_or(NationId(0))
    }

    fn player(r: &Ruleset, id: u8, kind: PlayerKind, name: &str, leader: &str) -> Player {
        let controller = match kind {
            PlayerKind::Major => Controller::Bot,
            PlayerKind::CityState => Controller::Minor,
            PlayerKind::Barbarian => Controller::Barbarian,
        };
        let seat = Seat::new(controller, SeatOverrides::default(), None);
        let mut p = Player::new(
            PlayerId(id),
            kind,
            name.into(),
            nation(r, name),
            Rgb::default(),
            seat,
            u32::from(W) * u32::from(H),
        );
        p.leader = leader.into();
        p
    }

    /// The state of the test game.
    pub fn state() -> State {
        let r = Ruleset::shared();
        let grass = r.lookup::<TerrainId>("Grassland").unwrap_or(TerrainId(0));
        let map =
            MapInfo { width: W, height: H, wrap_x: false, wrap_y: false, continents: Vec::new() };
        let tiles = Tiles::new(vec![Tile::new(grass); usize::from(W) * usize::from(H)]);
        let src = MapSource::Generated {
            size: MapSizeId(0),
            map_type: MapTypeId(0),
            edges: MapEdges::IceCaps,
            dims: None,
        };
        let speed = r.lookup::<SpeedId>("Standard").unwrap_or(SpeedId(0));
        let difficulty = r.lookup::<DifficultyId>("Prince").unwrap_or(DifficultyId(0));
        let cfg = GameConfig::new(7, src, speed, difficulty, EraId(0), BarbarianLevelId(1), 500);
        let players: PlayerVec<Player> = [
            player(r, 0, PlayerKind::Major, "Rome", "Augustus Caesar"),
            player(r, 1, PlayerKind::Major, "Greece", "Alexander"),
            player(r, 2, PlayerKind::CityState, "Geneva", ""),
            player(r, 3, PlayerKind::Barbarian, "Barbarians", ""),
        ]
        .into_iter()
        .collect();
        State::new(cfg, map, tiles, players).expect("a valid state")
    }

    /// The test game.
    pub fn duel() -> Game {
        Game::from_state(Ruleset::shared(), state(), Chronicle::new()).expect("a sound state")
    }

    /// Founds a city of `owner` on `t` as the state requires one: its id from the counter, its
    /// tile claimed, and the owner's capital if it had none.
    pub fn city(g: &mut Game, owner: PlayerId, t: TileIdx, name: &str) -> CityId {
        let id = g.st.ids_mut().next_city().expect("a city id");
        let turn = g.turn();
        g.add_city(City::new(id, name.into(), owner, t, turn)).expect("a new city");
        g.set_tile_owner(t, TileClaim::city(owner, id)).expect("a tile on the map");
        if let Some(p) = g.player_mut(owner, PlayerTouch::CAPITAL)
            && p.capital.is_none()
        {
            p.capital = Some(id);
            p.original_capital = Some(id);
        }
        id
    }

    /// Makes a unit of `base` for `owner` on `t`, its id from the counter.
    pub fn unit(g: &mut Game, owner: PlayerId, base: &str, t: TileIdx) -> UnitId {
        let id = g.st.ids_mut().next_unit().expect("a unit id");
        let base = g.rules.lookup::<BaseUnitId>(base).expect("a unit of the ruleset");
        let turn = g.turn();
        g.spawn_unit(Unit::new(id, base, owner, t, turn)).expect("a tile on the map");
        id
    }
}

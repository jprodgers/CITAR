//! Bot seats for the whole-game tests and the long runs (DESIGN.md P2.3.11): [`CountingBot`]
//! plays a seat as a `citar_bot::Bot` of its spec and keeps what each of its calls did, so that a
//! test can tell a bot looping on a refused action, and count what it made: attacks, cities
//! founded or taken.
//!
//! A `Bot` holds nothing between calls but its counts (DESIGN.md P2.3.1), so a fresh one plays
//! each call, and its counts are one bot turn's (or one answer's): [`CountingBot`] keeps the
//! worst of them and their sums.
//!
//! Package 2-13 adds the [`Lineup`] of a run's seats, which the soak, chaos and property P8 take
//! (`--drivers random|bot|mixed`): `RandomAgent`s in every major's seat as in Phase 1, `basic-1`
//! in every one, or half of each, and the [`Seat`] that plays either. A noisy bot
//! ([`CountingBot::noisy`]) reads the game and makes refused calls before its turn, after it and
//! before every answer, as a noisy `RandomAgent` does between its actions, for P8 with bot
//! drivers: none of it may change what the bot does.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::{NegotiationId, PlayerId, Turn};
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::{DriverOutcome, Game, SeatDriver};
use citar_engine::state::players::DriverMemory;
use serde::{Deserialize, Serialize};

use crate::agents::{self, RandomAgent};

/// More refusals of one tool than this in one bot call is a bot looping on a refused action: no
/// turn of a reference state comes near it (the fixture sweep's worst is a few dozen), and the
/// soak fails a game whose bot passes it.
pub const LOOPING: u32 = 200;

/// A `basic-1` (or any version's) seat that counts what its calls did.
#[derive(Clone, Debug)]
pub struct CountingBot {
    spec: Arc<BotSpec>,
    /// Whether it reads and makes refused calls around its turns and answers.
    noisy: bool,
    /// The most refusals of one tool in one call: how many, the tool and the turn.
    pub worst: (u32, &'static str, Turn),
    /// Every call's actions, by tool: (taken, refused).
    pub totals: BTreeMap<&'static str, (u64, u64)>,
    /// The turns it played.
    pub turns: u32,
}

impl CountingBot {
    /// A seat of `spec`.
    #[must_use]
    pub fn new(spec: Arc<BotSpec>) -> Self {
        Self { spec, noisy: false, worst: (0, "", 0), totals: BTreeMap::new(), turns: 0 }
    }

    /// A seat of `basic-1` at its defaults. The spec is built once and shared: a run that seats
    /// fresh bots at every step (chaos, the properties) would otherwise resolve the parameters
    /// again each time.
    #[must_use]
    pub fn basic1() -> Self {
        static SPEC: OnceLock<Arc<BotSpec>> = OnceLock::new();
        let spec = SPEC.get_or_init(|| {
            let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
            Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None))
        });
        Self::new(Arc::clone(spec))
    }

    /// The same seat, made noisy: before its turn, after it and before every answer it reads the
    /// game and makes calls the game refuses ([`agents::reads_and_refusals`]), from a stream of
    /// its own, so that property P8 can ask whether any of it changes what the bot does.
    #[must_use]
    pub const fn noisy(mut self) -> Self {
        self.noisy = true;
        self
    }

    /// Actions of `tool` it took.
    #[must_use]
    pub fn taken(&self, tool: &str) -> u64 {
        self.totals.get(tool).map_or(0, |&(ok, _)| ok)
    }

    /// Actions it took and had refused, over every tool.
    #[must_use]
    pub fn sums(&self) -> (u64, u64) {
        self.totals.values().fold((0, 0), |(a, b), &(ok, no)| (a + ok, b + no))
    }

    fn keep(&mut self, b: &Bot, turn: Turn) {
        for (tool, ok, refused) in b.refusals().iter() {
            let t = self.totals.entry(tool).or_default();
            t.0 += u64::from(ok);
            t.1 += u64::from(refused);
            if refused > self.worst.0 {
                self.worst = (refused, tool, turn);
            }
        }
    }

    /// The noise of one turn (`what` 0) or one answer, when the seat is noisy: a stream keyed
    /// apart from the bot's own (`Purpose::BotBase`) and from the agents' noise.
    fn noise(&self, g: &mut Game, pid: PlayerId, what: u64) -> Option<Rng> {
        self.noisy.then(|| {
            let turn = u64::try_from(g.turn()).unwrap_or(0);
            let key = [u64::from(pid.0), turn, u64::MAX - 1, what];
            Rng::keyed(g.state().seed(), Purpose::TestAgent, &key)
        })
    }
}

impl SeatDriver for CountingBot {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        let mut noise = self.noise(g, pid, 0);
        if let Some(rng) = noise.as_mut() {
            agents::reads_and_refusals(g, pid, rng);
        }
        let mut b = Bot::new(Arc::clone(&self.spec));
        let out = b.play_turn(g, pid, mem);
        // After its last action: what the drive does next, it does after these.
        if let Some(rng) = noise.as_mut() {
            agents::reads_and_refusals(g, pid, rng);
        }
        self.turns += 1;
        self.keep(&b, g.turn());
        out
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome {
        if let Some(mut rng) = self.noise(g, pid, 1 + u64::from(nid.get())) {
            agents::reads_and_refusals(g, pid, &mut rng);
        }
        let mut b = Bot::new(Arc::clone(&self.spec));
        let out = b.respond(g, pid, nid, mem);
        self.keep(&b, g.turn());
        out
    }
}

/// Who drives the major civilizations' seats of a game a long run plays (`--drivers`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lineup {
    /// A `RandomAgent` in every seat, as in Phase 1: every action the engine has, right and
    /// wrong, in any order.
    #[default]
    Random,
    /// `basic-1` at its defaults in every seat: the games the server plays.
    Bot,
    /// Half the seats each, which half drawn from the game's seed ([`Lineup::bot_plays`]).
    Mixed,
}

impl Lineup {
    /// Every lineup, as the command lines name them.
    pub const ALL: [Self; 3] = [Self::Random, Self::Bot, Self::Mixed];

    /// Its name on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Bot => "bot",
            Self::Mixed => "mixed",
        }
    }

    /// The lineup called `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.name() == name)
    }

    /// Whether a bot plays `pid`'s seat in `g`. A mixed game seats bots where the seat's number
    /// and the game's seed have the same parity: half of the seats, and over a run of games
    /// every seat, the first as often as the last. Taken from the game, so that a replay or a
    /// game loaded from its save seats the same drivers.
    #[must_use]
    pub fn bot_plays(self, g: &Game, pid: PlayerId) -> bool {
        match self {
            Self::Random => false,
            Self::Bot => true,
            Self::Mixed => (g.state().seed() ^ u64::from(pid.0)) & 1 == 0,
        }
    }

    /// One seat per player of `g`, noisy or not (only the majors' are ever seated). The agents
    /// draw from streams keyed by the game's seed and the seat, and the bots keep their memory
    /// in the game: fresh seats play a loaded game as the ones before them would have.
    #[must_use]
    pub fn seats(self, g: &Game, noisy: bool) -> Vec<Seat> {
        (0..g.state().players().len())
            .map(|i| {
                let pid = PlayerId(u8::try_from(i).unwrap_or(u8::MAX));
                match (self.bot_plays(g, pid), noisy) {
                    (true, false) => Seat::Bot(CountingBot::basic1()),
                    (true, true) => Seat::Bot(CountingBot::basic1().noisy()),
                    (false, false) => Seat::Agent(RandomAgent::new()),
                    (false, true) => Seat::Agent(RandomAgent::noisy()),
                }
            })
            .collect()
    }
}

/// A seat of a long run's game: a bot or an agent.
#[derive(Clone, Debug)]
pub enum Seat {
    Bot(CountingBot),
    Agent(RandomAgent),
}

impl Seat {
    /// The bot, if a bot plays the seat.
    #[must_use]
    pub const fn bot(&self) -> Option<&CountingBot> {
        match self {
            Self::Bot(b) => Some(b),
            Self::Agent(_) => None,
        }
    }
}

impl SeatDriver for Seat {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        match self {
            Self::Bot(b) => b.play_turn(g, pid, mem),
            Self::Agent(a) => a.play_turn(g, pid, mem),
        }
    }

    fn respond(
        &mut self,
        g: &mut Game,
        pid: PlayerId,
        nid: NegotiationId,
        mem: &mut DriverMemory,
    ) -> DriverOutcome {
        match self {
            Self::Bot(b) => b.respond(g, pid, nid, mem),
            Self::Agent(a) => a.respond(g, pid, nid, mem),
        }
    }
}

/// What the bots of a game's seats did, summed: the turns they played, the actions they took and
/// had refused, and the most refusals of one tool in one call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BotCounts {
    pub turns: u32,
    pub taken: u64,
    pub refused: u64,
    /// How many, the tool and the turn.
    pub worst: (u32, &'static str, Turn),
}

impl BotCounts {
    /// The sums over `seats`, or `None` when no bot sits in any of them.
    #[must_use]
    pub fn of(seats: &[Seat]) -> Option<Self> {
        let bots: Vec<&CountingBot> = seats.iter().filter_map(Seat::bot).collect();
        if bots.is_empty() {
            return None;
        }
        let mut out = Self::default();
        for b in bots {
            let (taken, refused) = b.sums();
            out.turns += b.turns;
            out.taken += taken;
            out.refused += refused;
            if b.worst.0 > out.worst.0 {
                out.worst = b.worst;
            }
        }
        Some(out)
    }

    /// Whether a bot looped on a refused action ([`LOOPING`]).
    #[must_use]
    pub const fn looped(&self) -> bool {
        self.worst.0 > LOOPING
    }
}

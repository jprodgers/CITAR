//! Bot seats for the whole-game tests (DESIGN.md P2.3.11): [`CountingBot`] plays a seat as a
//! `citar_bot::Bot` of its spec and keeps what each of its calls did, so that a test can tell a
//! bot looping on a refused action, and count what it made: attacks, cities founded or taken.
//!
//! A `Bot` holds nothing between calls but its counts (DESIGN.md P2.3.1), so a fresh one plays
//! each call, and its counts are one bot turn's (or one answer's): [`CountingBot`] keeps the
//! worst of them and their sums.

use std::collections::BTreeMap;
use std::sync::Arc;

use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::{NegotiationId, PlayerId, Turn};
use citar_engine::game::{DriverOutcome, Game, SeatDriver};
use citar_engine::state::players::DriverMemory;

/// A `basic-1` (or any version's) seat that counts what its calls did.
#[derive(Clone, Debug)]
pub struct CountingBot {
    spec: Arc<BotSpec>,
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
        Self { spec, worst: (0, "", 0), totals: BTreeMap::new(), turns: 0 }
    }

    /// A seat of `basic-1` at its defaults.
    #[must_use]
    pub fn basic1() -> Self {
        let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
        Self::new(Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None)))
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
}

impl SeatDriver for CountingBot {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        let mut b = Bot::new(Arc::clone(&self.spec));
        let out = b.play_turn(g, pid, mem);
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
        let mut b = Bot::new(Arc::clone(&self.spec));
        let out = b.respond(g, pid, nid, mem);
        self.keep(&b, g.turn());
        out
    }
}

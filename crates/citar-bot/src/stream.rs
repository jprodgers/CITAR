//! The bot's random streams (DESIGN.md P2.3.5): every draw is `Rng::keyed(seed,
//! Purpose::BotBase, [stream, pid, turn, ...])`, the game's seed keyed by what it decides about.
//!
//! Python drew from two sequential Mersenne Twisters per bot (`self.rng`, `self.rng_diplo`,
//! basic.py:681-683), so the order in which the bot asked moved every later draw, a resumed game
//! played differently, and handing a category to a language model could shift the rest. Keyed
//! streams fix all three. The sixth draw, ranged or melee (basic.py:1336), is the advisor's
//! (`Purpose::Advisor`, shared with automatic production). The words live here so that the
//! engine holds no bot vocabulary and `golden/rng.json` stays as it is.

use citar_engine::base::ids::{PlayerId, Turn};
use citar_engine::base::rng::{KeyPart, Purpose, Rng};
use citar_engine::game::Game;
use smallvec::SmallVec;

/// A kind of decision the bot draws for: the first key word under `Purpose::BotBase`. The
/// discriminants are frozen: changing one moves every game a bot plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Stream {
    /// The noise on tech values (basic.py:906), keyed by the tech.
    Research = 1,
    /// The spy targets' tie-breaking noise (basic.py:1057), keyed by the spy and the target.
    Spies = 2,
    /// Offering peace (basic.py:2427), keyed by the other player.
    Peace = 3,
    /// Offering friendship (basic.py:2438), keyed by the other player.
    Friendship = 4,
    /// Starting to prepare a war (basic.py:2449), keyed by the other player.
    WarPrep = 5,
}

impl Stream {
    /// Every stream.
    pub const ALL: [Self; 5] =
        [Self::Research, Self::Spies, Self::Peace, Self::Friendship, Self::WarPrep];

    /// Its key word.
    #[must_use]
    pub const fn word(self) -> u64 {
        self as u64
    }

    /// The generator for one decision of `pid` on the current turn of `g`, keyed further by
    /// `more` (the tech, the spy and target, the other player).
    #[must_use]
    pub fn rng(self, g: &Game, pid: PlayerId, more: &[u64]) -> Rng {
        self.keyed(g.state().seed(), pid, g.turn(), more)
    }

    /// The generator for one decision of `pid` on turn `turn` of the game seeded `seed`:
    /// `Rng::keyed(seed, Purpose::BotBase, [word, pid, turn, more...])`.
    #[must_use]
    pub fn keyed(self, seed: u64, pid: PlayerId, turn: Turn, more: &[u64]) -> Rng {
        let mut keys: SmallVec<[u64; 6]> = SmallVec::with_capacity(3 + more.len());
        keys.extend([self.word(), pid.key(), turn.key()]);
        keys.extend_from_slice(more);
        Rng::keyed(seed, Purpose::BotBase, &keys)
    }
}

//! The bot's diplomacy in whole games (package 2-05, gates 4 and 6; DESIGN.md P2.3.8, P2.3.11
//! points 6 and 8), the invariants checked at every settle, and the cache oracle too with
//! `CITAR_BOT_CHECKS=all` (as the war games, `war.rs`).
//!
//! - **The switches** (gate 4): four `basic-1` seats whose every diplomacy category the seat's
//!   language model owns, beside two `RandomAgent`s that declare war on every bot they have met
//!   from turn 60, 200 rounds on a small map. The bots open no negotiation, declare no war, give
//!   no city-state anything and move no spy (each is given one at round 100, and they meet
//!   city-states): they never even try (no such action taken or refused). Every negotiation put to them they defer to the host (`DriverOutcome::Deferred`),
//!   which closes it when the drive stops for it, as a host does when its wait runs out. They
//!   still fight the wars the agents declare on them: with the barbarians off, every attack they
//!   make is in such a war.
//! - **Nothing else moves** (gate 4): with no city-state and no civilization met before turn 30,
//!   a game whose bots own every category and one whose language models own them all give the
//!   same digest at the end of each of those rounds. The owners move no draw (the streams are
//!   keyed by decision, DESIGN.md P2.3.5) and the bots remember the same things.
//! - **Saves** (gate 6): a four-bot game saved and loaded at the end of every one of 120 rounds
//!   plays on as the uninterrupted one, round digest by round digest (a seat's memory is in the
//!   digest): the bots' memory, war plans and preparations included, is in the save. The game
//!   is one whose bots both prepare a war and fight one by plan, and the test fails if it stops
//!   holding either at the end of some round.

use std::collections::BTreeMap;
use std::sync::Arc;

use citar_bot::{BotSpec, Memory, Overrides, Owner, Owners, Tuning, VersionId};
use citar_engine::api::testops;
use citar_engine::base::ids::{NegotiationId, PlayerId};
use citar_engine::game::diplomacy::actions::DeclareWar;
use citar_engine::game::diplomacy::category::CATEGORIES;
use citar_engine::game::{
    Action, DebugOptions, DriveOptions, DriverOutcome, Drivers, Game, SeatDriver, Stop, espionage,
};
use citar_engine::state::Phase;
use citar_engine::state::chronicle::{EngineEvent, EventType};
use citar_engine::state::diplo::NegStatus;
use citar_engine::state::players::DriverMemory;
use citar_testkit::agents::RandomAgent;
use citar_testkit::bots::CountingBot;
use citar_testkit::games::{self, Round};
use serde_json::{Value, json};

/// A `basic-1` seat whose language model owns every category of diplomacy.
fn model_owned() -> Arc<BotSpec> {
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    let owners = CATEGORIES.into_iter().fold(Owners::default(), |o, c| o.with(c, Owner::Llm));
    Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None).with_owners(owners))
}

/// A `basic-1` seat at its defaults: the bot owns every category.
fn bot_owned() -> Arc<BotSpec> {
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    Arc::new(BotSpec::new(VersionId::Basic1, tuning, None, None))
}

/// A seat of the switch game.
enum Seat {
    /// A bot, with the answers it gave: deferred, and otherwise.
    Bot { bot: CountingBot, deferred: u32, answered: u32 },
    /// A random agent that, from turn 60, declares war on each of `bots` it has met.
    Agent { agent: RandomAgent, bots: Vec<PlayerId> },
}

impl SeatDriver for Seat {
    fn play_turn(&mut self, g: &mut Game, pid: PlayerId, mem: &mut DriverMemory) -> DriverOutcome {
        match self {
            Self::Bot { bot, .. } => bot.play_turn(g, pid, mem),
            Self::Agent { agent, bots } => {
                if g.turn() >= 60 {
                    for &b in bots.iter() {
                        if g.has_met(pid, b) && !g.at_war(pid, b) {
                            let declare = DeclareWar { player_id: i64::from(b.0), message: None };
                            // A treaty may still hold: the agent tries again next turn.
                            g.act(pid, Action::DeclareWar(declare)).ok();
                        }
                    }
                }
                agent.play_turn(g, pid, mem)
            }
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
            Self::Bot { bot, deferred, answered } => {
                let out = bot.respond(g, pid, nid, mem);
                if out == DriverOutcome::Deferred {
                    *deferred += 1;
                } else {
                    *answered += 1;
                }
                out
            }
            Self::Agent { agent, .. } => agent.respond(g, pid, nid, mem),
        }
    }
}

/// Plays `g` with `seats` (one per player) for `rounds` rounds or to its end, a seat at a time,
/// calling `hook` after each round. A drive that stops for a reply is answered as a host whose
/// wait runs out: the negotiations it waits on are closed as expired, and the drive goes on.
/// Returns the rounds played and the negotiations closed so.
fn play_hosted<D: SeatDriver>(
    g: &mut Game,
    seats: &mut [D],
    rounds: u32,
    hook: &mut dyn FnMut(&mut Game, Round) -> Result<(), String>,
) -> Result<(u32, u32), String> {
    let taken = |g: &Game| g.chain().map_or(0, |c| c.rounds());
    let mut played = 0;
    let mut expired = 0;
    while played < rounds && g.phase() == Phase::Playing {
        let before = taken(g);
        let n = g.state().players().len();
        let mut d = Drivers::none(n);
        for (i, s) in seats.iter_mut().enumerate().take(n) {
            let p = PlayerId(u8::try_from(i).map_err(|_| "too many players")?);
            if g.player(p).is_some_and(|x| x.is_major()) {
                d = d.with(p, s);
            }
        }
        let (stop, _) = g
            .drive(&mut d, DriveOptions::default().with_seat_limit(1))
            .map_err(|e| format!("turn {}: {}", g.turn(), e.message))?;
        drop(d);
        match stop {
            Stop::SeatLimit | Stop::GameOver => {}
            Stop::AwaitingReply { nids, .. } => {
                for nid in nids {
                    g.close_negotiation(nid, NegStatus::Expired, "No answer came in time.", None)
                        .map_err(|e| format!("turn {}: {}", g.turn(), e.message))?;
                    expired += 1;
                }
            }
            other => return Err(format!("turn {}: the drive stopped with {other:?}", g.turn())),
        }
        if taken(g) > before {
            let round = g.last_round().ok_or("a round with no digest")?;
            hook(g, round)?;
            played += 1;
        }
    }
    Ok((played, expired))
}

/// The checks at every settle: the invariants, and the cache oracle too with
/// `CITAR_BOT_CHECKS=all`.
#[allow(clippy::disallowed_methods, reason = "a test's switch")]
fn checks() -> DebugOptions {
    let all = std::env::var_os("CITAR_BOT_CHECKS").is_some_and(|v| v == "all");
    DebugOptions { invariants: true, verify_caches: all }
}

/// A small game of `players` seats, with the [`checks`].
fn small(seed: u64, map_type: &str, players: usize, extra: &Value) -> Game {
    let seat = json!({"controller": "bot", "nation": null});
    let mut settings = json!({
        "seed": seed,
        "map_size": "small",
        "map_type": map_type,
        "speed": "Quick",
        "difficulty": "Prince",
        "players": vec![seat; players],
    });
    if let (Some(s), Some(x)) = (settings.as_object_mut(), extra.as_object()) {
        s.extend(x.clone());
    }
    games::new_game(&settings, b"bot-diplomacy", checks()).expect("a small game")
}

/// The checks of a round: no violation, no broken invariant.
fn checked(g: &mut Game, round: Round) -> Result<(), String> {
    let problems = games::problems(g);
    if problems.is_empty() { Ok(()) } else { Err(format!("round {}: {problems:?}", round.0)) }
}

#[test]
#[allow(clippy::disallowed_macros, reason = "the test reports what it measured")]
fn bots_whose_model_owns_diplomacy_make_none_and_still_fight_the_wars_declared_on_them() {
    let mut g = small(9100, "pangaea", 6, &json!({"barbarians": "off"}));
    let bots: Vec<PlayerId> = (0..4).map(PlayerId).collect();
    let mut seats: Vec<Seat> = (0..g.state().players().len())
        .map(|i| {
            if i < 4 {
                Seat::Bot { bot: CountingBot::new(model_owned()), deferred: 0, answered: 0 }
            } else {
                Seat::Agent { agent: RandomAgent::new(), bots: bots.clone() }
            }
        })
        .collect();
    let mut seen = 0u32;
    // Wars with a bot, by who declared them.
    let mut wars: BTreeMap<(u8, u8), u32> = BTreeMap::new();
    let mut hook = |g: &mut Game, round: Round| -> Result<(), String> {
        checked(g, round)?;
        if round.0 == 100 {
            // A spy each, idle in its hideout: the bots are never to move them.
            let spies: Vec<Value> =
                bots.iter().map(|b| json!({"op": "add_spy", "player": b.0})).collect();
            testops::apply(g, &Value::Array(spies)).map_err(|e| e.message)?;
        }
        for e in g.events(seen, usize::MAX) {
            if e.kind == EventType::Engine(EngineEvent::WarDeclared)
                && let Some(d) = &e.data
                && let (Some(a), Some(b)) = (d.attacker, d.defender)
                && (bots.contains(&a) || bots.contains(&b))
            {
                *wars.entry((a.0, b.0)).or_default() += 1;
            }
        }
        seen = g.events(0, 1).last().map_or(seen, |e| e.id.get());
        Ok(())
    };
    let (played, expired) =
        play_hosted(&mut g, &mut seats, 200, &mut hook).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(played, 200);
    let mut deferred = 0;
    let mut attacks = 0;
    for (i, s) in seats.iter().enumerate() {
        let Seat::Bot { bot, deferred: d, answered } = s else { continue };
        eprintln!(
            "bot {i}: {} turns, {d} deferred, {answered} answered, actions {:?}",
            bot.turns, bot.totals
        );
        for tool in [
            "open_negotiation",
            "respond_negotiation",
            "declare_war",
            "city_state_action",
            "move_spy",
            "denounce",
        ] {
            assert_eq!(bot.totals.get(tool), None, "bot {i} used {tool}");
        }
        assert_eq!(*answered, 0, "bot {i} answered a negotiation its model owns");
        deferred += d;
        attacks += bot.taken("attack");
    }
    eprintln!(
        "wars with a bot {wars:?}; {deferred} deferred; {expired} closed by the host; {attacks} attacks by bots"
    );
    let opened = g.negotiations().iter().filter(|n| bots.contains(&n.initiator)).count();
    assert_eq!(opened, 0, "negotiations opened by bots");
    // The bots could have sent spies and courted city-states, and did neither.
    for &b in &bots {
        let spies = espionage::spies(&g, b);
        assert!(!spies.is_empty(), "bot {} has a spy", b.0);
        assert!(spies.iter().all(|s| s.city.is_none()), "bot {} moved a spy", b.0);
    }
    let courted = |b: PlayerId| g.city_states(false).filter(|cs| g.has_met(b, cs.id())).count();
    let met: Vec<usize> = bots.iter().map(|&b| courted(b)).collect();
    eprintln!("city-states met by each bot {met:?}");
    assert!(met.iter().any(|&n| n > 0), "no bot met a city-state");
    assert!(
        wars.keys().all(|(a, _)| !bots.contains(&PlayerId(*a))),
        "a bot declared war: {wars:?}"
    );
    assert!(!wars.is_empty(), "no war was declared on a bot");
    assert!(deferred > 0, "no negotiation was put to a bot");
    assert!(expired > 0, "the host closed no deferred negotiation");
    assert!(attacks > 0, "the bots did not fight the wars declared on them");
}

/// Plays a four-bot game of `seed` with no city-state for `rounds` rounds, every seat's bot of
/// `spec`: each round's digest, and the round at which two civilizations first met (if any).
fn isolated(seed: u64, spec: &Arc<BotSpec>, rounds: u32) -> (Vec<Round>, Option<u32>) {
    let mut g = small(seed, "archipelago", 4, &json!({"city_states": 0}));
    let mut seats: Vec<CountingBot> =
        (0..g.state().players().len()).map(|_| CountingBot::new(Arc::clone(spec))).collect();
    let mut out = Vec::new();
    let mut contact = None;
    let mut hook = |g: &mut Game, round: Round| -> Result<(), String> {
        checked(g, round)?;
        out.push(round);
        let majors: Vec<PlayerId> = g.majors(false).map(|p| p.id()).collect();
        let met = majors.iter().any(|&a| majors.iter().any(|&b| a < b && g.has_met(a, b)));
        if met && contact.is_none() {
            contact = Some(u32::try_from(out.len()).unwrap_or(u32::MAX));
        }
        Ok(())
    };
    let (played, _) =
        play_hosted(&mut g, &mut seats, rounds, &mut hook).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(played, rounds);
    (out, contact)
}

#[test]
fn before_contact_whoever_owns_diplomacy_the_game_is_the_same() {
    let rounds = 30;
    let (bots, met) = isolated(9200, &bot_owned(), rounds);
    let (models, met_too) = isolated(9200, &model_owned(), rounds);
    assert_eq!((met, met_too), (None, None), "civilizations met within {rounds} rounds");
    assert_eq!(bots.len(), rounds as usize);
    assert_eq!(bots, models, "the owners moved the game before any contact");
}

/// The seed of the game saved and loaded at every round: one whose bots prepare a war at the end
/// of 20 of its 120 rounds and fight one by plan at the end of 14 (seed 8000, the war games'
/// first, prepares one and never declares it in 120 rounds).
const SAVED_SEED: u64 = 8001;

/// What a game's bots held in memory of their wars: the rounds at whose end some seat was
/// preparing a war (`memory.war_prep`), and those at whose end some seat had a war plan
/// (`memory.war_plan`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct WarMemory {
    preparing: u32,
    planned: u32,
}

impl WarMemory {
    /// Counts the round just ended, from each major seat's memory in the game.
    fn count(&mut self, g: &Game) {
        let held: Vec<Memory> =
            g.majors(false).filter_map(|p| p.seat().driver()).map(Memory::decode).collect();
        self.preparing += u32::from(held.iter().any(|m| m.war_prep.is_some()));
        self.planned += u32::from(held.iter().any(|m| m.war_plan.is_some()));
    }
}

/// Plays the four-bot baseline game of `seed` for `rounds` rounds, saving and loading it after
/// every round when `reload`: each round's digest, and what the bots held of their wars.
fn baseline(seed: u64, rounds: u32, reload: bool) -> (Vec<Round>, WarMemory) {
    let mut g = small(seed, "continents", 4, &json!({"barbarians": "normal"}));
    let mut seats: Vec<CountingBot> =
        (0..g.state().players().len()).map(|_| CountingBot::new(bot_owned())).collect();
    let mut out = Vec::new();
    let mut wars = WarMemory::default();
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut hook = |g: &mut Game, round: Round| -> Result<(), String> {
        checked(g, round)?;
        out.push(round);
        wars.count(g);
        if reload {
            games::save_and_load(g, &mut chunks)?;
        }
        Ok(())
    };
    let (played, expired) =
        play_hosted(&mut g, &mut seats, rounds, &mut hook).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((played, expired), (rounds, 0));
    (out, wars)
}

#[test]
fn a_bot_game_saved_and_loaded_every_round_plays_as_the_uninterrupted_one() {
    let rounds = 120;
    // The engine runs no threads (DESIGN.md 6.13); the two games run side by side.
    #[allow(clippy::disallowed_methods, reason = "threads of the test's, not of the engine")]
    let ((whole, held), (reloaded, held_too)) = std::thread::scope(|s| {
        let a = s.spawn(|| baseline(SAVED_SEED, rounds, false));
        let b = s.spawn(|| baseline(SAVED_SEED, rounds, true));
        (a.join().expect("the whole game"), b.join().expect("the reloaded game"))
    });
    assert_eq!(whole.len(), rounds as usize);
    assert_eq!(whole, reloaded);
    assert_eq!(held, held_too);
    // The save is shown to carry the bots' wars only if the game has some: a war prepared at the
    // end of some rounds, and one fought for a city by plan at the end of others.
    assert!(held.preparing > 0 && held.planned > 0, "no war prepared, or none planned: {held:?}");
}

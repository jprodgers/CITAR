//! The stability properties P1 to P8 of DESIGN.md 9.5, as one runner that the property tests,
//! the chaos driver and the fuzz target share.
//!
//! A [`Run`] plays [`Step`]s on a game and checks, as it goes:
//! - **P1**, no panic: the caller's harness catches one (proptest, or chaos's `catch_unwind`);
//! - **P2**, a refused call leaves the digest, the revision and the events as they were, so it
//!   emitted nothing and settled nothing;
//! - **P3**, the invariants hold after every call that changed the game, no settle reported a
//!   violation, and the state has a digest after every step (a state with none, a NaN in it,
//!   would leave P2, P6 and P8 comparing nothing);
//! - **P4**, the caches equal a cold rebuild, every [`Options::verify_every`] steps and after the
//!   last;
//! - **P5**, every refusal reads as a sentence a model can use ([`refusal_rule_broken`]);
//! - **P6**, a save and a load, every [`Options::save_every`] steps, give the digest saved, and
//!   play goes on from the loaded game;
//! - **P7**, at the end, at most one more end of turn than there are living major civilizations
//!   (the seats whose turns the host ends; city-states and barbarians play inside the call)
//!   moves the turn on or ends the game ([`stall_limit`]).
//!
//! **P8**, reads are free, needs two runs of the same steps: [`reads_are_free`] plays them once
//! quietly and once with [`noise`] (queries of every kind, views, the briefing, snapshots, saves
//! and calls the game must refuse) before every step, its drivers noisy too (reading and making
//! refused calls inside their turns), and compares what each step returned and the digest after
//! it. With bot drivers ([`Options::drivers`]) that asks whether any read changes what the bot
//! decides (package 2-13).

use core::fmt;

use citar_engine::api::text_rule_broken;
use citar_engine::api::tools::registry::{TOOLS, ToolKind};
use citar_engine::base::digest::Digest;
use citar_engine::base::ids::PlayerId;
use citar_engine::base::rng::{Purpose, Rng};
use citar_engine::game::{DebugOptions, DriveOptions, Drivers, Game};
use citar_engine::state::Phase;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents;
use crate::bots::Lineup;
use crate::spec::{ActionSpec, Shape};
use crate::{checks, games};

/// One thing a run does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// A tool call, bound to the game when it is made.
    Call(ActionSpec),
    /// The host ends the turn of the player whose turn it is (`Game::end_turn`).
    EndTurn,
    /// The seat's driver plays the turn of the player whose turn it is, and the drive ends it: a
    /// `RandomAgent`, or a `basic-1` bot where the run's lineup seats one ([`Options::drivers`]).
    Agent,
}

/// A property of DESIGN.md 9.5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Property {
    P1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
    P8,
}

impl Property {
    /// What the property says, in a few words.
    #[must_use]
    pub const fn says(self) -> &'static str {
        match self {
            Self::P1 => "no panic",
            Self::P2 => "a refused call changes nothing",
            Self::P3 => "invariants hold",
            Self::P4 => "the caches equal a cold rebuild",
            Self::P5 => "a refusal reads as a sentence",
            Self::P6 => "a save and a load give the digest saved",
            Self::P7 => "the turns do not stall",
            Self::P8 => "reads are free",
        }
    }
}

/// A property broken at a step (its index in the run, from 0; the number of steps for a check
/// made after the last).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breach {
    pub property: Property,
    pub step: usize,
    pub what: String,
}

impl Breach {
    fn new(property: Property, step: usize, what: impl Into<String>) -> Self {
        Self { property, step, what: what.into() }
    }
}

impl fmt::Display for Breach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} ({}) at step {}: {}",
            self.property,
            self.property.says(),
            self.step,
            self.what
        )
    }
}

/// How often the costlier checks run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    /// P4 every this many steps and after the last; 0 for never, which leaves a bug the cache
    /// oracle would see to the other properties.
    pub verify_every: usize,
    /// P6 every this many steps, 0 for never.
    pub save_every: usize,
    /// Whether the drivers of [`Step::Agent`] are noisy (`RandomAgent::noisy`,
    /// `CountingBot::noisy`).
    pub noisy_agents: bool,
    /// Who drives the majors' seats at a [`Step::Agent`]: `RandomAgent`s, `basic-1` bots or half
    /// of each (package 2-13). A replay written before there was a choice has none, and its
    /// agents were random.
    #[serde(default)]
    pub drivers: Lineup,
}

impl Default for Options {
    /// DESIGN.md 9.5: the caches every 10 actions, a save and a load every 25; `RandomAgent`s.
    fn default() -> Self {
        Self { verify_every: 10, save_every: 25, noisy_agents: false, drivers: Lineup::Random }
    }
}

/// What one step returned: whether the call was taken, and the refusal's text if not; the
/// digest after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Taken {
    pub refused: Option<String>,
    pub digest: Digest,
}

/// Steps played on a game, checking P2 to P7 as they go (see the module's documentation).
#[derive(Debug)]
pub struct Run {
    g: Game,
    options: Options,
    chunks: Vec<Vec<u8>>,
    steps: usize,
}

impl Run {
    /// A run on `g`, which keeps checking every invariant at every settle and leaves the cache
    /// oracle to the steps [`Options::verify_every`] names.
    #[must_use]
    pub fn new(mut g: Game, options: Options) -> Self {
        g.set_debug_options(DebugOptions { invariants: true, verify_caches: false });
        let _stale = g.take_violations();
        Self { g, options, chunks: Vec::new(), steps: 0 }
    }

    /// The game as it stands.
    #[must_use]
    pub const fn game(&self) -> &Game {
        &self.g
    }

    /// The game as it stands, to change it outside the run (noise, for P8).
    pub const fn game_mut(&mut self) -> &mut Game {
        &mut self.g
    }

    /// How many steps have been taken.
    #[must_use]
    pub const fn steps(&self) -> usize {
        self.steps
    }

    /// Takes step `s` and checks it.
    ///
    /// # Errors
    /// The first property the step breaks.
    ///
    /// # Panics
    /// If the engine does (property P1): the caller's harness reports it.
    pub fn step(&mut self, s: &Step) -> Result<Taken, Breach> {
        let at = self.steps;
        self.steps += 1;
        let before = self.fingerprint(at)?;
        // A refusal: the call, its arguments (to read the text against) and the text.
        let refused = match *s {
            Step::Call(spec) => {
                let call = spec.bind(&self.g);
                let kind = spec.tool_spec().kind();
                match self.g.execute(call.pid, call.tool, &call.args) {
                    Ok(_) if kind == ToolKind::Query => {
                        // A query is a read: it may change nothing (P8, checked here too).
                        if self.fingerprint(at)? != before {
                            return Err(Breach::new(
                                Property::P8,
                                at,
                                format!("the query {} {} changed the game", call.tool, call.args),
                            ));
                        }
                        None
                    }
                    Ok(_) => None,
                    Err(e) => Some((
                        format!("{} {} by {}", call.tool, call.args, call.pid.0),
                        call.args,
                        e.message,
                    )),
                }
            }
            Step::EndTurn => {
                let p = self.g.current();
                self.g
                    .end_turn(p)
                    .err()
                    .map(|e| (format!("end_turn by {}", p.0), Value::Null, e.message))
            }
            Step::Agent => {
                self.agent_turn();
                None
            }
        };
        if let Some((call, args, text)) = &refused {
            if let Some(why) = refusal_rule_broken(&self.g, args, text) {
                return Err(Breach::new(Property::P5, at, format!("{call}: {why}: {text}")));
            }
            let after = self.fingerprint(at)?;
            if after != before {
                return Err(Breach::new(
                    Property::P2,
                    at,
                    format!(
                        "{call} was refused ({text}) yet changed the game: {before:?} -> {after:?}"
                    ),
                ));
            }
        } else {
            self.invariants(at)?;
        }
        if self.options.verify_every > 0 && self.steps.is_multiple_of(self.options.verify_every) {
            self.caches(at)?;
        }
        if self.options.save_every > 0 && self.steps.is_multiple_of(self.options.save_every) {
            games::save_and_load(&mut self.g, &mut self.chunks)
                .map_err(|e| Breach::new(Property::P6, at, e))?;
        }
        let (digest, _, _) = self.fingerprint(at)?;
        Ok(Taken { refused: refused.map(|(_, _, t)| t), digest })
    }

    /// The checks made after the last step: the caches (P4) and that the turns do not stall
    /// (P7, on a copy).
    ///
    /// # Errors
    /// The first property broken.
    pub fn finish(&mut self) -> Result<(), Breach> {
        let at = self.steps;
        if self.options.verify_every > 0 {
            self.caches(at)?;
        }
        no_stall(&self.g).map_err(|e| Breach::new(Property::P7, at, e))
    }

    /// The digest, the revision and the number of events: what a refused call must leave as it
    /// was.
    ///
    /// # Errors
    /// P3 at step `at` if the state has no digest: a NaN or an infinity somewhere in it, which no
    /// comparison of digests could then see past.
    fn fingerprint(&self, at: usize) -> Result<(Digest, u64, usize), Breach> {
        let digest = self.g.digest().map_err(|e| {
            Breach::new(Property::P3, at, format!("the state has no canonical digest: {e}"))
        })?;
        Ok((digest, self.g.rev(), self.g.chronicle().events().len()))
    }

    /// P3: what the settles reported and what the game breaks now.
    fn invariants(&mut self, at: usize) -> Result<(), Breach> {
        let mut found: Vec<String> =
            self.g.take_violations().iter().map(ToString::to_string).collect();
        found.extend(checks::invariants(&self.g));
        if found.is_empty() { Ok(()) } else { Err(Breach::new(Property::P3, at, found.join("; "))) }
    }

    /// P4: the cache oracle.
    fn caches(&self, at: usize) -> Result<(), Breach> {
        let found = checks::caches(&self.g);
        if found.is_empty() { Ok(()) } else { Err(Breach::new(Property::P4, at, found.join("; "))) }
    }

    /// A driver of the run's lineup in every major civilization's seat plays the turn of the one
    /// whose turn it is; the drive ends it. The others' answer any negotiation that waits on
    /// them within the drive.
    fn agent_turn(&mut self) {
        let n = self.g.state().players().len();
        let mut seats = self.options.drivers.seats(&self.g, self.options.noisy_agents);
        let mut d = Drivers::none(n);
        for (i, a) in seats.iter_mut().enumerate() {
            let p = PlayerId(u8::try_from(i).unwrap_or(u8::MAX));
            if self.g.player(p).is_some_and(|x| x.is_major()) {
                d = d.with(p, a);
            }
        }
        // A game that is over refuses the drive, which is no breach: nothing changed.
        let _stop = self.g.drive(&mut d, DriveOptions::default().with_seat_limit(1));
    }
}

/// P5: which of the text rules (`api::text_rule_broken`, DESIGN.md 8.5) the refusal `text`, which
/// `g` gave a call with arguments `args`, breaks, if any.
///
/// A null inside an argument's value is the caller's own, and the engine quotes it back as Python
/// would, as `None`: at once (`Unknown policy '[None]'.`), or later, from a name the game kept (a
/// civilization named with a list, `You have not met [None, 'x'].`). So the word `None` is read
/// as the caller's when this call's arguments hold such a null or a name the game keeps holds
/// the word ([`holds_none`]); every other rule, `Some(`, `Idx(` and `::` among them, still holds.
/// A null as an argument's whole value stands for the argument left out, and earns no such
/// reading.
#[must_use]
pub fn refusal_rule_broken(g: &Game, args: &Value, text: &str) -> Option<&'static str> {
    let words = citar_engine::base::text::find_word(text, "None");
    // Only a text with the word in it needs a look at the game's names.
    if words.is_empty() || !(null_below_top(args) || holds_none(g)) {
        return text_rule_broken(text);
    }
    let mut read = String::with_capacity(text.len());
    let mut from = 0;
    for w in words {
        read.push_str(&text[from..w.start]);
        read.push_str("null");
        from = w.end;
    }
    read.push_str(&text[from..]);
    text_rule_broken(&read)
}

/// Whether a name the game keeps and its refusals quote, which callers may set, holds the word
/// `None`: a civilization's or its leader's (`set_civ_name`), a city's, a unit's or a religion's
/// (the name `found_religion` was given, which a refusal to spread the religion where it is
/// followed already quotes), set from a value that held a null, as Python's `str` writes it.
/// Names only: the state keeps the word of its own elsewhere (a spy with nothing to do has the
/// action `None`).
#[must_use]
pub fn holds_none(g: &Game) -> bool {
    let none = |s: &str| !citar_engine::base::text::find_word(s, "None").is_empty();
    let st = g.state();
    st.players().iter().any(|(_, p)| none(&p.name) || none(&p.leader))
        || st.cities().iter().any(|c| none(&c.name))
        || st.units().iter().any(|u| u.name.as_deref().is_some_and(none))
        || st.world().religions.iter().any(|r| none(&r.display))
}

/// Whether `args` hold a null below their top: inside an argument's value, or anywhere inside
/// arguments that are no object at all.
#[must_use]
pub fn null_below_top(args: &Value) -> bool {
    fn inside(v: &Value) -> bool {
        match v {
            Value::Array(a) => a.iter().any(|x| x.is_null() || inside(x)),
            Value::Object(m) => m.values().any(|x| x.is_null() || inside(x)),
            _ => false,
        }
    }
    match args {
        Value::Object(m) => m.values().any(inside),
        other => inside(other),
    }
}

/// P7's bound: one end of turn more than there are living major civilizations, the seats whose
/// turns the host ends (`end_turn` plays the city-states' and the barbarians' turns itself, and
/// stops at the next living major). From the first major's turn a round takes one end of turn
/// for each; the one more is DESIGN.md 9.5's slack, and covers a major that a liberated city
/// brings back to life within the round.
#[must_use]
pub fn stall_limit(g: &Game) -> usize {
    g.majors(true).count() + 1
}

/// P7: from a copy of `g`, at most [`stall_limit`] ends of turn move the turn on or end the game.
///
/// # Errors
/// What stalled.
pub fn no_stall(g: &Game) -> Result<(), String> {
    let mut g = g.clone();
    g.set_debug_options(DebugOptions::OFF);
    let start = g.turn();
    let limit = stall_limit(&g);
    for _ in 0..limit {
        if g.phase() != Phase::Playing || g.turn() > start {
            return Ok(());
        }
        let p = g.current();
        g.end_turn(p).map_err(|e| {
            format!("turn {start}: player {}'s turn does not end: {}", p.0, e.message)
        })?;
    }
    if g.phase() != Phase::Playing || g.turn() > start {
        Ok(())
    } else {
        Err(format!(
            "turn {start}: {limit} ends of turn left it turn {} (player {}'s)",
            g.turn(),
            g.current().0
        ))
    }
}

/// Plays `steps` on a copy of `start`, checking P2 to P7 (see [`Run`]).
///
/// # Errors
/// The first property broken.
pub fn play(start: &Game, steps: &[Step], options: Options) -> Result<Vec<Taken>, Breach> {
    let mut run = Run::new(start.clone(), options);
    let taken = steps.iter().map(|s| run.step(s)).collect::<Result<Vec<_>, _>>()?;
    run.finish()?;
    Ok(taken)
}

/// Reads, snapshots, saves and refused calls, as hosts and models make them between two
/// actions: the agents' own ([`agents::reads_and_refusals`]: every `inspect` query, the views
/// and the briefing among them, a query tool, the reads the tools make, refusals it asserts, a
/// save now and then), query tools asked by anyone with arguments bound from specs, an action
/// asked by a major civilization whose turn it is not, which the game must refuse, and now and
/// then a snapshot saved as JSON and summarised. Draws from `rng` alone.
///
/// # Panics
/// If the game carries out a call it must refuse: that is the bug P8 is after.
pub fn noise(g: &mut Game, rng: &mut Rng) {
    let p = g.current();
    for _ in 0..=rng.below(2) {
        agents::reads_and_refusals(g, p, rng);
    }
    let queries: Vec<usize> =
        (0..TOOLS.len()).filter(|&i| TOOLS[i].kind() == ToolKind::Query).collect();
    let actions: Vec<usize> = (0..TOOLS.len())
        .filter(|&i| TOOLS[i].kind() == ToolKind::Action && !TOOLS[i].any_time)
        .collect();
    for _ in 0..rng.below(3) {
        let mut spec = ActionSpec::draw(rng);
        spec.tool = pick_tool(&queries, rng);
        let call = spec.bind(g);
        let _answer = g.execute_query(call.pid, call.tool, &call.args);
    }
    let others: Vec<PlayerId> =
        g.majors(true).map(|x| x.id()).filter(|&q| q != g.current()).collect();
    if let Some(&q) = rng.pick(&others) {
        let mut spec = ActionSpec::draw(rng);
        spec.tool = pick_tool(&actions, rng);
        spec.shape = Shape::Valid;
        let mut call = spec.bind(g);
        call.pid = q;
        let done = g.execute(call.pid, call.tool, &call.args);
        assert!(done.is_err(), "{} by {}, whose turn it is not, was carried out", call.tool, q.0);
    }
    if rng.chance(0.1)
        && let Ok(bytes) = g.snapshot().to_json()
    {
        let _summary = citar_engine::save::summary(&bytes);
    }
}

/// The index in the registry of one of `tools`, as a spec's tool byte.
fn pick_tool(tools: &[usize], rng: &mut Rng) -> u8 {
    rng.pick(tools).and_then(|&i| u8::try_from(i).ok()).unwrap_or(0)
}

/// P8: `steps` played on copies of `start` twice, quietly and with [`noise`] before every step
/// (drawn from a stream keyed by `seed`) and noisy drivers of the same lineup, return the same
/// and leave the same digest after every step. Both runs also check P2 to P7.
///
/// # Errors
/// The first property broken, P8 at the first step whose result or digest differs.
pub fn reads_are_free(
    start: &Game,
    steps: &[Step],
    seed: u64,
    options: Options,
) -> Result<(), Breach> {
    let quiet = play(start, steps, options)?;
    let mut rng = Rng::keyed(seed, Purpose::TestAgent, &[0x0008, 0x2019]);
    let mut run = Run::new(start.clone(), Options { noisy_agents: true, ..options });
    for (at, (s, want)) in steps.iter().zip(&quiet).enumerate() {
        noise(run.game_mut(), &mut rng);
        let got = run.step(s)?;
        if &got != want {
            return Err(Breach::new(
                Property::P8,
                at,
                format!("with reads before it, {s:?} gave {got:?}; without, {want:?}"),
            ));
        }
    }
    run.finish()
}

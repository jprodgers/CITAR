//! The Rust side of package 2-07's evidence for `refcheck/baseline/explained.toml`: the corpus's
//! Python bot games played on by `basic-1` from their own states (paired games), and the
//! baseline's games traced round by round (the counterpart of `scripts/refcheck/war_trace.py`).
//!
//! **Paired games.** The statistical baseline compares Rust games with Python games on
//! different maps (the Rust map generator is the engine's own), so a gap there may come from the
//! maps. Played on from the same state, on the same map and starts, a Rust game and the Python
//! game it continues differ only by the engines' rules and the bots' ports. For each of the
//! corpus's bot games (the scenarios left out), the Rust game starts from its state at `--from`
//! (bots in every major's seat with the baseline's aggressions, their memory fresh) and plays to
//! the last later checkpoint; one JSON line per game, side (`python` or `rust`) and later
//! checkpoint gives every major's stats row at the end of the turn before it, the wars it
//! declared on majors and the cities it took until then, and, as that turn starts, its units by
//! kind and its relations (met, friends, embassies, wars, mean opinion).
//!
//! **Traces** (`--trace N`): the baseline's small games 0 to N-1 (seed 5000 + i, the map types in
//! turn) played to their end, one JSON line per game: per round, each major's war being prepared
//! (`memory.war_prep`), its war plan, whom it is at war with, its friends, treaties, opinions and
//! military power, and, while it could start a war, what `consider_war` would see; and the wars,
//! peace and captures of the game. The lines are those `war_trace.py` writes of Python's games.
//!
//! ```text
//! CITAR_REFCHECK_CORPUS=<corpus> cargo run --profile ci -p citar-testkit --example paired -- \
//!     [--from TURN] [--until TURN] --out FILE
//! cargo run --profile ci -p citar-testkit --example paired -- --trace 120 --out FILE
//! ```

#![allow(
    clippy::disallowed_macros,
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "a command-line tool over the corpus files"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use citar_bot::memory::Memory;
use citar_bot::{Bot, BotSpec, Overrides, Tuning, VersionId};
use citar_engine::base::ids::{PlayerId, TileIdx, Turn};
use citar_engine::game::advisor::{hostile_units, threat_at};
use citar_engine::game::diplomacy::relations::{has_embassy, is_friends, opinion};
use citar_engine::game::victory::score::military_strength;
use citar_engine::game::{DebugOptions, Game};
use citar_engine::state::chronicle::{EngineEvent, EventType};
use citar_testkit::fixtures::{self, Fixture};
use citar_testkit::games;
use citar_testkit::golden::games::{baseline_aggression, baseline_settings};
use serde_json::{Map, Value, json};

/// The baseline's map types, in its rotation.
const MAPS: [&str; 5] = ["continents", "pangaea", "archipelago", "inland_sea", "fractal"];

/// The rounds a traced game may play: past any game's end.
const TRACE_ROUNDS: u32 = 400;

/// A `basic-1` bot at its defaults for every player of `g`, each with the baseline's aggression.
fn bots_for(g: &Game, tuning: &Arc<Tuning>) -> Vec<Bot> {
    let seed = g.state().seed();
    (0..g.state().players().len())
        .map(|i| {
            let pid = PlayerId(u8::try_from(i).unwrap_or(u8::MAX));
            let a = baseline_aggression(pid, seed);
            Bot::new(Arc::new(BotSpec::new(VersionId::Basic1, Arc::clone(tuning), None, Some(a))))
        })
        .collect()
}

/// A number parameter of `basic-1` at its default.
fn param(key: &str) -> f64 {
    citar_bot::params::defaults().get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

/// One more than the engine's military strength, as the bot compares it (`military_power`).
fn power(g: &Game, p: PlayerId) -> f64 {
    f64::from(military_strength(g, p)) + 1.0
}

fn alive(g: &Game, p: PlayerId) -> bool {
    g.player(p).is_some_and(|x| x.alive())
}

/// Per major, the units it has now, by kind.
fn units_now(g: &Game) -> Value {
    let r = g.rules();
    let mut out = Map::new();
    for p in g.majors(false) {
        let mut kinds: BTreeMap<String, u32> = BTreeMap::new();
        for u in g.player_units(p.id()) {
            *kinds.entry(r.base_units()[u.base].name.to_string()).or_default() += 1;
        }
        out.insert(p.id().0.to_string(), json!(kinds));
    }
    Value::Object(out)
}

/// Per living major, its relations with the other living majors now: how many it has met, its
/// friends, the embassies it holds, its wars, and its mean opinion of those it has met.
fn diplo_now(g: &Game) -> Value {
    let majors: Vec<PlayerId> = g.majors(true).map(|p| p.id()).collect();
    let mut out = Map::new();
    for &p in &majors {
        let met: Vec<PlayerId> =
            majors.iter().copied().filter(|&q| q != p && g.has_met(p, q)).collect();
        let n = |f: &dyn Fn(PlayerId) -> bool| met.iter().filter(|&&q| f(q)).count();
        let total: f64 = met.iter().map(|&q| opinion(g, p, q)).sum();
        #[allow(clippy::cast_precision_loss, reason = "a handful of civilizations")]
        let mean = if met.is_empty() { 0.0 } else { total / met.len() as f64 };
        out.insert(
            p.0.to_string(),
            json!({
                "met": met.len(),
                "friends": n(&|q| is_friends(g, p, q)),
                "embassies": n(&|q| has_embassy(g, p, q)),
                "wars": n(&|q| g.at_war(p, q)),
                "opinion": (mean * 10.0).round() / 10.0,
            }),
        );
    }
    Value::Object(out)
}

/// Per major, at the end of turn `turn`: its stats row and the wars it declared on majors and
/// the cities it took until then.
fn rows(g: &Game, turn: Turn) -> Value {
    let majors: Vec<PlayerId> = g.majors(false).map(|p| p.id()).collect();
    let mut wars: BTreeMap<u8, u32> = BTreeMap::new();
    let mut captures: BTreeMap<u8, u32> = BTreeMap::new();
    for e in g.events(0, usize::MAX).iter().filter(|e| e.turn <= turn) {
        let Some(d) = e.data.as_ref() else { continue };
        match e.kind {
            EventType::Engine(EngineEvent::WarDeclared) => {
                if let (Some(a), Some(b)) = (d.attacker, d.defender)
                    && majors.contains(&a)
                    && majors.contains(&b)
                {
                    *wars.entry(a.0).or_default() += 1;
                }
            }
            EventType::Engine(EngineEvent::CityCaptured) => {
                if let Some(p) = d.new_owner.filter(|p| majors.contains(p)) {
                    *captures.entry(p.0).or_default() += 1;
                }
            }
            _ => {}
        }
    }
    let row = g.stats(None).iter().find(|s| s.turn == turn);
    let civs: Vec<Value> = majors
        .iter()
        .map(|&p| {
            let s = row.and_then(|r| r.civs.iter().find(|c| c.player == p));
            json!({
                "pid": p.0,
                "stats": s.map(|s| json!({
                    "alive": s.alive, "score": s.score, "cities": s.cities,
                    "population": s.population, "land": s.land, "techs": s.techs,
                    "policies": s.policies, "military": s.military, "gold": s.gold,
                    "gold_per_turn": s.gold_per_turn, "science": s.science,
                    "culture": s.culture, "faith": s.faith, "production": s.production,
                    "happiness": s.happiness, "era": s.era.0, "units": s.units,
                    "golden_age": s.golden_age,
                })),
                "wars": wars.get(&p.0).copied().unwrap_or(0),
                "captures": captures.get(&p.0).copied().unwrap_or(0),
            })
        })
        .collect();
    Value::Array(civs)
}

fn load(f: &Fixture) -> Result<Game, String> {
    games::from_fixture(f, f.name.as_bytes(), DebugOptions::OFF)
}

/// One corpus game: the Python side at each later checkpoint up to `until`, and the Rust game
/// played on from its state at `from`.
fn paired(fixtures: &[Fixture], from: u32, until: u32) -> Vec<Value> {
    let Some(start) = fixtures.iter().find(|f| f.turn == from) else { return Vec::new() };
    let later: Vec<&Fixture> =
        fixtures.iter().filter(|f| f.turn > from && f.turn <= until).collect();
    let name = &start.case;
    let at = |f: &Fixture| Turn::try_from(f.turn).unwrap_or(0);
    let mut out = Vec::new();
    for f in &later {
        match load(f) {
            Ok(g) => out.push(json!({
                "case": name, "side": "python", "from": from, "at": f.turn,
                "civs": rows(&g, at(f) - 1), "units": units_now(&g), "diplo": diplo_now(&g),
            })),
            Err(e) => eprintln!("{}: {e}", f.name),
        }
    }
    let mut g = match load(start) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{}: {e}", start.name);
            return out;
        }
    };
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    let mut bots = bots_for(&g, &tuning);
    let wanted: Vec<Turn> = later.iter().map(|f| at(f)).collect();
    let mut seen: BTreeMap<Turn, (Value, Value)> = BTreeMap::new();
    let mut hook = |g: &mut Game, (turn, _): games::Round| -> Result<(), String> {
        // The round of turn T-1 ended: the state as turn T starts, as the fixture of T holds it.
        if wanted.contains(&(turn + 1)) {
            seen.insert(turn + 1, (units_now(g), diplo_now(g)));
        }
        Ok(())
    };
    let last = later.iter().map(|f| f.turn).max().unwrap_or(from);
    if let Err(e) = games::play_random(&mut g, &mut bots, last.saturating_sub(from), &mut hook) {
        eprintln!("{name}: the Rust game stopped: {e}");
    }
    for f in &later {
        let (units, diplo) = seen.get(&at(f)).cloned().unwrap_or((Value::Null, Value::Null));
        out.push(json!({
            "case": name, "side": "rust", "from": from, "at": f.turn,
            "civs": rows(&g, at(f) - 1), "units": units, "diplo": diplo,
        }));
    }
    out
}

/// What `consider_war` would see for `pid` as turn `now` starts (basic.py:2445-2451): `null`
/// before `war_min_turn`, at war or preparing one; else its cities' threat, its power and, per
/// living major met and at peace, [a treaty in force, friends, strong enough, a city in reach].
fn eligibility(
    g: &Game,
    pid: PlayerId,
    mem: Option<&Memory>,
    tuning: &Tuning,
    majors: &[PlayerId],
    now: Turn,
) -> Value {
    let at_war =
        majors.iter().any(|&q| q != pid && alive(g, q) && g.has_met(pid, q) && g.at_war(pid, q));
    let preparing = mem.is_some_and(|m| m.war_prep.is_some());
    if !alive(g, pid) || f64::from(now) <= param("war_min_turn") || at_war || preparing {
        return Value::Null;
    }
    let a = baseline_aggression(pid, g.state().seed());
    let pp = tuning.advisor(a);
    let hostile = hostile_units(g, pid);
    let homes: Vec<TileIdx> = g.player_cities(pid).map(|c| c.tile()).collect();
    let threat: f64 = homes.iter().map(|&t| threat_at(g, &hostile, t, &pp)).sum();
    let mine = power(g, pid);
    let ratio = param("war_power_ratio") - param("war_power_ratio_aggr") * a;
    let ours: Vec<Option<u16>> = homes.iter().map(|&t| g.continent(t)).collect();
    let explored = g.player(pid).map(|x| &x.explored);
    let mut per = Map::new();
    for &q in majors {
        if q == pid || !alive(g, q) || !g.has_met(pid, q) {
            continue;
        }
        let Some(rel) = g.relation(pid, q) else { continue };
        if rel.war {
            continue;
        }
        // `_reachable_city` with `war_overseas` off and `war_target_max_dist` past any map.
        let reach = g.state().cities().iter().any(|c| {
            c.owner() == q
                && explored.is_some_and(|e| e.contains(c.tile().0))
                && ours.contains(&g.continent(c.tile()))
        });
        per.insert(
            q.0.to_string(),
            json!([
                rel.treaty_until >= now,
                is_friends(g, pid, q),
                mine > power(g, q) * ratio,
                reach
            ]),
        );
    }
    json!({"threat": (threat * 100.0).round() / 100.0, "mine": mine, "q": per})
}

/// Baseline game `i` (seed 5000 + i, small, the map types in turn) traced round by round.
fn traced(i: u32) -> Value {
    let seed = 5000 + u64::from(i);
    let map = MAPS[i as usize % MAPS.len()];
    let mut g =
        games::new_game(&baseline_settings("small", map, seed), b"trace", DebugOptions::OFF)
            .expect("a small game");
    let majors: Vec<PlayerId> = g.majors(false).map(|p| p.id()).collect();
    let tuning = Arc::new(Tuning::new(VersionId::Basic1, Overrides::default()));
    let mut bots = bots_for(&g, &tuning);
    let mut rounds = Vec::new();
    let mut events = Vec::new();
    let mut seen = 0u32;
    let mut hook = |g: &mut Game, (turn, _): games::Round| -> Result<(), String> {
        for e in g.events(seen, usize::MAX) {
            let Some(d) = e.data.as_ref() else { continue };
            let pair = match e.kind {
                EventType::Engine(EngineEvent::WarDeclared) => {
                    Some(("war", d.attacker, d.defender))
                }
                EventType::Engine(EngineEvent::Peace) => Some(("peace", d.a, d.b)),
                EventType::Engine(EngineEvent::CityCaptured) => {
                    Some(("capture", d.new_owner, d.old_owner))
                }
                _ => None,
            };
            if let Some((kind, a, b)) = pair {
                let (a, b) = (a.map(|p| p.0), b.map(|p| p.0));
                events.push(json!({"t": e.turn, "kind": kind, "a": a, "b": b}));
            }
        }
        seen = g.events(0, 1).last().map_or(seen, |e| e.id.get());
        // As the Python tracer reads its round hook: the state as turn `now` starts.
        let now = turn + 1;
        let mut civs = Map::new();
        for &pid in &majors {
            let mem = g.player(pid).and_then(|p| p.seat().driver()).map(Memory::decode);
            let others: Vec<PlayerId> =
                majors.iter().copied().filter(|&q| q != pid && alive(g, q)).collect();
            let ids = |f: &dyn Fn(PlayerId) -> bool| -> Vec<u8> {
                others.iter().copied().filter(|&q| f(q)).map(|q| q.0).collect()
            };
            let opinions: Map<String, Value> = others
                .iter()
                .filter(|&&q| g.has_met(pid, q))
                .map(|&q| (q.0.to_string(), json!((opinion(g, pid, q) * 10.0).round() / 10.0)))
                .collect();
            let prep = mem.as_ref().and_then(|m| m.war_prep.as_ref());
            civs.insert(
                pid.0.to_string(),
                json!({
                    "alive": alive(g, pid),
                    "prep": prep.map(|w| json!([w.player.0, w.since])),
                    "plan": mem.as_ref().and_then(|m| m.war_plan.as_ref()).map(|w| w.city.0),
                    "wars": ids(&|q| g.at_war(pid, q)),
                    "friends": ids(&|q| is_friends(g, pid, q)),
                    "treaty": ids(&|q| g.relation(pid, q).is_some_and(|r| r.treaty_until >= now)),
                    "opinion": opinions,
                    "power": if alive(g, pid) { power(g, pid) } else { 0.0 },
                    "elig": eligibility(g, pid, mem.as_ref(), &tuning, &majors, now),
                }),
            );
        }
        rounds.push(json!({"t": turn, "civs": civs}));
        Ok(())
    };
    let turns = games::play_random(&mut g, &mut bots, TRACE_ROUNDS, &mut hook)
        .unwrap_or_else(|e| panic!("game {i}: {e}"));
    let aggression: Map<String, Value> = majors
        .iter()
        .map(|&p| {
            (p.0.to_string(), json!((baseline_aggression(p, seed) * 1000.0).round() / 1000.0))
        })
        .collect();
    json!({"i": i, "seed": seed, "map": map, "turns": turns, "aggression": aggression,
           "rounds": rounds, "events": events})
}

/// Runs `job` for each of `n` items on every core, each on a thread with a game's stack.
fn on_every_core(n: usize, job: &(dyn Fn(usize) -> Vec<Value> + Sync)) -> Vec<Value> {
    let next = AtomicUsize::new(0);
    let lines = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let got = std::thread::scope(|inner| {
                        std::thread::Builder::new()
                            .stack_size(16 << 20)
                            .spawn_scoped(inner, || job(i))
                            .expect("a thread")
                            .join()
                            .expect("the game")
                    });
                    lines.lock().expect("the lines").extend(got);
                }
            });
        }
    });
    lines.into_inner().expect("the lines")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut from, mut until, mut trace, mut out) = (1u32, 280u32, None::<u32>, None::<String>);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut number = || it.next().and_then(|t| t.parse().ok());
        match a.as_str() {
            "--from" => from = number().expect("--from TURN"),
            "--until" => until = number().expect("--until TURN"),
            "--trace" => trace = Some(number().expect("--trace GAMES")),
            "--out" => out = it.next().cloned(),
            other => panic!("unknown argument {other}"),
        }
    }
    let out = out.expect("--out FILE");
    let mut lines = if let Some(n) = trace {
        let mut lines =
            on_every_core(n as usize, &|i| vec![traced(u32::try_from(i).unwrap_or(u32::MAX))]);
        lines.sort_by_key(|v| v["i"].as_u64());
        lines
    } else {
        let corpus = fixtures::corpus().expect("the corpus folder").expect("CITAR_REFCHECK_CORPUS");
        let mut by_case: BTreeMap<String, Vec<Fixture>> = BTreeMap::new();
        for f in corpus.into_iter().filter(|f| !f.case.starts_with("scenario")) {
            by_case.entry(f.case.clone()).or_default().push(f);
        }
        let cases: Vec<Vec<Fixture>> = by_case.into_values().collect();
        on_every_core(cases.len(), &|i| paired(&cases[i], from, until))
    };
    lines.sort_by_key(|v| (v["case"].to_string(), v["side"].to_string(), v["at"].as_u64()));
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&out, text).unwrap_or_else(|e| panic!("{out}: {e}"));
}

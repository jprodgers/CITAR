//! A baseline run: the games of its options not yet in the file, played on worker threads and
//! written a line each as they finish (`baseline.py:163-249`, `common.py:205-286`).
//!
//! - **One file is one sample.** A run refuses to add to a file that holds a line from other
//!   code (another build id or bot) or a game `i` whose identity differs from this run's game
//!   `i`, and says which lines and why ([`existing`]).
//! - **Resumable.** Games already finished in the file are skipped; crashed ones are played
//!   again. A run stopped mid-write leaves a torn last line: the next run starts on a fresh line,
//!   and `summarize.py` skips the torn one.
//! - **Nothing holds a run up.** Each game has its time budget, checked between seats. A game
//!   stuck inside one turn cannot be interrupted from outside its thread, so when no game
//!   finishes for longer than any may take ([`stall_limit`]), the run writes the games caught in
//!   it as crash lines and stops; the stuck threads end with the process. A worker thread that
//!   dies is reported the same way.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use citar_bot::{BotSpec, Tuning, VersionId, build_id, clean, fingerprint};
use citar_engine::game::setup::config_from_value;
use citar_engine::rules::Ruleset;
use serde_json::Value;

use super::BaselineLine;
use super::play::{Code, Context, crash_line, play_one, round1};
use super::spec::{
    GameSpec, IDENTITY, Options, check_minutes, check_seeds, game_spec, output_path, stall_limit,
};
use crate::runner::panic_message;

/// The stack of a worker thread: a game's deepest call chains (settles inside drives inside
/// steps) with room to spare, whatever the platform's default.
const WORKER_STACK: usize = 16 << 20;

/// What became of a run, as its exit code says it (`baseline.main`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every game was played and none crashed: exit 0.
    Done,
    /// Some games crashed, or the run stopped before the end: exit 1. Run it again to play them.
    Incomplete,
    /// The run could not start: the file holds other games, or an option names nothing: exit
    /// 2.
    Refused,
}

impl Outcome {
    /// The exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Incomplete => 1,
            Self::Refused => 2,
        }
    }
}

/// Whether this build runs the engine's invariants when asked: a debug or ci build, or one with
/// the `checks` feature.
pub const CHECKS_BUILT: bool = cfg!(any(debug_assertions, feature = "checks"));

/// The code a run with options `o` plays with, and the bots' parameters: the build id, and the
/// bot with its parameters' fingerprint when they override any.
///
/// # Errors
/// Parameters the bot refuses, with its message.
pub fn code_of(rules: &Ruleset, o: &Options) -> Result<(Code, Arc<Tuning>), String> {
    let version = VersionId::Basic1;
    let overrides = clean(version.id(), o.params.as_ref().unwrap_or(&Value::Null))
        .map_err(|e| format!("--params: {e}"))?;
    let engine = build_id(rules);
    let plain = overrides.is_empty();
    let tuning = Arc::new(Tuning::new(version, overrides));
    let bot = if plain {
        version.id().to_owned()
    } else {
        // What plays, as the lab names it (DESIGN.md P2.8.6): the build, the version and the
        // overrides, the seat deciding the aggression.
        let spec = BotSpec::new(version, Arc::clone(&tuning), None, None);
        format!("{}+{}", version.id(), fingerprint(&spec, &engine))
    };
    Ok((Code { engine, bot }, tuning))
}

/// The games already finished in the file at `path`, and why this run may not add to it (empty
/// if it may): a line from other code, or a game `i` whose identity differs from this run's
/// game `i` (with options `o`, already effective), would mix two samples in one file
/// (`baseline.existing`). Lines that are not JSON (a run stopped mid-write) are ignored.
///
/// # Errors
/// The file exists and cannot be read.
pub fn existing(
    rules: &Ruleset,
    path: &Path,
    o: &Options,
    code: &Code,
) -> std::io::Result<(BTreeSet<u32>, Vec<String>)> {
    let mut done = BTreeSet::new();
    let mut clashes = Vec::new();
    let text = match std::fs::read(path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((done, clashes)),
        Err(e) => return Err(e),
    };
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let Ok(r) = serde_json::from_str::<Value>(line) else { continue };
        let Some(i) = r.get("i").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok()) else {
            clashes.push(format!("line {n} is not a game of a baseline run"));
            continue;
        };
        let (engine, bot) = (r.get("engine"), r.get("bot"));
        if engine.and_then(Value::as_str) != Some(code.engine.as_str())
            || bot.and_then(Value::as_str) != Some(code.bot.as_str())
        {
            clashes.push(format!(
                "line {n} (game {i}) was played by engine {}, bot {}; this code is engine {}, \
                 bot {}",
                plain(engine),
                plain(bot),
                code.engine,
                code.bot
            ));
            continue;
        }
        let want = game_spec(rules, o, i).identity();
        let diff: Vec<String> = IDENTITY
            .iter()
            .filter(|&&k| r.get(k) != want.get(k))
            .map(|&k| format!("{k} {} (this run: {})", shown(r.get(k)), shown(want.get(k))))
            .collect();
        if !diff.is_empty() {
            clashes.push(format!("line {n} (game {i}) has {}", diff.join(", ")));
        } else if !r.get("crash").is_some_and(truthy) {
            done.insert(i);
        }
    }
    Ok((done, clashes))
}

/// A value of a line as a message shows it, as Python's `repr` did: its JSON (a string quoted),
/// or `None` when it is missing.
fn shown(v: Option<&Value>) -> String {
    v.map_or_else(|| "None".to_owned(), Value::to_string)
}

/// A value of a line as text, as Python's f-string did: a string as it is.
fn plain(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        other => shown(other),
    }
}

/// Whether a value is set, as Python's `if r.get("crash")` reads it.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Every option names something the ruleset has: each combination of size, map type and
/// barbarian setting makes a configuration with the speed. The budget is a number of minutes,
/// and every game's seed is one (`seed + i` passes no `u64`).
fn check_options(rules: &'static Ruleset, o: &Options) -> Result<(), String> {
    if o.sizes.is_empty() || o.maps.is_empty() || o.barbarians.is_empty() {
        return Err("--sizes, --maps and --barbarians each need at least one value".to_owned());
    }
    check_minutes(o.max_minutes)?;
    check_seeds(o.seed, o.games)?;
    if o.checks && !CHECKS_BUILT {
        return Err("--checks needs a build with the engine's checks: a debug or ci build, or \
                    --features checks"
            .to_owned());
    }
    for s in &o.sizes {
        if rules.constants().map_size(s).is_none() {
            return Err(format!("--sizes: there is no map size {s:?}"));
        }
    }
    let combos = o.sizes.len() * o.maps.len() * o.barbarians.len();
    for i in 0..u32::try_from(combos).unwrap_or(u32::MAX) {
        let spec = game_spec(rules, o, i);
        config_from_value(rules, spec.config(rules))
            .map_err(|e| format!("{}/{}/{}: {e}", spec.size, spec.map_type, spec.barbarians))?;
    }
    Ok(())
}

/// Plays the run with options `o`, saying what it does through `say` a line at a time, and
/// returns what became of it (`baseline.main`).
pub fn run(o: &Options, say: &mut dyn FnMut(&str)) -> Outcome {
    let rules = Ruleset::shared();
    let o = o.effective();
    if let Err(e) = check_options(rules, &o) {
        say(&e);
        return Outcome::Refused;
    }
    let (code, tuning) = match code_of(rules, &o) {
        Ok(c) => c,
        Err(e) => {
            say(&e);
            return Outcome::Refused;
        }
    };
    let mut name = o.name.clone().unwrap_or_else(|| format!("rust-{}-{}", code.engine, code.bot));
    if o.smoke {
        name.push_str("-smoke");
    }
    let out = output_path(&o.dir, &name);
    if let Err(e) = std::fs::create_dir_all(&o.dir) {
        return refused(say, &out, &e);
    }
    if o.smoke
        && let Err(e) = std::fs::remove_file(&out)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        // A smoke run proves the pipeline from scratch every time.
        return refused(say, &out, &e);
    }
    let (have, clashes) = match existing(rules, &out, &o, &code) {
        Ok(x) => x,
        Err(e) => return refused(say, &out, &e),
    };
    if !clashes.is_empty() {
        say(&format!(
            "{} holds games from other code or other options, and one file must be one sample:",
            out.display()
        ));
        for c in clashes.iter().take(10) {
            say(&format!("  {c}"));
        }
        say(&format!(
            "  ({} line(s) in all.) Use another --name, or delete the file to start over.",
            clashes.len()
        ));
        return Outcome::Refused;
    }
    let todo: Vec<GameSpec> =
        (0..o.games).filter(|i| !have.contains(i)).map(|i| game_spec(rules, &o, i)).collect();
    say(&format!(
        "{} game(s) to play into {} ({} already there), {} worker(s).",
        todo.len(),
        out.display(),
        have.len(),
        o.workers
    ));
    let started = Instant::now();
    let mut file = match OpenOptions::new().create(true).read(true).append(true).open(&out) {
        Ok(f) => f,
        Err(e) => return refused(say, &out, &e),
    };
    if let Err(e) = end_torn_line(&mut file) {
        return refused(say, &out, &e);
    }
    let cx = Context { rules, code, version: VersionId::Basic1, tuning, checks: o.checks };
    let total = todo.len();
    let stall = stall_limit(todo.iter().map(|s| s.budget));
    let (mut played, mut crashed) = (0_usize, 0_usize);
    let mut failed = None;
    run_parallel(&cx, todo, o.workers, stall, &mut |spec, r| {
        played += 1;
        let line = r.unwrap_or_else(|why| {
            BaselineLine::Crash(crash_line(&cx, spec, why, String::new(), None))
        });
        let written = serde_json::to_string(&line)
            .map_err(std::io::Error::other)
            .and_then(|text| writeln!(file, "{text}"))
            .and_then(|()| file.flush());
        match &line {
            BaselineLine::Crash(c) => {
                crashed += 1;
                say(&format!(
                    "  [{played}/{total}] game {} (seed {}) CRASHED: {}\n{}",
                    c.i, c.seed, c.crash, c.trace
                ));
            }
            BaselineLine::Game(g) => say(&format!(
                "  [{played}/{total}] game {} seed {} {}/{}: {} turns, winner {} ({}), {:?}s",
                g.i,
                g.seed,
                g.size,
                g.map_type,
                g.turns,
                g.winner.map_or_else(|| "None".to_owned(), |w| w.to_string()),
                g.victory.as_deref().unwrap_or("None"),
                g.seconds
            )),
        }
        if let Err(e) = written {
            failed = Some(e);
            return false;
        }
        true
    });
    let took = round1(started.elapsed().as_secs_f64());
    if let Some(e) = failed {
        say(&format!(
            "{}: {e}; the run stopped. Run the same command again to resume.",
            out.display()
        ));
        return Outcome::Incomplete;
    }
    if played < total {
        say(&format!(
            "Stopped early after {took:?}s with {} game(s) unplayed. Run the same command again \
             to resume.",
            total - played
        ));
        return Outcome::Incomplete;
    }
    say(&format!(
        "Done in {took:?}s. Summarise with: python scripts/refcheck/summarize.py {}",
        out.display()
    ));
    if crashed > 0 { Outcome::Incomplete } else { Outcome::Done }
}

/// A run that cannot use its file.
fn refused(say: &mut dyn FnMut(&str), out: &Path, e: &std::io::Error) -> Outcome {
    say(&format!("{}: {e}", out.display()));
    Outcome::Refused
}

/// A job's news, from a worker to the run.
enum Msg {
    Started(u32),
    /// Boxed: a line is a few hundred bytes, a start four.
    Finished(u32, Box<Result<BaselineLine, String>>),
}

/// Plays `jobs` on `workers` threads, calling `on` with each game and its line as it finishes,
/// or with why it has none: it panicked outside the game, its thread died, or the run stalled
/// with it unfinished (`common.run_parallel`). `on` returns whether to go on. After a stall, or
/// when `on` stops the run, the jobs not yet started are left for the next run.
fn run_parallel(
    cx: &Context,
    jobs: Vec<GameSpec>,
    workers: usize,
    stall: Duration,
    on: &mut dyn FnMut(&GameSpec, Result<BaselineLine, String>) -> bool,
) {
    let specs: BTreeMap<u32, GameSpec> = jobs.iter().map(|s| (s.i, s.clone())).collect();
    let queue = Arc::new(Mutex::new(jobs.into_iter().collect::<VecDeque<_>>()));
    let stop = Arc::new(AtomicBool::new(false));
    let halt = || {
        stop.store(true, Ordering::Relaxed);
        queue.lock().unwrap_or_else(PoisonError::into_inner).clear();
    };
    let (tx, rx) = mpsc::channel::<Msg>();
    for w in 0..workers.clamp(1, specs.len().max(1)) {
        let (queue, stop, tx, cx) = (Arc::clone(&queue), Arc::clone(&stop), tx.clone(), cx.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("baseline-{w}"))
            .stack_size(WORKER_STACK)
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let next = queue.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
                    let Some(spec) = next else { break };
                    if tx.send(Msg::Started(spec.i)).is_err() {
                        break;
                    }
                    // play_one writes a panic of its game as the game's crash line; this catch
                    // is the last resort, for one in writing that line.
                    let line =
                        catch_unwind(AssertUnwindSafe(|| play_one(&cx, &spec))).map_err(|p| {
                            format!("the job raised a panic: {}", panic_message(p.as_ref()))
                        });
                    if tx.send(Msg::Finished(spec.i, Box::new(line))).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            // The jobs stay queued for the threads that did start, or for the next run.
            break;
        }
    }
    drop(tx);
    let mut running = BTreeSet::new();
    let mut last = Instant::now();
    loop {
        match rx.recv_timeout(stall.saturating_sub(last.elapsed())) {
            Ok(Msg::Started(i)) => {
                running.insert(i);
            }
            Ok(Msg::Finished(i, line)) => {
                running.remove(&i);
                last = Instant::now();
                if let Some(spec) = specs.get(&i)
                    && !on(spec, *line)
                {
                    halt();
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                halt();
                let why = format!(
                    "unfinished when the run stopped: no job finished for {:.0} minutes, so a \
                     worker is stuck inside a turn, where the game's budget cannot stop it",
                    stall.as_secs_f64() / 60.0
                );
                for spec in running.iter().filter_map(|i| specs.get(i)) {
                    if !on(spec, Err(why.clone())) {
                        break;
                    }
                }
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Every worker has gone: a game still running went with its thread.
                for spec in running.iter().filter_map(|i| specs.get(i)) {
                    if !on(spec, Err("its worker thread died".to_owned())) {
                        break;
                    }
                }
                return;
            }
        }
    }
}

/// Starts the next line of `file` on a line of its own: a run stopped mid-write leaves its last
/// line torn, without a newline.
fn end_torn_line(file: &mut File) -> std::io::Result<()> {
    let len = file.seek(SeekFrom::End(0))?;
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        file.write_all(b"\n")?;
    }
    Ok(())
}

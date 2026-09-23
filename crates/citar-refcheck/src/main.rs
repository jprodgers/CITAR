//! `cargo refcheck`: the reference-check command line (DESIGN.md 9.2, "The CLI").
//!
//! ```text
//! cargo refcheck run [--fixtures DIR]... [--groups G,...] [--case GLOB]... [--json OUT] [--strict] [--with-bot]
//! cargo refcheck explain <intended-id | group | group:path> [selection]
//! cargo refcheck suggest [selection]
//! cargo refcheck ratchet [--update]
//! cargo refcheck changelog
//! cargo refcheck list [--fixtures DIR]... [--case GLOB]...
//! ```
//!
//! Exit codes: 0 clean; 1 unexplained differences (or a ratchet rise); 2 a load failure, a bad
//! configuration file or a usage error; 3 stale intended entries under `--strict`.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use rayon::prelude::*;

use citar_refcheck::answer::{Answers, Engine};
use citar_refcheck::fixture::{self, Fixture, FixtureSet};
use citar_refcheck::ratchet::{DEFAULT_FIXTURES, Ratchet};
use citar_refcheck::run::{self, Config, INTENDED, RATCHET, RunOptions};
use citar_refcheck::{Error, Group, Result, report, suggest};

#[derive(Parser)]
#[command(
    name = "citar-refcheck",
    version,
    about = "Compares the Rust engine's answers with those recorded from the Python engine"
)]
struct Cli {
    /// The repository root (default: the nearest folder above this one with refcheck/intended.toml)
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compare, report, and fail on unexplained differences where enforced.toml says so
    Run {
        #[command(flatten)]
        selection: Selection,
        /// Also write the report as JSON
        #[arg(long, value_name = "OUT")]
        json: Option<PathBuf>,
        /// Every difference counts, every selected group must be compared, stale entries fail (exit 3)
        #[arg(long)]
        strict: bool,
        /// Unexplained differences printed per group; 0 prints them all
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show an intended entry and what it explains, or the differences at a place
    Explain {
        /// An intended id, a group, or group:path (a path pattern)
        target: String,
        #[command(flatten)]
        selection: Selection,
    },
    /// Print [[differences]] stubs for the unexplained differences (never accepted automatically)
    Suggest {
        #[command(flatten)]
        selection: Selection,
    },
    /// Check that no group's unexplained count rose (refcheck/ratchet.json)
    Ratchet {
        /// Record falls and newly compared groups; a rise is refused
        #[arg(long)]
        update: bool,
    },
    /// Print the intended differences as the CHANGELOG's list of rule fixes
    Changelog,
    /// List the fixtures, and the groups with their state
    List {
        #[arg(long = "fixtures", value_name = "DIR")]
        fixtures: Vec<PathBuf>,
        #[arg(long = "case", value_name = "GLOB")]
        cases: Vec<String>,
    },
}

/// Which fixtures and groups to check.
#[derive(Args)]
struct Selection {
    /// A fixture folder, repeatable (default: refcheck/fixtures-mini and refcheck/fixtures-late)
    #[arg(long = "fixtures", value_name = "DIR")]
    fixtures: Vec<PathBuf>,
    /// Groups to check, comma-separated (default: every group)
    #[arg(long, value_delimiter = ',', value_name = "GROUPS")]
    groups: Vec<String>,
    /// Only fixtures whose name, <case>/t<turn>, matches; repeatable
    #[arg(long = "case", value_name = "GLOB")]
    cases: Vec<String>,
    /// Also compare the bot's valuations (Phase 2)
    #[arg(long)]
    with_bot: bool,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Help and --version exit 0; usage errors exit 2, as a bad configuration does.
            let code = u8::try_from(e.exit_code()).unwrap_or(2);
            e.print().unwrap_or_default();
            return ExitCode::from(code);
        }
    };
    match execute(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("refcheck: {e}");
            ExitCode::from(2)
        }
    }
}

fn execute(cli: Cli) -> Result<u8> {
    let root = find_root(cli.root)?;
    let answers = Engine;
    match cli.command {
        Command::Run { selection, json, strict, limit } => {
            let config = Config::load(&root)?;
            let mut opts = options(&root, &selection)?;
            opts.strict = strict;
            let run = run::run(opts, &config, &answers)?;
            print!("{}", report::human(&run, limit));
            if let Some(out) = json {
                std::fs::write(&out, report::json(&run))
                    .map_err(|e| Error::new(format!("cannot write {}: {e}", out.display())))?;
            }
            Ok(run.exit_code())
        }
        Command::Explain { target, selection } => {
            let config = Config::load(&root)?;
            let run = run::run(options(&root, &selection)?, &config, &answers)?;
            print!("{}", report::explain(&run, &config.intended, &target)?);
            Ok(if run.load_failures.is_empty() { 0 } else { 2 })
        }
        Command::Suggest { selection } => {
            let config = Config::load(&root)?;
            let run = run::run(options(&root, &selection)?, &config, &answers)?;
            print!("{}", suggest::suggest(&run));
            Ok(if run.load_failures.is_empty() { 0 } else { 2 })
        }
        Command::Ratchet { update } => ratchet(&root, update, &answers),
        Command::Changelog => {
            let config = Config::load(&root)?;
            let text = config.intended.changelog();
            if text.is_empty() {
                eprintln!("refcheck: {INTENDED} lists no differences yet");
            }
            print!("{text}");
            Ok(0)
        }
        Command::List { fixtures, cases } => list(&root, &fixtures, &cases, &answers),
    }
}

/// The repository root: `--root`, else the nearest folder with the intended list, else the
/// checkout this binary was built from.
fn find_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        if root.join(INTENDED).is_file() {
            return Ok(root);
        }
        return Err(Error::new(format!("--root {}: there is no {INTENDED} there", root.display())));
    }
    if let Ok(cwd) = std::env::current_dir()
        && let Some(dir) = cwd.ancestors().find(|d| d.join(INTENDED).is_file())
    {
        return Ok(dir.to_path_buf());
    }
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    if built.join(INTENDED).is_file() {
        return Ok(built.canonicalize().unwrap_or(built));
    }
    Err(Error::new(format!(
        "cannot find the repository root (no {INTENDED} above here); use --root"
    )))
}

fn fixture_sets(root: &Path, dirs: &[PathBuf]) -> Vec<FixtureSet> {
    if dirs.is_empty() {
        DEFAULT_FIXTURES.iter().map(|d| FixtureSet::new(&root.join(d), root)).collect()
    } else {
        dirs.iter().map(|d| FixtureSet::new(d, root)).collect()
    }
}

fn options(root: &Path, s: &Selection) -> Result<RunOptions> {
    let mut opts = RunOptions::new(root, fixture_sets(root, &s.fixtures));
    if !s.groups.is_empty() {
        let mut groups =
            s.groups.iter().map(|g| Group::parse(g.trim())).collect::<Result<Vec<Group>>>()?;
        groups.sort();
        groups.dedup();
        opts.groups = groups;
    }
    opts.cases.clone_from(&s.cases);
    opts.with_bot = s.with_bot;
    Ok(opts)
}

fn ratchet(root: &Path, update: bool, answers: &dyn Answers) -> Result<u8> {
    let path = root.join(RATCHET);
    let ratchet = Ratchet::load(&path)?;
    let dirs: Vec<PathBuf> = ratchet.fixtures.iter().map(|d| root.join(d)).collect();
    let config = Config::load(root)?;
    let run = run::run(RunOptions::new(root, fixture_sets(root, &dirs)), &config, answers)?;
    if !run.load_failures.is_empty() {
        for f in &run.load_failures {
            println!("load failure {}: {}", f.name, f.error);
        }
        return Ok(2);
    }
    let counts = run.counts();
    let verdict = ratchet.check(&counts);
    println!("ratchet: unexplained differences per group over {}", ratchet.fixtures.join(", "));
    for (g, now) in &counts {
        let was = ratchet.unexplained.iter().find(|(h, _)| h == g).map(|(_, n)| *n);
        let note = match was {
            Some(w) if *now > w => format!("rose from {w}"),
            Some(w) if *now < w => format!("fell from {w}"),
            Some(_) => "unchanged".to_string(),
            None => "not yet recorded".to_string(),
        };
        println!("  {:<16} {now:>6}  {note}", g.name());
    }
    for g in &verdict.missing {
        println!("  {:<16} {:>6}  recorded, but not compared any more", g.name(), "-");
    }
    if counts.is_empty() && ratchet.unexplained.is_empty() {
        println!("  (no group is compared yet)");
    }
    if verdict.fails() {
        println!(
            "ratchet: refused. A rise is fixed, or explained in {INTENDED}; it is never recorded."
        );
        return Ok(1);
    }
    if update {
        match ratchet.updated(&counts) {
            Ok(next) if next != ratchet || !path.exists() => {
                std::fs::write(&path, next.to_text())
                    .map_err(|e| Error::new(format!("cannot write {}: {e}", path.display())))?;
                println!("ratchet: recorded in {RATCHET}");
            }
            Ok(_) => println!("ratchet: nothing to record"),
            Err(_) => return Ok(1),
        }
    } else if verdict.changes() {
        println!("ratchet: holds; `cargo refcheck ratchet --update` records the changes");
    } else {
        println!("ratchet: holds");
    }
    Ok(0)
}

fn list(root: &Path, dirs: &[PathBuf], cases: &[String], answers: &dyn Answers) -> Result<u8> {
    let sets = fixture_sets(root, dirs);
    let mut refs = fixture::discover(&sets)?;
    if !cases.is_empty() {
        let mut b = globset::GlobSetBuilder::new();
        for c in cases {
            b.add(
                globset::Glob::new(c)
                    .map_err(|e| Error::new(format!("bad --case glob `{c}`: {e}")))?,
            );
        }
        let set = b.build().map_err(|e| Error::new(e.to_string()))?;
        refs.retain(|r| set.is_match(&r.name));
    }
    let metas: Vec<Result<fixture::Meta>> = refs.par_iter().map(Fixture::load_meta).collect();
    let mut failed = 0;
    for (r, meta) in refs.iter().zip(&metas) {
        let set = &sets[r.set].label;
        match meta {
            Ok(m) => println!(
                "{set:<15} {:<44} {:<22} barbarians {:<7} {} players",
                r.name,
                format!("{}/{}", m.config.map_size, m.config.map_type),
                m.config.barbarians,
                m.config.players.len()
            ),
            Err(e) => {
                failed += 1;
                println!("{set:<15} {:<44} cannot load: {e}", r.name);
            }
        }
    }
    let per_set: Vec<String> = sets
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{} in {}", refs.iter().filter(|r| r.set == i).count(), s.shown))
        .collect();
    println!("\n{} states: {}", refs.len(), per_set.join(", "));

    let config = Config::load(root)?;
    let groups: Vec<String> = Group::ALL
        .iter()
        .map(|g| {
            let mut tags = Vec::new();
            if answers.module(*g).is_none() {
                tags.push("not ported");
            }
            if config.enforced.has_group(*g) {
                tags.push("enforced");
            }
            if g.scope() == citar_refcheck::group::Scope::Run {
                tags.push("once per run");
            }
            if tags.is_empty() {
                g.name().to_string()
            } else {
                format!("{} ({})", g.name(), tags.join(", "))
            }
        })
        .collect();
    println!("groups, in dependency order: {}", groups.join("; "));
    Ok(if failed == 0 { 0 } else { 2 })
}

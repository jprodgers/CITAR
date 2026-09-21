# Benchmarks

A benchmark plays full games and scores what happened. It is the main reason CITAR exists.

## The shape of it

A **suite** is a saved configuration: which models to test, and which scenarios to test them on.
Every enabled model plays every scenario, against the same scripted bots, on the same seed.

**Models** come from the [Servers](REPORTS.md#servers) page — a server, a model on it, and a load
profile. Include the **Dry run** server to check a suite end to end without spending GPU time.

**Scenarios** are game configurations:

| Setting | Default | Notes |
|---|---|---|
| Map size and type | Small, continents | |
| Bot opponents | 2 | And their aggression |
| Seed | fixed | The same map for every model, which is the point |
| Speed | Quick | |
| Turn limit | 330 | Quick's time-victory turn. `0` plays to the end |
| Max time per model turn | | A turn cut off after this is replayed later and left out of the timing statistics |

Every seat plays **BenchmarkCiv** unless the scenario names civilizations, so a comparison is about
the player rather than about Babylon's free scientist.

Per model, a suite can override the reasoning effort, pick a different load profile, and set a
display name. Tool mode, context length and GPU offload come from the server's model catalogue.

**Export** and **Import JSON** move suites between machines, which is how you reproduce somebody
else's benchmark.

## Running one

**Scheduling** is either *sequential* — one game at a time, the honest choice when turn timing
matters — or *parallel*, which runs servers at the same time, each up to its **Games at once**. On
servers where CITAR manages loading (LM Studio), each model is loaded with its profile before its
games start.

**Runs** shows every game grouped by server: status, turn progress, score against the best bot, a
score trend, turn time, errors and repeats, clean turn endings, the model's latest message and an
estimated time left. Click a game to watch it live. Pause, resume, skip or retry single games, or
pause and cancel whole runs.

**Restricted hours** are set per server on the Servers page. When a window starts, games on that
server finish the model turn in progress (up to the server's grace minutes), then pause, and no new
ones start there. The server's models can be unloaded so its GPU idles. Everything resumes when the
window ends; other servers keep going. This exists because a GPU in a room somebody sleeps in is a
real constraint.

**Multi-day runs survive restarts.** Runs are saved under `benchmarks/runs/`, and when the server
starts, games that were in progress are reloaded from their autosaves and continue. Starting a
benchmark that will take three days is a normal thing to do.

## Scoring

**Models** (top navigation) ranks every model by an overall score:

| Component | Weight | What it measures |
|---|---|---|
| **Benchmark performance** | 60% | 100 for a win, 0 if eliminated, otherwise the model's share of the score against the strongest bot — 50 means level. Each game counts in proportion to the turns played, so full games outweigh short tests. |
| **Reliability** | 25% | Clean turn endings, few rejected orders, little looping. |
| **Speed** | 15% | Average turn time on a log scale: 20 seconds or less is 100, 30 minutes or more is 0. |

The weights are adjustable under Benchmarks → ⚙. A model needs at least one benchmark game to get
an overall score, and dry-run games never count.

Clicking a model shows its benchmark games. The page also carries the detailed comparison table —
timing, steps, calls, errors, repeats, tokens, turn endings — across every running and saved game.

### What the metrics mean

Performance alone is misleading in both directions, which is why the other two exist.

- A model that plays well but times out on half its turns will score highly on performance and
  badly overall — correctly, because a player that cannot finish a turn is not playing.
- A model that ends every turn cleanly in four seconds by doing nothing scores well on reliability
  and speed and near zero on performance.
- **Looping** is the most diagnostic single number. A model that issues the same call repeatedly
  has usually misunderstood the state rather than the rules, and the fix is often the briefing.

## From the command line

For a quick check without the web app:

```bash
citar bench --all-models --load-context 32768 --turns 2
```

Each model plays the same seeded map against a bot; the script prints a comparison table and writes
`saves/benchmark-*.json`. These games count toward model scores too.

`--all-models` uses every LLM on the LM Studio server; pass `--model` several times to pick.
`--load-context` loads each model alone at that context length first.

For anything long, prefer the web app: the Benchmarks page shows progress and an estimated finish,
and a detached CLI run leaves you with no idea how far along it is.

## Reading a result honestly

- **One game is one sample.** Map luck is enormous in Civ. The model comparison in a
  [report](REPORTS.md) gives 95% confidence intervals and flags small samples for this reason.
- **Turn limits truncate.** A 330-turn game that ends on turn 330 scores on position, not victory.
  Two models can be a long way apart in a way the score does not show.
- **Bots are not a fixed yardstick across versions.** Bot behaviour changes between releases, so
  scores are comparable within a version and not across one. The release notes say when the bot
  changed.
- **Speed is hardware, not intelligence.** A model that is slow on a laptop GPU may be fast
  elsewhere. Reports separate the two; the overall score does not.

## Where the data goes

| | |
|---|---|
| Suites and runs | `benchmarks/` |
| Game saves | `saves/<game id>/` |
| Per-turn metrics | inside each game's save, and as CSV from the stats screen |
| Usage for costing | `saves/usage/*.jsonl` |

`citar where` prints the actual paths for your installation.

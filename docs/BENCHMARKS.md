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
| Mode | Full | *Full*: the model plays every turn. *Hybrid*: a bot plays, the model negotiates ([below](#hybrid-suites)) |

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

**A machine in use waits.** A job starts on a machine only while fewer things hold it than its
**Games at once**: every lobby game with a model seat there counts from the moment it is created,
loaded or brought back after a restart (paused or not, whoever moves first) until that seat is
changed or eliminated or the game ends, and so does a probe's turn case while it plays. A game paused to make way for higher-priority work does not
count until it resumes, not even while its model answers a chat. The job says what it is waiting for,
naming only the games and runs you may see (anyone else's is "someone's game").

**Pooled prompts drain first.** Short pooled prompts (reports' written analyses, probes' offers and
messages, the Servers page's test; chat answers later) may run on a machine between games. When a job starts there, those
waiting are refused and those running are cancelled, and the model's first request waits until
the ones its machine is still generating have finished, for up to two minutes, so none of them runs
beside a benchmark step. The run page shows that wait under the job's turn time ("drained 6s",
`drain_s` in the job's result). A job whose wait reached the two minutes goes ahead anyway and is
marked **timing contaminated** (`contaminated`): its turn times measured the other work too, so they
are left out of the model's speed score; its performance and reliability still count.

**Restricted hours** are set per server on the Servers page. When a window starts, games on that
server finish the model turn in progress (up to the server's grace minutes), then pause, and no new
ones start there. The server's models can be unloaded so its GPU idles. Everything resumes when the
window ends; other servers keep going. This exists because a GPU in a room somebody sleeps in is a
real constraint.

**Full and hybrid jobs keep off each other's machines.** A hybrid job holds no lease, but it counts
against its machine's **Games at once** like any job, and it never starts on a machine where a full
benchmark job (this server's, or a lease anyone's benchmark holds) is playing: its prompts wait for
benchmarks, so beside one they would sit behind its steps for hours. A full job likewise waits for a
running hybrid job on its machine. The job says which it waits for.

**Multi-day runs survive restarts.** Runs are saved under `benchmarks/runs/`, and when the server
starts, games that were in progress are reloaded from their autosaves and continue. Starting a
benchmark that will take three days is a normal thing to do.

## Hybrid suites

A scenario in **Hybrid** mode seats the model in a [hybrid seat](AI_PLAYERS.md#hybrid-seats): the
seat's bot plays every turn, and the model decides only the kinds of diplomacy the scenario hands it.
The scenario's hybrid settings:

| Setting | Default | Notes |
|---|---|---|
| The model decides | All but war | A preset (trade only, all but war, everything) or a choice of its own among the ten kinds |
| The seat's bot | the standard profile | "Best bot" is pinned when each job starts, as a lobby seat's is |
| Handicap | AI bonuses | Or a person's values; the control game copies it |
| Call the diplomat at least every N turns | 10 | It is also called on its triggers (first contact, a war, a deal running out...) |

In a benchmark the advisor is always off, and the model's prompts (its diplomat calls and its chat
answers) **wait for other benchmarks** with no start deadline: a call that waits behind another
benchmark is answered by the model when its turn comes, not by the fallback bot, and the chat clock
does not count that wait. The clock's model timeout still applies once the prompt has started, so a
model that is reachable but silent falls back, and that fallback is the model's.

**Every hybrid job has a control game**: the same game, settings and seed, with the hybrid seat
played by the same bot profile deciding everything, at the hybrid seat's handicap and difficulty
level (its automatic decisions, the United Nations vote and annexing, are a bot seat's). The pair
differs only in who decides the model's categories. Controls are bot-only: they play headless on the
server's CPU while the hybrid games play, once per scenario, seed, build and bots, and are kept under
`benchmarks/controls/` and shared by every model's jobs of that scenario and seed, so a second model's
run of a suite plays no control again. A control is played from its settings, so the same control
played twice gives the same game, event for event.

A hybrid job's row shows its control game's state and, once both games are over, its **pair line**:
the score difference (hybrid minus control) at the end, and at turns 100, 200 and 300 where both
games reached them; the deals made and their mean value; and the control's own deals. Hybrid jobs
stay out of the model scores below: their turns are the bot's.

**The diplomacy record.** Both games of a pair are recorded the same way. After every drive, answer
and tool call, the hybrid seat's (and the control seat's) negotiations that moved are read from the
game's history, and each proposal the seat made or received is valued by its own bot profile deciding
everything, in gold from the seat's side, on the proposal's recorded terms against the game at that
moment. Many proposals between bots open and close inside one drive, so reading the history after it
is what values both arms alike. An accepted deal keeps the value of the proposal that was accepted.
The game's events add the wars declared by and on the seat and how they ended, cities captured and
lost, and peace made.

## Scoring

**Models** (top navigation) ranks every model by an overall score:

| Component | Weight | What it measures |
|---|---|---|
| **Benchmark performance** | 60% | 100 for a win, 0 if eliminated, otherwise the model's share of the score against the strongest bot — 50 means level. Each game counts in proportion to the turns played, so full games outweigh short tests. |
| **Reliability** | 25% | Clean turn endings, few rejected orders, little looping. |
| **Speed** | 15% | The model's own generation time per turn on a log scale: 20 seconds or less is 100, 30 minutes or more is 0. Time spent waiting for a busy machine never counts, and a job marked *timing contaminated* is left out; a model with no other timed turn has no speed score, and its overall score is made of the other two. |

The weights are adjustable under Benchmarks → ⚙. A model needs at least one benchmark game to get
an overall score, and dry-run games never count.

Clicking a model shows its benchmark games. The page also carries the detailed comparison table —
timing, steps, calls, errors, repeats, tokens, turn endings — across every running and saved game.

### The diplomacy leaderboard

The Models page's **Diplomacy** tab ranks models by what their diplomacy did in hybrid benchmark
pairs, one line per model and owners preset (what the model decided):

| Column | What it measures |
|---|---|
| Pairs | Hybrid jobs with their control games played |
| Score vs control | The hybrid seat's final score minus its control's, averaged, with a 90% bootstrap interval (2,000 resamples, a fixed seed, so a line reads the same every time) |
| Deals / game, mean deal value | The hybrid seat's deals and what its bot makes of them, in gold from the seat's side |
| Accepted | How many of the seat's own proposals the other side accepted |
| Wars declared, captures / losses | Per game |
| Control | The control games' deals per game and their mean value, valued the same way |
| Fallbacks | Chats the fallback bot answered and diplomat calls that did not run because of the model (it timed out or failed), per game; those a benchmark's queue or a refusal caused are listed beside, not counted against it |

Only benchmark pairs count (a lobby game has no control). A member's lines are made from their own
pairs and the pairs of other members' runs they may see whose games pool (the [pooling
rule](server/ACCOUNTS.md#data-pooling)): another member's pairs count only once enough accounts are behind a line,
and are never named. An administrator's lines cover every pair.

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
| Control games of hybrid jobs | `benchmarks/controls/` |
| Game saves | `saves/<game id>/` |
| Per-turn metrics | inside each game's save, and as CSV from the stats screen |
| Usage for costing | `saves/usage/*.jsonl` |

`citar where` prints the actual paths for your installation.

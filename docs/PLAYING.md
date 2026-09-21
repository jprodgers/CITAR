# Playing in the browser

CITAR's client is a single page served by the game server. Everything below works the same whether
the server is on your own machine or somewhere else.

## Starting, resuming and watching

The lobby lists running games and saved ones.

- **Create game** — map, speed, difficulty, victory conditions and seats. Each seat picks a
  civilization or takes a random one; you can rename your civ and your cities afterwards.
  **BenchmarkCiv** has no unique abilities, units or buildings, which is what benchmark seats use
  so that a comparison is about the player and not the civ.
- **Load** on a saved game with a human seat takes you straight in. Games with no LLM seats resume
  automatically; games with them start paused so you can check the model settings first.
- **▶ Play** resumes a running game. **Watch** opens a game with no human seat with full vision.
- **Delete** removes every save of a game.

Games autosave every turn to `saves/<game id>/autosave.citar`. **Save** in the game screen writes a
named save beside it.

### Watching AI games

With no human seat you can watch with full vision, pause, put a delay between AI turns, view the
map as a particular civilization saw it, and read each AI's reasoning as it arrives. This is the
most useful thing in CITAR for understanding *why* a model is losing.

## The map

| Action | How |
|---|---|
| Select a unit | Left-click it |
| Move | With a unit selected, click a tile — hover first to see the route and turn count |
| Attack | Red tiles are targets; the tooltip predicts the damage and the outcome |
| Capture a civilian | Move onto it |
| Pan | Drag |
| Zoom | Scroll |
| Next unit needing orders | `N`, or it is selected automatically after one finishes |

Clicking a city tile selects, in order: units that still need orders, then the city, then units
that have already moved. Whatever else is on the tile appears as one-click chips under "Also here"
or "In city", and a city's name banner always opens the city.

Settlers and work boats show suggested sites as green dashed tiles. Tile tooltips show yields.

### Keys

`N` next unit · `F` fortify · `S` sleep · `Space` skip · `E` explore · `A` automate worker ·
`B` found city · `R` road · `H` heal · `P` pillage · `T` tech tree · `D` diplomacy ·
`C` centre on capital · `Shift+Enter` end turn

## Cities

The production queue can be reordered (▲ ▼) and edited (✕). **Now** puts something at the front,
**+ Queue** adds it to the end, and the gold button buys it outright. Click a tile in the city's
area to lock a citizen onto it (🔒). **Auto-pick production** hands the choice to the built-in
advisor whenever the queue empties.

## What needs attention

The buttons beside **End Turn** jump to the thing that needs you: a city with an empty queue, the
tech tree when research is idle, a city that can bombard an enemy in range, a promotion to pick.

The ⚠ tab in the side panel lists problems — falling gold, unhappiness, threatened cities, starving
cities, civilians in danger — and clicking one takes you there. End Turn warns about idle cities or
empty research before it lets the turn go.

**Empire** in the top bar summarises gold, happiness, science and victory progress, and lists every
city and unit, with obsolete units flagged and a Disband button for cutting upkeep.

## The empire screens

In the top bar: the tech tree, social policies, religion (pantheon, founding, enhancing), great
people, espionage (spies, coups), city-states (gifts, protection, tribute), and victory progress
for every victory type.

**Tech tree** turn estimates include missing prerequisites, so a tech four steps away shows what it
really costs. Clicking a locked tech sets it as a research goal and CITAR fills in the path. The
mouse wheel scrolls the tree sideways.

**Diplomacy** (`D`) is where you send messages, denounce, and open negotiations. The deal builder
covers gold, gold per turn, resources, open borders, embassies, peace, declarations of friendship,
research agreements, defensive pacts, war on a third party, cities, maps and technologies.
One-click suggestions show what each side has that the other wants. AI players answer in seconds;
humans get a popup. United Nations votes are cast here too.

## After the game

**Recap** — available when a game ends, and at any time for AI-only games — scrubs through every
turn: territory, cities and units on the map, score, military and tech graphs, per-turn events,
diplomatic messages, and every AI's recorded reasoning. You can show the map as a particular player
saw it, which is the only honest way to judge a decision made under fog of war.

**📊 AI stats** (top bar, or **Stats** in the lobby) shows live per-seat metrics for any game with
an AI player: turn times, model steps, tool calls, errors, loops, tokens, and how each turn ended.
[BENCHMARKS.md](BENCHMARKS.md#what-the-metrics-mean) explains what each number is for.

## Playing with other people

Start the server with `--host 0.0.0.0` and other people on your network can join with their seat
link. For anything beyond that — accounts, invitations, sharing and budgets — run a server:
[server/DEPLOY.md](server/DEPLOY.md).

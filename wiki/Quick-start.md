# Quick start

Three things, in the order most people want them: a game, a model in a seat, and a benchmark.

If CITAR is not installed yet, [INSTALL.md](Installing) has every route. The short one:

```bash
pipx install "citar[all]"
citar setup
```

---

## 1. A game, with no model at all

```bash
citar
```

The browser opens at <http://127.0.0.1:8765>. That is the lobby.

Under **New game**:

1. Leave the defaults — Standard speed, Prince difficulty, a small continents map.
2. In **Seats**, the first seat is you. Set the other three to **Scripted bot**.
3. Press **Create game**.

You are on the map. A few things worth knowing before you play:

- Left-click a unit, then click where it should go. Hovering shows the route and how many turns it
  takes. Red tiles are attack targets, and the tooltip says what will happen if you attack.
- `N` selects the next unit that needs orders; `Shift+Enter` ends the turn.
- The buttons beside **End Turn** jump to whatever needs attention — a city with nothing in
  production, an empty research slot, a promotion to pick.
- Your first two decisions are where to found the capital (**B** on a good tile, usually right
  where you start) and what to research (`T`).

This is a complete game of Civ V. It ends in science, culture, domination, diplomatic or time
victory, and a 4-player Standard game runs to 500 turns if nobody wins sooner.

**Why start with bots?** Because the bot is the yardstick. Every model score in CITAR is measured
against these opponents, so knowing how they play tells you what a score means.

---

## 2. A model in a seat

### If you have LM Studio or Ollama running

`citar setup` will already have found it. If you installed it since, run `citar setup` again — or
open **Servers** in the top navigation and press **Detect models**.

Then create a game, set a seat to **LLM**, and choose:

- **Server** — the machine the model runs on
- **Model** — which one
- **Load profile** — on LM Studio, the context length and GPU offload to load it with

Press Create. On that civilization's turn the model gets a written briefing, calls tools to move
units and manage cities, and ends its turn. Watch the **AI thoughts** panel to see it think.

A 4B model takes one to two minutes a turn; a 27B model on a consumer GPU takes five to ten. Set
**Reasoning effort** to `low` — for local models it is about four times faster with no loss in play
quality.

### If you have an API key

On the **Servers** page, open **Anthropic API**, paste your key under **Connection → API key**, and
pick a model such as `claude-opus-5`. The key goes to your operating system's credential store —
never to the project folder, a save file or a report.

### If you want Claude Code to play

1. Create a game with a seat set to **MCP client**.
2. Open **Join / Seats** and copy the `claude mcp add …` command for that seat.
3. Run it in a terminal, start `claude`, and say:

   > You are a player in the CITAR game via the citar MCP tools. Call wait_for_turn, and whenever
   > it is your turn, read get_briefing, play the turn well and call end_turn. Answer negotiations
   > with respond_negotiation. Keep your long-term strategy in write_notes. Continue until the game
   > is over.

`wait_for_turn` blocks until it is that seat's turn, so the agent idles rather than polling.

### Nothing is connecting

```bash
citar doctor
```

It checks every model endpoint in your registry and says which are reachable. The usual causes are
LM Studio's local server not switched on, a model that is not loaded, or a base URL pointing at the
wrong port. [TROUBLESHOOTING.md](Troubleshooting) has the rest.

---

## 3. A benchmark

A benchmark plays full games and scores what happened. Open **Benchmarks** in the top navigation.

1. **New suite.** Give it a name.
2. **Servers and models** — tick the models to test. Start with two, and include **Dry run** if you
   want to check the setup without spending GPU time.
3. **Scenarios** — the games each model plays. One is enough to start:
   - Map size **Small**, type **Continents**
   - **2** bot opponents, aggression 0.4
   - Speed **Quick**, turn limit **330** (Quick's time-victory turn)
   - A fixed **seed**, so every model gets the same map
4. **Save**, then **Run**.

Under **Runs** you get every game grouped by server: turn progress, score against the best bot,
turn time, errors, and an estimated finish. Click a game to watch it live.

When it finishes, **Models** ranks what you tested by an overall score — 60% performance, 25%
reliability, 15% speed, with the weights adjustable.

Expect this to take a while. A 330-turn Quick game against a local model is hours, not minutes.
Runs survive restarts: games in progress are reloaded from their autosaves when the server comes
back, so a multi-day benchmark is a normal thing to start.

For a two-minute smoke test instead:

```bash
citar bench --all-models --turns 2
```

---

## Where your files are

```bash
citar where
```

Saved games, the server registry, benchmark runs, reports and the database. In a git checkout they
sit beside the code; installed, they go in your user data directory. Nothing else needs backing up.

---

## Next

- [PLAYING.md](Playing-in-the-browser) — every screen and shortcut in the browser client
- [AI_PLAYERS.md](AI-players) — how a model actually takes a turn, and the guard rails for
  weaker ones
- [BENCHMARKS.md](Benchmarks) — scheduling, scoring, and what the numbers mean
- [SCENARIOS.md](Scenarios-and-probes) — testing one decision instead of a whole game
- [server/DEPLOY.md](Deploying-a-server) — running this for other people


---

*This page is generated from [`docs/QUICKSTART.md`](https://github.com/jprodgers/CITAR/blob/main/docs/QUICKSTART.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*

# Known issues and limitations

What is broken, missing or misleading in 0.1.5. Written plainly, because finding this out after
installing something is worse than reading it first.

---

## The scripted bot expands instead of fighting

**The big one**, because the bot is what every model score is measured against.

The 0.1.4 bot no longer stalls: on a Quick Small map it reaches 12 to 13 cities by turn 300 (0.1.3:
two to five by turn 150), finishes the tech tree and wins by science often enough to see. What it
does not do is fight. It captures about 0.08 cities per game, where the old defaults captured 0.42,
and a bot of middling aggression may play a whole game without declaring war at all.

What it means for a score: a model that beats the bot has beaten a strong builder that will mostly
leave it alone, not an opponent that will punish a weak army. A model that loses badly to it is
genuinely struggling. Unhappiness is still a real limit — bots are unhappy on 41 to 52% of turns.

Work in progress — the campaign log is [docs/research/BOT_TUNING.md](docs/research/BOT_TUNING.md).
Scores are comparable within a release and **not across releases where the bot changed**; the
changelog says when that is.
0.1.5 did not change the bot, but it made barbarians far more aggressive and stopped the event feed
naming civilizations an agent has not met, so 0.1.5 scores are not comparable with 0.1.4 either.

## Models are not very good at this yet

Not a bug, but worth stating. See the [FAQ](docs/FAQ.md#how-strong-are-the-models).

---

## Measurement

**One game is one sample.** Map luck in Civ is enormous. A single benchmark game says very little;
reports give 95% confidence intervals and flag small samples, and the model page does not.

**Turn limits truncate.** A 330-turn game that reaches turn 330 is scored on position. Two models
far apart in strength can finish close on score.

**Speed is hardware.** The 15% speed component of the overall score measures the machine as much as
the model. Reports separate them; the single score does not.

**Energy on helper machines is estimated.** The CITAR helper does not report power draw, so a
machine on the Servers page is costed from the watts entered under Power & costs (idle, and extra
while the model generates). Only the machine CITAR itself runs on is sampled live. A plug-in meter
reading makes the energy and cost-per-task figures far better than the hardware-class guesses.

**Priority takes effect at a safe point.** Work moved to the top of a machine's queue waits for the
running work to reach its next safe point: the end of the model's turn (at most 15 minutes) for a
game or benchmark job, the end of the case for a probe run. A report already being written finishes.

---

## Platforms and packaging

**Windows artifacts are unsigned.** SmartScreen warns about the installer until enough people have
run it, and some antivirus products flag PyInstaller output on sight. SHA-256 hashes are published
for every asset and every build is a public CI run. See
[packaging/README.md](packaging/README.md#code-signing).

**macOS binaries are not notarised**, for the same reason — including the CITAR helper, which
needs `xattr -d com.apple.quarantine` before macOS will run it. The `pip`, `pipx` and Homebrew routes
are unaffected. There is no helper build for Intel Macs; `pipx install "citar[worker]"` and
`citar worker` do the same job there.

**Homebrew, Scoop and winget lag the release** by a day or so — each needs the GitHub release to
exist before its manifest can be updated.

**`install.sh` is verified in CI, not on every distribution.** It is tested on Ubuntu and macOS
runners. On something unusual, read it first; it is short, and `--no-setup` makes it do nothing but
install.

---

## Server

**One core, one game.** Games are CPU-bound and single-threaded per game. A single-core VPS runs
about one concurrent game before turns start dragging. `default_max_concurrent_games` is the lever.

**Backups are local by default.** The nightly timer writes to the same disk, which protects against
mistakes and not against losing the machine. Copying them off-site is documented but not automatic.

**No payments, deliberately.** Budgets are accounting and admission control only. There is no
billing anywhere in CITAR and none is planned.

**Self-hosted mail is not covered.** CITAR sends through an SMTP relay you provide. Running your
own mail server is out of scope.

---

## Game rules

**19 of 402 unique types are unreferenced.** Mostly map-generation region hints. A rule using one
of them is silently inert rather than an error; `scripts/check_uniques.py` lists them.

**Some UnCiv mechanics are simplified.** The ruleset data is faithful; a few interactions between
systems are approximations. Where a difference is known it is noted in the engine module's
docstring.

**Saves do not survive a ruleset version change.** When `RULES_VERSION` changes, old saves will not
load, because the rules they were played under no longer exist. The changelog says when.

---

## Interfaces

**Tool names and arguments may change before 1.0.** `GET /api/tools` is always correct for the
version you are running; an agent that reads the schema rather than hard-coding it will survive
most changes.

**The browser client assumes a recent browser.** ES modules, no transpilation, no polyfills.
Current Chrome, Firefox, Safari and Edge.

**Phones get a check-in site, not the game.** On a phone the server shows games, standings, AI
status, benchmarks, reports and machines, with pause and resume. Playing still wants a bigger
screen: a hex map and a city screen on a phone are not something anyone should endure. Tablets get
the full site.

---

## Reporting something not listed

<https://github.com/jprodgers/CITAR/issues> — with the output of `citar doctor`.

For a security problem, please use
[private reporting](https://github.com/jprodgers/CITAR/security/advisories/new) instead.

# Known issues and limitations

What is broken, missing or misleading in 0.1.6. Written plainly, because finding this out after
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

Work in progress — the campaign log is [docs/research/BOT_TUNING.md](Bot-tuning-log).
Scores are comparable within a release and **not across releases where the bot changed**; the
changelog says when that is.
0.1.5 did not change the bot, but it made barbarians far more aggressive and stopped the event feed
naming civilizations an agent has not met, so 0.1.5 scores are not comparable with 0.1.4 either.

## Models are not very good at this yet

Not a bug, but worth stating. See the [FAQ](Questions#how-strong-are-the-models).

---

## Measurement

**One game is one sample.** Map luck in Civ is enormous. A single benchmark game says very little;
reports give 95% confidence intervals and flag small samples, and the model page does not.

**Turn limits truncate.** A 330-turn game that reaches turn 330 is scored on position. Two models
far apart in strength can finish close on score.

**Speed is hardware.** The 15% speed component of the overall score measures the machine as much as
the model. Reports separate them; the single score does not.

**Energy is estimated where power cannot be read.** The machine CITAR runs on, and every machine whose
CITAR helper runs, sample their power live: the GPUs' draw from `nvidia-smi` and the CPU's utilisation.
On a machine whose GPU's draw its driver does not report (an AMD or Intel GPU, a Mac) only the CPU is
sampled: the GPU's share is estimated from the watts entered under Power & costs (extra while the
model works), and those minutes count as estimated. A machine started with `--no-power`, and one the
server dials itself without a helper, are costed from the entered watts alone (idle, and extra while
the model or the CPU works). Even a sampled machine's idle and CPU figures are the entered ones: a
plug-in meter reading makes the energy and cost-per-task figures far better than the hardware-class
guesses.

**Priority takes effect at a safe point.** Work moved to the top of a machine's queue waits for the
running work to reach its next safe point: the end of the model's turn (at most 15 minutes) for a
game or benchmark job, the end of the case for a probe run. A report already being written finishes.

---

## Platforms and packaging

**Windows artifacts are unsigned.** SmartScreen warns about the installer until enough people have
run it, and some antivirus products flag PyInstaller output on sight. SHA-256 hashes are published
for every asset and every build is a public CI run. See
[packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md#code-signing).

**macOS binaries are not notarised**, for the same reason — including the CITAR helper, which
needs `xattr -d com.apple.quarantine` before macOS will run it. The `pip`, `pipx` and Homebrew routes
are unaffected. There is no helper build for Intel Macs yet (the Python worker that stood in for it
there is gone in 0.1.6); build one from a checkout with `cargo build --release -p citar-helper`.

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

**A restart costs the round in progress.** A game's autosave is taken as each round begins, so a
game the server brings back after a crash, a kill or a power cut resumes at the start of the round
it was in: that round's moves, chats and deals are gone, unless a save was taken during it (a seat
change saves at once). A chat that was in the save is answered again after the restart, with its
time counted afresh.

---

## Shared machines (new in 0.1.6)

**A Mac holds its CPU limits softly.** A helper on macOS plays the lab's games in the background priority band with
as many games as its threads, and a watchdog ends its job host past its memory: the operating system offers no hard CPU
cap to an ordinary program, so the machine settings dialog shows both limits "soft". Lend fewer threads for a tighter
hold.

**A patient cheater can slip some games past the sampling.** A `trusted` machine's results are replayed about one in
fifty, so a machine that alters a result now and then contributes about 50 games on average before a replay catches
it. Every unverified result it gave is withdrawn when it is caught, with its credits.

**A model served on a member's machine cannot be checked.** A helper could serve a smaller model than the one it names;
its results count on the global boards only once an administrator trusts the machine, and over-reported generation
time is flagged, but the model itself is not verified.

**Short games are not timed.** A game of under a fifth of a second of CPU says nothing of a machine's speed (Windows
counts a thread's time in steps of about 16 ms), so a machine that plays only short games has no measured speed and
cannot be flagged as out of line with its class; its CPU is still bounded by the reference.

**A group's visibility shares nothing yet.** A group's machines are listed and lent through its
grants only; making a group visible to members or published lists nothing more.

**A machine in its quiet hours still says "available to you now"** on the Servers page: that line
is about grants, and the quiet hours pause work separately.

**An administrator who makes a pending account active does not mark its email verified.**

**What a game's watchers can read.** Anyone who may watch a game reads each model seat's provider
settings in its metrics (a member's seat names a machine, so for them that is a machine id and a
model; an administrator's inline block shows its address) and the seats' last errors in the game's
details.

**A probe run shared more widely than its scenario** lets those it is shared with open its cases'
end states, which show the scenario's map.

**A plain bot seat in a scenario saved from a running game** keeps no bot profile: it plays the
default bot.

**The full site's top navigation does not fit a phone-sized window** (phones get the check-in
site, which is unaffected).

---

## Game rules

**14 of the 402 unique types the ruleset uses are inert.** Mostly map-generation region hints. A
rule using one of them compiles and does nothing, as it did in the Python engine;
`crates/citar-engine/unique_supported.toml` lists them, each with its reason.

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


---

*This page is generated from [`KNOWN_ISSUES.md`](https://github.com/jprodgers/CITAR/blob/main/KNOWN_ISSUES.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*

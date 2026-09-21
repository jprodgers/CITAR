# Known issues and limitations

What is broken, missing or misleading in 0.1.0. Written plainly, because finding this out after
installing something is worse than reading it first.

---

## The scripted bot stalls at two to five cities

**The big one**, because the bot is what every model score is measured against.

On a Quick Small map the bot reaches two to five cities by about turn 150 and stops expanding. It
is limited by happiness: it will not settle into unhappiness, and it does not push hard enough on
the things that would fix that.

What it means for a score: a model that beats the bot has beaten a competent but self-limiting
opponent, not a good Civ player. A model that loses badly to it is genuinely struggling.

Work in progress — the campaign log is [docs/research/BOT_TUNING.md](Bot-tuning-log).
Scores are comparable within a release and **not across releases where the bot changed**; the
changelog says when that is.

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

---

## Platforms and packaging

**Windows artifacts are unsigned.** SmartScreen warns about the installer until enough people have
run it, and some antivirus products flag PyInstaller output on sight. SHA-256 hashes are published
for every asset and every build is a public CI run. See
[packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md#code-signing).

**macOS binaries are not notarised**, for the same reason. The `pip`, `pipx` and Homebrew routes
are unaffected.

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

**Mobile is not supported.** The layout adapts, but a hex map and a city screen on a phone are not
something anyone should endure.

---

## Reporting something not listed

<https://github.com/jprodgers/CITAR/issues> — with the output of `citar doctor`.

For a security problem, please use
[private reporting](https://github.com/jprodgers/CITAR/security/advisories/new) instead.


---

*This page is generated from [`KNOWN_ISSUES.md`](https://github.com/jprodgers/CITAR/blob/main/KNOWN_ISSUES.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*

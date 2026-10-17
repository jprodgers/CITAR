# Contributing

Thanks for looking. CITAR is a game and an instrument, and it needs help with both.

## Useful things that need no deep knowledge

- **Run a benchmark and report what you find.** Real numbers from hardware that is not mine are the
  most useful thing anyone can contribute. Which model, which hardware, what happened.
- **Write a probe scenario** for a decision you care about — a trade a model should refuse, a war
  it should not start. [docs/SCENARIOS.md](Scenarios-and-probes).
- **Tell us where the documentation is wrong.** Especially if you followed something and it did not
  work.
- **Play a game and report what the AI did that was stupid.** With the game's recap, that is a
  reproducible bug.

## Setting up

```bash
git clone https://github.com/jprodgers/CITAR && cd CITAR
pip install -e ".[dev]"                  # builds the Rust engine too, so it needs a Rust toolchain
python -m unittest discover -s tests     # about 650 tests, a few minutes; about 200 of them skip
citar serve --debug                      # http://127.0.0.1:8765
```

**A Rust toolchain is needed to build from source.** The package includes the Rust engine as the
extension module `citar._engine` (`crates/citar-py`), and installing from a checkout builds it with
maturin. Install [rustup](https://rustup.rs) (on Windows with the MSVC build tools its installer
offers); `rust-toolchain.toml` pins the exact release, and rustup fetches it on first use. An
editable install builds the `ci` profile (release optimisation with debug assertions on), which
takes a few minutes the first time and seconds after a small change; `pip install .` builds the
release profile, which takes longer. Rebuild after a Rust change with the same command. Neither
includes the engine's test operations, so about 200 of the suite's tests skip: every rule script
of `tests/rules/` and the other tests that need them (see [Tests](#tests)). The extension's own dev
loop under [Rust](#rust), `cargo xtask develop`, builds with them, and the whole suite runs with
none skipped; run it that way before you send a change to the rules. Released wheels need no
toolchain.

A checkout keeps its state beside the code — `saves/`, `config/`, `benchmarks/` — rather than in
your user directory, so your test games are where you can see and delete them.

The browser client has no build step. Edit a file in `citar/web/js/` and reload.

## Before you send a change

```bash
python -m unittest discover -s tests
ruff check .
python scripts/audit_routes.py --strict   # if you touched any HTTP route
```

For front-end changes, check the syntax as an **ES module** — `node --check` on a `.js` file parses
it as CommonJS and misses errors that break the whole module graph in a browser:

```bash
cp citar/web/js/thing.js /tmp/thing.mjs && node --check /tmp/thing.mjs
```

The client's pure parts (no network: `citar/web/js/machines.js`, the Servers page's rules; `chatmodel.js`, the chat
window's state; `seats.js`, the seat labels and the hybrid seat's payload; `util.js`'s dialogs and `chat.js`'s windows,
over a few stand-ins for the DOM) have Node tests in
`citar/web/tests/`. Node 22 or later takes the files as a pattern, not a folder:

```bash
node --test "citar/web/tests/*.test.mjs"     # or `npm test` in citar/web
```

CI runs all of this on Linux, Windows and macOS (the front-end checks on Linux).

## Conventions

These are not style preferences; each one exists because breaking it caused a real problem.

- **The engine does no I/O.** `crates/citar-engine` must not read files, open sockets, read the
  clock or touch the database. That property is what makes games serialisable, tests fast and the
  same engine usable from four different interfaces. The bindings (`crates/citar-py`) and the
  facade (`citar/engine_api.py`) do the I/O around it.
- **`citar/auth/access.py` is the only place that answers "may this viewer do this".** Never
  hand-roll a permission check.
- **Refusals are 404 when the caller cannot see the object**, not 403. A 403 confirms it exists and
  turns id-guessing into enumeration.
- **Run the route audit after adding a route.** It found an entirely ungated endpoint once, which
  is why it exists. Routes that are public on purpose are listed in it *with a reason*, so public
  is a decision rather than an omission.
- **Paths come from `citar/paths.py`.** Nothing else computes a directory from `__file__`; that is
  what put saved games inside `site-packages` before.
- **Availability windows are stored as weekday plus minutes in the owner's time zone**, resolved
  late. Never as UTC instants — that breaks twice a year.
- **Write errors that explain the rule.** `"That tile is not adjacent"`, not `"invalid"`. A
  language model reads these, and a model that is told why usually fixes it.

## Comments and docstrings

The codebase is documented at a particular density on purpose: **say why, not what**.

```python
# Bad: increments the counter
# Good: counted per turn rather than per game, because a model that loops for 200 calls
#       in one turn and behaves for the rest is a different problem from one that is
#       slightly repetitive throughout.
```

Every module has a docstring saying what it is for. Public functions have one saying what they do
and anything surprising about it. Internal helpers have one when the name is not enough.

If you find yourself writing "this is a bit odd because…", that sentence is the most valuable one
in the file — keep it.

## Tests

`unittest`, not pytest, run with `python -m unittest discover -s tests`. Every test module starts
with `import tests`, which redirects saves and the server registry to temporary directories — a run
that skips it writes test games into the real `saves/`.

What is worth testing:

- **Rules** — a rule script in `tests/rules/` ([its README](https://github.com/jprodgers/CITAR/blob/main/tests/rules/README.md)), which both the
  Rust harness and the Python suite play, or a Rust test in `crates/citar-testkit`. A rule with no
  test will eventually be broken by a refactor.
- **Permissions** — `tests/test_access.py`. Every new object kind needs its "a stranger gets 404"
  test.
- **Anything that was a bug.** The test is the part that stops it coming back.

Reserved handles (`admin`, `root`, `mod`, `guest`, …) are rejected, so fixtures must use other
names.

**One door to the engine.** Everything reaches the game through `citar/engine_api.py`, over the
extension `citar._engine`, so the suite needs the extension built (see [Rust](#rust)). The tests
that need the engine's test operations (the rule scripts, `EngineGame.inspect` and `test_ops`) skip
in a build without them (`tests.has_test_ops()`): `cargo xtask develop` and CI's test builds have
them, a release wheel does not. The Python engine the Rust one was ported from was removed in 0.1.6
(package 2-12); the tag `python-engine-0.1.6` keeps it, with its own tests and the recorders of
`scripts/refcheck/`.

## Rust

The engine is the Rust workspace in `crates/`.
[crates/citar-engine/README.md](https://github.com/jprodgers/CITAR/blob/main/crates/citar-engine/README.md) has the rules every change to it
follows, and [crates/citar-engine/DESIGN.md](https://github.com/jprodgers/CITAR/blob/main/crates/citar-engine/DESIGN.md) the design.

`rust-toolchain.toml` pins the exact toolchain, and rustup installs it on first use. Then:

```bash
cargo nextest run --workspace --exclude citar-py    # tests (cargo install cargo-nextest)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask check                                   # layering, dependencies, version, and more
cargo golden check                                  # determinism goldens
cargo fmt --all
```

These are what CI runs, on Linux, Windows and macOS. `citar-py` (the Python extension) has no
Rust tests, since Python tests it, so the tests leave it out; clippy and the docs cover it, and
building it needs a Python 3.11 or later on the `PATH` for PyO3. It does not need Python's
development package: an extension never links libpython, and `.cargo/config.toml` sets
`PYO3_BUILD_EXTENSION_MODULE` (as maturin does) so that a plain `cargo build` builds it that way
on every OS. CI also lints the engine
alone in each feature set, because the workspace build turns on features the shipped engine
does not have (testkit enables `legacy` and `test-ops`). When you change what a feature gates, run
`cargo clippy -p citar-engine --all-targets -- -D warnings` and again with
`--no-default-features`. Two more tools join the loop: `cargo refcheck run` (the engine's answers
against the Python engine's, recorded on the committed fixtures; `refcheck/README.md`) and `cargo
xtask perf` (benchmarks against their budgets).

**Goldens.** `crates/citar-testkit/golden/` holds answers that must come out identical on all five
targets; the determinism workflow checks them on each. When a change is meant to move them (a new
RNG `Purpose`, a `libm` or toolchain bump), run `cargo golden bless` and say why in the commit.
`pyfmt.json` is Python's own answers, and the hex-grid answers (`crates/citar-testkit/data/`) the
Python engine's, recorded by scripts the tag `python-engine-0.1.6` keeps: no bless writes them. `long.json`, whole
games on every map size, is the nightly run's: `cargo golden check --long` checks it (about a
minute) and `cargo golden bless long` writes it, so bless it too when a change moves the games.

**The jobs golden set.** `crates/citar-jobs/golden/jobs.json` holds four short CPU jobs (lab,
ranking and reference games) with their specs, results and chains of round digests, which a
helper and the server must compute byte for byte alike. citar-jobs' own test binary checks it,
in both profiles: `cargo nextest run -p citar-jobs` and `cargo nextest run -p citar-jobs
--cargo-profile release` (the determinism workflow does both on all five targets). A result
names the build that played it (`engine`, each seat's `build` and `fingerprint`, a reference
game's `bot`), which moves with any edit of the engine's or the bot's sources and with CITAR's
version; the check gives those keys this build's values, so such an edit needs no bless. A change
to the engine, the bot, the runner or how a job's result is built that moves what a job computes
re-blesses it: `CITAR_BLESS_JOBS=1 cargo test -p citar-jobs --test golden -- bless`, and say why
in the commit. Its shortest job is also the self-check a job host plays before it takes work.

**The lab's parity games.** `tests/data/lab_results_*.json` are seven lab games as the Python
result building played them before the lab moved to the jobs crate; `tests/test_jobs.py` holds
`lab.play` to them while the build is the one that recorded them, and to the old building kept in
`tests/lab_play_oracle.py`, played live, on every build. To record them again under the build at
hand (after a change that moves the games, for the exact comparison), run `python -m
tests.lab_results --force` from the repository's root with the extension built, and commit the
files saying why.

When the targets disagree, the determinism workflow's `divergence` artifact holds each odd
target's state at the first round where a game parted from the committed file, the same state
played on Linux, and the places where they differ. To look into one by hand:

```bash
cargo golden check --states states/                 # a game that differs leaves its state there
                                                    # (each check clears the last one's list)
cargo golden dump random:random-duel-continents-s101 57 --out here.json   # the same game, here
cargo golden diff here.json states/random--random-duel-continents-s101--t57.json
cargo golden dump --list                             # every game dump plays
```

**Long runs.** `nightly.yml` runs the properties at 10,000 cases (P8 also with bots driving every
seat), 20 minutes of chaos and a short soak on each OS, the same again with the bots in the seats,
the long golden set on every target and, on Sundays, the benchmarks. Its schedule
and a manual run need the workflow on the default branch; elsewhere, label a pull request `nightly`
to run it there (`gh label create nightly` once; take the label off and add it again to run it
again). A soak its time budget stops before its last game fails (exit 3). Locally:

```bash
cargo chaos --seconds 600                           # random games with tool calls of every kind
cargo soak --games 12                               # whole games on every map size, checked
cargo chaos --drivers mixed --seconds 600           # the same with basic-1 in half the seats
cargo soak --drivers bot --games 12                 # whole bot games, every check on
PROPTEST_CASES=10000 cargo nextest run -p citar-testkit --test props --cargo-profile ci --profile nightly
```

`--drivers` seats `RandomAgent`s (`random`, the default), `basic-1` bots (`bot`) or half of each
(`mixed`) in the majors' seats; a game is the same game whatever its drivers. With bots the soak
also reports their actions taken and refused, and fails a game in which a bot loops on a refused
action.

The laptop soak is 200 games, 33 or 34 of each map size, each to its 330-turn limit: about 40
minutes on one core, so split it over a few processes with `--shard K/N` (each takes the laps of
the six sizes whose number is K modulo N: the same games whatever the split, every size in each
shard). A failure names the game, and `cargo soak --seed S --game N` plays it again alone.

```bash
for k in 0 1 2 3; do cargo soak --games 200 --shard $k/4 --json soak-$k.json > soak-$k.log & done; wait
```

The corpus checks that go with them are in `refcheck/README.md` ("The nightly run and the
corpus"). Before a release, the bots' soak runs at full size too: 1,000 small bot games in six
shards (`--sizes small --games 1000 --drivers bot`, about 15 minutes on the laptop) and 100 larger
ones (40 standard, 30 large, 20 huge, 10 gargantuan, each size a run of its own with `--sizes`, the
larger sizes in shards: about 10 minutes side by side).

**The server soak** plays one lobby game the way people do, on a dev server of its own: a model
seat and eight bots on a Standard map, played to the end through the MCP bridge, with a chat the
model opens with a bot, a restart that kills the server mid-round (the game must come back from its
autosave at most a round behind), every bot turn broadcast, the replay at the end and the time each
save held the game's lock. It takes a few minutes and empties the directory it is given, which must
be new, empty or one an earlier soak left (it refuses any other, and the dev server's port 8799):

```bash
python scripts/server_soak.py C:/dev/server-soak --json server-soak.json    # a free port, its own state
```

**The Phase 3 QA script** checks each feature of the accounts, queue, ledger, chat, hybrid, minimap
and machine work end to end, over HTTP and the game socket, on a server-mode dev server of its own
(design/phase3-features.md P3.9.4). It makes an administrator, two members and a moderator (and a
third member for `all` and `exit`) with `citar admin create-user`, signs them in as a browser would
(a cookie and the CSRF token), serves fake OpenAI-compatible endpoints that record every connection
they get and answer from a scripted policy that plays (tool calls read from the request: a turn's
orders, a chat's answer, a hybrid seat's diplomat phase; streamed as server-sent events when asked,
recording when the client went), starts real helpers (the Rust `citar-helper` that `cargo xtask
develop` builds, or `--helper-bin PATH`, each with `--home` in the QA directory) in front of them for
helper machines, runs the part's checks, and writes
each check and its numbers to `<dir>/qa-<part>.json` (with the throwaway accounts' password, for the
browser checklist). It exits 2, touching nothing, for the dev server's port 8799, a port in use, a
directory holding anything without its `.qa-phase3` marker, the owner's `C:/dev/citar-play`, CITAR's
own data directory or a source checkout. Once it has taken the directory it never refuses: it exits 0
when every check passed and 1 when one failed, a directory it could not empty, a server that did not
start, an account it could not make and a part that stopped on an error each counting as a failed
check, with the evidence written all the same.

The parts: `accounts`, `queue`, `ledger`, `chat`, `hybrid`, `minimap` and `machines` (one per
feature), `exit` (Phase 3 end to end: one game of six kinds of seat for 40 rounds on a helper
machine shared between three members, with pooled work, a benchmark beside it, a helper restart
and a server killed mid-chat) and `real` (the same plumbing against a real model, below). `all`
runs every part in that order on one server, raising the members' game limit since each part leaves
its games open; its `qa-all.json` lists every check under the part that made it, with each part's
count and time at its top (`parts`), and it writes each part's own `qa-<part>.json` beside it, as a
run of that part alone would, for the browser checklist's scripts.

```bash
python scripts/qa_phase3.py accounts --dir C:/dev/qa-phase3          # about a minute; a free port
python scripts/qa_phase3.py accounts --dir C:/dev/qa-phase3 --keep   # leave the server up for the browser
python scripts/qa_phase3.py all --dir C:/dev/qa-phase3               # everything, the real model last
```

`--keep` leaves the server running at the address it prints (the public origin, `localhost`), for the
browser checklist of P3.9.5; stop it when you are done.

**The `real` part** plays a hybrid seat and a full LLM seat on a real model for 20 rounds beside a
person seat and two bots, with a member's report analysis on the same machine: a helper machine
whose real helper's upstream is LM Studio at `http://localhost:1234/v1` serving
`google/gemma-4-e2b`. Load that model in LM Studio first: the part checks that LM Studio lists it
loaded (`GET /api/v0/models`) and fails with what to do otherwise, and it never loads, unloads or
swaps a model, changes LM Studio's settings or names another model. A small model on a laptop takes
tens of minutes for the 20 rounds; the part tests the plumbing, not the play, and records each model
turn and how it ended, the tool calls made and refused, each chat's answer and its time, every
fallback to the bot with its cause, and the generation seconds metered against the compute ledger.

**The Phase 4 QA script** (`scripts/qa_phase4.py`, design/phase4-helper.md P4.7.6) runs on the same
harness, imported rather than copied, with the same refusals and evidence. Its parts so far:
`helper` (a helper machine end to end with the real `citar-helper`: its key and card, settings, a
seat playing through it, a cancel stopped at the endpoint, a 0.1.5 worker refused, a key reset, the
rate limit and a revoke) and `llm-real` (the helper's real-model checks through the hub and the
scheduler: with `google/gemma-4-e2b` loaded in LM Studio, a request with tools parsed on the server,
a long generation cancelled at 2 s acknowledged within a second, and the requests after it no
slower than those before). `llm-real` reads LM Studio's model list before and after, never loads,
unloads or swaps a model, and sends nothing when the model is not loaded. `--stand-in` plays LM
Studio with the scripted endpoint to check the part itself, and `tests/test_helper_llm_real.py` runs
the same checks in every test run against stand-ins: they pass against one that stops a cancelled
generation, and fail against one that goes on generating after the cancel, one whose generation is
over before it, a model that is not loaded (with nothing sent to it) and one unloaded during the run.
`scan` runs a helper that scans and measures, with `citar-fake-probe` as its `nvidia-smi` (below): its
hello's scan as the machine's last report, Rescan now within 5 s, a minute of samples as one power
row, costing measuring the machine's energy from it, every reading a call in the fake's log, and a
`--no-hardware --no-power` helper sending neither. `lab` plays the lab inside the QA server on its own job host
(the host's CPU set to two threads, two cores' worth at most and 400 MB): a member's six duels to their end with
their lines, events, ratings and `GET /api/lab/jobs`; an administrator's ranking and reference runs through
`citar lab submit`; Stop and Resume; the server killed twice with two games playing (each seat step slowed by the
test build's `CITAR_JOBS_TEST_STEP_MS`), playing exactly those two again; the Queue page's CPU card.

```bash
python scripts/qa_phase4.py helper --dir C:/dev/qa-phase4             # about 20 s; a free port
python scripts/qa_phase4.py llm-real --dir C:/dev/qa-phase4           # the model loaded in LM Studio first
python scripts/qa_phase4.py llm-real --stand-in --dir C:/dev/qa-phase4
python scripts/qa_phase4.py scan --dir C:/dev/qa-phase4               # about 80 s: a minute of samples
python scripts/qa_phase4.py lab --dir C:/dev/qa-phase4                # about 60 s
```

**The suite's servers play no lab.** `tests/__init__.py` sets `CITAR_TEST_NO_DISPATCHER`, so an app a test
module starts runs no job dispatcher (the modules share one saves folder, whose lab queue may hold another test's
experiment). `tests/test_dispatcher.py` makes and ticks its own, with small limits; the QA script clears the
switch for its server.

**Build outside synced folders.** A `target/` directory inside OneDrive (or Dropbox, or iCloud)
fails with "os error 32" when the sync client locks a file mid-build, and uploads gigabytes of
build output. Point `CARGO_TARGET_DIR` somewhere else, one directory per checkout or worktree so
parallel builds of different branches do not thrash each other:

```bash
export CARGO_TARGET_DIR=C:/dev/target/citar-main          # Git Bash; a Dev Drive is faster still
$env:CARGO_TARGET_DIR = "C:\dev\target\citar-main"        # PowerShell
```

**The Python extension: the dev loop.** `citar._engine` (`crates/citar-py`) is the Rust engine
as Python sees it. Build it with `cargo xtask develop`, not `maturin develop`, which would write
the library into the checkout: inside a synced folder that fails mid-build, and a running server
holds the file open. Give each checkout or worktree its own venv, outside the checkout:

```bash
python -m venv C:/dev/venv/citar-main                     # once; --system-site-packages reuses what is installed
source C:/dev/venv/citar-main/Scripts/activate            # Git Bash; bin/activate on Linux and macOS
export CARGO_TARGET_DIR=C:/dev/target/citar-main
cargo xtask develop                                       # the ci profile with the test operations
cargo xtask develop --release                             # the release profile, for timing
python -m unittest discover -s tests                      # imports the extension just built
```

`develop` refuses a venv another worktree already uses (its `citar-dev.pth` names that worktree),
installs the project's dependencies into the venv whenever `pyproject.toml` has changed, builds
`citar-py` into `$CARGO_TARGET_DIR/develop` with the build label from `git describe` (of the nearest
`v*` tag), copies the
library to `$CARGO_TARGET_DIR/citar-ext/` (and the helper beside it, below), and writes `citar-dev.pth` into the venv: the checkout
on `sys.path` and `CITAR_EXT_DIR`, which `citar/__init__.py` puts first on the package's path. It
writes nothing under the checkout. Rebuild after every Rust change you want Python to see. On
Windows, stop the dev server and the lab before a rebuild: a library a running process holds is
renamed aside rather than replaced, and the process keeps the old engine until it restarts.
`tests/test_engine_module.py` tests the extension itself and is skipped when it is not built.
`pip install -e .` builds the extension too, into `citar/` and without the test operations: fine
on Linux, macOS and in CI, and the thing to avoid in a synced folder.

**The extension in CI.** `pyproject.toml` builds the package with maturin (`[tool.maturin]`), and
its feature list never names `test-ops`: `cargo xtask check` refuses one that would turn on the
test operations or `legacy`, so no wheel carries them. `test.yml`'s build-ext job builds a test
wheel per OS (the ci profile with `--features test-ops`); each test job installs only the
dependencies (`scripts/ci/requirements.py all`) and unpacks the wheel's library into the checkout
(`scripts/ci/unpack_ext.py`, which fails unless it imports with the test operations), and the
package job builds the release wheel as it would ship. Every wheel and the source distribution are
built with one maturin, `MATURIN_VERSION` at the top of `test.yml`, and `scripts/ci/check_dist.py`
holds each to the checkout: the files git tracks under `citar/`, the library, and nothing stray,
and in the source distribution the ruleset the engine compiles in (`crates/citar-engine/data/`).
After changing `[tool.maturin]`'s `include` or `exclude`, or moving that pin, run it on a local
build (`python scripts/ci/check_dist.py wheel DIR`, or `sdist DIR`). To run the suite as a test job
does:

```bash
maturin build --profile ci --features test-ops --out "$CARGO_TARGET_DIR/wheels"
python scripts/ci/unpack_ext.py "$CARGO_TARGET_DIR/wheels" --test-ops   # citar/_engine.pyd or .abi3.so
python -m unittest discover -s tests
```

The unpacked library is ignored by git. Delete it before going back to the dev loop, whose tests
check that nothing was built into the checkout.

**The helper.** `citar-helper` (`crates/citar-helper`) is the program a lender runs; its protocol
is `crates/citar-helper/PROTOCOL.md`, and `design/phase4-helper.md` P4.3 the design. Every build a
contributor makes for running should have the development guard:

```bash
cargo build -p citar-helper --features dev-guard          # or cargo xtask develop, which puts it in citar-ext/
"$CARGO_TARGET_DIR/debug/citar-helper" --home C:/dev/scratch/helper-1 --server http://127.0.0.1:8000 --token ...
cargo nextest run -p citar-helper -p citar-jobs --features citar-helper/dev-guard
```

A `dev-guard` build, and any build with one of the helper's test features (`test-faults`,
`test-update-key`) even without it, dials loopback servers only and refuses to start without an
explicit `--home` outside the per-user CITAR folder, so a forgotten flag never writes a real
installation's `helper.json` or machine key, and no test build can reach a real server. `cargo xtask develop` always builds the helper that way, in its own
profile, next to the extension, where the end-to-end tests and the QA scripts look for it. Give
every helper you run its own `--home` (and the same in `CITAR_DATA_DIR`), and never point one at a
server that is not your own. The crate's tests play the shared transcripts of
`crates/citar-helper/protocol/v2/transcripts/` against the helper's own session
(`tests/conformance.rs`: on a paused clock, and those that run a completion in real time against a
fake model endpoint over HTTP on loopback, which streams with delays and records when its client
went), hold the session's own rules beyond them (`tests/session.rs`: a hostile server's frames, a
flaky endpoint, quiet hours all week, a hardware file that never answers) and the proxy's
(`tests/proxy.rs`: a cancel closing the endpoint's connection within a second, the parameters that
reach it, its errors, the slots, `localhost` at once, a finished answer's connection kept for the
next), run the real binary against a fake hub, a fake LM Studio and the
fake endpoint on loopback (`tests/real_binary.rs`, about 35 s), and check the machine key's file
permissions (`tests/keys.rs`); `tests/config_cases.rs` holds its settings to the table recorded
from the 0.1.5 helper (`tests/data/helper_config_cases.json`). The hardware scan and the power
samples (`src/scan`, `src/power.rs`) run the machine's own probe programs; a development or test build
runs instead the ones `CITAR_HELPER_NVIDIA_SMI`, `CITAR_HELPER_POWERSHELL` and
`CITAR_HELPER_SYSTEM_PROFILER` name (a release build never reads them), and `citar-fake-probe`, a
second binary of the crate that only `dev-guard` builds make (`cargo xtask develop` puts it beside the
helper), stands in for any of them: it prints fixed answers (one 24 GB GPU drawing 123.45 W) and logs
each call beside itself (or where `CITAR_FAKE_PROBE_LOG` says). A fake first on the `PATH` would not
do, since Windows looks in System32, where the NVIDIA driver puts the real `nvidia-smi.exe`, before
the `PATH`. `tests/scan.rs` holds the scan's and the samples' rules on the paused clock and against
the fake, and `tests/power_guess.rs` the power guess to the Python collector's answers on twelve
recorded outputs (`tests/data/hwinfo_power_cases.json`); `citar-helper scan` prints what a scan
holds. One test talks to a real model, and
only when asked (an ignored test, which other runs report as skipped): with LM Studio on this
machine already serving `google/gemma-4-e2b`, `CITAR_TEST_LMSTUDIO=http://localhost:1234 cargo
nextest run -p citar-helper --features citar-helper/dev-guard --run-ignored only -E
'binary(real_model)' --no-capture` sends it a request with a tool and cancels a long generation
through the real binary. It reads LM Studio's model list before and after, never loads, unloads or
swaps a model, and sends nothing when the model is not loaded. Its checks also run in every run
against a stand-in for LM Studio (the fake endpoint with one model answering one request at a
time): they pass there, and fail against a server that goes on generating after a cancel, a model
that is not loaded and one unloaded during the run. `citar-jobs` holds the one module of the helper
crates allowed `unsafe` (`src/sys/`), the operating-system calls no safe wrapper makes; `cargo
xtask check` refuses `unsafe` anywhere else in them, and their test features and `dev-guard` in
anything that ships.

**The helper's updates** (`src/update`, design/phase4-helper.md P4.3.8) are tested with two builds of
one tree that `cargo xtask develop` puts beside the helper: `citar-helper-update-1` and `-2`, built
with the `test-update-key` feature and `CITAR_HELPER_VERSION` at compile time (`0.1.6-test.1`,
`0.1.6-test.2`; the second also `CITAR_HELPER_JOBS_SALT`, so it is another jobs build, as another
release would be). Such a build trusts the update key `--update-key FILE` names, besides the ones
compiled in (`crates/citar-jobs/update-keys.pub`, empty until the first signed release), and has
three test hooks: `CITAR_HELPER_TEST_IDLE_WAIT_S` (the hand-off's wait for idle, an hour),
`CITAR_HELPER_TEST_EXIT_AT_START=<version>` (that version exits at its start, before it can be
healthy) and `CITAR_HELPER_TEST_STALE_HOST` (the binary an update installed starts its first job
host from the one it replaced). `tests/update.rs` plays the refusals in-process against a loopback
asset server, and the start count and `update rollback` with the real binary;
`tests/test_helper_update_e2e.py` updates, rolls back and hands off the two builds against the real
hub, and `scripts/qa_phase4.py update` does it with the release side's own tools: `cargo xtask
keygen --out DIR` (a throwaway key, refused inside any git working tree), `cargo xtask
helper-manifest --assets DIR --version V` and `cargo xtask sign-manifest --key FILE MANIFEST`.
Never put a key file in the checkout: `tests/test_no_private_keys.py` fails on one.

**Building in WSL:** clone the repository into your Linux home directory (`~/`), not under
`/mnt/c`, where every file access crosses the Windows boundary and builds crawl. Instruction-count
benchmarks need valgrind, so they run there too, and so do the helper's Linux checks (`cargo
nextest run -p citar-helper -p citar-jobs`, which builds ring's C and assembly: the Linux side
needs a C compiler, `CC` pointing at one when `cc` is not on the `PATH`). A clone made from a
`git bundle` of your branch keeps the Windows checkout out of it entirely.

**On a busy machine**, `CARGO_BUILD_JOBS=4` keeps a build from starving everything else, and
wall-clock benchmark numbers are only indicative.

## Pull requests

- One change per pull request.
- Say what you tried that did not work. It saves the reviewer from suggesting it.
- If it changes behaviour somebody depends on, say so — it belongs in the changelog.
- Draft pull requests are welcome for "is this the right idea?".

## Adding content

Most content is data, not code: a new building or unit is a JSON entry.
[docs/MODDING.md](Modding) covers it, including how to check that the rule text you wrote
is one the engine understands.

Documentation lives in `docs/` and nowhere else. The [wiki](https://github.com/jprodgers/CITAR/wiki)
and the [site](https://jprodgers.github.io/CITAR) are both generated from it, so editing a wiki page
directly only means losing that edit on the next build. The site publishes itself when `docs/`
changes; the wiki needs a push:

```
python scripts/build_wiki.py --push
```

That uses your own git credentials. The docs workflow can do it instead, but only with a `WIKI_TOKEN`
secret holding a **classic** personal access token with the `repo` scope — `GITHUB_TOKEN` cannot
write to a wiki, and fine-grained tokens have no wiki permission to grant.

## Releasing

For maintainers: bump `citar/__init__.py` and `[workspace.package] version` in `Cargo.toml`
together (`cargo xtask check` fails if they differ), add the section to `CHANGELOG.md`, tag
`vX.Y.Z` and push. CI checks that the tag, the source version and the changelog agree, then
builds and publishes everything. [packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md) covers the manifests
that need updating afterwards.

## Code of conduct

[CODE_OF_CONDUCT.md](https://github.com/jprodgers/CITAR/blob/main/CODE_OF_CONDUCT.md). Short version: be decent, assume good faith, and
criticise the work rather than the person.

## Licence

CITAR is MPL-2.0. By contributing you agree that your contribution is licensed the same way. There
is no CLA.


---

*This page is generated from [`CONTRIBUTING.md`](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*

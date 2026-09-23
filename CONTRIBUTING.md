# Contributing

Thanks for looking. CITAR is a game and an instrument, and it needs help with both.

## Useful things that need no deep knowledge

- **Run a benchmark and report what you find.** Real numbers from hardware that is not mine are the
  most useful thing anyone can contribute. Which model, which hardware, what happened.
- **Write a probe scenario** for a decision you care about — a trade a model should refuse, a war
  it should not start. [docs/SCENARIOS.md](docs/SCENARIOS.md).
- **Tell us where the documentation is wrong.** Especially if you followed something and it did not
  work.
- **Play a game and report what the AI did that was stupid.** With the game's recap, that is a
  reproducible bug.

## Setting up

```bash
git clone https://github.com/jprodgers/CITAR && cd CITAR
pip install -e ".[dev]"
python -m unittest discover -s tests     # 456 tests, about four minutes
citar serve --debug                      # http://127.0.0.1:8765
```

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

CI runs all of this on Linux, Windows and macOS.

## Conventions

These are not style preferences; each one exists because breaking it caused a real problem.

- **The engine does no I/O.** `citar/engine/` must not read files at runtime, open sockets or touch
  the database. That property is what makes games serialisable, tests fast and the same engine
  usable from four different interfaces.
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

- **Rules** — `tests/test_mechanics.py`. A rule with no test will eventually be broken by a
  refactor.
- **Permissions** — `tests/test_access.py`. Every new object kind needs its "a stranger gets 404"
  test.
- **Anything that was a bug.** The test is the part that stops it coming back.

Reserved handles (`admin`, `root`, `mod`, `guest`, …) are rejected, so fixtures must use other
names.

## Rust

The Rust engine is being built in `crates/` to replace `citar/engine/` in 0.1.6.
[crates/citar-engine/README.md](crates/citar-engine/README.md) has the rules every change to it
follows, and [crates/citar-engine/DESIGN.md](crates/citar-engine/DESIGN.md) the design.

`rust-toolchain.toml` pins the exact toolchain, and rustup installs it on first use. Then:

```bash
cargo nextest run                                   # tests (cargo install cargo-nextest)
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask check                                   # layering, dependencies, version, and more
cargo fmt --all
```

These are what CI runs, on Linux, Windows and macOS. The later tools join the loop as they land:
`cargo golden check` (determinism goldens), `cargo refcheck run --fixtures refcheck/fixtures-mini`
(answers compared with the Python engine) and `cargo xtask perf` (benchmarks against their
budgets).

**Build outside synced folders.** A `target/` directory inside OneDrive (or Dropbox, or iCloud)
fails with "os error 32" when the sync client locks a file mid-build, and uploads gigabytes of
build output. Point `CARGO_TARGET_DIR` somewhere else, one directory per checkout or worktree so
parallel builds of different branches do not thrash each other:

```bash
export CARGO_TARGET_DIR=C:/dev/target/citar-main          # Git Bash; a Dev Drive is faster still
$env:CARGO_TARGET_DIR = "C:\dev\target\citar-main"        # PowerShell
```

**Building in WSL:** clone the repository into your Linux home directory (`~/`), not under
`/mnt/c`, where every file access crosses the Windows boundary and builds crawl. Instruction-count
benchmarks need valgrind, so they run there too.

**On a busy machine**, `CARGO_BUILD_JOBS=4` keeps a build from starving everything else, and
wall-clock benchmark numbers are only indicative.

## Pull requests

- One change per pull request.
- Say what you tried that did not work. It saves the reviewer from suggesting it.
- If it changes behaviour somebody depends on, say so — it belongs in the changelog.
- Draft pull requests are welcome for "is this the right idea?".

## Adding content

Most content is data, not code: a new building or unit is a JSON entry.
[docs/MODDING.md](docs/MODDING.md) covers it, including how to check that the rule text you wrote
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
builds and publishes everything. [packaging/README.md](packaging/README.md) covers the manifests
that need updating afterwards.

## Code of conduct

[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Short version: be decent, assume good faith, and
criticise the work rather than the person.

## Licence

CITAR is MPL-2.0. By contributing you agree that your contribution is licensed the same way. There
is no CLA.

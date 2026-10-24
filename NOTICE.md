# Notices and attribution

CITAR is licensed under the **Mozilla Public License, version 2.0**. The full text is in
[LICENSE](LICENSE); a copy is also at <https://mozilla.org/MPL/2.0/>.

## Why MPL-2.0

CITAR's rules, numbers and a substantial part of its game logic are derived from
**[UnCiv](https://github.com/yairm210/Unciv)** (© Yair Morgenstern and contributors), which is
licensed under the MPL-2.0. The MPL is a file-level copyleft: a file that contains MPL-covered code
stays MPL-covered, whoever edits it. Rather than draw a fuzzy line through an engine that was ported
module by module, the whole project uses one licence.

In practice this means:

- You may use CITAR, including commercially, and combine it with code under other licences.
- If you modify a CITAR source file and distribute the result, that file's source must be made
  available under the MPL-2.0.
- Code you write in separate files, linking to CITAR, stays yours under whatever licence you choose.

## What came from UnCiv

| Part | Relationship to UnCiv |
|---|---|
| `crates/citar-engine/data/ruleset/` | Generated from UnCiv's "Civ V - Gods & Kings" ruleset by `scripts/import_unciv.py`. Numeric values and rule ("unique") text only. |
| `crates/citar-engine/src/` | Ported logic: a port of CITAR's Python engine of 0.1.5 (the tag `python-engine-0.1.6` keeps it), which ported UnCiv's. Each Python module's docstring named the UnCiv classes it derived from — for example `cities.py` ported `City`, `CityStats`, `CityPopulationManager` and `CityConstructions` — and each Rust module names the Python lines it replaces. |
| `crates/citar-engine/unique_types.tsv` | Generated from UnCiv's `UniqueType` enum by `scripts/gen_unique_types.py`. |

**Not** taken from UnCiv: graphics, sounds, music, fonts, civilopedia articles, leader dialogue,
quotations, tutorials and all other flavour text. CITAR ships no UnCiv art assets of any kind.

`crates/citar-engine/data/custom/` holds CITAR's own additions (such as `BenchmarkCiv`) in the same
format.

To move to a newer UnCiv release, point `scripts/import_unciv.py` at a checkout of it and regenerate
`crates/citar-engine/data/ruleset/`. See [docs/MODDING.md](docs/MODDING.md).

## Trademarks

*Sid Meier's Civilization* is a trademark of Take-Two Interactive Software, Inc. CITAR is an
independent project. It is **not** affiliated with, endorsed by, or sponsored by Take-Two, 2K,
Firaxis Games, or the UnCiv project. "Civ Inspired Tool for AI Research" describes what the software
is inspired by; no claim is made to any mark.

CITAR contains no assets from any commercial Civilization title, and you do not need to own one to
use it.

## Third-party dependencies

**The Rust crates in the binaries.** CITAR's Python extension (`citar._engine`, built from
`crates/citar-py`) and its helper (`citar-helper`) statically link about 180 third-party Rust
crates, among them tokio, rustls and ring, reqwest and hyper, serde, PyO3, clap, sysinfo and the
Windows crates, each under its own permissive licence: most under MIT or Apache-2.0, the others
under BSD-3-Clause, ISC or Unicode-3.0 (the file names each crate's). Their notices travel with
every binary in [`packaging/THIRD-PARTY-LICENSES.txt`](packaging/THIRD-PARTY-LICENSES.txt): each
crate the two link on CITAR's release targets, with its version, its licence and the licence and
notice texts its package ships, those of code it bundles included. Every wheel carries the file in
its `.dist-info/licenses/`, every packaged helper archive (the Homebrew, Scoop and winget builds)
beside the binary, and each release as an asset of its own.

`cargo xtask licenses` writes the file from `Cargo.lock` and the crates' published sources, and
refuses any crate whose licence is outside an allow-list of permissive licences: MIT, Apache-2.0
(with or without its LLVM exception), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0,
Unicode-DFS-2016, MPL-2.0, CC0-1.0, BSL-1.0, 0BSD and CDLA-Permissive-2.0. CI fails when the lock
changes without the file.

**The Python packages.** CITAR depends on the packages listed in [`pyproject.toml`](pyproject.toml),
each under its own licence: notably FastAPI, Starlette and uvicorn (MIT/BSD), SQLAlchemy (MIT),
Alembic (MIT), Authlib (BSD), argon2-cffi (MIT), httpx (BSD), and the official `anthropic`,
`openai` and `mcp` clients (MIT). pip installs each with its own licence files; run `pip-licenses`
in an installed environment for the exact set and versions you have. The Windows bundle, which
carries the Python packages it runs on, lists them with their licences in `PYTHON-LICENSES.txt`.

No dependency's source is vendored into this repository: the crates come from crates.io, pinned
by `Cargo.lock`, and the Python packages from PyPI.

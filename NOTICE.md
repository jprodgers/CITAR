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
| `citar/data/ruleset/` | Generated from UnCiv's "Civ V - Gods & Kings" ruleset by `scripts/import_unciv.py`. Numeric values and rule ("unique") text only. |
| `citar/engine/` | Ported logic. Each module's docstring names the UnCiv classes it derives from — for example `cities.py` ports `City`, `CityStats`, `CityPopulationManager` and `CityConstructions`. |
| `citar/engine/unique_types.py` | Generated from UnCiv's `UniqueType` enum by `scripts/gen_unique_types.py`. |

**Not** taken from UnCiv: graphics, sounds, music, fonts, civilopedia articles, leader dialogue,
quotations, tutorials and all other flavour text. CITAR ships no UnCiv art assets of any kind.

`citar/data/custom/` holds CITAR's own additions (such as `BenchmarkCiv`) in the same format.

To move to a newer UnCiv release, point `scripts/import_unciv.py` at a checkout of it and regenerate
`citar/data/ruleset/`. See [docs/MODDING.md](docs/MODDING.md).

## Trademarks

*Sid Meier's Civilization* is a trademark of Take-Two Interactive Software, Inc. CITAR is an
independent project. It is **not** affiliated with, endorsed by, or sponsored by Take-Two, 2K,
Firaxis Games, or the UnCiv project. "Civ Inspired Tool for AI Research" describes what the software
is inspired by; no claim is made to any mark.

CITAR contains no assets from any commercial Civilization title, and you do not need to own one to
use it.

## Third-party dependencies

CITAR depends on the packages listed in [`pyproject.toml`](pyproject.toml), each under its own
licence — notably FastAPI, Starlette and uvicorn (MIT/BSD), SQLAlchemy (MIT), Alembic (MIT), Authlib
(BSD), argon2-cffi (MIT), httpx (BSD), and the official `anthropic`, `openai` and `mcp` clients
(MIT). Run `pip-licenses` in an installed environment for the exact set and versions you have.

No dependency is vendored into this repository.

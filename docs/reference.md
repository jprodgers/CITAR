# Python reference

Generated from the source. The prose explanation of how the pieces fit is in
[Architecture](ARCHITECTURE.md); this is the detail.

Only the modules worth calling from outside are listed. The game engine is Rust (`crates/`), and
Python reaches it through one module, `citar.engine_api`, below; the engine's own documentation is
its Rust API documentation (see [The game engine](#the-game-engine)).

---

## Paths and configuration

::: citar.paths

::: citar.settings
    options:
      members:
        - data_dir
        - get
        - reset
        - SettingsError

---

## The command line

::: citar.cli

::: citar.doctor
    options:
      members:
        - main
        - Report

---

## Setup

::: citar.wizard

::: citar.wizard.detect

::: citar.wizard.prompts

---

## The game engine

Everything in CITAR reaches the game through `citar.engine_api`, the facade over the engine's
extension `citar._engine`. The engine itself is the Rust workspace in
[`crates/`](https://github.com/jprodgers/CITAR/tree/main/crates): its API documentation is generated
from the source with `cargo doc --workspace --no-deps --open`, starting at the crate
[`citar-engine`](https://github.com/jprodgers/CITAR/blob/main/crates/citar-engine/src/lib.rs), whose
design is [crates/citar-engine/DESIGN.md](https://github.com/jprodgers/CITAR/blob/main/crates/citar-engine/DESIGN.md).

::: citar.engine_api
    options:
      show_source: false
      members:
        - rules_version
        - ruleset_counts
        - check_ruleset
        - build_info
        - tool_list
        - tool_kind
        - bot_instance
        - bot_versions
        - run_game
        - EngineGame

---

## Servers and costing

::: citar.servers
    options:
      members:
        - load
        - save
        - list_servers
        - get
        - upsert
        - delete
        - resolve_llm
        - seat_ref
        - restricted
        - default_server
        - default_model
        - empty_registry

::: citar.usage

::: citar.costing

---

## Hardware

::: citar.hwinfo
    options:
      members:
        - collect
        - guess_power

---

## Keys

::: citar.keystore
    options:
      members:
        - backends
        - store
        - get
        - delete
        - status
        - unlock
        - lock

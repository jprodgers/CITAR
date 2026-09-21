# Python reference

Generated from the source. The prose explanation of how the pieces fit is in
[Architecture](ARCHITECTURE.md); this is the detail.

Only the modules worth calling from outside are listed. The engine's internals are documented in
place — every module has a docstring saying what it is for — but a reference page for two hundred
rule functions would be a worse way to read them than the source.

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

::: citar.engine.rules
    options:
      members:
        - get_rules
        - RULES_VERSION

::: citar.engine.tools
    options:
      members:
        - tool
        - registry

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

//! Layer 3: `Game`, the caches that serve it, every rule system, and turn flow.
//!
//! All writes to `State` go through `game::mutate`; reads take `&self` and validate their memos
//! lazily; consequential writes happen only in settle (DESIGN.md 6). Rule systems may call each
//! other freely inside this layer.
//!
//! Replaces the rules in `citar/engine/`: `game.py`, `turns.py`, `economy.py`, `tiles.py`,
//! `cities.py`, `research.py`, `policies.py`, `religion.py`, `great_people.py`, `triggers.py`,
//! `ruins.py`, `visibility.py`, `units.py`, `movement.py`, `combat.py`, `conquest.py`,
//! `workers.py`, `actions.py`, `automation.py`, `diplomacy.py`, `espionage.py`,
//! `barbarians.py`, `city_states.py` and `victory.py`, and the production advisor in
//! `citar/bots/basic.py:777-875, 1146-1590` (DESIGN.md 3.3 maps each file to its module).

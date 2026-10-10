//! Diplomacy (`diplomacy.py:17-963`).
//!
//! - [`category`]: the kinds of diplomatic decision, and the category of each deal item
//!   (`diplomacy.py:34-57`);
//! - [`relations`]: war and peace, pacts, friendship, embassies, denouncements and opinions
//!   (`diplomacy.py:60-300`);
//! - [`deals`]: deal items read as callers write them, whether a side can give them, how they
//!   read, research agreements' cost, carrying a deal out, and the round's end for deals and
//!   agreements (`diplomacy.py:340-692`);
//! - [`negotiation`]: messages, and negotiations in the chat model of Phase 0, with the rule
//!   that no one ends a turn while one of theirs is open (`diplomacy.py:303-337, 694-963`);
//! - [`actions`]: the tools `send_message`, `open_negotiation`, `respond_negotiation`,
//!   `declare_war`, `denounce` and `end_turn` as typed actions.
//!
//! Package 1b-02 ported the relations a scenario needs; package 1c-05 the rest.

pub mod actions;
pub mod category;
pub mod deals;
pub mod negotiation;
pub mod relations;

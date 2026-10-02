//! Who decides each kind of diplomacy for a seat: the bot or the seat's language model
//! (DESIGN.md P2.3.8; `BasicBot.set_diplomacy`, `_llm` and `owns_negotiation`, basic.py:709-738).
//!
//! A hybrid seat hands some categories to its model; the bot leaves those alone, and a
//! negotiation whose proposal touches one is the model's to answer (a deal is answered whole).

use citar_engine::game::diplomacy::category::{CATEGORIES, Category, proposal_categories};
use citar_engine::state::diplo::Negotiation;
use serde_json::{Map, Value};

/// Who decides a category.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Owner {
    /// The bot: the default for every category not named.
    #[default]
    Bot,
    /// The seat's language model.
    Llm,
}

impl Owner {
    /// Its name: `bot`, `llm`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Llm => "llm",
        }
    }

    /// The owner called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Bot, Self::Llm].into_iter().find(|o| o.name() == name)
    }
}

/// Owners that do not parse, with `set_diplomacy`'s message (a `ValueError` in Python).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OwnersError(pub String);

/// The owner of each category, in [`CATEGORIES`] order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Owners([Owner; CATEGORIES.len()]);

fn slot(c: Category) -> usize {
    CATEGORIES.iter().position(|&x| x == c).unwrap_or_default()
}

impl Owners {
    /// The bot decides everything.
    pub const ALL_BOT: Self = Self([Owner::Bot; CATEGORIES.len()]);

    /// Owners from `{category: "bot" | "llm"}`, `bot` for any category not named; `null` is
    /// none named (`set_diplomacy`, basic.py:709-723).
    ///
    /// # Errors
    /// Not an object; an unknown category; an owner that is neither `bot` nor `llm`. The
    /// messages are Python's.
    pub fn from_json(v: &Value) -> Result<Self, OwnersError> {
        let empty = Map::new();
        let map = match v {
            Value::Null => &empty,
            Value::Object(m) => m,
            _ => {
                return Err(OwnersError(
                    "Diplomacy owners are an object of {category: \"bot\" | \"llm\"}.".to_owned(),
                ));
            }
        };
        let unknown: Vec<&str> =
            map.keys().map(String::as_str).filter(|k| Category::from_name(k).is_none()).collect();
        if !unknown.is_empty() {
            let all: Vec<&str> = CATEGORIES.iter().map(|c| c.name()).collect();
            return Err(OwnersError(format!(
                "Unknown diplomacy categor{} {} (the categories are {}).",
                if unknown.len() == 1 { "y" } else { "ies" },
                unknown.join(", "),
                all.join(", ")
            )));
        }
        let bad: Vec<&str> = map
            .iter()
            .filter(|(_, o)| o.as_str().and_then(Owner::from_name).is_none())
            .map(|(k, _)| k.as_str())
            .collect();
        if !bad.is_empty() {
            return Err(OwnersError(format!(
                "Diplomacy categories are owned by 'bot' or 'llm' ({} is neither).",
                bad.join(", ")
            )));
        }
        let mut out = Self::ALL_BOT;
        for (k, o) in map {
            if let (Some(c), Some(o)) =
                (Category::from_name(k), o.as_str().and_then(Owner::from_name))
            {
                out.set(c, o);
            }
        }
        Ok(out)
    }

    /// Hands category `c` to `owner`.
    pub fn set(&mut self, c: Category, owner: Owner) {
        self.0[slot(c)] = owner;
    }

    /// Who decides category `c`.
    #[must_use]
    pub fn of(&self, c: Category) -> Owner {
        self.0[slot(c)]
    }

    /// Whether the seat's language model decides category `c` (`_llm`).
    #[must_use]
    pub fn llm(&self, c: Category) -> bool {
        self.of(c) == Owner::Llm
    }

    /// Whether the bot answers negotiation `n` itself (`owns_negotiation`, basic.py:729-738): not
    /// when the proposal on the table touches a category the model owns, and, with no proposal,
    /// not when the model owns chat.
    #[must_use]
    pub fn owns(&self, n: &Negotiation) -> bool {
        let touched = proposal_categories(n.proposal.as_ref());
        if touched.is_empty() {
            return !self.llm(Category::Chat);
        }
        !touched.iter().any(|&c| self.llm(c))
    }

    /// `{category: owner}` for every category, in [`CATEGORIES`] order.
    #[must_use]
    pub fn to_json(&self) -> Value {
        CATEGORIES.iter().map(|&c| (c.name().to_owned(), Value::from(self.of(c).name()))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owners_parse_with_set_diplomacys_messages() {
        assert_eq!(Owners::from_json(&Value::Null), Ok(Owners::ALL_BOT));
        let o = Owners::from_json(&json!({"war": "llm", "trades": "bot"})).expect("valid");
        assert!(o.llm(Category::War) && !o.llm(Category::Trades) && !o.llm(Category::Chat));
        assert_eq!(o.to_json()["war"], "llm");
        let e = Owners::from_json(&json!({"wars": "llm"})).expect_err("unknown");
        assert_eq!(
            e.0,
            "Unknown diplomacy category wars (the categories are trades, agreements, peace, war, \
             denounce, un, city_states, espionage, captured_cities, chat)."
        );
        let e = Owners::from_json(&json!({"wars": "llm", "chats": "bot"})).expect_err("unknown");
        assert!(e.0.starts_with("Unknown diplomacy categories wars, chats ("), "{}", e.0);
        let e = Owners::from_json(&json!({"war": "human", "peace": "llm", "chat": 1}))
            .expect_err("bad owner");
        assert_eq!(e.0, "Diplomacy categories are owned by 'bot' or 'llm' (war, chat is neither).");
    }
}

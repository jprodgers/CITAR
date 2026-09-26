//! The kinds of diplomatic decision (`diplomacy.py:34-57`): what a hybrid seat may hand to a
//! language model while a bot plays the rest. Every deal item belongs to exactly one; `un` and
//! `captured_cities` are the engine's automatic decisions, and `denounce` and `chat` have no bot
//! logic behind them.

use crate::state::diplo::{DealItemKind, Terms};

/// A kind of diplomatic decision (`CATEGORIES`, `diplomacy.py:37-38`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Trades,
    Agreements,
    Peace,
    War,
    Denounce,
    Un,
    CityStates,
    Espionage,
    CapturedCities,
    Chat,
}

/// Every category, in Python's order.
pub const CATEGORIES: [Category; 10] = [
    Category::Trades,
    Category::Agreements,
    Category::Peace,
    Category::War,
    Category::Denounce,
    Category::Un,
    Category::CityStates,
    Category::Espionage,
    Category::CapturedCities,
    Category::Chat,
];

impl Category {
    /// Its name: `city_states`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Trades => "trades",
            Self::Agreements => "agreements",
            Self::Peace => "peace",
            Self::War => "war",
            Self::Denounce => "denounce",
            Self::Un => "un",
            Self::CityStates => "city_states",
            Self::Espionage => "espionage",
            Self::CapturedCities => "captured_cities",
            Self::Chat => "chat",
        }
    }

    /// The category called `name`, exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        CATEGORIES.into_iter().find(|c| c.name() == name)
    }
}

/// The category a deal item belongs to (`ITEM_CATEGORY`, `diplomacy.py:39-42`).
#[must_use]
pub const fn item_category(kind: DealItemKind) -> Category {
    match kind {
        DealItemKind::Gold
        | DealItemKind::GoldPerTurn
        | DealItemKind::Resource
        | DealItemKind::Tech
        | DealItemKind::ShareMap
        | DealItemKind::City => Category::Trades,
        DealItemKind::Embassy
        | DealItemKind::OpenBorders
        | DealItemKind::DeclarationOfFriendship
        | DealItemKind::ResearchAgreement
        | DealItemKind::DefensivePact => Category::Agreements,
        DealItemKind::PeaceTreaty => Category::Peace,
        DealItemKind::DeclareWar => Category::War,
    }
}

/// Every category the items of a proposal touch, on both sides, in [`CATEGORIES`] order
/// (`proposal_categories`, `diplomacy.py:55-57`); none for no proposal.
#[must_use]
pub fn proposal_categories(proposal: Option<&Terms>) -> Vec<Category> {
    let mut out: Vec<Category> = proposal
        .into_iter()
        .flat_map(|t| t.sides.iter())
        .flat_map(|s| s.items.iter())
        .map(|i| item_category(i.kind()))
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::ids::PlayerId;
    use crate::state::diplo::{DealItem, Side};

    #[test]
    fn every_item_has_one_category_and_names_round_trip() {
        for c in CATEGORIES {
            assert_eq!(Category::from_name(c.name()), Some(c));
        }
        let agreements = DealItemKind::ALL
            .into_iter()
            .filter(|&k| item_category(k) == Category::Agreements)
            .count();
        assert_eq!(agreements, 5);
        let t = Terms {
            sides: [
                Side { giver: PlayerId(0), items: vec![DealItem::Gold { amount: 5 }] },
                Side { giver: PlayerId(1), items: vec![DealItem::PeaceTreaty, DealItem::ShareMap] },
            ],
        };
        assert_eq!(proposal_categories(Some(&t)), [Category::Trades, Category::Peace]);
        assert!(proposal_categories(None).is_empty());
    }
}

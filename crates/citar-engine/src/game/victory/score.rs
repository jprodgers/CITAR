//! A civilization's score and the strength of its army (`victory.score` and
//! `victory.military_strength`, `victory.py:23-52`, UnCiv's `calculateScoreBreakdown`).

use serde_json::{Value, json};

use crate::base::ids::PlayerId;
use crate::base::num;
use crate::game::Game;
use crate::game::economy::city_tiles;

/// Where a civilization's score comes from (`victory.score`): each part rounded to a tenth, and
/// the total their sum, truncated.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Score {
    /// Ten per city.
    pub cities: f64,
    /// The ruleset's `score_from_population` per citizen.
    pub population: f64,
    /// One per land tile its cities own.
    pub tiles: f64,
    /// The ruleset's `score_from_wonders` per world wonder.
    pub wonders: i64,
    /// Four per tech.
    pub technologies: f64,
    /// Ten per future tech.
    pub future_tech: f64,
    pub total: i32,
}

impl Score {
    /// Python's dict: the parts, then the total.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "cities": self.cities,
            "population": self.population,
            "tiles": self.tiles,
            "wonders": self.wonders,
            "technologies": self.technologies,
            "future_tech": self.future_tech,
            "total": self.total,
        })
    }
}

/// A civilization's score, broken down by where it comes from (`victory.score`,
/// `victory.py:23-43`). Cities, citizens and land count for more on maps smaller than 1276 tiles
/// (a third of the ratio's excess over one), for the same on larger ones.
#[must_use]
pub fn score(g: &Game, p: PlayerId) -> Score {
    let r = g.rules();
    let f = &r.constants().formulas;
    let mut msm = 1276.0 / f64::from(g.grid().size().max(1));
    if msm > 1.0 {
        msm = (msm - 1.0) / 3.0 + 1.0;
    }
    let (mut cities, mut pop, mut land, mut wonders) = (0_u32, 0_u32, 0_u32, 0_u32);
    for c in g.player_cities(p) {
        cities += 1;
        pop += u32::from(c.pop);
        let own = city_tiles(g, c.id()).into_iter().filter(|&t| !g.is_water(t)).count();
        land += num::saturate_u32(own);
        wonders +=
            num::saturate_u32(c.buildings.iter().filter(|&b| r.buildings()[b].is_wonder).count());
    }
    let (techs, future) = g
        .player(p)
        .map_or((0, 0), |x| (num::saturate_u32(x.tech.known.len()), x.tech.future_techs));
    let tenth = |x: f64| num::round_ndigits(x, 1);
    let mut s = Score {
        cities: tenth(f64::from(cities) * 10.0 * msm),
        population: tenth(f64::from(pop) * f64::from(f.score_from_population) * msm),
        tiles: tenth(f64::from(land) * msm),
        wonders: i64::from(f.score_from_wonders) * i64::from(wonders),
        technologies: tenth(f64::from(techs) * 4.0),
        future_tech: tenth(f64::from(future) * 10.0),
        total: 0,
    };
    // Python summed the parts in order, from an integer 0; the wonders' part is a whole number
    // far below 2^53.
    #[allow(clippy::cast_precision_loss, reason = "the wonders' part is small")]
    let sum = s.cities + s.population + s.tiles + s.wonders as f64 + s.technologies + s.future_tech;
    s.total = num::trunc_i32(sum);
    s
}

/// The living major civilization with the highest score, the first of equals by id
/// (`max(g.majors(), key=score)`, `victory.py:269, 359`).
#[must_use]
pub fn best_score(g: &Game) -> Option<PlayerId> {
    let mut best: Option<(i32, PlayerId)> = None;
    for pl in g.majors(true) {
        let s = score(g, pl.id()).total;
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, pl.id()));
        }
    }
    best.map(|(_, p)| p)
}

/// The combined strength of a civilization's army (`victory.military_strength`,
/// `victory.py:46-52`): each unit's strength, or ranged strength if greater, by its health. A
/// city-state's fear of a major reads it too (`city_states.tribute_modifiers`).
#[must_use]
pub fn military_strength(g: &Game, p: PlayerId) -> i32 {
    let r = g.rules();
    let total: f64 = g
        .player_units(p)
        .map(|u| {
            let d = &r.base_units()[u.base];
            f64::from(d.strength.max(d.ranged_strength)) * f64::from(u.hp) / 100.0
        })
        .sum();
    num::trunc_i32(total)
}
